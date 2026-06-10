use super::events::EventType;

/// Canonical event names POSTed by hooks/copiwaifu-hook.js, plus the raw
/// Claude Code hook names as a courtesy so manual curl tests work unchanged.
///
/// Note: `PostToolUseFailure` lands on ToolResult — a failed tool call is a
/// normal mid-turn beat for Claude Code, not a session-level error.
pub fn normalize_event(raw_event: &str) -> Result<EventType, String> {
    let normalized = match raw_event {
        "session_start" | "SessionStart" => Some(EventType::SessionStart),
        "session_end" | "SessionEnd" => Some(EventType::SessionEnd),
        "thinking" | "UserPromptSubmit" => Some(EventType::Thinking),
        "tool_use" | "PreToolUse" => Some(EventType::ToolUse),
        "tool_result" | "PostToolUse" | "PostToolUseFailure" => Some(EventType::ToolResult),
        "error" => Some(EventType::Error),
        "complete" | "Stop" => Some(EventType::Complete),
        "permission_request" | "PermissionRequest" | "Notification" | "needs_attention" => {
            Some(EventType::NeedsAttention)
        }
        _ => None,
    };

    normalized.ok_or_else(|| format!("unsupported event: {raw_event}"))
}
