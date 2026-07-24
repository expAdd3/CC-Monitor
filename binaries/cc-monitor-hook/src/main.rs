use adapter_claude::hook::{normalize_hook, now_ms, MAX_HOOK_INPUT_BYTES};
use monitor_domain::{AgentEvent, EventSource};
use rusqlite::{Connection, OpenFlags};
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
    let payload: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| "invalid_json")?;
    let ingestion_id = Uuid::now_v7();
    let event = normalize_hook(&payload, ingestion_id, now_ms()).map_err(|_| "invalid_payload")?;
    insert_with_retry(&args.database, &event).map_err(|_| "storage_unavailable")
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
    #[allow(dead_code)]
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

fn insert_with_retry(path: &Path, event: &AgentEvent) -> rusqlite::Result<()> {
    let mut delays = RETRY_DELAYS.iter();
    loop {
        match insert_once(path, event) {
            Ok(()) => return Ok(()),
            Err(error) if is_busy(&error) => {
                let Some(delay) = delays.next() else {
                    return Err(error);
                };
                thread::sleep(*delay);
            }
            Err(error) => return Err(error),
        }
    }
}

fn insert_once(path: &Path, event: &AgentEvent) -> rusqlite::Result<()> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.execute(
        "INSERT INTO raw_events (
            id, agent_kind, session_id, source, source_event, occurred_at_ms,
            received_at_ms, sequence_no, dedupe_key, payload_version, payload_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
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
        ],
    )?;
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
    fn repeated_insert_of_one_invocation_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("state.db");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE raw_events (
                    id TEXT PRIMARY KEY, agent_kind TEXT, session_id TEXT, source TEXT,
                    source_event TEXT, occurred_at_ms INTEGER, received_at_ms INTEGER,
                    sequence_no INTEGER, dedupe_key TEXT UNIQUE, payload_version INTEGER,
                    payload_json TEXT, processed_at_ms INTEGER, process_error TEXT
                )",
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
        insert_once(&database, &event).unwrap();
        insert_once(&database, &event).unwrap();
        let connection = Connection::open(database).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM raw_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
