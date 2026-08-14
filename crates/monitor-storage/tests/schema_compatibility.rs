use monitor_storage::{
    connect, connect_and_migrate, migrate, StorageMigrationError, MIGRATOR,
    SUPPORTED_SCHEMA_VERSION,
};
use std::fs;

async fn mark_as_future_schema(pool: &sqlx::SqlitePool) {
    MIGRATOR.run(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO _sqlx_migrations (
            version,description,success,checksum,execution_time
         ) VALUES (?1,'artificial future schema',1,X'00',0)",
    )
    .bind(SUPPORTED_SCHEMA_VERSION + 1)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn fresh_and_existing_databases_stop_at_supported_version() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");

    let pool = connect_and_migrate(&database).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(version),0) FROM _sqlx_migrations WHERE success=1"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        SUPPORTED_SCHEMA_VERSION
    );
    pool.close().await;

    let reopened = connect_and_migrate(&database).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM _sqlx_migrations WHERE success=1")
            .fetch_one(&reopened)
            .await
            .unwrap(),
        SUPPORTED_SCHEMA_VERSION
    );
}

#[tokio::test]
async fn future_schema_is_rejected_without_database_or_sidecar_writes() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let pool = connect(&database).await.unwrap();
    mark_as_future_schema(&pool).await;
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let before = fs::read(&database).unwrap();
    let mut entries_before: Vec<_> = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries_before.sort();

    let error = connect_and_migrate(&database).await.unwrap_err();
    assert!(matches!(
        error,
        StorageMigrationError::UnsupportedFutureSchema {
            found,
            supported
        } if found == SUPPORTED_SCHEMA_VERSION + 1
            && supported == SUPPORTED_SCHEMA_VERSION
    ));
    assert_eq!(fs::read(&database).unwrap(), before);

    let mut entries_after: Vec<_> = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries_after.sort();
    assert!(
        entries_after
            .iter()
            .all(|entry| entries_before.contains(entry)),
        "read-only rejection must not create a new SQLite sidecar"
    );
}

#[tokio::test]
async fn direct_migration_also_rejects_future_schema() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let pool = connect(&database).await.unwrap();
    mark_as_future_schema(&pool).await;

    let error = migrate(&pool).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "unsupported_future_schema: database version {} is newer than supported version {}",
            SUPPORTED_SCHEMA_VERSION + 1,
            SUPPORTED_SCHEMA_VERSION
        )
    );
}
