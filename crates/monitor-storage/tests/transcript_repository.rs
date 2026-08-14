use monitor_domain::{AgentEvent, AgentKind, EventId, EventSource, SessionId};
use monitor_storage::{
    connect, load_cursor, migrate, remove_missing_transcripts, StoredUsage,
    TranscriptCursorPosition, TranscriptIngestRepository,
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

fn usage(id: &str, path: &str, dedupe_key: &str) -> StoredUsage {
    StoredUsage {
        id: id.into(),
        session_id: id.into(),
        transcript_path: path.into(),
        source_location: format!("{path}:1"),
        request_id: None,
        message_id: Some(id.into()),
        model_id: "model".into(),
        local_day: "2026-08-03".into(),
        input_tokens: 1,
        output_tokens: 2,
        cache_write_tokens: 3,
        cache_read_tokens: 4,
        cost_pico_usd: 5,
        cost_known: true,
        dedupe_key: dedupe_key.into(),
        observed_at_ms: 1,
        is_sidechain: false,
        final_message: true,
    }
}

async fn publish_usage(
    repository: &mut TranscriptIngestRepository,
    path: &str,
    id: &str,
    dedupe_key: &str,
) {
    let cursor = TranscriptCursorPosition::default();
    repository
        .begin(path.into(), id.into(), true, false, 1)
        .await
        .unwrap();
    repository
        .chunk(Vec::new(), vec![usage(id, path, dedupe_key)], 1)
        .await
        .unwrap();
    repository.commit(&cursor, 1).await.unwrap();
}

async fn publish_record(
    repository: &mut TranscriptIngestRepository,
    path: &str,
    record: StoredUsage,
) {
    let session_id = record.session_id.clone();
    let scanned_at_ms = record.observed_at_ms;
    repository
        .begin(path.into(), session_id, true, false, scanned_at_ms)
        .await
        .unwrap();
    repository
        .chunk(Vec::new(), vec![record], scanned_at_ms)
        .await
        .unwrap();
    repository
        .commit(&TranscriptCursorPosition::default(), scanned_at_ms)
        .await
        .unwrap();
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
        .begin(path.into(), "session".into(), true, false, 1)
        .await
        .unwrap();
    repository
        .chunk(vec![event("event", "session")], Vec::new(), 1)
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

#[tokio::test]
async fn publishing_one_file_does_not_rewrite_unaffected_usage_winners() {
    let directory = tempfile::tempdir().unwrap();
    let pool = connect(&directory.path().join("state.db")).await.unwrap();
    migrate(&pool).await.unwrap();
    let mut repository = TranscriptIngestRepository::new(pool.clone());
    publish_usage(&mut repository, "/tmp/stable.jsonl", "stable", "stable-key").await;
    sqlx::query(
        "CREATE TRIGGER reject_unaffected_usage_delete
         BEFORE DELETE ON usage_records
         WHEN OLD.dedupe_key='stable-key'
         BEGIN
             SELECT RAISE(FAIL,'unaffected usage winner was rewritten');
         END",
    )
    .execute(&pool)
    .await
    .unwrap();

    publish_usage(&mut repository, "/tmp/new.jsonl", "new", "new-key").await;

    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM usage_records")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2,
    );
}

#[tokio::test]
async fn removing_one_file_does_not_rewrite_unaffected_usage_winners() {
    let directory = tempfile::tempdir().unwrap();
    let pool = connect(&directory.path().join("state.db")).await.unwrap();
    migrate(&pool).await.unwrap();
    let mut repository = TranscriptIngestRepository::new(pool.clone());
    let stable_path = "/tmp/stable.jsonl";
    publish_usage(&mut repository, stable_path, "stable", "stable-key").await;
    publish_usage(
        &mut repository,
        "/tmp/removed.jsonl",
        "removed",
        "removed-key",
    )
    .await;
    sqlx::query(
        "CREATE TRIGGER reject_unaffected_usage_delete
         BEFORE DELETE ON usage_records
         WHEN OLD.dedupe_key='stable-key'
         BEGIN
             SELECT RAISE(FAIL,'unaffected usage winner was rewritten');
         END",
    )
    .execute(&pool)
    .await
    .unwrap();

    let affected = remove_missing_transcripts(&pool, &BTreeSet::from([stable_path.into()]))
        .await
        .unwrap();

    assert_eq!(affected, vec!["removed"]);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT dedupe_key FROM usage_records")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "stable-key",
    );
}

#[tokio::test]
async fn winner_replacement_moves_aggregates_and_removal_restores_the_loser() {
    let directory = tempfile::tempdir().unwrap();
    let pool = connect(&directory.path().join("state.db")).await.unwrap();
    migrate(&pool).await.unwrap();
    let mut repository = TranscriptIngestRepository::new(pool.clone());
    let loser_path = "/tmp/aggregate-loser.jsonl";
    let winner_path = "/tmp/aggregate-winner.jsonl";
    let mut loser = usage("loser", loser_path, "shared-key");
    loser.session_id = "old-session".into();
    loser.model_id = "old-model".into();
    loser.local_day = "2026-08-01".into();
    loser.input_tokens = 10;
    loser.cost_pico_usd = 100;
    loser.observed_at_ms = 1;
    publish_record(&mut repository, loser_path, loser).await;

    let mut winner = usage("winner", winner_path, "shared-key");
    winner.session_id = "new-session".into();
    winner.model_id = "new-model".into();
    winner.local_day = "2026-08-02".into();
    winner.input_tokens = 20;
    winner.cost_pico_usd = 200;
    winner.cost_known = false;
    winner.observed_at_ms = 2;
    publish_record(&mut repository, winner_path, winner).await;

    assert_eq!(
        sqlx::query_as::<_, (String, i64, i64, i64)>(
            "SELECT session_id,input_tokens,cost_known,unpriced_tokens
               FROM usage_session_aggregates"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        ("new-session".into(), 20, 0, 29),
    );
    assert_eq!(
        sqlx::query_as::<_, (String, String, i64)>(
            "SELECT session_id,model_id,input_tokens FROM usage_model_aggregates"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        ("new-session".into(), "new-model".into(), 20),
    );
    assert_eq!(
        sqlx::query_as::<_, (String, i64)>(
            "SELECT local_day,input_tokens FROM usage_daily_aggregates"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        ("2026-08-02".into(), 20),
    );

    remove_missing_transcripts(&pool, &BTreeSet::from([loser_path.into()]))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_as::<_, (String, i64, i64, i64)>(
            "SELECT session_id,input_tokens,cost_known,unpriced_tokens
               FROM usage_session_aggregates"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        ("old-session".into(), 10, 1, 0),
    );
    assert_eq!(
        sqlx::query_as::<_, (String, String, String)>(
            "SELECT session_id,model_id,local_day FROM usage_records"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        (
            "old-session".into(),
            "old-model".into(),
            "2026-08-01".into()
        ),
    );
}

#[tokio::test]
async fn aggregate_refresh_saturates_and_preserves_partial_cost_coverage_after_deletion() {
    let directory = tempfile::tempdir().unwrap();
    let pool = connect(&directory.path().join("state.db")).await.unwrap();
    migrate(&pool).await.unwrap();
    let mut repository = TranscriptIngestRepository::new(pool.clone());
    let priced_path = "/tmp/priced.jsonl";
    let unpriced_path = "/tmp/unpriced.jsonl";
    for (id, path, known) in [
        ("priced", priced_path, true),
        ("unpriced", unpriced_path, false),
    ] {
        let mut record = usage(id, path, &format!("{id}-key"));
        record.session_id = "large".into();
        record.model_id = "large-model".into();
        record.input_tokens = i64::MAX - 10;
        record.output_tokens = 0;
        record.cache_write_tokens = 0;
        record.cache_read_tokens = 0;
        record.cost_pico_usd = i64::MAX - 10;
        record.cost_known = known;
        publish_record(&mut repository, path, record).await;
    }

    assert_eq!(
        sqlx::query_as::<_, (i64, i64, i64, i64)>(
            "SELECT input_tokens,cost_pico_usd,cost_known,unpriced_tokens
               FROM usage_session_aggregates WHERE session_id='large'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        (i64::MAX, i64::MAX, 0, i64::MAX - 10),
    );

    remove_missing_transcripts(&pool, &BTreeSet::from([priced_path.into()]))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, i64, i64)>(
            "SELECT input_tokens,cost_pico_usd,cost_known,unpriced_tokens
               FROM usage_session_aggregates WHERE session_id='large'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        (i64::MAX - 10, i64::MAX - 10, 1, 0),
    );
}
