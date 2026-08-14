use monitor_storage::{
    connect, connect_options, migrate, BUSY_TIMEOUT, MIGRATOR, SUPPORTED_SCHEMA_VERSION,
};
use sqlx::{sqlite::SqlitePoolOptions, Row};

fn checksum_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn shipped_migration_bytes_are_frozen() {
    assert_eq!(SUPPORTED_SCHEMA_VERSION, 2);
    let migrations = MIGRATOR.iter().collect::<Vec<_>>();
    assert_eq!(migrations.len(), 2);
    assert_eq!(migrations[0].version, 1);
    assert_eq!(migrations[1].version, 2);
    assert_eq!(
        checksum_hex(&migrations[0].checksum),
        "c5910a96b5ec199b89e503cdfd119f2531b72c4f38ada2fceec18beb2127fb67dc93b42e83bc91a63543e2a58343ef68"
    );
    assert_eq!(
        checksum_hex(&migrations[1].checksum),
        "1f2fb54e6df19541dd7a2399e9e64f2fdfd77eccb839a1ca267298c7046d320d5ce07b4c6024dc706bc9fe7e48b3d18e"
    );
    let reference = include_str!("../../../docs/schema-v1.sql");
    let (_, reference_schema) = reference
        .split_once("PRAGMA foreign_keys = ON;\n\n")
        .expect("schema reference keeps its documented preamble");
    assert_eq!(migrations[0].sql.as_ref(), reference_schema);
}

#[tokio::test]
async fn fresh_database_migrates_twice_to_v2_with_required_pragmas() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let pool = connect(&database).await.unwrap();

    migrate(&pool).await.unwrap();
    migrate(&pool).await.unwrap();

    assert_eq!(
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT COALESCE(MAX(version),0),COUNT(*)
               FROM _sqlx_migrations WHERE success=1"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        (2, 2)
    );
    for table in [
        "raw_events",
        "session_projection",
        "turns",
        "usage_records",
        "transcript_cursors",
        "notification_outbox",
        "settings",
        "price_overrides",
        "installation",
        "notification_provider_health",
        "transcript_event_stage",
        "transcript_usage_stage",
        "background_task_health",
        "usage_session_aggregates",
        "usage_model_aggregates",
        "usage_daily_aggregates",
        "usage_aggregate_state",
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
            "missing required table {table}"
        );
    }
    for obsolete in ["daily_usage", "transcript_publish_generation"] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name=?1"
            )
            .bind(obsolete)
            .fetch_one(&pool)
            .await
            .unwrap(),
            0,
            "pre-release compatibility table must not enter the supported schema: {obsolete}"
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT is_current FROM usage_aggregate_state WHERE singleton=1"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1,
    );

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
async fn existing_v1_database_upgrades_and_backfills_usage_aggregates() {
    let directory = tempfile::tempdir().unwrap();
    let pool = connect(&directory.path().join("state.db")).await.unwrap();
    MIGRATOR.run_to(1, &pool).await.unwrap();
    sqlx::query(
        "INSERT INTO usage_records (
            id,agent_kind,session_id,transcript_path,source_location,
            model_id,local_day,input_tokens,output_tokens,cache_write_tokens,
            cache_read_tokens,cost_pico_usd,cost_known,dedupe_key,observed_at_ms
         ) VALUES (
            'usage','claude','session','/tmp/session.jsonl','line:1',
            'model','2026-08-14',10,20,30,40,50,1,'usage',1
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    migrate(&pool).await.unwrap();

    assert_eq!(
        sqlx::query_as::<_, (i64, i64, i64)>(
            "SELECT input_tokens,output_tokens,cost_pico_usd
               FROM usage_session_aggregates
              WHERE agent_kind='claude' AND session_id='session'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        (10, 20, 50),
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(version),0) FROM _sqlx_migrations WHERE success=1",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2,
    );
}

#[tokio::test]
async fn incomplete_usage_aggregate_backfill_is_repaired_with_saturating_totals() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("state.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options(&database))
        .await
        .unwrap();
    migrate(&pool).await.unwrap();
    for (id, known) in [("priced", true), ("unpriced", false)] {
        sqlx::query(
            "INSERT INTO usage_records (
                id,agent_kind,session_id,transcript_path,source_location,
                model_id,local_day,input_tokens,output_tokens,
                cache_write_tokens,cache_read_tokens,cost_pico_usd,
                cost_known,dedupe_key,observed_at_ms
             ) VALUES (?1,'claude','session','/tmp/session.jsonl',?1,
                'model','2026-08-14',?2,0,0,0,?2,?3,?1,1)",
        )
        .bind(id)
        .bind(i64::MAX - 10)
        .bind(known)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "WITH RECURSIVE sequence(value) AS (
             SELECT 1 UNION ALL SELECT value+1 FROM sequence WHERE value<513
         )
         INSERT INTO usage_records (
            id,agent_kind,session_id,transcript_path,source_location,
            model_id,local_day,input_tokens,output_tokens,cache_write_tokens,
            cache_read_tokens,cost_pico_usd,cost_known,dedupe_key,observed_at_ms
         )
         SELECT printf('paged-%04d',value),'claude','paged-session',
                '/tmp/paged.jsonl',printf('paged:%d',value),'paged-model',
                '2026-08-13',1,0,0,0,1,1,printf('paged-key-%04d',value),value
           FROM sequence",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE usage_aggregate_state SET is_current=0 WHERE singleton=1")
        .execute(&pool)
        .await
        .unwrap();

    migrate(&pool).await.unwrap();

    macro_rules! assert_backfilled {
        ($table:literal, $filter:literal) => {
            assert_eq!(
                sqlx::query_as::<_, (i64, i64, i64, i64)>(concat!(
                    "SELECT input_tokens,cost_pico_usd,cost_known,unpriced_tokens FROM ",
                    $table,
                    " WHERE ",
                    $filter
                ))
                .fetch_one(&pool)
                .await
                .unwrap(),
                (i64::MAX, i64::MAX, 0, i64::MAX - 10),
                "backfill mismatch for {}",
                $table,
            );
        };
    }
    assert_backfilled!("usage_session_aggregates", "session_id='session'");
    assert_backfilled!("usage_model_aggregates", "session_id='session'");
    assert_backfilled!("usage_daily_aggregates", "local_day='2026-08-14'");
    for query in [
        "SELECT input_tokens FROM usage_session_aggregates WHERE session_id='paged-session'",
        "SELECT input_tokens FROM usage_model_aggregates WHERE session_id='paged-session'",
        "SELECT input_tokens FROM usage_daily_aggregates WHERE local_day='2026-08-13'",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(query)
                .fetch_one(&pool)
                .await
                .unwrap(),
            513,
        );
    }
}

#[tokio::test]
async fn initial_schema_contains_every_current_lifecycle_field() {
    let directory = tempfile::tempdir().unwrap();
    let pool = connect(&directory.path().join("state.db")).await.unwrap();
    migrate(&pool).await.unwrap();

    for (table, columns) in [
        (
            "raw_events",
            &["transcript_path", "notifications_allowed"][..],
        ),
        ("usage_records", &["is_sidechain", "final_message"][..]),
        ("transcript_cursors", &["content_anchor"][..]),
        ("transcript_event_stage", &["staged_at_ms"][..]),
        (
            "transcript_usage_stage",
            &["staged_at_ms", "is_sidechain", "final_message"][..],
        ),
        ("price_overrides", &["disabled"][..]),
        ("background_task_health", &["last_transition_at_ms"][..]),
    ] {
        for column in columns {
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name=?2"
                )
                .bind(table)
                .bind(column)
                .fetch_one(&pool)
                .await
                .unwrap(),
                1,
                "missing {table}.{column}"
            );
        }
    }

    sqlx::query(
        "INSERT INTO price_overrides(
            model_id,input_pico_usd_per_million,output_pico_usd_per_million,
            cache_write_pico_usd_per_million,cache_read_pico_usd_per_million,updated_at_ms
         ) VALUES('model',11,22,33,44,55)",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT disabled FROM price_overrides WHERE model_id='model'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );

    for task in [
        "incremental_index",
        "engine_processing",
        "engine_reconciliation",
        "startup_reconciliation",
        "retention_cleanup",
    ] {
        sqlx::query("INSERT INTO background_task_health(task) VALUES(?1)")
            .bind(task)
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM pragma_table_list
              WHERE schema='main' AND name NOT LIKE 'sqlite_%'
                AND name!='_sqlx_migrations' AND strict=0"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
}
