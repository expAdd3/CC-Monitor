use monitor_domain::{
    reduce, AgentEvent, AgentKind, Confidence, EventId, EventSource, NotificationKind, SessionId,
    SessionLifecycle, StateReason, TurnState,
};
use serde_json::Value;
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
};

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/hooks")
        .join(name);
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn enum_lifecycle(value: &str) -> SessionLifecycle {
    match value {
        "active" => SessionLifecycle::Active,
        "ended" => SessionLifecycle::Ended,
        other => panic!("unknown lifecycle {other}"),
    }
}

fn enum_turn(value: &str) -> TurnState {
    match value {
        "running" => TurnState::Running,
        "waiting" => TurnState::Waiting,
        "needs_input" => TurnState::NeedsInput,
        "failed" => TurnState::Failed,
        other => panic!("unknown turn state {other}"),
    }
}

fn enum_reason(value: &str) -> StateReason {
    match value {
        "session_started" => StateReason::SessionStarted,
        "user_prompt_submitted" => StateReason::UserPromptSubmitted,
        "ask_user_question" => StateReason::AskUserQuestion,
        "tool_running" => StateReason::ToolRunning,
        "permission_prompt" => StateReason::PermissionPrompt,
        "elicitation_dialog" => StateReason::ElicitationDialog,
        "idle_prompt" => StateReason::IdlePrompt,
        "auth_success" => StateReason::AuthSuccess,
        "elicitation_complete" => StateReason::ElicitationComplete,
        "elicitation_response" => StateReason::ElicitationResponse,
        "unknown_notification" => StateReason::UnknownNotification,
        "turn_stopped" => StateReason::TurnStopped,
        "stop_failed" => StateReason::StopFailed,
        "session_ended" => StateReason::SessionEnded,
        other => panic!("unknown reason {other}"),
    }
}

fn enum_notification(value: &str) -> NotificationKind {
    match value {
        "needs_input" => NotificationKind::NeedsInput,
        "done" => NotificationKind::Done,
        "failed" => NotificationKind::Failed,
        other => panic!("unknown notification {other}"),
    }
}

fn assert_projection_snapshot(
    projection: &monitor_domain::SessionProjection,
    expected: &Value,
    context: &str,
) {
    assert_eq!(
        projection.lifecycle,
        enum_lifecycle(expected["lifecycle"].as_str().unwrap()),
        "{context}"
    );
    assert_eq!(
        projection.turn_state,
        enum_turn(expected["turn_state"].as_str().unwrap()),
        "{context}"
    );
    if let Some(reason) = expected.get("reason").and_then(Value::as_str) {
        assert_eq!(projection.reason, enum_reason(reason), "{context}");
    }
    if let Some(source) = expected.get("source").and_then(Value::as_str) {
        let source = match source {
            "hook" => EventSource::Hook,
            "transcript" => EventSource::Transcript,
            "recovery" => EventSource::Recovery,
            other => panic!("unknown source {other}"),
        };
        assert_eq!(projection.source, source, "{context}");
    }
    if let Some(confidence) = expected.get("confidence").and_then(Value::as_str) {
        let confidence = match confidence {
            "definitive" => Confidence::Definitive,
            "inferred" => Confidence::Inferred,
            other => panic!("unknown confidence {other}"),
        };
        assert_eq!(projection.confidence, confidence, "{context}");
    }
    if let Some(revision) = expected.get("revision").and_then(Value::as_u64) {
        assert_eq!(projection.revision, revision, "{context}");
    }
    if let Some(changed) = expected.get("changed_at_ms").and_then(Value::as_i64) {
        assert_eq!(projection.changed_at_ms, changed, "{context}");
    }
    if let Some(observed) = expected.get("last_observed_at_ms").and_then(Value::as_i64) {
        assert_eq!(projection.last_observed_at_ms, observed, "{context}");
    }
}

fn fixture_timestamp_ms(payload: &Value) -> i64 {
    const BASE_MS: i64 = 1_768_039_200_000;
    payload
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|timestamp| timestamp.get(17..19))
        .and_then(|seconds| seconds.parse::<i64>().ok())
        .map_or(BASE_MS, |seconds| BASE_MS + seconds * 1_000)
}

fn hook_event(payload: &Value, _ordinal: usize) -> AgentEvent {
    let identity = payload
        .get("_ingestion_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let mut hasher = DefaultHasher::new();
            payload.to_string().hash(&mut hasher);
            format!("fixture-{:016x}", hasher.finish())
        });
    let mut normalized_payload = serde_json::Map::new();
    for key in [
        "notification_type",
        "tool_name",
        "cwd",
        "transcript_path",
        "client_bundle_id",
    ] {
        if let Some(value) = payload.get(key).filter(|value| !value.is_null()) {
            normalized_payload.insert(key.to_owned(), value.clone());
        }
    }
    AgentEvent {
        id: EventId(identity.clone()),
        agent_kind: AgentKind::claude(),
        session_id: SessionId(
            payload["session_id"]
                .as_str()
                .unwrap_or("fixture-session")
                .to_owned(),
        ),
        source: EventSource::Hook,
        source_event: payload["hook_event_name"].as_str().unwrap().to_owned(),
        occurred_at_ms: fixture_timestamp_ms(payload),
        received_at_ms: fixture_timestamp_ms(payload),
        sequence_no: None,
        dedupe_key: format!("hook|{identity}"),
        payload_version: 1,
        payload: Value::Object(normalized_payload),
    }
}

#[test]
fn normalized_event_snapshots_deserialize_as_domain_events() {
    for snapshot in fixture("normalized_event_snapshots.json")
        .as_array()
        .unwrap()
    {
        let event: AgentEvent = serde_json::from_value(snapshot["expected"].clone()).unwrap();
        assert_eq!(event.id.0, snapshot["ingestion_id"].as_str().unwrap());
        assert_eq!(event.agent_kind, AgentKind::claude());
        assert_eq!(event.source, EventSource::Hook);
        assert_eq!(event.dedupe_key, format!("hook|{}", event.id.0));
        assert_eq!(event.payload_version, 1);
    }
}

#[test]
fn transition_table_fixture_rows_reduce_to_expected_state() {
    let expected_projections = fixture("expected_projection_snapshots.json");
    let expected_steps = fixture("transition_step_snapshots.json");
    for case in fixture("transition_cases.json").as_array().unwrap() {
        let events: Vec<_> = case["payloads"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(index, payload)| hook_event(payload, index + 1))
            .collect();
        let reduced = reduce(events.clone());
        let projection = reduced.projection.unwrap();
        let name = case["name"].as_str().unwrap();
        let expected = &expected_projections[name];
        assert_projection_snapshot(&projection, expected, name);
        assert_eq!(projection.agent_kind, AgentKind::claude(), "{name}");
        assert_eq!(
            projection.session_id,
            SessionId::from("fixture-session"),
            "{name}"
        );

        let steps = expected_steps[name].as_array().unwrap();
        let mut previous_notifications = 0;
        for end in 1..=events.len() {
            let prefix = reduce(events[..end].iter().cloned());
            let prefix_projection = prefix.projection.unwrap();
            let step = &steps[end - 1];
            assert_eq!(
                prefix_projection.lifecycle,
                enum_lifecycle(step["lifecycle"].as_str().unwrap()),
                "{name} step {end}"
            );
            assert_eq!(
                prefix_projection.turn_state,
                enum_turn(step["turn_state"].as_str().unwrap()),
                "{name} step {end}"
            );
            let expected_new: Vec<_> = step["new_notifications"]
                .as_array()
                .unwrap()
                .iter()
                .map(|kind| enum_notification(kind.as_str().unwrap()))
                .collect();
            let actual_new = &prefix.notifications[previous_notifications..];
            assert_eq!(
                actual_new.iter().map(|edge| edge.kind).collect::<Vec<_>>(),
                expected_new,
                "{name} step {end}"
            );
            assert!(
                actual_new
                    .iter()
                    .all(|edge| edge.projection_revision == prefix_projection.revision),
                "{name} step {end}"
            );
            previous_notifications = prefix.notifications.len();
        }
    }
}

#[test]
fn duplicates_are_idempotent() {
    for case in fixture("duplicate_journals.json").as_array().unwrap() {
        let events: Vec<_> = case["payloads"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(index, payload)| hook_event(payload, index + 1))
            .collect();
        let reduced = reduce(events);
        let expected = &case["expected"];
        let projection = reduced.projection.unwrap();
        assert_eq!(projection.revision, expected["revision"].as_u64().unwrap());
        assert_eq!(
            projection.lifecycle,
            enum_lifecycle(expected["lifecycle"].as_str().unwrap())
        );
        assert_eq!(
            projection.turn_state,
            enum_turn(expected["turn_state"].as_str().unwrap())
        );
        let actual_keys: Vec<_> = reduced
            .notifications
            .iter()
            .map(|edge| {
                let kind = match edge.kind {
                    NotificationKind::NeedsInput => "needs_input",
                    NotificationKind::Done => "done",
                    NotificationKind::Failed => "failed",
                };
                format!(
                    "{}:{}:{kind}",
                    projection.session_id.0, edge.projection_revision
                )
            })
            .collect();
        let expected_keys: Vec<_> = expected["notification_keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|key| key.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(actual_keys, expected_keys, "{}", case["name"]);
    }
}

#[test]
fn shuffled_cross_source_fixture_has_one_deterministic_result() {
    let journal = fixture("ordering_journal.json");
    let events: Vec<AgentEvent> = serde_json::from_value(journal["events"].clone()).unwrap();
    let mut ordered = events.clone();
    ordered.sort_by(AgentEvent::replay_cmp);
    assert_eq!(
        ordered
            .iter()
            .map(|event| event.id.0.as_str())
            .collect::<Vec<_>>(),
        journal["expected"]["ordered_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap())
            .collect::<Vec<_>>()
    );
    let forward = reduce(events.clone());
    let reverse = reduce(events.into_iter().rev());
    assert_eq!(forward.projection, reverse.projection);
    assert_eq!(forward.notifications, reverse.notifications);

    let projection = forward.projection.unwrap();
    assert_projection_snapshot(&projection, &journal["expected"]["projection"], "ordering");
    assert_eq!(
        forward
            .notifications
            .iter()
            .map(|edge| (edge.kind, edge.projection_revision))
            .collect::<Vec<_>>(),
        vec![
            (NotificationKind::NeedsInput, 4),
            (NotificationKind::Done, 5),
        ]
    );
}

#[test]
fn ended_and_terminal_guards_match_fixture_contract() {
    for case in fixture("terminal_guard_journals.json").as_array().unwrap() {
        let events: Vec<_> = case["payloads"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(index, payload)| hook_event(payload, index + 1))
            .collect();
        if case["name"] == "same_ms_distinct_ingestions_are_distinct" {
            assert_eq!(events[0].occurred_at_ms, events[1].occurred_at_ms);
            assert_ne!(events[0].dedupe_key, events[1].dedupe_key);
        }
        let reduced = reduce(events);
        let projection = reduced.projection.unwrap();
        let expected = &case["expected"];
        let name = case["name"].as_str().unwrap();
        assert_projection_snapshot(&projection, expected, name);
        let expected_kinds: Vec<_> = expected["notifications"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kind| enum_notification(kind.as_str().unwrap()))
            .collect();
        assert_eq!(
            reduced
                .notifications
                .iter()
                .map(|edge| edge.kind)
                .collect::<Vec<_>>(),
            expected_kinds,
            "{name}"
        );
        let expected_revisions: &[u64] = match name {
            "ended_ignores_late_events" | "session_start_reactivates" => &[],
            "first_terminal_wins_then_new_turn_resets" => &[2, 6],
            "same_ms_distinct_ingestions_are_distinct" => &[2],
            other => panic!("uncovered terminal fixture {other}"),
        };
        assert_eq!(
            reduced
                .notifications
                .iter()
                .map(|edge| edge.projection_revision)
                .collect::<Vec<_>>(),
            expected_revisions,
            "{name}"
        );
    }
}

#[test]
fn transcript_transition_rows_and_boundaries_are_deterministic() {
    let cases = [
        ("TranscriptAssistantToolUse", None, TurnState::Running),
        ("TranscriptAssistantThinking", None, TurnState::Running),
        ("TranscriptToolResult", None, TurnState::Running),
        ("TranscriptAssistantText", Some(-2_000), TurnState::Running),
        ("TranscriptAssistantText", Some(7_000), TurnState::Running),
        ("TranscriptAssistantText", Some(8_000), TurnState::Running),
        ("TranscriptAssistantText", Some(20_000), TurnState::Running),
        ("TranscriptAssistantText", Some(30_000), TurnState::Running),
        ("TranscriptAssistantText", Some(31_000), TurnState::Waiting),
    ];
    for (index, (source_event, idle_ms, expected)) in cases.into_iter().enumerate() {
        let event = AgentEvent {
            id: EventId(format!("transcript-{index}")),
            agent_kind: AgentKind::claude(),
            session_id: SessionId(format!("transcript-session-{index}")),
            source: EventSource::Transcript,
            source_event: source_event.to_owned(),
            occurred_at_ms: 1_000,
            received_at_ms: 1_000,
            sequence_no: Some(index as i64),
            dedupe_key: format!("transcript|{index}"),
            payload_version: 1,
            payload: idle_ms.map_or_else(
                || serde_json::json!({}),
                |value| serde_json::json!({ "idle_ms": value }),
            ),
        };
        let reduced = reduce([event]);
        let projection = reduced.projection.unwrap();
        assert_eq!(projection.lifecycle, SessionLifecycle::Active);
        assert_eq!(projection.turn_state, expected);
        assert!(reduced.notifications.is_empty());
    }
}

fn event_at(
    id: &str,
    source: EventSource,
    source_event: &str,
    at_ms: i64,
    payload: Value,
) -> AgentEvent {
    AgentEvent {
        id: EventId::from(id),
        agent_kind: AgentKind::claude(),
        session_id: SessionId::from("truth-guard-session"),
        source,
        source_event: source_event.to_owned(),
        occurred_at_ms: at_ms,
        received_at_ms: at_ms,
        sequence_no: None,
        dedupe_key: format!("{source_event}|{id}"),
        payload_version: 1,
        payload,
    }
}

#[test]
fn transcript_truth_guard_protects_exact_boundary_and_allows_after_it() {
    let hook = event_at(
        "hook-running",
        EventSource::Hook,
        "UserPromptSubmit",
        1_000,
        serde_json::json!({}),
    );
    let exact = event_at(
        "transcript-exact",
        EventSource::Transcript,
        "TranscriptAssistantText",
        121_000,
        serde_json::json!({ "idle_ms": 31_000 }),
    );
    let protected = reduce([hook.clone(), exact]);
    let projection = protected.projection.unwrap();
    assert_eq!(projection.turn_state, TurnState::Running);
    assert_eq!(projection.reason, StateReason::UserPromptSubmitted);
    assert_eq!(projection.source, EventSource::Hook);
    assert_eq!(projection.confidence, Confidence::Definitive);

    let outside = event_at(
        "transcript-outside",
        EventSource::Transcript,
        "TranscriptAssistantText",
        121_001,
        serde_json::json!({ "idle_ms": 31_000 }),
    );
    let inferred = reduce([hook, outside]).projection.unwrap();
    assert_eq!(inferred.turn_state, TurnState::Waiting);
    assert_eq!(inferred.reason, StateReason::TranscriptIdle);
    assert_eq!(inferred.source, EventSource::Transcript);
    assert_eq!(inferred.confidence, Confidence::Inferred);
}

#[test]
fn hook_needs_input_remains_sticky_past_truth_window_until_hook_resolution() {
    let needs_input = event_at(
        "hook-needs-input",
        EventSource::Hook,
        "Notification",
        1_000,
        serde_json::json!({ "notification_type": "permission_prompt" }),
    );
    let stale_transcript = event_at(
        "transcript-stale",
        EventSource::Transcript,
        "TranscriptAssistantText",
        500_000,
        serde_json::json!({ "idle_ms": 31_000 }),
    );
    let still_sticky = reduce([needs_input.clone(), stale_transcript]);
    assert_eq!(
        still_sticky.projection.unwrap().turn_state,
        TurnState::NeedsInput
    );
    assert_eq!(
        still_sticky
            .notifications
            .iter()
            .map(|edge| edge.kind)
            .collect::<Vec<_>>(),
        vec![NotificationKind::NeedsInput]
    );

    let resolved = event_at(
        "hook-resolved",
        EventSource::Hook,
        "PostToolUse",
        500_001,
        serde_json::json!({}),
    );
    let after_resolution = event_at(
        "transcript-after-resolution",
        EventSource::Transcript,
        "TranscriptAssistantText",
        620_002,
        serde_json::json!({ "idle_ms": 31_000 }),
    );
    let inferred = reduce([needs_input, resolved, after_resolution])
        .projection
        .unwrap();
    assert_eq!(inferred.turn_state, TurnState::Waiting);
    assert_eq!(inferred.source, EventSource::Transcript);
}

#[test]
fn post_tool_use_resolves_ask_user_question_before_stop() {
    assert_hook_resolution_then_done("PostToolUse", serde_json::json!({}));
}

#[test]
fn normal_pre_tool_use_resolves_ask_user_question_before_stop() {
    assert_hook_resolution_then_done("PreToolUse", serde_json::json!({ "tool_name": "Read" }));
}

fn assert_hook_resolution_then_done(resolving_event: &str, resolving_payload: Value) {
    let ask = event_at(
        "ask",
        EventSource::Hook,
        "PreToolUse",
        1_000,
        serde_json::json!({ "tool_name": "AskUserQuestion" }),
    );
    let resolved = event_at(
        "resolved",
        EventSource::Hook,
        resolving_event,
        2_000,
        resolving_payload,
    );
    let stop = event_at(
        "stop",
        EventSource::Hook,
        "Stop",
        3_000,
        serde_json::json!({}),
    );
    let reduced = reduce([ask, resolved, stop]);
    let projection = reduced.projection.unwrap();
    assert_eq!(projection.turn_state, TurnState::Waiting);
    assert_eq!(projection.reason, StateReason::TurnStopped);
    assert_eq!(projection.revision, 3);
    assert_eq!(
        reduced
            .notifications
            .iter()
            .map(|edge| (edge.kind, edge.projection_revision))
            .collect::<Vec<_>>(),
        vec![
            (NotificationKind::NeedsInput, 1),
            (NotificationKind::Done, 3),
        ]
    );
}
