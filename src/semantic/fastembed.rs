use crate::error::{AppError, Result};
use crate::semantic::embed::SemanticEmbedder;
use crate::semantic::types::DEFAULT_EMBEDDING_BATCH_SIZE;
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
use std::path::PathBuf;
use std::sync::OnceLock;

pub struct FastembedEmbedder {
    model: TextEmbedding,
}

impl FastembedEmbedder {
    pub fn new() -> Result<Self> {
        Self::new_with_download_progress(crate::semantic::cache::model_cache_dir(), true)
    }

    pub fn new_quiet() -> Result<Self> {
        Self::new_with_download_progress(crate::semantic::cache::model_cache_dir(), false)
    }

    pub fn cache_dir() -> PathBuf {
        crate::semantic::cache::model_cache_dir()
    }

    fn new_with_download_progress(
        cache_dir: PathBuf,
        show_download_progress: bool,
    ) -> Result<Self> {
        configure_runtime()?;
        let model = TextEmbedding::try_new(
            TextInitOptions::new(EmbeddingModel::BGESmallENV15)
                .with_cache_dir(cache_dir)
                .with_show_download_progress(show_download_progress),
        )
        .map_err(to_config_error)?;
        Ok(Self { model })
    }
}

/// Share two non-spinning CPU workers instead of creating a full-machine pool
/// for every embedding session. This also bounds memory use during indexing.
fn configure_runtime() -> Result<()> {
    static INITIALIZED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    INITIALIZED
        .get_or_init(|| {
            let pool = ort::environment::GlobalThreadPoolOptions::default()
                .with_intra_threads(2)
                .and_then(|pool| pool.with_inter_threads(1))
                .and_then(|pool| pool.with_spin_control(false))
                .map_err(|error| error.to_string())?;
            ort::init().with_global_thread_pool(pool).commit();
            Ok(())
        })
        .as_ref()
        .map_err(to_config_error)
        .copied()
}

impl SemanticEmbedder for FastembedEmbedder {
    fn embed_passages(&mut self, passages: &[String]) -> Result<Vec<Vec<f32>>> {
        self.model
            .embed(
                prefixed_passages(passages),
                Some(DEFAULT_EMBEDDING_BATCH_SIZE),
            )
            .map_err(to_config_error)
    }

    fn embed_query(&mut self, query: &str) -> Result<Option<Vec<f32>>> {
        let embeddings = self
            .model
            .embed(
                vec![prefixed_query(query)],
                Some(DEFAULT_EMBEDDING_BATCH_SIZE),
            )
            .map_err(to_config_error)?;
        Ok(embeddings.first().cloned())
    }
}

// BGE retrieval uses this instruction on queries and plain, unprefixed passages.
pub fn prefixed_query(query: &str) -> String {
    format!("Represent this sentence for searching relevant passages: {query}")
}

pub fn prefixed_passages(passages: &[String]) -> Vec<String> {
    passages.to_vec()
}

fn to_config_error(err: impl std::fmt::Display) -> AppError {
    AppError::ConfigError(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_query_for_fastembed() {
        assert_eq!(
            prefixed_query("rust cache"),
            "Represent this sentence for searching relevant passages: rust cache"
        );
    }

    #[test]
    fn prefixes_passages_for_fastembed() {
        assert_eq!(
            prefixed_passages(&["one".to_string(), "two".to_string()]),
            vec!["one".to_string(), "two".to_string()]
        );
    }
}

#[cfg(test)]
mod model_smoke_tests {
    use super::*;

    #[test]
    #[ignore = "runs the real local embedding model; may download model files"]
    fn real_model_semantic_smoke() {
        let mut model = FastembedEmbedder::new_quiet().unwrap();
        let passages=vec![
            "Persistent authentication stores a refresh credential in a secure cookie so users stay signed in across browser restarts.".to_string(),
            "Roast vegetables in a hot oven with olive oil and salt until tender.".to_string(),
        ];
        let embeddings = model.embed_passages(&passages).unwrap();
        let query = model
            .embed_query("keeping a web session logged in after closing the browser")
            .unwrap()
            .unwrap();
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(a, b)| a * b).sum::<f32>();
        // Regressions for the user's actual query shapes, through filtering,
        // real embeddings, and the same ranker as the TUI.
        let examples = [
            "hey isovis is pretty much just straight vanilla js, but with threejs, right? Yes: Isovis is vanilla JavaScript using Three.js for rendering.",
            "Use this:\n```sh\nlines --depth 2 -d\n```",
            "A question about a conversation about asking questions and getting answers.",
            "Roast carrots and potatoes in a hot oven.",
        ];
        let texts = examples
            .iter()
            .map(|text| {
                crate::semantic::filter::filter_turn(
                    crate::semantic::filter::SemanticTurnRole::Assistant,
                    text,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let vectors = model.embed_passages(&texts).unwrap();
        let chunks = texts
            .into_iter()
            .zip(vectors)
            .enumerate()
            .map(
                |(index, (text, embedding))| crate::semantic::types::EmbeddedChunk {
                    conversation_index: index,
                    source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                    session: index.to_string(),
                    chunk_index: 0,
                    key: index.to_string(),
                    text,
                    message_range: crate::agent::refs::MessageRange::single(1),
                    embedding,
                },
            )
            .collect::<Vec<_>>();
        for (query, expected) in [
            ("question about isovis", 0),
            ("cli command for counting lines", 1),
        ] {
            let vector = model.embed_query(query).unwrap().unwrap();
            let hits = crate::semantic::rank::rank_chunks(
                query,
                &vector,
                &chunks,
                &crate::semantic::types::SemanticCancellationToken::new(),
            )
            .unwrap();
            assert_eq!(
                hits[0].conversation_index, expected,
                "query {query}: {hits:?}"
            );
        }

        assert!(dot(&query, &embeddings[0]) > dot(&query, &embeddings[1]));
        for round in 0..2 {
            let texts = (0..17)
                .map(|i| {
                    if i % 2 == 0 {
                        "A long conversation about authentication and refresh cookies. ".repeat(150)
                    } else {
                        "Short text.".to_string()
                    }
                })
                .collect::<Vec<_>>();
            let vectors = model.embed_passages(&texts).unwrap();
            assert_eq!(vectors.len(), texts.len());
            for vector in vectors
                .iter()
                .chain(&embeddings)
                .chain(std::iter::once(&query))
            {
                assert_eq!(vector.len(), 384);
                assert!(vector.iter().all(|value| value.is_finite()));
                assert!((dot(vector, vector) - 1.0).abs() < 0.001);
            }
            assert!(
                model
                    .embed_query("a different query after indexing")
                    .unwrap()
                    .is_some()
            );
            eprintln!(
                "Real-model smoke round {round}: semantic ordering, mixed lengths, batch boundaries, and query-after-indexing passed"
            );
        }
    }
}
