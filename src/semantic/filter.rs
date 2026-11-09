#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticTurnRole {
    User,
    Assistant,
}

pub fn filter_turn(role: SemanticTurnRole, turn: &str) -> Option<String> {
    // Pi goal prompts are task wrappers, not conversation topics. Keep the
    // user's objective while dropping the repeated control instructions.
    let turn = if role == SemanticTurnRole::User
        && turn.trim_start().starts_with("Goal mode is active.")
        && turn.contains("<!-- pi-goal-prompt:")
    {
        let Some((_, rest)) = turn.split_once("<goal_objective>") else {
            return None;
        };
        let Some((objective, _)) = rest.split_once("</goal_objective>") else {
            return None;
        };
        objective
    } else {
        turn
    };
    let turn = slash_command_args(turn).unwrap_or(turn);
    let without_fences = strip_markdown_code_fences(turn);
    let without_tags = strip_structural_tag_spans(&without_fences);
    let normalized = normalize_whitespace(&without_tags);
    let kept = split_semantic_blocks(&normalized)
        .into_iter()
        .map(str::trim)
        .filter(|block| !block.is_empty())
        .filter(|block| !is_low_value_turn(block))
        .filter(|block| !is_status_artifact(role, block))
        .filter(|block| !is_artifact_block(block))
        .collect::<Vec<_>>()
        .join(" ");
    let normalized = normalize_whitespace(&kept);
    (!normalized.is_empty()).then_some(normalized)
}

fn strip_markdown_code_fences(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;

    while let Some(start) = rest.find("```") {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + 3..];
        let Some(end) = after_open.find("```") else {
            rest = "";
            break;
        };
        let block = &after_open[..end];
        if let Some(command) = short_shell_command(block) {
            output.push(' ');
            output.push_str(command);
            output.push(' ');
        }
        rest = &after_open[end + 3..];
    }

    output.push_str(rest);
    output
}

/// Keep compact command answers, not source files, logs, or UI mockups.
fn short_shell_command(block: &str) -> Option<&str> {
    let (language, body) = block.split_once('\n')?;
    let body = body.trim();
    if body.is_empty() || body.len() > 800 || body.lines().count() > 6 {
        return None;
    }
    let shell = matches!(
        language.trim().to_ascii_lowercase().as_str(),
        "sh" | "bash" | "zsh" | "fish" | "shell" | "powershell" | "pwsh"
    );
    let bare_command = language.trim().is_empty()
        && body.lines().count() == 1
        && body.split_whitespace().next().is_some_and(|word| {
            word.chars()
                .all(|c| c.is_ascii_lowercase() || matches!(c, '-' | '_' | '/' | '.'))
        })
        && body
            .split_whitespace()
            .skip(1)
            .any(|word| word.starts_with('-') || word == "|");
    (shell || bare_command).then_some(body)
}

fn strip_structural_tag_spans(text: &str) -> String {
    let mut stripped = text.to_string();
    for (open, close) in [
        ("<system-reminder>", "</system-reminder>"),
        ("<local-command-caveat>", "</local-command-caveat>"),
        ("<local-command-stdout>", "</local-command-stdout>"),
        ("<command-message>", "</command-message>"),
        ("<command-name>", "</command-name>"),
    ] {
        stripped = strip_tag_pair(&stripped, open, close);
    }
    stripped
}

fn strip_tag_pair(text: &str, open: &str, close: &str) -> String {
    let mut output = String::new();
    let mut rest = text;

    while let Some(start) = rest.find(open) {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + open.len()..];
        let Some(end) = after_open.find(close) else {
            rest = "";
            break;
        };
        rest = &after_open[end + close.len()..];
    }

    output.push_str(rest);
    output
}

fn slash_command_args(turn: &str) -> Option<&str> {
    if !turn.contains("<command-name>") || !turn.contains("</command-name>") {
        return None;
    }
    let args_start = turn.find("<command-args>")? + "<command-args>".len();
    let args_end = turn[args_start..].find("</command-args>")? + args_start;
    let args = turn[args_start..args_end].trim();
    (!args.is_empty()).then_some(args)
}

fn split_semantic_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut start = 0;
    let mut previous_artifact = false;

    for (index, ch) in text.char_indices() {
        let artifact = is_artifact_char(ch);
        if index > start && artifact != previous_artifact {
            blocks.push(&text[start..index]);
            start = index;
        }
        previous_artifact = artifact;
    }
    if start < text.len() {
        blocks.push(&text[start..]);
    }
    blocks
}

fn is_artifact_char(ch: char) -> bool {
    matches!(
        ch,
        '─' | '━'
            | '│'
            | '┃'
            | '┌'
            | '┐'
            | '└'
            | '┘'
            | '├'
            | '┤'
            | '┬'
            | '┴'
            | '┼'
            | '╭'
            | '╮'
            | '╰'
            | '╯'
            | '═'
            | '║'
            | '╔'
            | '╗'
            | '╚'
            | '╝'
    )
}

fn is_low_value_turn(turn: &str) -> bool {
    let trimmed = turn.trim();
    trimmed.is_empty()
        || trimmed.starts_with('/')
        || contains_any(
            trimmed,
            &[
                "<system-reminder>",
                "</system-reminder>",
                "<local-command-caveat>",
                "</local-command-caveat>",
                "<local-command-stdout>",
                "</local-command-stdout>",
                "<command-message>",
                "</command-message>",
                "<command-name>",
                "</command-name>",
            ],
        )
}

fn is_status_artifact(role: SemanticTurnRole, block: &str) -> bool {
    if role != SemanticTurnRole::Assistant {
        return false;
    }
    let lower = block.to_lowercase();
    let validation_terms = [
        "validation passed",
        "all checks pass",
        "working tree clean",
        "committed",
        "commit:",
        "validation:",
    ];
    let has_validation = validation_terms.iter().any(|term| lower.contains(term));
    let command_terms = ["cargo test", "just check", "git status", "git diff"];
    let command_hits = command_terms
        .iter()
        .filter(|term| lower.contains(**term))
        .count();
    has_validation && command_hits > 0
}

fn is_artifact_block(block: &str) -> bool {
    let chars = block.chars().filter(|c| !c.is_whitespace()).count();
    if chars == 0 {
        return true;
    }

    let box_chars = block.chars().filter(|c| is_artifact_char(*c)).count();
    if box_chars * 4 >= chars || (box_chars >= 3 && chars <= 24) {
        return true;
    }

    let punctuation = block
        .chars()
        .filter(|c| matches!(c, '|' | '-' | '+' | '=' | '_' | '`'))
        .count();
    let alphanumeric = block.chars().filter(|c| c.is_alphanumeric()).count();
    if punctuation * 2 > alphanumeric && block.contains('|') {
        return true;
    }

    let code_markers = [
        "::{",
        "=>",
        "</",
        "<div",
        "function ",
        "const ",
        "let ",
        "fn ",
    ];
    contains_any(block, &code_markers) && punctuation > alphanumeric / 4
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<&str>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_inline_fenced_blocks_and_preserves_surrounding_discussion() {
        let filtered = filter_turn(
            SemanticTurnRole::Assistant,
            "The idea is a detail pane. ```text ╭─ Search ─╮ │ mockup │ ╰──────────╯ ``` It helps users recognize sessions.",
        )
        .unwrap();

        assert!(filtered.contains("detail pane"));
        assert!(filtered.contains("recognize sessions"));
        assert!(!filtered.contains("mockup"));
        assert!(!filtered.contains("```"));
    }

    #[test]
    fn strips_multiline_fenced_blocks() {
        let filtered = filter_turn(
            SemanticTurnRole::Assistant,
            "Semantic cache stores dialogue.\n```rust\nlet secret = tool_output();\n```\nUse generated embeddings.",
        )
        .unwrap();

        assert_eq!(
            filtered,
            "Semantic cache stores dialogue. Use generated embeddings."
        );
    }

    #[test]
    fn drops_box_drawing_artifacts_but_keeps_text_blocks() {
        let filtered = filter_turn(
            SemanticTurnRole::Assistant,
            "Use a responsive preview pane.\n╭────────╮\n│ Search │\n╰────────╯\nIt should summarize the selected conversation.",
        )
        .unwrap();

        assert!(filtered.contains("responsive preview pane"));
        assert!(filtered.contains("selected conversation"));
        assert!(!filtered.contains("╭"));
        assert!(!filtered.contains("Search │"));
    }

    #[test]
    fn strips_structural_tag_spans_without_dropping_surrounding_text() {
        let filtered = filter_turn(
            SemanticTurnRole::Assistant,
            "Useful explanation <system-reminder>noise</system-reminder> more explanation",
        )
        .unwrap();

        assert_eq!(filtered, "Useful explanation more explanation");
    }

    #[test]
    fn command_args_require_command_wrapper() {
        let filtered = filter_turn(
            SemanticTurnRole::User,
            "Discuss literal <command-args>syntax examples</command-args> in docs",
        )
        .unwrap();

        assert!(filtered.contains("Discuss literal"));
        assert!(filtered.contains("syntax examples"));
    }

    #[test]
    fn drops_command_wrapper_turns_without_args() {
        assert_eq!(
            filter_turn(
                SemanticTurnRole::User,
                "<command-message>debate</command-message> <command-name>/debate</command-name>",
            ),
            None
        );
    }

    #[test]
    fn keeps_command_args_without_wrapper_markup() {
        let filtered = filter_turn(
            SemanticTurnRole::User,
            "<command-message>debate</command-message> <command-name>/debate</command-name> <command-args>find robust semantic filtering</command-args>",
        )
        .unwrap();

        assert_eq!(filtered, "find robust semantic filtering");
    }

    #[test]
    fn drops_assistant_validation_status_artifacts() {
        assert_eq!(
            filter_turn(
                SemanticTurnRole::Assistant,
                "Validation passed: cargo test and just check. Committed abc123.",
            ),
            None
        );
    }

    #[test]
    fn preserves_compact_technical_explanations() {
        let filtered = filter_turn(
            SemanticTurnRole::Assistant,
            "The race condition is in scheduler::rebalance() because the cache signature updates before scope ranking.",
        )
        .unwrap();

        assert!(filtered.contains("race condition"));
        assert!(filtered.contains("cache signature"));
    }
}

#[cfg(test)]
mod retrieval_regressions {
    use super::*;

    #[test]
    fn goal_prompts_keep_objectives_not_controller_boilerplate() {
        let text = "Goal mode is active. Complete this goal fully:\n<goal_objective>Improve Isovis rendering.</goal_objective>\nGoal-mode rules: keep working, call goal_complete, goal_wait...\n<!-- pi-goal-prompt:example -->";
        assert_eq!(
            filter_turn(SemanticTurnRole::User, text).as_deref(),
            Some("Improve Isovis rendering.")
        );
        assert!(
            filter_turn(
                SemanticTurnRole::User,
                "Goal mode is active. missing objective <!-- pi-goal-prompt:example -->"
            )
            .is_none()
        );
    }

    #[test]
    fn short_cli_answers_are_searchable_but_large_scripts_stay_out() {
        for fence in ["sh", "bash", ""] {
            let text = format!("Use this:\n```{fence}\nlines --depth 2 -d\n```");
            let result = filter_turn(SemanticTurnRole::Assistant, &text).unwrap();
            assert!(result.contains("lines --depth 2 -d"));
            assert!(!result.contains("```"));
        }
        let script = format!("Explanation.\n```bash\n{}\n```", "echo noisy\n".repeat(20));
        assert_eq!(
            filter_turn(SemanticTurnRole::Assistant, &script).as_deref(),
            Some("Explanation.")
        );
    }
}
