use rusqlite::Connection;
use serde_json::json;
use std::{
    io::Write,
    path::Path,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};
use tempfile::tempdir;
use uuid::Uuid;

fn run_hook(database: &Path, input: &[u8]) -> Output {
    let mut child = spawn_hook(database);
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn spawn_hook(database: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_cc-monitor-hook"))
        .args([
            "--database",
            database.to_str().unwrap(),
            "--installation-id",
            &Uuid::now_v7().to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

async fn migrated_database(path: &Path) {
    let pool = monitor_storage::connect(path).await.unwrap();
    monitor_storage::migrate(&pool).await.unwrap();
    pool.close().await;
}

#[tokio::test]
async fn valid_input_inserts_only_sanitized_raw_event() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("state.db");
    migrated_database(&database).await;
    let output = run_hook(
        &database,
        serde_json::to_vec(&json!({
            "session_id":"s1",
            "hook_event_name":"PreToolUse",
            "tool_name":"AskUserQuestion",
            "tool_input":{"secret":"must-not-persist"}
        }))
        .unwrap()
        .as_slice(),
    );
    assert!(output.status.success());
    let connection = Connection::open(database).unwrap();
    let (count, payload): (i64, String) = connection
        .query_row("SELECT COUNT(*), payload_json FROM raw_events", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(payload, r#"{"tool_name":"AskUserQuestion"}"#);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM session_projection", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn empty_invalid_and_oversized_inputs_exit_zero() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("state.db");
    migrated_database(&database).await;
    for input in [
        Vec::new(),
        b"{bad".to_vec(),
        vec![b'x'; adapter_claude::hook::MAX_HOOK_INPUT_BYTES + 1],
    ] {
        let output = run_hook(&database, &input);
        assert!(output.status.success());
        assert!(output.stderr.len() <= 96);
    }
}

#[tokio::test]
async fn missing_and_corrupt_databases_exit_zero_quickly() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("missing.db");
    let corrupt = dir.path().join("corrupt.db");
    std::fs::write(&corrupt, b"not sqlite").unwrap();
    for database in [&missing, &corrupt] {
        let started = Instant::now();
        let output = run_hook(database, br#"{"session_id":"s","hook_event_name":"Stop"}"#);
        assert!(output.status.success());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    assert!(!missing.exists(), "hook must not create a missing database");
}

#[tokio::test]
async fn locked_database_has_a_bounded_exit() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("state.db");
    migrated_database(&database).await;
    let connection = Connection::open(&database).unwrap();
    connection.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let started = Instant::now();
    let output = run_hook(&database, br#"{"session_id":"s","hook_event_name":"Stop"}"#);
    assert!(output.status.success());
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(connection);
}

#[tokio::test]
async fn stdin_kept_open_exits_zero_at_deadline() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("state.db");
    migrated_database(&database).await;
    let mut child = spawn_hook(&database);
    let open_stdin = child.stdin.take().unwrap();
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            assert!(started.elapsed() < Duration::from_millis(1_500));
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(open_stdin);
}

#[tokio::test]
async fn identical_separate_invocations_create_distinct_raw_events() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("state.db");
    migrated_database(&database).await;
    let input = br#"{"session_id":"s","hook_event_name":"Stop"}"#;
    assert!(run_hook(&database, input).status.success());
    assert!(run_hook(&database, input).status.success());
    let connection = Connection::open(database).unwrap();
    let (rows, keys): (i64, i64) = connection
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT dedupe_key) FROM raw_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((rows, keys), (2, 2));
}

#[tokio::test]
async fn minimum_integer_timestamp_exits_zero_and_falls_back_safely() {
    let dir = tempdir().unwrap();
    let database = dir.path().join("state.db");
    migrated_database(&database).await;
    let input = format!(
        r#"{{"session_id":"s","hook_event_name":"Stop","timestamp_ms":{}}}"#,
        i64::MIN
    );
    assert!(run_hook(&database, input.as_bytes()).status.success());
    let connection = Connection::open(database).unwrap();
    let (occurred, received): (i64, i64) = connection
        .query_row(
            "SELECT occurred_at_ms, received_at_ms FROM raw_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(occurred, received);
    assert!(occurred > 0);
}
