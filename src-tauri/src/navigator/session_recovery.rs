use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::{
    events::{AgentEvent, AgentType, EventData, EventType},
    state::NavigatorState,
};
use crate::platform;

const SESSION_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// Completed sessions are only restored if they finished this recently —
/// otherwise every app start would resurrect long-gone 完工 badges.
const COMPLETED_RESTORE_WINDOW_MS: i64 = 5 * 60 * 1000;

pub fn recover(state: &mut NavigatorState) {
    let sessions_dir = match home_sessions_dir() {
        Some(p) => p,
        None => return,
    };

    if !sessions_dir.exists() {
        return;
    }

    let entries = match fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(err) => {
            log::warn!("[recovery] read_dir failed: {err}");
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            recover_session(state, &path);
        }
    }
}

fn recover_session(state: &mut NavigatorState, path: &PathBuf) {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(err) => {
            log::warn!("[recovery] read failed {path:?}: {err}");
            return;
        }
    };

    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(err) => {
            log::warn!("[recovery] parse failed {path:?}: {err}");
            return;
        }
    };

    // 已结束的 session 直接删除
    if json.get("endedAt").is_some_and(|v| !v.is_null()) {
        let _ = fs::remove_file(path);
        return;
    }

    // 超过 24 小时的 session 删除
    if let Some(last_updated_ms) = json["lastUpdated"].as_i64() {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let age = Duration::from_millis((now_ms - last_updated_ms).max(0) as u64);
        if age > SESSION_MAX_AGE {
            let _ = fs::remove_file(path);
            return;
        }
    }

    if json["status"].as_str() == Some("completed") {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let fresh = json["lastUpdated"]
            .as_i64()
            .is_some_and(|last| now_ms.saturating_sub(last) < COMPLETED_RESTORE_WINDOW_MS);
        if !fresh {
            return; // file stays for the 24h cull; no stale badge resurrected
        }
    }

    let session_id = match json["sessionId"].as_str() {
        Some(s) => s.to_string(),
        None => return,
    };

    let agent = match json["agent"].as_str().and_then(recover_agent) {
        Some(agent) => agent,
        // Session files from agents this build no longer integrates are stale
        // app-managed cache — clean them up like any aged session file.
        None if json["agent"].as_str().is_some() => {
            let _ = fs::remove_file(path);
            return;
        }
        None => return,
    };

    // Never resurrect NeedsAttention across restarts: a persisted
    // needsAttention flag says nothing about whether the AI tool's dialog is still
    // open, and stale ones used to produce ghost popups on every app start.
    let event_type = match json["status"].as_str() {
        Some("working") => EventType::Thinking,
        Some("error") => EventType::Error,
        Some("completed") => EventType::Complete,
        _ => EventType::SessionStart,
    };

    let last_event = &json["lastEvent"];
    let tool_name = last_event["toolName"].as_str().map(str::to_string);
    let summary = recover_summary(&json);

    let event = AgentEvent {
        agent,
        session_id,
        event: event_type,
        data: EventData {
            tool_name,
            summary,
            working_directory: json["workingDirectory"].as_str().map(str::to_string),
            session_title: json["sessionTitle"].as_str().map(str::to_string),
            needs_attention: Some(false),
            attention_kind: None,
            turn_start: false,
            turn_fingerprint: None,
        },
    };

    log::info!(
        "[recovery] {} status={} restored",
        super::short_id(&event.session_id),
        json["status"].as_str().unwrap_or("?"),
    );
    state.apply_recovered_event(event);
}

fn recover_summary(json: &serde_json::Value) -> Option<String> {
    json["aiTalkContext"]["lastMeaningfulSummary"]
        .as_str()
        .or_else(|| json["lastMeaningfulSummary"].as_str())
        .map(str::to_string)
        .or_else(|| recover_summary_from_events(json))
        .or_else(|| json["lastEvent"]["summary"].as_str().map(str::to_string))
}

fn recover_summary_from_events(json: &serde_json::Value) -> Option<String> {
    let events = json["events"].as_array()?;
    events.iter().rev().find_map(|event| {
        if !event["informative"].as_bool().unwrap_or(false) {
            return None;
        }

        event["summary"].as_str().map(str::to_string)
    })
}

fn recover_agent(value: &str) -> Option<AgentType> {
    match value {
        "claude-code" => Some(AgentType::ClaudeCode),
        "codex" => Some(AgentType::Codex),
        _ => None,
    }
}

fn home_sessions_dir() -> Option<PathBuf> {
    platform::home_dir().map(|home| home.join(".copiwaifu").join("sessions"))
}

#[cfg(test)]
mod tests {
    use super::recover_agent;
    use super::super::events::AgentType;

    #[test]
    fn recovers_codex_session_agent() {
        assert_eq!(recover_agent("codex"), Some(AgentType::Codex));
        assert_eq!(recover_agent("claude-code"), Some(AgentType::ClaudeCode));
        assert_eq!(recover_agent("gemini"), None);
    }
}
