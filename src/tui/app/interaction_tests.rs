use super::*;
use crate::config::KeyBinding;
use crate::semantic::types::{SemanticChunkIdentity, SemanticQuality, SemanticRationaleKind};
use chrono::TimeZone;

fn test_conversation(path: PathBuf, custom_title: Option<String>) -> Conversation {
    let mut full_text = "hello body".to_string();
    if let Some(title) = &custom_title {
        full_text = format!("{} {}", title, full_text);
    }
    Conversation {
        path,
        index: 0,
        timestamp: Local.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        preview: "hello body".to_string(),
        preview_first: "hello body".to_string(),
        preview_last: "hello body".to_string(),
        search_text_lower: search::normalize_for_search(&full_text),
        semantic_turns: vec!["hello body".to_string()],
        semantic_turn_ranges: vec![crate::agent::refs::MessageRange::single(1)],
        full_text,
        agent_search_text: String::new(),
        project_name: Some("project".to_string()),
        project_path: None,
        cwd: None,
        parent_session: None,
        message_count: 1,
        parse_errors: Vec::new(),
        summary: None,
        custom_title,
        model: None,
        total_tokens: 0,
        duration_minutes: None,
    }
}

fn app_with_conversation(path: PathBuf, custom_title: Option<String>) -> App {
    App::new(
        vec![test_conversation(path, custom_title)],
        ToolDisplayMode::Hidden,
        false,
        KeyBindings::default(),
        vec![],
    )
}

fn write_conversation(path: &std::path::Path, title: Option<&str>) {
    let mut lines = vec![r#"{"type":"user","timestamp":"2024-01-01T00:00:00Z","message":{"role":"user","content":"hello body"}}"#.to_string()];
    if let Some(title) = title {
        lines.push(format!(
            r#"{{"type":"custom-title","customTitle":"{}","sessionId":"abc123"}}"#,
            title
        ));
    }
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn write_named_conversation(path: &std::path::Path, text: &str) {
    let line = serde_json::json!({
        "type": "user",
        "timestamp": "2024-01-01T00:00:00Z",
        "message": {"role": "user", "content": text}
    })
    .to_string();
    std::fs::write(path, format!("{line}\n")).unwrap();
}

fn write_tool_conversation(path: &std::path::Path) {
    let line = r#"{"type":"assistant","timestamp":"2024-01-01T00:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"one\ntwo\nthree\nfour\nfive"}}]}}"#;
    std::fs::write(path, format!("{line}\n")).unwrap();
}

fn app_with_tool_conversation(path: PathBuf) -> App {
    let mut app = App::new(
        vec![test_conversation(path, None)],
        ToolDisplayMode::Truncated,
        false,
        KeyBindings::default(),
        vec![],
    );
    app.selected = Some(0);
    app.enter_view_mode(80);
    app
}

fn tool_click_row(app: &App, frame: Rect) -> u16 {
    if let AppMode::View(state) = app.app_mode() {
        let layout = ui::view_layout_rects(frame, app, state);
        let idx = state
            .rendered_lines
            .iter()
            .position(|line| line.clickable)
            .unwrap();
        layout.content.y + (idx - state.scroll_offset) as u16
    } else {
        unreachable!()
    }
}

fn view_text(app: &App) -> String {
    if let AppMode::View(state) = app.app_mode() {
        state
            .rendered_lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|(text, _)| text.as_str()))
            .collect()
    } else {
        unreachable!()
    }
}

fn view_expanded_tool_id(app: &App) -> ToolOutputId {
    if let AppMode::View(state) = app.app_mode() {
        assert_eq!(state.expanded_tool_outputs.len(), 1);
        state.expanded_tool_outputs.iter().next().unwrap().clone()
    } else {
        unreachable!()
    }
}

fn test_semantic_metadata(
    conversation_index: usize,
    evidence_preview: &str,
) -> SemanticResultMetadata {
    SemanticResultMetadata {
        score_breakdown: SemanticScoreBreakdown {
            hybrid: 1.0,
            semantic: 1.0,
            lexical: 0.0,
        },
        explanation: SemanticExplanation {
            quality: SemanticQuality::Strong,
            quality_label: "strong",
            matched_terms: Vec::new(),
            evidence_preview: evidence_preview.to_string(),
            rationale_kind: SemanticRationaleKind::SemanticOnly,
            chunk: SemanticChunkIdentity {
                conversation_index,
                source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                session: "test-session".to_string(),
                chunk_index: 0,
                message_range: crate::agent::refs::MessageRange::single(1),
            },
        },
    }
}

#[test]
fn semantic_ranked_selection_opens_selected_conversation_and_returns() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.jsonl");
    let second = dir.path().join("second.jsonl");
    write_named_conversation(&first, "first body");
    write_named_conversation(&second, "second body");
    let mut app = App::new_with_options(
        vec![
            test_conversation(first.clone(), None),
            test_conversation(second.clone(), None),
        ],
        ToolDisplayMode::Hidden,
        false,
        KeyBindings::default(),
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    let (_request_tx, request_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(_request_tx);
    app.semantic_search.worker_rx = Some(response_rx);
    app.list_search_mode = ListSearchMode::Semantic;
    app.search_generation = 7;
    app.semantic_search.pending_generation = Some(7);
    drop(request_rx);

    response_tx
        .send(SemanticSearchMessage::Complete(
            crate::tui::semantic_worker::SemanticSearchResponse {
                generation: 7,
                filtered: vec![1, 0],
                metadata: HashMap::from([(
                    1,
                    test_semantic_metadata(1, "second semantic preview"),
                )]),
                error: None,
                progress: SemanticProgress::Complete,
                prewarm: false,
            },
        ))
        .unwrap();

    assert!(app.receive_search_results());
    assert_eq!(app.filtered(), &[1, 0]);
    assert_eq!(app.selected(), Some(0));
    app.enter_view_mode(80);
    assert!(matches!(app.app_mode(), AppMode::View(_)));
    if let AppMode::View(state) = app.app_mode() {
        assert_eq!(state.conversation_path, second);
    }
    assert!(view_text(&app).contains("second body"));

    app.exit_view_mode();

    assert!(matches!(app.app_mode(), AppMode::List));
    assert_eq!(app.filtered(), &[1, 0]);
    assert_eq!(app.selected(), Some(0));
}

#[test]
fn semantic_list_click_uses_three_line_rows() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.jsonl");
    let second = dir.path().join("second.jsonl");
    write_named_conversation(&first, "first body");
    write_named_conversation(&second, "second body");
    let mut app = App::new_with_options(
        vec![
            test_conversation(first, None),
            test_conversation(second, None),
        ],
        ToolDisplayMode::Hidden,
        false,
        KeyBindings::default(),
        vec![],
        TuiSearchOptions {
            default_mode: ListSearchMode::Semantic,
        },
    );
    let (_request_tx, request_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    app.semantic_search.worker_tx = Some(_request_tx);
    app.semantic_search.worker_rx = Some(response_rx);
    app.list_search_mode = ListSearchMode::Semantic;
    app.query = "needle".to_string();
    app.cursor_pos = app.query.chars().count();
    app.search_generation = 7;
    app.semantic_search.pending_generation = Some(7);
    drop(request_rx);
    response_tx
        .send(SemanticSearchMessage::Complete(
            crate::tui::semantic_worker::SemanticSearchResponse {
                generation: 7,
                filtered: vec![0, 1],
                metadata: HashMap::from([
                    (0, test_semantic_metadata(0, "first")),
                    (1, test_semantic_metadata(1, "second")),
                ]),
                error: None,
                progress: SemanticProgress::Complete,
                prewarm: false,
            },
        ))
        .unwrap();
    app.receive_search_results();
    let frame = Rect::new(0, 0, 80, 20);

    assert!(app.handle_list_click(6, frame));

    assert_eq!(app.selected(), Some(1));
}

#[test]
fn view_click_toggles_clickable_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool.jsonl");
    write_tool_conversation(&path);
    let mut app = app_with_tool_conversation(path);
    let frame = Rect::new(0, 0, 120, 20);
    let row = tool_click_row(&app, frame);

    assert!(app.handle_view_click(row, frame, 17));
    let expanded_id = view_expanded_tool_id(&app);
    assert!(view_text(&app).contains("five"));

    assert!(app.handle_view_click(row, frame, 17));
    if let AppMode::View(state) = app.app_mode() {
        assert!(state.expanded_tool_outputs.is_empty());
        assert_eq!(state.hovered_tool_output, Some(expanded_id));
    }
}

#[test]
fn view_click_uses_cached_entries_after_file_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool.jsonl");
    write_tool_conversation(&path);
    let mut app = app_with_tool_conversation(path.clone());
    std::fs::remove_file(&path).unwrap();
    let frame = Rect::new(0, 0, 120, 20);
    let row = tool_click_row(&app, frame);

    assert!(app.handle_view_click(row, frame, 17));
    let expanded_id = view_expanded_tool_id(&app);
    assert!(view_text(&app).contains("five"));

    assert!(app.handle_view_click(row, frame, 17));
    if let AppMode::View(state) = app.app_mode() {
        assert!(state.expanded_tool_outputs.is_empty());
        assert_eq!(state.hovered_tool_output, Some(expanded_id));
    }
    assert!(!view_text(&app).contains("five"));
}

#[test]
fn single_file_view_click_uses_cached_entries_after_file_removed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool.jsonl");
    write_tool_conversation(&path);
    let mut app = App::new_single_file(
        path.clone(),
        ToolDisplayMode::Truncated,
        false,
        KeyBindings::default(),
    );
    app.check_view_resize(80, 17);
    std::fs::remove_file(&path).unwrap();
    let frame = Rect::new(0, 0, 120, 20);
    let row = tool_click_row(&app, frame);

    assert!(app.handle_view_click(row, frame, 17));
    assert!(view_text(&app).contains("five"));

    assert!(app.handle_view_click(row, frame, 17));
    if let AppMode::View(state) = app.app_mode() {
        assert!(state.expanded_tool_outputs.is_empty());
    }
    assert!(!view_text(&app).contains("five"));
}

#[test]
fn view_hover_tracks_clickable_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool.jsonl");
    write_tool_conversation(&path);
    let mut app = app_with_tool_conversation(path);
    let frame = Rect::new(0, 0, 120, 20);
    let (row, id) = if let AppMode::View(state) = app.app_mode() {
        let layout = ui::view_layout_rects(frame, &app, state);
        let idx = state
            .rendered_lines
            .iter()
            .position(|line| line.clickable)
            .unwrap();
        let id = state.rendered_lines[idx].tool_output_id.clone().unwrap();
        (layout.content.y + (idx - state.scroll_offset) as u16, id)
    } else {
        unreachable!()
    };

    assert!(app.handle_view_mouse_move(row, frame));
    if let AppMode::View(state) = app.app_mode() {
        assert_eq!(state.hovered_tool_output, Some(id));
    }
    assert!(app.handle_view_mouse_move(0, frame));
    if let AppMode::View(state) = app.app_mode() {
        assert_eq!(state.hovered_tool_output, None);
    }
}

#[test]
fn cancel_rename_keeps_existing_title() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, Some("old"));
    let mut app = app_with_conversation(path, Some("old".to_string()));

    app.start_rename();
    assert!(matches!(app.dialog_mode, DialogMode::Rename { .. }));
    app.handle_rename_key(KeyCode::Esc, KeyModifiers::empty());

    assert_eq!(app.conversations[0].custom_title, Some("old".to_string()));
    assert_eq!(app.dialog_mode, DialogMode::None);
}

#[test]
fn configured_rename_key_starts_rename() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, None);
    let keys = KeyBindings {
        rename: KeyBinding {
            code: KeyCode::Char('t'),
            modifiers: KeyModifiers::CONTROL,
        },
        ..Default::default()
    };
    let mut app = App::new(
        vec![test_conversation(path, None)],
        ToolDisplayMode::Hidden,
        false,
        keys,
        vec![],
    );

    app.handle_key(KeyCode::Char('t'), KeyModifiers::CONTROL, 10);

    assert!(matches!(app.dialog_mode, DialogMode::Rename { .. }));
}

#[test]
fn bare_r_remains_search_input() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, None);
    let mut app = app_with_conversation(path, None);

    app.handle_key(KeyCode::Char('r'), KeyModifiers::empty(), 10);

    assert_eq!(app.query(), "r");
    assert_eq!(app.dialog_mode, DialogMode::None);
}

#[test]
fn submit_rename_reparses_and_updates_search_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, Some("old"));
    let mut app = app_with_conversation(path.clone(), Some("old".to_string()));

    app.start_rename();
    app.handle_rename_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    app.handle_rename_key(KeyCode::Char('n'), KeyModifiers::empty());
    app.handle_rename_key(KeyCode::Char('e'), KeyModifiers::empty());
    app.handle_rename_key(KeyCode::Char('w'), KeyModifiers::empty());
    app.handle_rename_key(KeyCode::Enter, KeyModifiers::empty());

    assert_eq!(app.conversations[0].custom_title, Some("new".to_string()));
    assert!(search::search(&app.conversations, &app.searchable, "new", Local::now()).contains(&0));
    assert!(search::search(&app.conversations, &app.searchable, "old", Local::now()).is_empty());
}

#[test]
fn submit_rename_preserves_selected_path() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.jsonl");
    let second = dir.path().join("second.jsonl");
    write_conversation(&first, None);
    write_conversation(&second, None);
    let mut app = App::new(
        vec![
            test_conversation(first, None),
            test_conversation(second.clone(), None),
        ],
        ToolDisplayMode::Hidden,
        false,
        KeyBindings::default(),
        vec![],
    );
    app.selected = Some(1);

    app.start_rename();
    app.handle_rename_key(KeyCode::Char('n'), KeyModifiers::empty());
    app.handle_rename_key(KeyCode::Enter, KeyModifiers::empty());

    assert_eq!(app.get_selected_path().as_deref(), Some(second.as_path()));
}

#[test]
fn submit_empty_rename_clears_searchable_title() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("abc123.jsonl");
    write_conversation(&path, Some("old"));
    let mut app = app_with_conversation(path.clone(), Some("old".to_string()));

    app.start_rename();
    app.handle_rename_key(KeyCode::Char('u'), KeyModifiers::CONTROL);
    app.handle_rename_key(KeyCode::Enter, KeyModifiers::empty());

    assert_eq!(app.conversations[0].custom_title, None);
    assert!(search::search(&app.conversations, &app.searchable, "old", Local::now()).is_empty());
}

#[test]
fn reload_reads_appended_messages_preserving_position_settings_and_search() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut file = std::fs::File::create(&path).unwrap();
    for i in 0..30 {
        writeln!(file, "{}", serde_json::json!({
            "type": "user", "message": {"role": "user", "content": format!("Original message {i}")}
        })).unwrap();
    }
    let mut app = app_with_conversation(path.clone(), None);
    app.enter_view_mode(80);
    let id = crate::tui::viewer::ToolOutputId("saved-expansion".to_string());
    if let AppMode::View(state) = &mut app.app_mode {
        state.scroll_offset = 10;
        state.tool_display = ToolDisplayMode::Off;
        state.show_thinking = false;
        state.show_timing = true;
        state.expanded_tool_outputs.insert(id.clone());
        state.hovered_tool_output = Some(id.clone());
        state.search_mode = ViewSearchMode::Active;
        state.search_query = "appended-marker".to_string();
    }
    writeln!(file, "{}", serde_json::json!({
        "type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "appended-marker"}]}
    })).unwrap();
    app.re_render_view(10);
    assert!(
        !view_text(&app).contains("appended-marker"),
        "display rerender should keep its cached snapshot"
    );
    assert!(
        app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE, 10)
            .is_none()
    );
    assert!(view_text(&app).contains("appended-marker"));
    let AppMode::View(state) = app.app_mode() else {
        panic!("expected view")
    };
    assert_eq!(state.scroll_offset, 10);
    assert_eq!(state.tool_display, ToolDisplayMode::Off);
    assert!(!state.show_thinking);
    assert!(state.show_timing);
    assert!(state.expanded_tool_outputs.contains(&id));
    assert!(state.hovered_tool_output.is_none());
    assert_eq!(state.message_ranges.len(), 31);
    assert_eq!(state.search_matches.len(), 1);
    assert_eq!(app.status_message.as_ref().unwrap().0, "Session reloaded");
}

#[test]
fn reload_single_file_handles_replaced_contents_and_clamps_scroll() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    write_named_conversation(&path, &"Old text\n\n".repeat(30));
    let mut app = App::new_single_file(
        path.clone(),
        ToolDisplayMode::Off,
        false,
        KeyBindings::default(),
    );
    app.check_view_resize(80, 10);
    app.handle_key(KeyCode::End, KeyModifiers::NONE, 10);
    write_named_conversation(&path, "Replacement text");
    app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE, 10);
    let text = view_text(&app);
    assert!(text.contains("Replacement text"));
    assert!(!text.contains("Old text"));
    let AppMode::View(state) = app.app_mode() else {
        panic!("expected view")
    };
    assert_eq!(state.scroll_offset, 0);
    assert_eq!(state.message_ranges.len(), 1);
}

#[test]
fn reload_failure_keeps_previous_snapshot_and_reports_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    write_named_conversation(&path, "Keep this text");
    let mut app = app_with_conversation(path.clone(), None);
    app.enter_view_mode(80);
    let before = view_text(&app);
    let snapshot = match app.app_mode() {
        AppMode::View(state) => state.parsed_entries.clone().unwrap(),
        _ => unreachable!(),
    };
    std::fs::remove_file(&path).unwrap();
    app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE, 10);
    assert_eq!(view_text(&app), before);
    let AppMode::View(state) = app.app_mode() else {
        panic!("expected view")
    };
    assert!(Arc::ptr_eq(
        &snapshot,
        state.parsed_entries.as_ref().unwrap()
    ));
    assert!(
        app.status_message
            .as_ref()
            .unwrap()
            .0
            .starts_with("Failed to reload session:")
    );
}

#[test]
fn reload_key_does_not_steal_search_typing_or_ctrl_r_resume() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    write_named_conversation(&path, "Original");
    let mut app = app_with_conversation(path.clone(), None);
    app.enter_view_mode(80);
    write_named_conversation(&path, "Changed");
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE, 10);
    app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE, 10);
    let AppMode::View(state) = app.app_mode() else {
        panic!("expected view")
    };
    assert_eq!(state.search_query, "r");
    assert!(!view_text(&app).contains("Changed"));
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE, 10);
    assert!(
        matches!(app.handle_key(KeyCode::Char('r'), KeyModifiers::CONTROL, 10), Some(Action::Resume(p)) if p == path)
    );
    assert!(!view_text(&app).contains("Changed"));
}

fn append_reload_message(path: &std::path::Path, text: &str) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({"type":"user","message":{"role":"user","content":text}})
    )
    .unwrap();
}

fn allow_auto_reload_poll(app: &mut App) {
    app.last_auto_reload_check = None;
}

#[test]
fn auto_reload_is_opt_in_and_a_toggles_it_without_stealing_search_input() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    write_named_conversation(&path, "Original");
    let mut app = app_with_conversation(path.clone(), None);
    assert!(!app.auto_reload());
    app.enter_view_mode(80);
    append_reload_message(&path, "Automatic addition");
    app.check_auto_reload(10);
    assert!(!view_text(&app).contains("Automatic addition"));
    assert_eq!(app.auto_reload_wait(), None);
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE, 10);
    assert!(app.auto_reload());
    assert_eq!(app.auto_reload_wait(), Some(Duration::ZERO));
    app.check_auto_reload(10);
    assert!(view_text(&app).contains("Automatic addition"));
    assert!(
        app.status_message.is_none(),
        "background refresh must not cover the shortcuts"
    );
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE, 10);
    assert!(!app.auto_reload());
    app.handle_key(KeyCode::Char('/'), KeyModifiers::NONE, 10);
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE, 10);
    assert!(!app.auto_reload());
    let AppMode::View(state) = app.app_mode() else {
        unreachable!()
    };
    assert_eq!(state.search_query, "a");
}

#[test]
fn auto_reload_skips_unchanged_files_and_throttles_checks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    write_named_conversation(&path, "Original");
    let mut app = app_with_conversation(path.clone(), None);
    app.enter_view_mode(80);
    app.set_auto_reload(true);
    let snapshot = match app.app_mode() {
        AppMode::View(state) => state.parsed_entries.clone().unwrap(),
        _ => unreachable!(),
    };
    app.check_auto_reload(10);
    let AppMode::View(state) = app.app_mode() else {
        unreachable!()
    };
    assert!(Arc::ptr_eq(
        &snapshot,
        state.parsed_entries.as_ref().unwrap()
    ));
    append_reload_message(&path, "Wait for next poll");
    app.check_auto_reload(10);
    assert!(!view_text(&app).contains("Wait for next poll"));
    app.last_auto_reload_check = Some(std::time::Instant::now() - Duration::from_secs(2));
    app.check_auto_reload(10);
    assert!(view_text(&app).contains("Wait for next poll"));
    app.exit_view_mode();
    assert!(app.auto_reload()); // Remember the toggle across sessions.
    assert_eq!(app.auto_reload_wait(), None); // No polling in the session list.
}

#[test]
fn auto_reload_follows_bottom_but_preserves_earlier_reading_position() {
    for follow in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        write_named_conversation(&path, "Start");
        for i in 0..30 {
            append_reload_message(&path, &format!("Message {i}"));
        }
        let mut app = app_with_conversation(path.clone(), None);
        app.enter_view_mode(80);
        app.set_auto_reload(true);
        if let AppMode::View(state) = &mut app.app_mode {
            state.scroll_offset = if follow {
                state.total_lines.saturating_sub(10)
            } else {
                10
            };
        }
        for i in 0..10 {
            append_reload_message(&path, &format!("New {i}"));
        }
        app.check_auto_reload(10);
        let AppMode::View(state) = app.app_mode() else {
            unreachable!()
        };
        assert_eq!(
            state.scroll_offset,
            if follow {
                state.total_lines.saturating_sub(10)
            } else {
                10
            }
        );
        assert_eq!(state.message_ranges.len(), 41);
    }
}

#[test]
fn auto_reload_does_not_tail_follow_during_search_or_message_navigation() {
    for searching in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        write_named_conversation(&path, "Start");
        for i in 0..30 {
            append_reload_message(&path, &format!("Message {i}"));
        }
        let mut app = app_with_conversation(path.clone(), None);
        app.enter_view_mode(80);
        app.set_auto_reload(true);
        let old_scroll;
        if let AppMode::View(state) = &mut app.app_mode {
            state.scroll_offset = state.total_lines.saturating_sub(10);
            old_scroll = state.scroll_offset;
            if searching {
                state.search_mode = ViewSearchMode::Active;
                state.search_query = "New search match".to_string();
            } else {
                state.message_nav_active = true;
                state.focused_message = Some(state.message_ranges.len() - 1);
            }
        } else {
            unreachable!()
        }
        append_reload_message(&path, "New search match");
        app.check_auto_reload(10);
        let AppMode::View(state) = app.app_mode() else {
            unreachable!()
        };
        assert_eq!(state.scroll_offset, old_scroll);
        if searching {
            assert_eq!(state.search_matches.len(), 1);
        } else {
            assert_eq!(state.focused_message, Some(30));
        }
    }
}

#[test]
fn auto_reload_single_file_recovers_after_read_failure_and_partial_append() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    write_named_conversation(&path, "Original");
    let mut app = App::new_single_file(
        path.clone(),
        ToolDisplayMode::Off,
        false,
        KeyBindings::default(),
    );
    app.check_view_resize(80, 10);
    app.set_auto_reload(true);
    std::fs::remove_file(&path).unwrap();
    app.check_auto_reload(10);
    assert!(view_text(&app).contains("Original"));
    assert!(
        app.status_message
            .as_ref()
            .unwrap()
            .0
            .starts_with("Failed to reload session:")
    );
    write_named_conversation(&path, "Recovered");
    allow_auto_reload_poll(&mut app);
    app.check_auto_reload(10);
    assert!(view_text(&app).contains("Recovered"));
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    write!(
        file,
        "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"Partially"
    )
    .unwrap();
    file.flush().unwrap();
    allow_auto_reload_poll(&mut app);
    app.check_auto_reload(10);
    assert!(view_text(&app).contains("Recovered"));
    assert!(!view_text(&app).contains("Partially"));
    writeln!(file, " completed\"}}}}").unwrap();
    file.flush().unwrap();
    allow_auto_reload_poll(&mut app);
    app.check_auto_reload(10);
    assert!(view_text(&app).contains("Partially completed"));
}

#[test]
fn auto_reload_config_is_optional_and_accepts_both_values() {
    let display = crate::config::DisplayConfig::default();
    assert_eq!(display.auto_reload, None);
    for enabled in [false, true] {
        let config: crate::config::ConfigFile =
            toml::from_str(&format!("[display]\nauto_reload = {enabled}\n")).unwrap();
        assert_eq!(config.display.unwrap().auto_reload, Some(enabled));
    }
}
