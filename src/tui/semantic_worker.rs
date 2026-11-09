use crate::error::Result;
use crate::history::Conversation;
use crate::search::literal::{build_literal_corpus, exact_fallback};
use crate::search::query::ParsedQuery;
use crate::semantic::cache::write_embedding_cache;
use crate::semantic::fastembed::FastembedEmbedder;
use crate::semantic::index::{
    SemanticIndexCandidate, SemanticIndexProgress, SemanticIndexRequest, SemanticIndexResponse,
    SemanticIndexState,
};
use crate::semantic::types::{
    SemanticCancellationToken, SemanticChunkIdentity, SemanticExplanation, SemanticQuality,
    SemanticRationaleKind, SemanticScoreBreakdown,
};
use crate::tui::app::{SemanticProgress, SemanticResultMetadata};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc;

#[derive(Clone)]
pub enum SemanticWorkerCommand {
    UpdateCorpus {
        corpus_version: u64,
        conversations: Arc<Vec<Arc<Conversation>>>,
    },
    UpdateScope {
        corpus_version: u64,
        scope_version: u64,
        indices: Arc<Vec<usize>>,
    },
    Search {
        generation: u64,
        query: ParsedQuery,
        corpus_version: u64,
        scope_version: u64,
        prewarm: bool,
    },
}

#[derive(Clone)]
struct SemanticSearchRequest {
    generation: u64,
    query: ParsedQuery,
    corpus_version: u64,
    scope_version: u64,
    prewarm: bool,
}

impl SemanticSearchRequest {
    fn is_noop(&self) -> bool {
        self.query.is_effectively_empty() && !self.prewarm
    }
}

pub enum SemanticSearchMessage {
    Progress {
        generation: u64,
        progress: SemanticProgress,
    },
    Partial(SemanticSearchResponse),
    Complete(SemanticSearchResponse),
}

pub struct SemanticSearchResponse {
    pub generation: u64,
    pub filtered: Vec<usize>,
    pub metadata: HashMap<usize, SemanticResultMetadata>,
    pub error: Option<String>,
    pub progress: SemanticProgress,
    pub prewarm: bool,
}

pub fn spawn_semantic_worker() -> (
    mpsc::Sender<SemanticWorkerCommand>,
    mpsc::Receiver<SemanticSearchMessage>,
) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<SemanticWorkerCommand>();
    let (res_tx, res_rx) = mpsc::channel::<SemanticSearchMessage>();

    std::thread::Builder::new()
        .name("semantic-search-worker".into())
        .spawn(move || run_semantic_worker(cmd_rx, res_tx))
        .expect("failed to spawn semantic search worker thread");

    (cmd_tx, res_rx)
}

fn run_semantic_worker(
    cmd_rx: mpsc::Receiver<SemanticWorkerCommand>,
    res_tx: mpsc::Sender<SemanticSearchMessage>,
) {
    let mut worker = SemanticWorkerState::default();
    let mut state = SemanticIndexState::new();
    let mut embedder: Option<FastembedEmbedder> = None;
    let mut cancellation = SemanticCancellationToken::new();

    let mut queued = VecDeque::new();
    while let Some(command) = queued.pop_front().or_else(|| cmd_rx.recv().ok()) {
        let mut request = worker.apply_command(command, &cancellation);
        while let Some(pending) = queued.pop_front().or_else(|| cmd_rx.try_recv().ok()) {
            if let Some(search) = worker.apply_command(pending, &cancellation) {
                request = Some(search);
            }
        }

        let Some(request) = request else {
            continue;
        };
        let Some(version_state) = worker.version_state_for(&request) else {
            continue;
        };
        match version_state {
            VersionState::Future => {
                worker.pending_search = Some(request);
                continue;
            }
            VersionState::Stale => continue,
            VersionState::Current => {}
        }

        cancellation = SemanticCancellationToken::new();
        if request.is_noop() {
            let _ = res_tx.send(SemanticSearchMessage::Complete(SemanticSearchResponse {
                generation: request.generation,
                filtered: Vec::new(),
                metadata: HashMap::new(),
                error: None,
                progress: if request.prewarm {
                    SemanticProgress::CacheReady
                } else {
                    SemanticProgress::Idle
                },
                prewarm: request.prewarm,
            }));
            continue;
        }
        if request.query.is_quoted_only() {
            let response = exact_literal_semantic_response(
                request.generation,
                request.prewarm,
                worker.corpus.as_ref(),
                worker.scope.as_ref(),
                &request.query,
            );
            let _ = res_tx.send(SemanticSearchMessage::Complete(response));
            continue;
        }
        let full_corpus = worker.full_corpus_candidates();
        let scope = worker.semantic_candidates();
        let index_request = SemanticIndexRequest {
            query: request.query.semantic_text(),
            literal_filters: request.query.literals(),
            full_corpus: &full_corpus,
            scope: &scope,
            corpus_version: request.corpus_version,
            prewarm: request.prewarm,
        };
        let response = match state.has_chunks(&index_request, &cancellation) {
            Ok(true) => {
                if embedder.is_none() {
                    let _ = res_tx.send(SemanticSearchMessage::Progress {
                        generation: request.generation,
                        progress: SemanticProgress::InitializingModel,
                    });
                    embedder = match FastembedEmbedder::new_quiet() {
                        Ok(embedder) => Some(embedder),
                        Err(error) => {
                            let _ = res_tx.send(SemanticSearchMessage::Complete(
                                failed_semantic_response(
                                    request.generation,
                                    request.prewarm,
                                    error.to_string(),
                                ),
                            ));
                            continue;
                        }
                    };
                }
                rank_or_prewarm_semantic_request(
                    request.generation,
                    &index_request,
                    &request.query,
                    &mut state,
                    embedder.as_mut().unwrap(),
                    &cancellation,
                    &mut SemanticWorkerIo {
                        results: &res_tx,
                        commands: &cmd_rx,
                        queued: &mut queued,
                        persist: write_embedding_cache,
                    },
                )
                .unwrap_or_else(|error| {
                    failed_semantic_response(request.generation, request.prewarm, error.to_string())
                })
            }
            Ok(false) => match state.clear_empty(&index_request, &cancellation) {
                Ok(()) => empty_semantic_response(request.generation, request.prewarm),
                Err(error) => {
                    failed_semantic_response(request.generation, request.prewarm, error.to_string())
                }
            },
            Err(error) => {
                failed_semantic_response(request.generation, request.prewarm, error.to_string())
            }
        };

        let _ = res_tx.send(SemanticSearchMessage::Complete(response));
    }
}

#[derive(Default)]
struct SemanticWorkerState {
    corpus_version: u64,
    scope_version: u64,
    scope_corpus_version: u64,
    corpus: Arc<Vec<Arc<Conversation>>>,
    scope: Arc<Vec<usize>>,
    pending_search: Option<SemanticSearchRequest>,
}

enum VersionState {
    Current,
    Future,
    Stale,
}

impl SemanticWorkerState {
    fn apply_command(
        &mut self,
        command: SemanticWorkerCommand,
        cancellation: &SemanticCancellationToken,
    ) -> Option<SemanticSearchRequest> {
        match command {
            SemanticWorkerCommand::UpdateCorpus {
                corpus_version,
                conversations,
            } => {
                if corpus_version >= self.corpus_version {
                    self.corpus_version = corpus_version;
                    self.corpus = conversations;
                    cancellation.cancel();
                }
                self.take_ready_pending()
            }
            SemanticWorkerCommand::UpdateScope {
                corpus_version,
                scope_version,
                indices,
            } => {
                if scope_version >= self.scope_version {
                    self.scope_corpus_version = corpus_version;
                    self.scope_version = scope_version;
                    self.scope = indices;
                    cancellation.cancel();
                }
                self.take_ready_pending()
            }
            SemanticWorkerCommand::Search {
                generation,
                query,
                corpus_version,
                scope_version,
                prewarm,
            } => {
                cancellation.cancel();
                Some(SemanticSearchRequest {
                    generation,
                    query,
                    corpus_version,
                    scope_version,
                    prewarm,
                })
            }
        }
    }

    fn take_ready_pending(&mut self) -> Option<SemanticSearchRequest> {
        let request = self.pending_search.take()?;
        match self.version_state_for(&request) {
            Some(VersionState::Current) | Some(VersionState::Stale) => Some(request),
            Some(VersionState::Future) | None => {
                self.pending_search = Some(request);
                None
            }
        }
    }

    fn version_state_for(&self, request: &SemanticSearchRequest) -> Option<VersionState> {
        if self.scope_corpus_version != self.corpus_version {
            return Some(VersionState::Future);
        }
        if self.corpus_version == request.corpus_version
            && self.scope_version == request.scope_version
        {
            Some(VersionState::Current)
        } else if self.corpus_version < request.corpus_version
            || self.scope_version < request.scope_version
        {
            Some(VersionState::Future)
        } else {
            Some(VersionState::Stale)
        }
    }

    fn full_corpus_candidates(&self) -> Vec<SemanticIndexCandidate> {
        self.corpus
            .iter()
            .enumerate()
            .map(|(index, conversation)| SemanticIndexCandidate {
                index,
                source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                conversation: conversation.clone(),
            })
            .collect()
    }

    fn semantic_candidates(&self) -> Vec<SemanticIndexCandidate> {
        self.scope
            .iter()
            .filter_map(|&index| {
                self.corpus
                    .get(index)
                    .map(|conversation| SemanticIndexCandidate {
                        index,
                        source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                        conversation: conversation.clone(),
                    })
            })
            .collect()
    }
}

struct SemanticWorkerIo<'a> {
    results: &'a mpsc::Sender<SemanticSearchMessage>,
    commands: &'a mpsc::Receiver<SemanticWorkerCommand>,
    queued: &'a mut VecDeque<SemanticWorkerCommand>,
    persist: fn(&crate::semantic::types::EmbeddingCache),
}

fn rank_or_prewarm_semantic_request(
    generation: u64,
    request: &SemanticIndexRequest<'_>,
    parsed: &ParsedQuery,
    state: &mut SemanticIndexState,
    embedder: &mut dyn crate::semantic::embed::SemanticEmbedder,
    cancellation: &SemanticCancellationToken,
    io: &mut SemanticWorkerIo<'_>,
) -> Result<SemanticSearchResponse> {
    if let Some(cached) = state.rank_cached(request, embedder, cancellation)? {
        let _ = io
            .results
            .send(SemanticSearchMessage::Partial(semantic_search_response(
                generation,
                cached,
                request.full_corpus,
                parsed,
            )));
    }
    // Reuse a query vector to publish newly indexed matches too, including
    // the first batch of a completely cold index. Do not wait for a new query.
    let vector = if request.prewarm {
        None
    } else {
        embedder.embed_query(request.query)?
    };
    let config = state.chunk_config;
    let results = io.results;
    let persist = io.persist;
    let progress_state = std::cell::Cell::new(SemanticIndexProgress::CacheReady);
    let mut last_update = None::<std::time::Instant>;
    let response = state.refresh_or_prewarm(
        request,
        embedder,
        cancellation,
        |progress| {
            progress_state.set(progress);
            interrupt_for_queued_commands(io.commands, io.queued, cancellation);
            let _ = results.send(SemanticSearchMessage::Progress {
                generation,
                progress: semantic_progress(progress),
            });
        },
        |cache| {
            persist(cache);
            if cancellation.is_cancelled()
                || last_update
                    .is_some_and(|time| time.elapsed() < std::time::Duration::from_secs(2))
            {
                return;
            }
            if let Some(vector) = vector.as_deref()
                && let Ok(Some(mut partial)) = SemanticIndexState::rank_cached_snapshot(
                    request,
                    cache,
                    config,
                    vector,
                    cancellation,
                )
            {
                partial.progress = progress_state.get();
                let _ = results.send(SemanticSearchMessage::Partial(semantic_search_response(
                    generation,
                    partial,
                    request.full_corpus,
                    parsed,
                )));
                last_update = Some(std::time::Instant::now());
            }
        },
    )?;
    Ok(semantic_search_response(
        generation,
        response,
        request.full_corpus,
        parsed,
    ))
}

// Poll between bounded embedding batches, rather than leaving new queries
// stuck behind a full-history rebuild. Completed batches remain cached.
fn interrupt_for_queued_commands(
    rx: &mpsc::Receiver<SemanticWorkerCommand>,
    queued: &mut VecDeque<SemanticWorkerCommand>,
    cancellation: &SemanticCancellationToken,
) {
    loop {
        match rx.try_recv() {
            Ok(command) => queued.push_back(command),
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                cancellation.cancel();
                break;
            }
        }
    }
    if !queued.is_empty() {
        cancellation.cancel();
    }
}

fn semantic_search_response(
    generation: u64,
    response: SemanticIndexResponse,
    _conversations: &[SemanticIndexCandidate],
    _parsed: &ParsedQuery,
) -> SemanticSearchResponse {
    let filtered = response
        .hits
        .iter()
        .map(|hit| hit.conversation_index)
        .collect::<Vec<_>>();
    let metadata = response
        .hits
        .into_iter()
        .map(|hit| {
            let explanation = hit.explanation;
            (
                hit.conversation_index,
                SemanticResultMetadata {
                    score_breakdown: hit.score_breakdown,
                    explanation,
                },
            )
        })
        .collect();

    SemanticSearchResponse {
        generation,
        filtered,
        metadata,
        error: None,
        progress: semantic_progress(response.progress),
        prewarm: response.prewarm,
    }
}

fn empty_semantic_response(generation: u64, prewarm: bool) -> SemanticSearchResponse {
    SemanticSearchResponse {
        generation,
        filtered: Vec::new(),
        metadata: HashMap::new(),
        error: None,
        progress: SemanticProgress::EmptyCorpus,
        prewarm,
    }
}

fn exact_literal_semantic_response(
    generation: u64,
    prewarm: bool,
    conversations: &[Arc<Conversation>],
    scope: &[usize],
    parsed: &ParsedQuery,
) -> SemanticSearchResponse {
    let plain_conversations = conversations
        .iter()
        .map(|conversation| conversation.as_ref().clone())
        .collect::<Vec<_>>();
    let corpus = build_literal_corpus(&plain_conversations);
    let scope = scope
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    let filtered = exact_fallback(&plain_conversations, &corpus, parsed.literals(), |index| {
        scope.contains(&index)
    });

    let metadata = filtered
        .iter()
        .filter_map(|index| {
            conversations
                .get(*index)
                .map(|conversation| (*index, exact_literal_metadata(conversation, parsed)))
        })
        .collect();

    SemanticSearchResponse {
        generation,
        filtered,
        metadata,
        error: None,
        progress: SemanticProgress::Complete,
        prewarm,
    }
}

fn literal_evidence_preview(
    conversation: &Conversation,
    parsed: &ParsedQuery,
    visible_preview: &str,
) -> String {
    parsed
        .literals()
        .iter()
        .filter(|literal| !literal.matches(visible_preview))
        .chain(parsed.literals().iter())
        .find_map(|literal| {
            literal
                .match_ranges(&conversation.full_text)
                .first()
                .map(|range| evidence_window(&conversation.full_text, *range))
                .or_else(|| {
                    conversation.project_name.as_deref().and_then(|project| {
                        literal
                            .match_ranges(project)
                            .first()
                            .map(|range| evidence_window(project, *range))
                    })
                })
        })
        .unwrap_or_else(|| conversation.preview.clone())
}

fn evidence_window(text: &str, range: (usize, usize)) -> String {
    const CONTEXT_CHARS: usize = 80;
    let (start, end) = range;
    let prefix_start = text[..start]
        .char_indices()
        .rev()
        .nth(CONTEXT_CHARS)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let suffix_end = text[end..]
        .char_indices()
        .nth(CONTEXT_CHARS)
        .map(|(index, _)| end + index)
        .unwrap_or(text.len());
    let mut preview = String::new();
    if prefix_start > 0 {
        preview.push('…');
    }
    preview.push_str(text[prefix_start..suffix_end].trim());
    if suffix_end < text.len() {
        preview.push('…');
    }
    preview
}

fn exact_literal_metadata(
    conversation: &Conversation,
    parsed: &ParsedQuery,
) -> SemanticResultMetadata {
    SemanticResultMetadata {
        score_breakdown: SemanticScoreBreakdown {
            hybrid: 0.0,
            semantic: 0.0,
            lexical: 0.0,
        },
        explanation: SemanticExplanation {
            quality: SemanticQuality::Strong,
            quality_label: "exact",
            matched_terms: parsed
                .literals()
                .iter()
                .map(|literal| literal.text().to_string())
                .collect(),
            evidence_preview: literal_evidence_preview(conversation, parsed, ""),
            rationale_kind: SemanticRationaleKind::LexicalBoosted,
            chunk: SemanticChunkIdentity {
                conversation_index: conversation.index,
                source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                session: conversation
                    .path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("?")
                    .to_string(),
                chunk_index: 0,
                message_range: crate::agent::refs::MessageRange::single(1),
            },
        },
    }
}

fn failed_semantic_response(
    generation: u64,
    prewarm: bool,
    error: String,
) -> SemanticSearchResponse {
    SemanticSearchResponse {
        generation,
        filtered: Vec::new(),
        metadata: HashMap::new(),
        error: Some(error),
        progress: SemanticProgress::Failed,
        prewarm,
    }
}

fn semantic_progress(progress: SemanticIndexProgress) -> SemanticProgress {
    match progress {
        SemanticIndexProgress::Embedding { completed, total } => {
            SemanticProgress::Embedding { completed, total }
        }
        SemanticIndexProgress::CacheReady => SemanticProgress::CacheReady,
        SemanticIndexProgress::Ranking => SemanticProgress::Ranking,
        SemanticIndexProgress::Complete => SemanticProgress::Complete,
        SemanticIndexProgress::EmptyCorpus => SemanticProgress::EmptyCorpus,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::Conversation;
    use crate::search::normalize_for_search;
    use crate::semantic::types::{
        SemanticChunkIdentity, SemanticExplanation, SemanticQuality, SemanticRationaleKind,
        SemanticScoreBreakdown,
    };
    use chrono::{Duration as ChronoDuration, Local};
    use std::path::PathBuf;
    use std::time::Duration;

    fn conversation(path: &str, semantic_turns: Vec<&str>) -> Conversation {
        Conversation {
            path: PathBuf::from(path),
            index: 0,
            timestamp: Local::now(),
            preview: "preview sentinel".to_string(),
            preview_first: "preview sentinel".to_string(),
            preview_last: "preview sentinel".to_string(),
            full_text:
                "title sentinel summary sentinel cwd sentinel project sentinel tool output sentinel"
                    .to_string(),
            agent_search_text: String::new(),
            semantic_turn_ranges: (1..=semantic_turns.len())
                .map(crate::agent::refs::MessageRange::single)
                .collect(),
            semantic_turns: semantic_turns.into_iter().map(str::to_string).collect(),
            search_text_lower: normalize_for_search(
                "title sentinel summary sentinel cwd sentinel project sentinel tool output sentinel",
            ),
            project_name: Some("project sentinel".to_string()),
            project_path: Some(PathBuf::from("/projects/project-sentinel")),
            cwd: Some(PathBuf::from("/cwd/sentinel")),
            parent_session: None,
            message_count: 1,
            parse_errors: Vec::new(),
            summary: Some("summary sentinel".to_string()),
            custom_title: Some("title sentinel".to_string()),
            model: None,
            total_tokens: 0,
            duration_minutes: None,
        }
    }

    #[test]
    fn completely_cold_index_publishes_first_batch_before_finishing() {
        struct Fake {
            passage_calls: usize,
        }
        impl crate::semantic::embed::SemanticEmbedder for Fake {
            fn embed_passages(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
                self.passage_calls += 1;
                Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
            }
            fn embed_query(&mut self, _: &str) -> Result<Option<Vec<f32>>> {
                Ok(Some(vec![1.0, 0.0]))
            }
        }
        let candidates = (0..9)
            .map(|index| SemanticIndexCandidate {
                index,
                source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                conversation: Arc::new(conversation(
                    &format!("/cold-{index}.jsonl"),
                    vec!["visible alpha"],
                )),
            })
            .collect::<Vec<_>>();
        let parsed = ParsedQuery::parse("alpha");
        let request = SemanticIndexRequest {
            query: "alpha",
            literal_filters: &[],
            full_corpus: &candidates,
            scope: &candidates,
            corpus_version: 1,
            prewarm: false,
        };
        let config = crate::semantic::types::ChunkConfig::default();
        let mut state = SemanticIndexState::with_cache(
            config,
            crate::semantic::cache::empty_embedding_cache(config),
        );
        let mut fake = Fake { passage_calls: 0 };
        let (result_tx, result_rx) = mpsc::channel();
        let (_command_tx, command_rx) = mpsc::channel();
        let mut queued = VecDeque::new();
        let full = rank_or_prewarm_semantic_request(
            1,
            &request,
            &parsed,
            &mut state,
            &mut fake,
            &SemanticCancellationToken::new(),
            &mut SemanticWorkerIo {
                results: &result_tx,
                commands: &command_rx,
                queued: &mut queued,
                persist: |_| {},
            },
        )
        .unwrap();
        assert_eq!(full.filtered.len(), 9);
        assert_eq!(fake.passage_calls, 2);
        let partials = result_rx
            .try_iter()
            .filter_map(|message| match message {
                SemanticSearchMessage::Partial(response) => Some(response.filtered.len()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(partials, vec![8]);
    }

    fn corpus(conversations: Vec<Conversation>) -> Arc<Vec<Arc<Conversation>>> {
        Arc::new(conversations.into_iter().map(Arc::new).collect())
    }

    #[test]
    fn maps_domain_hits_to_original_indices_and_metadata() {
        let response = semantic_search_response(
            7,
            SemanticIndexResponse {
                hits: vec![crate::semantic::types::SemanticHit::new(
                    SemanticScoreBreakdown {
                        hybrid: 1.2,
                        semantic: 1.0,
                        lexical: 0.2,
                    },
                    SemanticExplanation {
                        quality: SemanticQuality::Strong,
                        quality_label: "strong",
                        matched_terms: vec!["beta".to_string()],
                        evidence_preview: "visible beta".to_string(),
                        rationale_kind: SemanticRationaleKind::LexicalBoosted,
                        chunk: SemanticChunkIdentity {
                            conversation_index: 42,
                            source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                            session: "session-b".to_string(),
                            chunk_index: 0,
                            message_range: crate::agent::refs::MessageRange::single(1),
                        },
                    },
                )],
                chunk_hits: Vec::new(),
                indexed_chunk_count: 1,
                query_embedding_returned: true,
                progress: SemanticIndexProgress::Complete,
                prewarm: false,
            },
            &[],
            &ParsedQuery::parse("beta"),
        );

        assert_eq!(response.generation, 7);
        assert_eq!(response.filtered, vec![42]);
        assert_eq!(response.progress, SemanticProgress::Complete);
        let metadata = &response.metadata[&42];
        assert_eq!(metadata.score_breakdown.hybrid, 1.2);
        assert_eq!(metadata.explanation.evidence_preview, "visible beta");
        assert_eq!(metadata.explanation.chunk.conversation_index, 42);
    }

    #[test]
    fn empty_visible_dialogue_returns_before_embedder_initialization() {
        let (tx, rx) = spawn_semantic_worker();
        tx.send(SemanticWorkerCommand::UpdateCorpus {
            corpus_version: 1,
            conversations: corpus(vec![conversation(
                "/projects/project-a/session-a.jsonl",
                vec![],
            )]),
        })
        .expect("send corpus");
        tx.send(SemanticWorkerCommand::UpdateScope {
            corpus_version: 1,
            scope_version: 1,
            indices: Arc::new(vec![0]),
        })
        .expect("send scope");
        tx.send(SemanticWorkerCommand::Search {
            generation: 1,
            query: ParsedQuery::parse("alpha"),
            corpus_version: 1,
            scope_version: 1,
            prewarm: false,
        })
        .expect("send semantic request");
        let message = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("empty response");

        match message {
            SemanticSearchMessage::Partial(_) => panic!("unexpected partial response"),
            SemanticSearchMessage::Complete(response) => {
                assert!(response.filtered.is_empty());
                assert!(response.metadata.is_empty());
                assert_eq!(response.error, None);
                assert_eq!(response.progress, SemanticProgress::EmptyCorpus);
            }
            SemanticSearchMessage::Progress { progress, .. } => {
                panic!("unexpected progress before empty response: {progress:?}");
            }
        }
    }

    #[test]
    fn empty_quoted_search_returns_idle_without_embedding() {
        let (tx, rx) = spawn_semantic_worker();
        tx.send(SemanticWorkerCommand::UpdateCorpus {
            corpus_version: 1,
            conversations: corpus(vec![conversation(
                "/projects/project-a/session-a.jsonl",
                vec!["visible dialogue"],
            )]),
        })
        .expect("send corpus");
        tx.send(SemanticWorkerCommand::UpdateScope {
            corpus_version: 1,
            scope_version: 1,
            indices: Arc::new(vec![0]),
        })
        .expect("send scope");
        tx.send(SemanticWorkerCommand::Search {
            generation: 1,
            query: ParsedQuery::parse("\"\""),
            corpus_version: 1,
            scope_version: 1,
            prewarm: false,
        })
        .expect("send semantic request");
        let message = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("empty response");

        match message {
            SemanticSearchMessage::Partial(_) => panic!("unexpected partial response"),
            SemanticSearchMessage::Complete(response) => {
                assert!(response.filtered.is_empty());
                assert!(response.metadata.is_empty());
                assert_eq!(response.error, None);
                assert_eq!(response.progress, SemanticProgress::Idle);
            }
            SemanticSearchMessage::Progress { progress, .. } => {
                panic!("unexpected progress before empty response: {progress:?}");
            }
        }
    }

    #[test]
    fn empty_prewarm_search_builds_cache_without_idle_short_circuit() {
        let (tx, rx) = spawn_semantic_worker();
        tx.send(SemanticWorkerCommand::UpdateCorpus {
            corpus_version: 1,
            conversations: corpus(vec![conversation(
                "/projects/project-a/session-a.jsonl",
                vec!["visible dialogue"],
            )]),
        })
        .expect("send corpus");
        tx.send(SemanticWorkerCommand::UpdateScope {
            corpus_version: 1,
            scope_version: 1,
            indices: Arc::new(vec![0]),
        })
        .expect("send scope");
        tx.send(SemanticWorkerCommand::Search {
            generation: 1,
            query: ParsedQuery::parse("\"\""),
            corpus_version: 1,
            scope_version: 1,
            prewarm: true,
        })
        .expect("send semantic prewarm request");

        loop {
            match rx
                .recv_timeout(Duration::from_secs(5))
                .expect("prewarm response")
            {
                SemanticSearchMessage::Progress { .. } => continue,
                SemanticSearchMessage::Partial(_) => panic!("unexpected partial response"),
                SemanticSearchMessage::Complete(response) => {
                    assert!(response.prewarm);
                    assert!(response.filtered.is_empty());
                    assert!(response.metadata.is_empty());
                    assert_eq!(response.error, None);
                    assert_ne!(response.progress, SemanticProgress::Idle);
                    break;
                }
            }
        }
    }

    #[test]
    fn quoted_only_search_uses_literal_fallback_without_embedding() {
        let (tx, rx) = spawn_semantic_worker();
        tx.send(SemanticWorkerCommand::UpdateCorpus {
            corpus_version: 1,
            conversations: corpus(vec![conversation(
                "/projects/project-a/session-a.jsonl",
                vec![],
            )]),
        })
        .expect("send corpus");
        tx.send(SemanticWorkerCommand::UpdateScope {
            corpus_version: 1,
            scope_version: 1,
            indices: Arc::new(vec![0]),
        })
        .expect("send scope");
        tx.send(SemanticWorkerCommand::Search {
            generation: 1,
            query: ParsedQuery::parse("\"title sentinel\""),
            corpus_version: 1,
            scope_version: 1,
            prewarm: false,
        })
        .expect("send semantic request");
        let message = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("literal response");

        match message {
            SemanticSearchMessage::Partial(_) => panic!("unexpected partial response"),
            SemanticSearchMessage::Complete(response) => {
                assert_eq!(response.filtered, vec![0]);
                assert!(
                    response.metadata[&0]
                        .explanation
                        .evidence_preview
                        .contains("title sentinel")
                );
                assert_eq!(response.error, None);
                assert_eq!(response.progress, SemanticProgress::Complete);
            }
            SemanticSearchMessage::Progress { progress, .. } => {
                panic!("unexpected progress before literal response: {progress:?}");
            }
        }
    }

    #[test]
    fn quoted_only_search_respects_scope_and_orders_newest_first() {
        let mut old = conversation("/projects/project-a/session-a.jsonl", vec![]);
        old.timestamp = Local::now() - ChronoDuration::days(1);
        let mut newest = conversation("/projects/project-a/session-b.jsonl", vec![]);
        newest.timestamp = Local::now();
        let mut hidden = conversation("/projects/project-a/session-c.jsonl", vec![]);
        hidden.timestamp = Local::now() + ChronoDuration::days(1);
        let (tx, rx) = spawn_semantic_worker();
        tx.send(SemanticWorkerCommand::UpdateCorpus {
            corpus_version: 1,
            conversations: corpus(vec![old, newest, hidden]),
        })
        .expect("send corpus");
        tx.send(SemanticWorkerCommand::UpdateScope {
            corpus_version: 1,
            scope_version: 1,
            indices: Arc::new(vec![0, 1]),
        })
        .expect("send scope");
        tx.send(SemanticWorkerCommand::Search {
            generation: 1,
            query: ParsedQuery::parse("\"title sentinel\""),
            corpus_version: 1,
            scope_version: 1,
            prewarm: false,
        })
        .expect("send semantic request");

        match rx
            .recv_timeout(Duration::from_secs(2))
            .expect("literal response")
        {
            SemanticSearchMessage::Partial(_) => panic!("unexpected partial response"),
            SemanticSearchMessage::Complete(response) => {
                assert_eq!(response.filtered, vec![1, 0]);
                assert!(
                    response.metadata[&1]
                        .explanation
                        .evidence_preview
                        .contains("title sentinel")
                );
                assert!(
                    response.metadata[&0]
                        .explanation
                        .evidence_preview
                        .contains("title sentinel")
                );
            }
            SemanticSearchMessage::Progress { progress, .. } => {
                panic!("unexpected progress before literal response: {progress:?}");
            }
        }
    }

    #[test]
    fn worker_builds_distinct_full_corpus_and_scoped_candidates() {
        let mut worker = SemanticWorkerState::default();
        let cancellation = SemanticCancellationToken::new();
        worker.apply_command(
            SemanticWorkerCommand::UpdateCorpus {
                corpus_version: 1,
                conversations: corpus(vec![
                    conversation("/projects/project-a/session-a.jsonl", vec!["hidden"]),
                    conversation("/projects/project-a/session-b.jsonl", vec!["visible"]),
                ]),
            },
            &cancellation,
        );
        worker.apply_command(
            SemanticWorkerCommand::UpdateScope {
                corpus_version: 1,
                scope_version: 1,
                indices: Arc::new(vec![1]),
            },
            &cancellation,
        );

        let full_corpus = worker.full_corpus_candidates();
        let candidates = worker.semantic_candidates();

        assert_eq!(full_corpus.len(), 2);
        assert_eq!(full_corpus[0].index, 0);
        assert_eq!(full_corpus[1].index, 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].index, 1);
        assert_eq!(candidates[0].conversation.semantic_turns, vec!["visible"]);
    }

    #[test]
    fn stale_search_is_not_current_after_scope_advances() {
        let mut worker = SemanticWorkerState::default();
        let cancellation = SemanticCancellationToken::new();
        worker.apply_command(
            SemanticWorkerCommand::UpdateCorpus {
                corpus_version: 1,
                conversations: corpus(vec![conversation(
                    "/projects/project-a/session-a.jsonl",
                    vec!["visible"],
                )]),
            },
            &cancellation,
        );
        worker.apply_command(
            SemanticWorkerCommand::UpdateScope {
                corpus_version: 1,
                scope_version: 2,
                indices: Arc::new(vec![0]),
            },
            &cancellation,
        );
        let request = SemanticSearchRequest {
            generation: 1,
            query: ParsedQuery::parse("visible"),
            corpus_version: 1,
            scope_version: 1,
            prewarm: false,
        };

        assert!(matches!(
            worker.version_state_for(&request),
            Some(VersionState::Stale)
        ));
    }
}

#[cfg(test)]
mod prewarm_regressions {
    use super::*;
    #[test]
    fn empty_prewarm_must_reach_indexing_not_report_fake_readiness() {
        let mut request = SemanticSearchRequest {
            generation: 1,
            query: ParsedQuery::parse(""),
            corpus_version: 1,
            scope_version: 1,
            prewarm: true,
        };
        assert!(!request.is_noop());
        request.prewarm = false;
        assert!(request.is_noop());
    }
}

#[cfg(test)]
mod indexing_interruption_tests {
    use super::*;
    #[test]
    fn incoming_queries_interrupt_indexing_without_losing_commands() {
        let (tx, rx) = mpsc::channel();
        let mut queued = VecDeque::new();
        let token = SemanticCancellationToken::new();
        interrupt_for_queued_commands(&rx, &mut queued, &token);
        assert!(!token.is_cancelled());
        for generation in [2, 3] {
            tx.send(SemanticWorkerCommand::Search {
                generation,
                query: ParsedQuery::parse("different question"),
                corpus_version: 1,
                scope_version: 1,
                prewarm: false,
            })
            .unwrap();
        }
        interrupt_for_queued_commands(&rx, &mut queued, &token);
        assert!(token.is_cancelled());
        for expected in [2, 3] {
            assert!(
                matches!(queued.pop_front(), Some(SemanticWorkerCommand::Search { generation, .. }) if generation == expected)
            );
        }
        assert!(queued.is_empty());
        let closed = SemanticCancellationToken::new();
        drop(tx);
        interrupt_for_queued_commands(&rx, &mut queued, &closed);
        assert!(closed.is_cancelled());
    }
}
