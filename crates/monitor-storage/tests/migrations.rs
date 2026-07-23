use monitor_storage::{connect, migrate, BUSY_TIMEOUT};
use sqlx::Row;

#[tokio::test]
async fn fresh_database_migrates_twice_and_has_required_pragmas() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let pool = connect(&database).await.unwrap();

    migrate(&pool).await.unwrap();
    migrate(&pool).await.unwrap();

    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_sqlx_migrations'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 10);

    let row = sqlx::query(
        "SELECT \
         (SELECT foreign_keys FROM pragma_foreign_keys()) AS foreign_keys, \
         (SELECT journal_mode FROM pragma_journal_mode()) AS journal_mode, \
         (SELECT synchronous FROM pragma_synchronous()) AS synchronous, \
         (SELECT timeout FROM pragma_busy_timeout()) AS busy_timeout",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<i64, _>("foreign_keys"), 1);
    assert_eq!(row.get::<String, _>("journal_mode").to_lowercase(), "wal");
    assert_eq!(row.get::<i64, _>("synchronous"), 1);
    assert_eq!(
        row.get::<i64, _>("busy_timeout"),
        BUSY_TIMEOUT.as_millis() as i64
    );

    let versions: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(versions, 3);
}
