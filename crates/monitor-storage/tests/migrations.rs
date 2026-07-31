use monitor_storage::{connect, migrate, BUSY_TIMEOUT, MIGRATOR, SUPPORTED_SCHEMA_VERSION};
use sqlx::Row;
use std::borrow::Cow;

fn migrator_through(version: i64) -> sqlx::migrate::Migrator {
    sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            MIGRATOR
                .iter()
                .take_while(|migration| migration.version <= version)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    }
}

fn checksum_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn shipped_v1_through_v13_migration_bytes_are_frozen() {
    let expected = [
        (
            1,
            "ac43e25161ca8f0e13907c9cfe8282a40d484a675444058a1c9461caa88cab7073e4077d6354eb3a88552d822337be84",
        ),
        (
            2,
            "467af9ad1994be32f2cd9ed0f7b6bf32aca82090e9419b4cb179b271aebb45f6499334209ee59d642857f65372143b08",
        ),
        (
            3,
            "456a4c089680231d0dee826a89e5c3ca8a52c2d11738e9fc1dfd293e7f39a42b72f415b3a44a3ba89bafd9fa21dfc120",
        ),
        (
            4,
            "787c95b36a9054d58bfbb1ee1adc910d7b56166f5c1309a6fcd5db05b6a24fe794ad85abb8a316c1b523855a5c723667",
        ),
        (
            5,
            "710cf36e70ad8df3106c31f0ed618a02a77f26e60ec0a96e62403044dc67b01681fb566ad9c671198fc2e0c776631412",
        ),
        (
            6,
            "bc33b8aa9e8ce68d441365b382ee13e543878f10850cd9f72bd997b01d638d214c4c30d613935e9b758fac50403d1590",
        ),
        (
            7,
            "01c2a2ac5b374a0681321da1a06720f79bcbf86970453ed1c39683cc063fd3ac78c4ea05184f51e0b6c311e5c4660cb3",
        ),
        (
            8,
            "fea9797745ee87aa4d867c2c7ccbcc74104e6ecd6dabbac4de2baf63f6520a14673c843991817eeb7824393e1cb558b0",
        ),
        (
            9,
            "2ce7c5b9913cadec7f8606eca26d6627fdcaf1205446596caf7a02d0f1af3d1c0d566a8f063a4a52cc67b5028a06276d",
        ),
        (
            10,
            "30effa7d49ebdad845bb843a84b47553bc361c096e85b620ce0bfd67ee973e9c56e0d4ac328c85874ecb8ee460650c03",
        ),
        (
            11,
            "dfd4de5420707d824a60b5a3fec3c36f460a7aeea4228236f081433bacbd1abdc2c2228e8104b6c32f3a9a5b36cba9b4",
        ),
        (
            12,
            "390e74b582228c7e3e0e106554b614354b4b8957a3277e757120a1676e1d5725fa40370deae1a87629ad3126e9e007c6",
        ),
        (
            13,
            "ceb9677c60e2d02908209f598f1cf080e785a9f317f81e56ac512bda4715019d85404e5fdc75c77d9eb42e25aa97b371",
        ),
    ];

    let actual: Vec<_> = MIGRATOR
        .iter()
        .take_while(|migration| migration.version <= SUPPORTED_SCHEMA_VERSION)
        .map(|migration| (migration.version, checksum_hex(&migration.checksum)))
        .collect();
    assert_eq!(
        actual,
        expected
            .into_iter()
            .map(|(version, checksum)| (version, checksum.to_owned()))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn fresh_database_migrates_twice_to_v13_with_required_pragmas() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let pool = connect(&database).await.unwrap();

    migrate(&pool).await.unwrap();
    migrate(&pool).await.unwrap();

    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(version),0) FROM _sqlx_migrations WHERE success=1"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        SUPPORTED_SCHEMA_VERSION
    );
    for table in [
        "raw_events",
        "session_projection",
        "turns",
        "usage_records",
        "daily_usage",
        "transcript_cursors",
        "notification_outbox",
        "notification_provider_health",
        "transcript_event_stage",
        "transcript_usage_stage",
        "transcript_publish_generation",
        "background_task_health",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1"
            )
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap(),
            1,
            "missing v13 table {table}"
        );
    }

    let row = sqlx::query(
        "SELECT
         (SELECT foreign_keys FROM pragma_foreign_keys()) AS foreign_keys,
         (SELECT journal_mode FROM pragma_journal_mode()) AS journal_mode,
         (SELECT synchronous FROM pragma_synchronous()) AS synchronous,
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
}

#[tokio::test]
async fn version_nine_upgrades_staging_lifecycle_through_v13() {
    let directory = tempfile::tempdir().unwrap();
    let pool = connect(&directory.path().join("state.db")).await.unwrap();
    migrator_through(9).run(&pool).await.unwrap();

    migrate(&pool).await.unwrap();
    let columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('transcript_usage_stage')
         WHERE name IN ('staged_at_ms','is_sidechain','final_message')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(columns, 3);
}

#[tokio::test]
async fn version_ten_upgrades_publish_generation_through_v13() {
    let directory = tempfile::tempdir().unwrap();
    let pool = connect(&directory.path().join("state.db")).await.unwrap();
    migrator_through(10).run(&pool).await.unwrap();

    migrate(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE (type='table' AND name='transcript_publish_generation')
                OR (type='index' AND name IN (
                    'idx_transcript_event_stage_cleanup',
                    'idx_transcript_usage_stage_cleanup'))"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        3
    );
}

#[tokio::test]
async fn version_twelve_expands_background_health_without_data_loss() {
    let directory = tempfile::tempdir().unwrap();
    let pool = connect(&directory.path().join("state.db")).await.unwrap();
    migrator_through(12).run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO background_task_health (
            task,success_count,failure_count,consecutive_failures,
            last_error_code,last_succeeded_at_ms
         ) VALUES ('incremental_index',4,2,0,NULL,10)",
    )
    .execute(&pool)
    .await
    .unwrap();

    migrate(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, Option<i64>)>(
            "SELECT success_count,failure_count,last_transition_at_ms
               FROM background_task_health WHERE task='incremental_index'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        (4, 2, None)
    );
}
