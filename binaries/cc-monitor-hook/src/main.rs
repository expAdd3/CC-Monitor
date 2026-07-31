use adapter_claude::hook::{normalize_hook, now_ms, MAX_HOOK_INPUT_BYTES};
use adapter_claude::terminal_identity::for_term_program;
use monitor_domain::{AgentEvent, EventSource};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    env,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};
use uuid::Uuid;

const BUSY_TIMEOUT: Duration = Duration::from_millis(120);
const STDIN_DEADLINE: Duration = Duration::from_millis(500);
const RETRY_DELAYS: &[Duration] = &[Duration::from_millis(20), Duration::from_millis(40)];

fn main() {
    if let Err(code) = run() {
        diagnostic(code);
    }
    // Intentionally return normally: every outcome must be exit status zero.
}

fn run() -> Result<(), &'static str> {
    let args = Args::parse().ok_or("invalid_arguments")?;
    let raw = read_stdin_with_deadline()?;
    if raw.is_empty() {
        return Err("empty_input");
    }
    let mut payload: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|_| "invalid_json")?;
    let term_program = env::var("TERM_PROGRAM").ok();
    enrich_client_bundle_id(&mut payload, term_program.as_deref());
    let ingestion_id = Uuid::now_v7();
    let event = normalize_hook(&payload, ingestion_id, now_ms()).map_err(|_| "invalid_payload")?;
    match insert_with_retry(&args.database, args.installation_id, &event) {
        Ok(()) => Ok(()),
        Err(InsertError::Ownership) => Err("installation_mismatch"),
        Err(InsertError::Storage(_)) => Err("storage_unavailable"),
    }
}

fn enrich_client_bundle_id(payload: &mut serde_json::Value, term_program: Option<&str>) {
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    if object
        .get("client_bundle_id")
        .or_else(|| object.get("app_bundle_id"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
    {
        return;
    }
    let Some(bundle_id) = term_program
        .and_then(for_term_program)
        .map(|identity| identity.canonical_bundle_id())
    else {
        return;
    };
    object.insert(
        "client_bundle_id".to_owned(),
        serde_json::Value::String(bundle_id.to_owned()),
    );
}

fn read_stdin_with_deadline() -> Result<Vec<u8>, &'static str> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("bounded-stdin".to_owned())
        .spawn(move || {
            let result = read_bounded(io::stdin());
            let _ = sender.send(result);
        })
        .map_err(|_| "input_unavailable")?;
    match receiver.recv_timeout(STDIN_DEADLINE) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(_)) => Err("invalid_input"),
        Err(mpsc::RecvTimeoutError::Timeout) => Err("input_timeout"),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err("input_unavailable"),
    }
}

struct Args {
    database: PathBuf,
    installation_id: Uuid,
}

impl Args {
    fn parse() -> Option<Self> {
        let mut values = env::args_os().skip(1);
        let mut database = None;
        let mut installation_id = None;
        while let Some(flag) = values.next() {
            match flag.to_str()? {
                "--database" => database = Some(PathBuf::from(values.next()?)),
                "--installation-id" => {
                    installation_id = Some(values.next()?.to_str()?.parse().ok()?)
                }
                _ => return None,
            }
        }
        Some(Self {
            database: database?,
            installation_id: installation_id?,
        })
    }
}

fn read_bounded(mut input: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take((MAX_HOOK_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_HOOK_INPUT_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "oversized"));
    }
    Ok(bytes)
}

#[derive(Debug)]
enum InsertError {
    Ownership,
    Storage(rusqlite::Error),
}

fn insert_with_retry(
    path: &Path,
    installation_id: Uuid,
    event: &AgentEvent,
) -> Result<(), InsertError> {
    let mut delays = RETRY_DELAYS.iter();
    loop {
        match insert_once(path, installation_id, event) {
            Ok(()) => return Ok(()),
            Err(InsertError::Storage(error)) if is_busy(&error) => {
                let Some(delay) = delays.next() else {
                    return Err(InsertError::Storage(error));
                };
                thread::sleep(*delay);
            }
            Err(error) => return Err(error),
        }
    }
}

fn insert_once(path: &Path, installation_id: Uuid, event: &AgentEvent) -> Result<(), InsertError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(InsertError::Storage)?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(InsertError::Storage)?;
    let installation_id = installation_id.to_string();
    let inserted = connection
        .execute(
            "INSERT INTO raw_events (
                id, agent_kind, session_id, source, source_event, occurred_at_ms,
                received_at_ms, sequence_no, dedupe_key, payload_version, payload_json
             )
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11
             WHERE EXISTS (
                 SELECT 1 FROM installation
                 WHERE singleton = 1 AND installation_id = ?12
             )
             ON CONFLICT(dedupe_key) DO NOTHING",
            rusqlite::params![
                event.id.0,
                event.agent_kind.0,
                event.session_id.0,
                match event.source {
                    EventSource::Hook => "hook",
                    EventSource::Transcript => "transcript",
                    EventSource::Recovery => "recovery",
                },
                event.source_event,
                event.occurred_at_ms,
                event.received_at_ms,
                event.sequence_no,
                event.dedupe_key,
                event.payload_version,
                event.payload.to_string(),
                installation_id,
            ],
        )
        .map_err(InsertError::Storage)?;
    if inserted == 0 {
        // A zero-row INSERT can also be an idempotent dedupe conflict. The
        // follow-up read is only diagnostic: collection safety is guaranteed
        // by the capability predicate inside the INSERT statement itself.
        let owned: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM installation
                 WHERE singleton = 1 AND installation_id = ?1",
                [installation_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(InsertError::Storage)?;
        if owned.is_none() {
            return Err(InsertError::Ownership);
        }
    }
    Ok(())
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

fn diagnostic(code: &str) {
    let safe: String = code
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .take(64)
        .collect();
    eprintln!("cc-monitor-hook:{safe}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_reader_rejects_oversized_input() {
        let input = vec![b'a'; MAX_HOOK_INPUT_BYTES + 1];
        assert!(read_bounded(input.as_slice()).is_err());
    }

    #[test]
    fn warp_term_program_enriches_without_overriding_explicit_identity() {
        let mut inferred = serde_json::json!({"session_id": "s"});
        enrich_client_bundle_id(&mut inferred, Some("WarpTerminal"));
        assert_eq!(
            inferred["client_bundle_id"],
            serde_json::json!("dev.warp.Warp-Stable")
        );

        let mut explicit = serde_json::json!({
            "session_id": "s",
            "client_bundle_id": "com.example.explicit"
        });
        enrich_client_bundle_id(&mut explicit, Some("Warp"));
        assert_eq!(
            explicit["client_bundle_id"],
            serde_json::json!("com.example.explicit")
        );
    }

    #[test]
    fn repeated_insert_of_one_invocation_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("state.db");
        let installation_id = Uuid::now_v7();
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE raw_events (
                    id TEXT PRIMARY KEY, agent_kind TEXT, session_id TEXT, source TEXT,
                    source_event TEXT, occurred_at_ms INTEGER, received_at_ms INTEGER,
                    sequence_no INTEGER, dedupe_key TEXT UNIQUE, payload_version INTEGER,
                    payload_json TEXT, processed_at_ms INTEGER, process_error TEXT
                );
                CREATE TABLE installation (
                    singleton INTEGER PRIMARY KEY,
                    installation_id TEXT NOT NULL
                )",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO installation(singleton, installation_id) VALUES (1, ?1)",
                [installation_id.to_string()],
            )
            .unwrap();
        drop(connection);
        let id = Uuid::now_v7();
        let event = normalize_hook(
            &serde_json::json!({"session_id":"s","hook_event_name":"Stop"}),
            id,
            1,
        )
        .unwrap();
        insert_once(&database, installation_id, &event).unwrap();
        insert_once(&database, installation_id, &event).unwrap();
        let connection = Connection::open(database).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn mismatched_installation_never_inserts_an_event() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("state.db");
        let owned = Uuid::now_v7();
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE raw_events (
                    id TEXT PRIMARY KEY, agent_kind TEXT, session_id TEXT, source TEXT,
                    source_event TEXT, occurred_at_ms INTEGER, received_at_ms INTEGER,
                    sequence_no INTEGER, dedupe_key TEXT UNIQUE, payload_version INTEGER,
                    payload_json TEXT, processed_at_ms INTEGER, process_error TEXT
                );
                CREATE TABLE installation (
                    singleton INTEGER PRIMARY KEY,
                    installation_id TEXT NOT NULL
                )",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO installation(singleton, installation_id) VALUES (1, ?1)",
                [owned.to_string()],
            )
            .unwrap();
        drop(connection);
        let event = normalize_hook(
            &serde_json::json!({"session_id":"s","hook_event_name":"Stop"}),
            Uuid::now_v7(),
            1,
        )
        .unwrap();

        assert!(matches!(
            insert_once(&database, Uuid::now_v7(), &event),
            Err(InsertError::Ownership)
        ));
        let connection = Connection::open(database).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn capability_revoked_while_writer_waits_never_inserts_an_event() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("state.db");
        let installation_id = Uuid::now_v7();
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE raw_events (
                    id TEXT PRIMARY KEY, agent_kind TEXT, session_id TEXT, source TEXT,
                    source_event TEXT, occurred_at_ms INTEGER, received_at_ms INTEGER,
                    sequence_no INTEGER, dedupe_key TEXT UNIQUE, payload_version INTEGER,
                    payload_json TEXT, processed_at_ms INTEGER, process_error TEXT
                 );
                 CREATE TABLE installation (
                    singleton INTEGER PRIMARY KEY,
                    installation_id TEXT NOT NULL
                 );",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO installation(singleton, installation_id) VALUES (1, ?1)",
                [installation_id.to_string()],
            )
            .unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        connection
            .execute("DELETE FROM installation WHERE singleton = 1", [])
            .unwrap();

        let database_for_writer = database.clone();
        let writer = thread::spawn(move || {
            let event = normalize_hook(
                &serde_json::json!({"session_id":"s","hook_event_name":"Stop"}),
                Uuid::now_v7(),
                1,
            )
            .unwrap();
            insert_with_retry(&database_for_writer, installation_id, &event)
        });
        thread::sleep(Duration::from_millis(30));
        connection.execute_batch("COMMIT").unwrap();

        assert!(matches!(
            writer.join().unwrap(),
            Err(InsertError::Ownership)
        ));
        let connection = Connection::open(database).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
