use monitor_storage::{
    connect, delete_price_override, list_price_overrides, load_cursor, migrate, put_price_override,
    replace_session_usage, save_cursor, PriceOverride, StoredCursor, StoredUsage,
};
use sqlx::Row;
use tempfile::tempdir;

#[tokio::test]
async fn cursor_usage_reindex_and_price_overrides_are_durable() {
    let temp = tempdir().unwrap();
    let pool = connect(&temp.path().join("state.db")).await.unwrap();
    migrate(&pool).await.unwrap();

    let cursor = StoredCursor {
        transcript_path: "/fixture/session.jsonl".into(),
        file_identity: Some("1:2".into()),
        byte_offset: 42,
        file_size: 48,
        modified_at_ms: Some(100),
        content_anchor: Some("anchor".into()),
        last_scanned_at_ms: 200,
        last_error: None,
    };
    save_cursor(&pool, &cursor).await.unwrap();
    assert_eq!(
        load_cursor(&pool, &cursor.transcript_path).await.unwrap(),
        Some(cursor)
    );

    let usage = StoredUsage {
        id: "usage-1".into(),
        session_id: "session".into(),
        transcript_path: "/fixture/session.jsonl".into(),
        source_location: "main@1:2:0".into(),
        request_id: Some("request".into()),
        message_id: Some("message".into()),
        model_id: "claude-sonnet-4-5-20250929".into(),
        local_day: "2026-01-10".into(),
        input_tokens: 100,
        output_tokens: 50,
        cache_write_tokens: 20,
        cache_read_tokens: 30,
        cost_pico_usd: 1_134_000_000,
        cost_known: true,
        dedupe_key: "claude:session:mr:message:request".into(),
        observed_at_ms: 100,
    };
    replace_session_usage(&pool, "session", std::slice::from_ref(&usage), 300)
        .await
        .unwrap();
    replace_session_usage(&pool, "session", &[usage], 301)
        .await
        .unwrap();
    let row = sqlx::query(
        "SELECT COUNT(*),SUM(input_tokens),SUM(cost_pico_usd) FROM daily_usage WHERE session_id='session'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<i64, _>(0), 1);
    assert_eq!(row.get::<i64, _>(1), 100);
    assert_eq!(row.get::<i64, _>(2), 1_134_000_000);

    let price = PriceOverride {
        model_id: "custom".into(),
        input: 1,
        output: 2,
        cache_write: 3,
        cache_read: 4,
        updated_at_ms: 5,
    };
    put_price_override(&pool, &price).await.unwrap();
    assert_eq!(list_price_overrides(&pool).await.unwrap(), vec![price]);
    delete_price_override(&pool, "custom").await.unwrap();
    assert!(list_price_overrides(&pool).await.unwrap().is_empty());
}
