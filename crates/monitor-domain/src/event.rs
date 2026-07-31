use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;

pub const EVENT_TIME_TRUST_WINDOW_MS: i64 = 24 * 60 * 60 * 1_000;

/// Accepts only positive source times within the inclusive receipt-time window.
pub fn trusted_event_time_ms(occurred_at_ms: i64, received_at_ms: i64) -> i64 {
    if occurred_at_ms > 0
        && occurred_at_ms.abs_diff(received_at_ms) <= EVENT_TIME_TRUST_WINDOW_MS as u64
    {
        occurred_at_ms
    } else {
        received_at_ms
    }
}

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
        trusted_event_time_ms(self.occurred_at_ms, self.received_at_ms)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(source: EventSource, occurred_at_ms: i64, received_at_ms: i64) -> AgentEvent {
        AgentEvent {
            id: EventId("event".into()),
            agent_kind: AgentKind::claude(),
            session_id: SessionId("session".into()),
            source,
            source_event: "TranscriptAssistantText".into(),
            occurred_at_ms,
            received_at_ms,
            sequence_no: None,
            dedupe_key: "event".into(),
            payload_version: 1,
            payload: json!({}),
        }
    }

    #[test]
    fn every_source_uses_the_same_inclusive_receipt_window() {
        const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
        let received = 1_800_000_000_000;
        for source in [
            EventSource::Hook,
            EventSource::Transcript,
            EventSource::Recovery,
        ] {
            for occurred in [received - DAY_MS, received, received + DAY_MS] {
                assert_eq!(
                    event(source, occurred, received).logical_at_ms(),
                    occurred,
                    "{source:?} should trust the inclusive boundary"
                );
            }
            for occurred in [
                i64::MIN,
                -1,
                0,
                received - DAY_MS - 1,
                received + DAY_MS + 1,
                i64::MAX,
            ] {
                assert_eq!(
                    event(source, occurred, received).logical_at_ms(),
                    received,
                    "{source:?} should fall back to receipt for {occurred}"
                );
            }
        }
    }
}
