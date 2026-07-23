use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

string_id!(EventId);
string_id!(SessionId);
string_id!(TurnId);

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentKind(pub String);

impl AgentKind {
    pub fn claude() -> Self {
        Self("claude".to_owned())
    }
}

impl From<&str> for AgentKind {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    Hook,
    Transcript,
    Recovery,
}

impl EventSource {
    pub const fn priority(self) -> u8 {
        match self {
            Self::Transcript => 0,
            Self::Recovery => 1,
            Self::Hook => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentEvent {
    pub id: EventId,
    #[serde(alias = "agent")]
    pub agent_kind: AgentKind,
    pub session_id: SessionId,
    pub source: EventSource,
    pub source_event: String,
    pub occurred_at_ms: i64,
    pub received_at_ms: i64,
    #[serde(alias = "sequence")]
    pub sequence_no: Option<i64>,
    pub dedupe_key: String,
    pub payload_version: i64,
    pub payload: Value,
}

impl AgentEvent {
    pub fn logical_at_ms(&self) -> i64 {
        const TRUST_WINDOW_MS: i64 = 24 * 60 * 60 * 1_000;
        if self.occurred_at_ms > 0
            && self.occurred_at_ms.abs_diff(self.received_at_ms) <= TRUST_WINDOW_MS as u64
        {
            self.occurred_at_ms
        } else {
            self.received_at_ms
        }
    }

    pub fn replay_cmp(&self, other: &Self) -> Ordering {
        (
            self.logical_at_ms(),
            self.source.priority(),
            self.sequence_no.unwrap_or(i64::MAX),
            self.received_at_ms,
            &self.dedupe_key,
        )
            .cmp(&(
                other.logical_at_ms(),
                other.source.priority(),
                other.sequence_no.unwrap_or(i64::MAX),
                other.received_at_ms,
                &other.dedupe_key,
            ))
    }
}
