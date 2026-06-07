//! Tool-specific formatting for nicer display of tool calls.
//!
//! Instead of showing raw JSON, this module formats each tool's input
//! in a human-readable way that highlights the most relevant information.

use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};

/// Formatted tool call representation
pub struct FormattedToolCall {
    /// The header line (e.g., "Task (Explore): description" or "$ command")
    pub header: String,
    /// Optional continuation lines (e.g., prompt text, diff lines)
    pub body: Option<String>,
}

static RENDER_EDIT_DIFFS: AtomicBool = AtomicBool::new(true);

pub fn set_render_edit_diffs(enabled: bool) {
    RENDER_EDIT_DIFFS.store(enabled, Ordering::Relaxed);
}

pub fn toggle_render_edit_diffs() -> bool {
    let next = !RENDER_EDIT_DIFFS.load(Ordering::Relaxed);
    RENDER_EDIT_DIFFS.store(next, Ordering::Relaxed);
    next
}

pub fn render_edit_diffs() -> bool {
    RENDER_EDIT_DIFFS.load(Ordering::Relaxed)
}

fn normalize_tool_name(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('-', "_")
}

/// Format a tool call for display
///
/// The `max_width` parameter controls line wrapping for tools with long content (e.g., Bash commands).
pub fn format_tool_call(name: &str, input: &Value, max_width: usize) -> FormattedToolCall {
    match name {
        "Task" | "task" => format_task(input),
        "Bash" | "bash" => format_bash(input, max_width),
        "Read" | "read" => format_read(input),
        "Grep" | "grep" => format_grep(input),
        "Glob" | "glob" | "Find" | "find" => format_glob(input),
        "Ls" | "ls" => format_ls(input),
        "Edit" | "edit" => format_edit(input),
        "Write" | "write" => format_write(input),
        "WebFetch" | "web_fetch" | "webFetch" => format_web_fetch(input),
        "WebSearch" | "web_search" | "webSearch" => format_web_search(input),
        _ => format_custom_tool(name, input),
    }
}

fn format_task(input: &Value) -> FormattedToolCall {
    let subagent_type = input
        .get("subagent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("agent");
    let description = input
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let prompt = input.get("prompt").and_then(|v| v.as_str());

    FormattedToolCall {
        header: format!("Task ({}): {}", subagent_type, description),
        body: prompt.map(|p| p.to_string()),
    }
}

fn format_bash(input: &Value, max_width: usize) -> FormattedToolCall {
    let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let prefix = "Bash: ";
    let prefix_len = prefix.len();

    // Available width for command text (accounting for prefix on first line)
    let available_width = max_width.saturating_sub(prefix_len);

    let wrapped: Vec<_> = if command.contains('\n') {
        command
            .lines()
            .flat_map(|line| {
                if available_width == 0 || line.chars().count() <= available_width {
                    vec![line.to_string()]
                } else {
                    textwrap::wrap(line, available_width)
                        .into_iter()
                        .map(|cow| cow.into_owned())
                        .collect()
                }
            })
            .collect()
    } else if available_width == 0 || command.chars().count() <= available_width {
        return FormattedToolCall {
            header: format!("{}{}", prefix, command),
            body: None,
        };
    } else {
        textwrap::wrap(command, available_width)
            .into_iter()
            .map(|cow| cow.into_owned())
            .collect()
    };

    if wrapped.len() <= 1 {
        return FormattedToolCall {
            header: format!("{}{}", prefix, command),
            body: None,
        };
    }

    // First line goes in header, rest in body
    let header = format!("{}{}", prefix, wrapped[0]);
    let body = wrapped[1..].join("\n");

    FormattedToolCall {
        header,
        body: Some(body),
    }
}

fn format_read(input: &Value) -> FormattedToolCall {
    let file_path = string_field(input, &["file_path", "path"]).unwrap_or("");
    let offset = input.get("offset").and_then(|v| v.as_u64());
    let limit = input.get("limit").and_then(|v| v.as_u64());

    let header = match (offset, limit) {
        (Some(o), Some(l)) => format!("Read: {}:{}-{}", file_path, o, o + l),
        (Some(o), None) => format!("Read: {}:{}", file_path, o),
        _ => format!("Read: {}", file_path),
    };

    FormattedToolCall { header, body: None }
}

fn format_grep(input: &Value) -> FormattedToolCall {
    let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    let path = input.get("path").and_then(|v| v.as_str());
    let glob = input.get("glob").and_then(|v| v.as_str());

    let location = match (path, glob) {
        (Some(p), Some(g)) => format!("{}/{}", p, g),
        (Some(p), None) => p.to_string(),
        (None, Some(g)) => g.to_string(),
        (None, None) => ".".to_string(),
    };

    FormattedToolCall {
        header: format!("Grep: \"{}\" in {}", pattern, location),
        body: None,
    }
}

fn format_glob(input: &Value) -> FormattedToolCall {
    let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    let path = input.get("path").and_then(|v| v.as_str());

    let header = match path {
        Some(p) => format!("Find: {} in {}", pattern, p),
        None => format!("Find: {}", pattern),
    };

    FormattedToolCall { header, body: None }
}

fn format_ls(input: &Value) -> FormattedToolCall {
    let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    FormattedToolCall {
        header: format!("Ls: {}", path),
        body: None,
    }
}

fn format_edit(input: &Value) -> FormattedToolCall {
    let file_path = string_field(input, &["file_path", "path"]).unwrap_or("");
    let body = render_edit_diffs().then(|| edit_diff_body(input)).flatten();

    FormattedToolCall {
        header: format!("Edit: {}", file_path),
        body,
    }
}

fn edit_diff_body(input: &Value) -> Option<String> {
    let old_string = string_field(input, &["old_string", "oldText"]);
    let new_string = string_field(input, &["new_string", "newText"]);
    if let (Some(old), Some(new)) = (old_string, new_string) {
        return Some(diff_strings(old, new));
    }

    let edits = input.get("edits").and_then(|v| v.as_array())?;
    let mut parts = Vec::new();
    for (idx, edit) in edits.iter().enumerate() {
        let old = string_field(edit, &["old_string", "oldText"]);
        let new = string_field(edit, &["new_string", "newText"]);
        if let (Some(old), Some(new)) = (old, new) {
            if edits.len() > 1 {
                parts.push(format!("@@ edit {} @@", idx + 1));
            }
            parts.push(diff_strings(old, new));
        }
    }

    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn diff_strings(old: &str, new: &str) -> String {
    let mut diff = String::new();
    for line in old.lines() {
        diff.push_str(&format!("- {}\n", line));
    }
    for line in new.lines() {
        diff.push_str(&format!("+ {}\n", line));
    }
    if diff.ends_with('\n') {
        diff.pop();
    }
    diff
}

fn format_write(input: &Value) -> FormattedToolCall {
    let file_path = string_field(input, &["file_path", "path"]).unwrap_or("");
    let content = input.get("content").and_then(|v| v.as_str());

    FormattedToolCall {
        header: format!("Write: {}", file_path),
        body: content.map(|c| c.to_string()),
    }
}

fn format_custom_tool(name: &str, input: &Value) -> FormattedToolCall {
    let action = input.get("action").and_then(|v| v.as_str());
    let text = input.get("text").and_then(|v| v.as_str());
    let id = input.get("id").and_then(|v| v.as_u64());
    let path = input.get("path").and_then(|v| v.as_str());

    let mut parts = Vec::new();
    if let Some(action) = action {
        parts.push(action.to_string());
    }
    if let Some(id) = id {
        parts.push(format!("#{id}"));
    }
    if let Some(text) = text {
        parts.push(text.to_string());
    }
    if let Some(path) = path {
        parts.push(path.to_string());
    }

    let header = if parts.is_empty() {
        format!("{}:", display_tool_name(name))
    } else {
        format!("{}: {}", display_tool_name(name), parts.join(" "))
    };

    let body = if parts.is_empty() {
        serde_json::to_string_pretty(input).ok()
    } else {
        None
    };

    FormattedToolCall { header, body }
}

fn display_tool_name(name: &str) -> String {
    normalize_tool_name(name)
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_web_fetch(input: &Value) -> FormattedToolCall {
    let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let prompt = input.get("prompt").and_then(|v| v.as_str());

    FormattedToolCall {
        header: format!("Fetch: {}", url),
        body: prompt.map(|p| p.to_string()),
    }
}

fn format_web_search(input: &Value) -> FormattedToolCall {
    let query = input.get("query").and_then(|v| v.as_str()).unwrap_or("");

    FormattedToolCall {
        header: format!("Search: \"{}\"", query),
        body: None,
    }
}

fn string_field<'a>(input: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| input.get(*key).and_then(|v| v.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_format_task() {
        let input = json!({
            "subagent_type": "Explore",
            "description": "Find the bug",
            "prompt": "Look for issues in the code"
        });
        let result = format_tool_call("Task", &input, 80);
        assert_eq!(result.header, "Task (Explore): Find the bug");
        assert_eq!(result.body, Some("Look for issues in the code".to_string()));
    }

    #[test]
    fn test_format_bash() {
        let input = json!({
            "command": "git status",
            "description": "Check repo status"
        });
        let result = format_tool_call("Bash", &input, 80);
        assert_eq!(result.header, "Bash: git status");
        assert_eq!(result.body, None);
    }

    #[test]
    fn test_format_bash_wrapping() {
        let long_command = "cargo build --release --features 'feature1 feature2 feature3' --target x86_64-unknown-linux-gnu";
        let input = json!({
            "command": long_command
        });
        // With width 40, command should wrap (available width is 40 - 6 = 34 for command text)
        let result = format_tool_call("Bash", &input, 40);
        assert!(result.header.starts_with("Bash: cargo"));
        assert!(
            result.body.is_some(),
            "Long command should have body for continuation"
        );
    }

    #[test]
    fn test_format_bash_no_wrap_when_fits() {
        let input = json!({
            "command": "ls -la"
        });
        let result = format_tool_call("Bash", &input, 80);
        assert_eq!(result.header, "Bash: ls -la");
        assert_eq!(result.body, None);
    }

    #[test]
    fn test_format_bash_multiline_command_has_body() {
        let input = json!({
            "command": "one\ntwo"
        });
        let result = format_tool_call("Bash", &input, 80);
        assert_eq!(result.header, "Bash: one");
        assert_eq!(result.body, Some("two".to_string()));
    }

    #[test]
    fn test_format_bash_wraps_long_lines_in_multiline_command() {
        let input = json!({
            "command": "alpha beta gamma\nnext"
        });
        let result = format_tool_call("Bash", &input, 12);
        assert_eq!(result.header, "Bash: alpha");
        assert_eq!(result.body, Some("beta\ngamma\nnext".to_string()));
    }

    #[test]
    fn test_format_read_with_range() {
        let input = json!({
            "file_path": "/src/main.rs",
            "offset": 100,
            "limit": 50
        });
        let result = format_tool_call("Read", &input, 80);
        assert_eq!(result.header, "Read: /src/main.rs:100-150");
    }

    #[test]
    fn test_format_grep() {
        let input = json!({
            "pattern": "fn main",
            "path": "src",
            "glob": "*.rs"
        });
        let result = format_tool_call("Grep", &input, 80);
        assert_eq!(result.header, "Grep: \"fn main\" in src/*.rs");
    }

    #[test]
    fn test_format_edit() {
        let input = json!({
            "file_path": "/src/lib.rs",
            "old_string": "old code",
            "new_string": "new code"
        });
        let result = format_tool_call("Edit", &input, 80);
        assert_eq!(result.header, "Edit: /src/lib.rs");
        assert_eq!(result.body, Some("- old code\n+ new code".to_string()));
    }
}
