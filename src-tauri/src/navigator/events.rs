use serde::{Deserialize, Serialize};

use super::providers;

/// Agents copiwaifu integrates. The wire format and session files use the
/// kebab form; OpenCode is a single word so it gets an explicit rename
/// (otherwise kebab-case would yield "open-code").
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentType {
    ClaudeCode,
    #[serde(rename = "opencode")]
    OpenCode,
}

impl AgentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::OpenCode => "opencode",
        }
    }
}

/// Why a session is waiting on the user — drives popup card copy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    /// CC is asking for a tool permission (PermissionRequest / Notification).
    Permission,
    /// CC is asking the user to pick something (AskUserQuestion / ExitPlanMode).
    Choice,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    SessionStart,
    SessionEnd,
    Thinking,
    ToolUse,
    ToolResult,
    Error,
    Complete,
    NeedsAttention,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Idle,
    Thinking,
    ToolUse,
    Error,
    Complete,
    NeedsAttention,
}

impl AgentState {
    pub fn priority(&self) -> u8 {
        match self {
            Self::NeedsAttention => 5,
            Self::Error => 4,
            Self::ToolUse => 3,
            Self::Thinking => 2,
            Self::Complete => 1,
            Self::Idle => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Idle,
    Processing,
    RunningTool,
    WaitingAttention,
    Completed,
    Error,
}

impl SessionPhase {
    pub fn as_agent_state(&self) -> AgentState {
        match self {
            Self::Idle => AgentState::Idle,
            Self::Processing => AgentState::Thinking,
            Self::RunningTool => AgentState::ToolUse,
            Self::WaitingAttention => AgentState::NeedsAttention,
            Self::Completed => AgentState::Complete,
            Self::Error => AgentState::Error,
        }
    }

    pub fn priority(&self) -> u8 {
        self.as_agent_state().priority()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct EventData {
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub session_title: Option<String>,
    #[serde(default)]
    pub needs_attention: Option<bool>,
    #[serde(default)]
    pub attention_kind: Option<AttentionKind>,
    #[serde(default)]
    pub turn_start: bool,
    #[serde(default)]
    pub turn_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTalkEventDigest {
    pub event_type: EventType,
    pub timestamp_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub informative: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTalkContext {
    pub agent: AgentType,
    pub session_id: String,
    pub state: AgentState,
    pub phase: SessionPhase,
    pub turn_index: u64,
    pub updated_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_event_type: Option<EventType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_meaningful_summary: Option<String>,
    pub has_context: bool,
    pub missing_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<AiTalkEventDigest>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentEvent {
    pub agent: AgentType,
    pub session_id: String,
    pub event: EventType,
    #[serde(default)]
    pub data: EventData,
}

/// Wire shape POSTed by hooks/copiwaifu-hook.js: { agent, session_id, event, data }.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct IncomingHookEvent {
    #[serde(default)]
    pub agent: Option<AgentType>,
    pub session_id: String,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub data: EventData,
}

impl IncomingHookEvent {
    pub fn into_agent_event(self) -> Result<AgentEvent, String> {
        let agent = self.agent.ok_or_else(|| "missing agent".to_string())?;
        let raw_event = self
            .event
            .as_deref()
            .ok_or_else(|| "missing event".to_string())?;
        let event = providers::normalize_event(raw_event)?;

        Ok(AgentEvent {
            agent,
            session_id: self.session_id,
            event,
            data: self.data,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StateChangePayload {
    pub state: AgentState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_attention: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_talk_context: Option<AiTalkContext>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NavigatorSessionPayload {
    pub agent: AgentType,
    pub session_id: String,
    pub phase: SessionPhase,
    pub state: AgentState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_attention: Option<bool>,
    /// Bumped each time the session enters needs_attention — identifies one
    /// concrete approval/choice request, so notifications map 1:1 to popups.
    pub attention_epoch: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention_kind: Option<AttentionKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_talk_context: Option<AiTalkContext>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NavigatorSessionsPayload {
    pub sessions: Vec<NavigatorSessionPayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_port: Option<u16>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NavigatorStatus {
    pub current: StateChangePayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_port: Option<u16>,
}

#[derive(Clone, Debug)]
pub enum NavigatorEmission {
    StateChange(StateChangePayload),
    SessionsChanged(NavigatorSessionsPayload),
}
