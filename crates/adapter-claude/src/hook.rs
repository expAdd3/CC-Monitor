use monitor_domain::{
    trusted_event_time_ms, AgentEvent, AgentKind, EventId, EventSource, SessionId,
};
use serde_json::{Map, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use uuid::Uuid;

pub const MAX_HOOK_INPUT_BYTES: usize = 256 * 1024;
pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "Notification",
    "PostToolUse",
    "PreToolUse",
];

#[derive(Debug, Error)]
pub enum NormalizeError {
    #[error("payload is not a JSON object")]
    NotObject,
    #[error("missing or invalid session_id")]
    SessionId,
    #[error("missing or unsupported hook_event_name")]
    Event,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub fn normalize_hook(
    value: &Value,
    ingestion_id: Uuid,
    received_at_ms: i64,
) -> Result<AgentEvent, NormalizeError> {
    let object = value.as_object().ok_or(NormalizeError::NotObject)?;
    let session_id = nonempty_string(object, "session_id").ok_or(NormalizeError::SessionId)?;
    let source_event = nonempty_string(object, "hook_event_name").ok_or(NormalizeError::Event)?;
    if !HOOK_EVENTS.contains(&source_event) {
        return Err(NormalizeError::Event);
    }

    let mut payload = Map::new();
    for key in [
        "notification_type",
        "tool_name",
        "cwd",
        "transcript_path",
        "client_bundle_id",
        "app_bundle_id",
    ] {
        if let Some(Value::String(value)) = object.get(key) {
            if !value.is_empty() {
                let normalized_key = if key == "app_bundle_id" {
                    "client_bundle_id"
                } else {
                    key
                };
                payload
                    .entry(normalized_key.to_owned())
                    .or_insert_with(|| Value::String(value.clone()));
            }
        }
    }
    let occurred_at_ms = trusted_event_time_ms(
        source_timestamp_ms(object).unwrap_or(received_at_ms),
        received_at_ms,
    );
    let ingestion = ingestion_id.to_string();
    Ok(AgentEvent {
        id: EventId(ingestion.clone()),
        agent_kind: AgentKind::claude(),
        session_id: SessionId(session_id.to_owned()),
        source: EventSource::Hook,
        source_event: source_event.to_owned(),
        occurred_at_ms,
        received_at_ms,
        sequence_no: integer(object.get("sequence_no")).or_else(|| integer(object.get("sequence"))),
        dedupe_key: format!("hook:{ingestion}"),
        payload_version: 1,
        payload: Value::Object(payload),
    })
}

fn nonempty_string<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn integer(value: Option<&Value>) -> Option<i64> {
    value.and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|v| i64::try_from(v).ok()))
    })
}

fn source_timestamp_ms(object: &Map<String, Value>) -> Option<i64> {
    for key in ["timestamp_ms", "occurred_at_ms", "timestamp"] {
        let Some(value) = object.get(key) else {
            continue;
        };
        if let Some(number) = value.as_i64() {
            return Some(if number.unsigned_abs() < 10_000_000_000 {
                number.saturating_mul(1_000)
            } else {
                number
            });
        }
        if let Some(number) = value.as_f64() {
            if number.is_finite() {
                let milliseconds = if number.abs() < 10_000_000_000.0 {
                    number * 1_000.0
                } else {
                    number
                };
                if milliseconds >= i64::MIN as f64 && milliseconds <= i64::MAX as f64 {
                    return Some(milliseconds.round() as i64);
                }
            }
        }
        if let Some(text) = value.as_str() {
            if let Ok(timestamp) = OffsetDateTime::parse(text, &Rfc3339) {
                return i64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000).ok();
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalization_keeps_only_approved_metadata() {
        let id = Uuid::now_v7();
        let event = normalize_hook(
            &json!({
                "session_id": "s1",
                "hook_event_name": "PreToolUse",
                "tool_name": "AskUserQuestion",
                "tool_input": {"secret": "not retained"},
                "timestamp": 1234.5
            }),
            id,
            9,
        )
        .unwrap();
        assert_eq!(event.id.0, id.to_string());
        assert_eq!(event.dedupe_key, format!("hook:{id}"));
        assert_eq!(event.occurred_at_ms, 1_234_500);
        assert_eq!(event.payload, json!({"tool_name":"AskUserQuestion"}));
    }

    #[test]
    fn missing_timestamp_falls_back_to_receipt() {
        let event = normalize_hook(
            &json!({"session_id":"s","hook_event_name":"Stop"}),
            Uuid::now_v7(),
            42,
        )
        .unwrap();
        assert_eq!(event.occurred_at_ms, 42);
    }

    #[test]
    fn implausible_numeric_timestamps_fall_back_to_receipt() {
        let received = 1_800_000_000_000;
        for timestamp in [
            serde_json::json!(-1),
            serde_json::json!(received - 86_400_001),
            serde_json::json!(received + 86_400_001),
        ] {
            let event = normalize_hook(
                &json!({
                    "session_id":"s",
                    "hook_event_name":"Stop",
                    "timestamp_ms": timestamp
                }),
                Uuid::now_v7(),
                received,
            )
            .unwrap();
            assert_eq!(event.occurred_at_ms, received);
        }
    }

    #[test]
    fn inclusive_boundary_and_rfc3339_timestamps_are_accepted() {
        let received = 1_800_000_000_000;
        for timestamp in [received - 86_400_000, received + 86_400_000] {
            let event = normalize_hook(
                &json!({
                    "session_id":"s",
                    "hook_event_name":"Stop",
                    "timestamp_ms": timestamp
                }),
                Uuid::now_v7(),
                received,
            )
            .unwrap();
            assert_eq!(event.occurred_at_ms, timestamp);
        }
        let event = normalize_hook(
            &json!({
                "session_id":"s",
                "hook_event_name":"Stop",
                "timestamp":"2027-01-15T08:00:00Z"
            }),
            Uuid::now_v7(),
            1_800_000_000_000,
        )
        .unwrap();
        assert_eq!(event.occurred_at_ms, 1_800_000_000_000);
    }

    #[test]
    fn far_rfc3339_timestamp_falls_back() {
        let received = 1_800_000_000_000;
        let event = normalize_hook(
            &json!({
                "session_id":"s",
                "hook_event_name":"Stop",
                "timestamp":"2000-01-01T00:00:00Z"
            }),
            Uuid::now_v7(),
            received,
        )
        .unwrap();
        assert_eq!(event.occurred_at_ms, received);
    }

    #[test]
    fn extreme_integer_timestamps_never_overflow() {
        let received = 1_800_000_000_000;
        for timestamp in [i64::MIN, i64::MAX] {
            let event = normalize_hook(
                &json!({
                    "session_id":"s",
                    "hook_event_name":"Stop",
                    "timestamp_ms": timestamp
                }),
                Uuid::now_v7(),
                received,
            )
            .unwrap();
            assert_eq!(event.occurred_at_ms, received);
        }
    }

    #[test]
    fn serde_json_cannot_construct_non_finite_numbers() {
        assert!(serde_json::Number::from_f64(f64::NAN).is_none());
        assert!(serde_json::Number::from_f64(f64::INFINITY).is_none());
        assert!(serde_json::Number::from_f64(f64::NEG_INFINITY).is_none());
    }
}
