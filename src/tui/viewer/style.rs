//! Label and style helpers shared across viewer entry/summary rendering.

use std::borrow::Cow;

use crate::pi;

/// Create a label for subagent entries from a parent_tool_use_id.
pub(super) fn subagent_label(parent_tool_use_id: &str) -> String {
    format!("↳{}", pi::short_parent_id(parent_tool_use_id))
}

/// Resolve the assistant-side label for the current entry: the nested
/// arrow form for subagent messages, otherwise the literal "Pi".
pub(super) fn assistant_label(parent_id: Option<&str>) -> Cow<'static, str> {
    match parent_id {
        Some(p) => Cow::Owned(subagent_label(p)),
        None => Cow::Borrowed("Pi"),
    }
}
