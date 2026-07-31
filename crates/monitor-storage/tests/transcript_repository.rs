use monitor_domain::{AgentEvent, AgentKind, EventId, EventSource, SessionId};
use monitor_storage::{
    connect, load_cursor, migrate, remove_missing_transcripts, TranscriptCursorPosition,
    TranscriptIngestRepository,
};
use serde_json::json;
use std::collections::BTreeSet;

fn event(id: &str, session: &str) -> AgentEvent {
    AgentEvent {
        id: EventId(id.into()),
        agent_kind: AgentKind::claude(),
        session_id: SessionId(session.into()),
        source: EventSource::Transcript,
        source_event: "TranscriptAssistantText".into(),
        occurred_at_ms: 1,
        received_at_ms: 1,
        sequence_no: None,
        dedupe_key: id.into(),
        payload_version: 1,
        payload: json!({}),
    }
}

#[tokio::test]
async fn commit_advances_cursor_and_missing_file_removal_returns_affected_session() {
    let directory = tempfile::tempdir().unwrap();
    let pool = connect(&directory.path().join("state.db")).await.unwrap();
    migrate(&pool).await.unwrap();
    let path = "/tmp/session.jsonl";
    let cursor = TranscriptCursorPosition {
        byte_offset: 42,
        file_size: 42,
        content_anchor: Some("anchor".into()),
        ..TranscriptCursorPosition::default()
    };
    let mut repository = TranscriptIngestRepository::new(pool.clone());
    repository
        .begin(path.into(), "session".into(), true, false, &cursor, 1)
        .await
        .unwrap();
    repository
        .chunk(vec![event("event", "session")], Vec::new(), &cursor, 1)
        .await
        .unwrap();
    repository.commit(&cursor, 1).await.unwrap();
    assert_eq!(
        load_cursor(&pool, path).await.unwrap().unwrap().byte_offset,
        42
    );

    let affected = remove_missing_transcripts(&pool, &BTreeSet::new())
        .await
        .unwrap();
    assert_eq!(affected, vec!["session"]);
    assert!(load_cursor(&pool, path).await.unwrap().is_none());
}
