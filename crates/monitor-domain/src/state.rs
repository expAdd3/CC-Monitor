use crate::{AgentKind, EventSource, SessionId, TurnId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifecycle {
    Active,
    Ended,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    Running,
    Waiting,
    NeedsInput,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateReason {
    SessionStarted,
    UserPromptSubmitted,
    AskUserQuestion,
    ToolRunning,
    PermissionPrompt,
    ElicitationDialog,
    IdlePrompt,
    AuthSuccess,
    ElicitationComplete,
    ElicitationResponse,
    UnknownNotification,
    TurnStopped,
    StopFailed,
    SessionEnded,
    TranscriptToolUse,
    TranscriptThinking,
    TranscriptToolResult,
    TranscriptActive,
    TranscriptIdle,
    Recovery,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Definitive,
    Inferred,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionProjection {
    pub agent_kind: AgentKind,
    pub session_id: SessionId,
    pub lifecycle: SessionLifecycle,
    pub turn_state: TurnState,
    pub reason: StateReason,
    pub source: EventSource,
    pub confidence: Confidence,
    pub revision: u64,
    pub changed_at_ms: i64,
    pub last_observed_at_ms: i64,
    pub current_turn_id: Option<TurnId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    NeedsInput,
    Done,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NotificationEdge {
    pub kind: NotificationKind,
    pub projection_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reduction {
    pub projection: Option<SessionProjection>,
    pub notifications: Vec<NotificationEdge>,
}
