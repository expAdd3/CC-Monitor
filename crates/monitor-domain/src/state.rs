use crate::{AgentKind, EventId, EventSource, SessionId, TurnId};
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

macro_rules! state_reasons {
    ($($variant:ident => $code:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum StateReason {
            $($variant),+
        }

        impl StateReason {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub const fn code(self) -> &'static str {
                match self {
                    $(Self::$variant => $code),+
                }
            }
        }
    };
}

state_reasons! {
    SessionStarted => "session_started",
    UserPromptSubmitted => "user_prompt_submitted",
    AskUserQuestion => "ask_user_question",
    ToolRunning => "tool_running",
    PermissionPrompt => "permission_prompt",
    ElicitationDialog => "elicitation_dialog",
    IdlePrompt => "idle_prompt",
    AuthSuccess => "auth_success",
    ElicitationComplete => "elicitation_complete",
    ElicitationResponse => "elicitation_response",
    UnknownNotification => "unknown_notification",
    TurnStopped => "turn_stopped",
    StopFailed => "stop_failed",
    SessionEnded => "session_ended",
    TranscriptToolUse => "transcript_tool_use",
    TranscriptThinking => "transcript_thinking",
    TranscriptToolResult => "transcript_tool_result",
    TranscriptActive => "transcript_active",
    TranscriptIdle => "transcript_idle",
    Recovery => "recovery",
    Unknown => "unknown",
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
    pub triggering_event_id: EventId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reduction {
    pub projection: Option<SessionProjection>,
    pub notifications: Vec<NotificationEdge>,
}

#[cfg(test)]
mod tests {
    use super::StateReason;
    use serde::Deserialize;
    use std::collections::HashSet;

    #[derive(Deserialize)]
    struct ReasonContract {
        code: String,
        label: String,
        produced: bool,
    }

    #[test]
    fn produced_reason_codes_exactly_match_the_cross_language_contract() {
        let contract: Vec<ReasonContract> = serde_json::from_str(include_str!(
            "../../../contracts/session-state-reasons.json"
        ))
        .unwrap();
        let produced: Vec<_> = contract
            .iter()
            .filter(|entry| entry.produced)
            .map(|entry| entry.code.as_str())
            .collect();
        let domain: Vec<_> = StateReason::ALL
            .iter()
            .map(|reason| reason.code())
            .collect();

        assert_eq!(domain, produced);
        assert!(contract.iter().all(|entry| !entry.label.trim().is_empty()));
        assert_eq!(
            contract
                .iter()
                .map(|entry| entry.code.as_str())
                .collect::<HashSet<_>>()
                .len(),
            contract.len()
        );
        for reason in StateReason::ALL {
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                serde_json::Value::String(reason.code().to_owned())
            );
        }
    }
}
