use crate::pricing::{extract_usage, PriceCatalog, TokenUsage};
use chrono::{DateTime, Local, Utc};
use monitor_domain::{AgentEvent, AgentKind, EventId, EventSource, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

pub const MAX_TRANSCRIPT_LINE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_BATCH_RECORDS: usize = 256;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TranscriptCursor {
    pub file_identity: Option<String>,
    /// Start of the next unprocessed line. Incomplete lines are reread.
    pub byte_offset: u64,
    pub file_size: u64,
    pub modified_at_ms: Option<i64>,
    pub content_anchor: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredTranscript {
    path: PathBuf,
    canonical_root: PathBuf,
    family_root: PathBuf,
}

impl DiscoveredTranscript {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn session_id(&self) -> String {
        if self
            .path
            .parent()
            .and_then(Path::file_name)
            .and_then(|v| v.to_str())
            == Some("subagents")
        {
            self.path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .and_then(|v| v.to_str())
                .unwrap_or_default()
                .to_owned()
        } else {
            self.path
                .file_stem()
                .and_then(|v| v.to_str())
                .unwrap_or_default()
                .to_owned()
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageRecord {
    pub session_id: String,
    pub transcript_path: String,
    pub source_location: String,
    pub request_id: Option<String>,
    pub message_id: Option<String>,
    pub model_id: String,
    pub local_day: String,
    pub usage: TokenUsage,
    pub cost_pico_usd: i64,
    pub cost_known: bool,
    pub observed_at_ms: i64,
    pub is_sidechain: bool,
    pub final_message: bool,
}

impl UsageRecord {
    pub fn dedupe_key(&self) -> String {
        let kind = match (&self.message_id, &self.request_id) {
            (Some(_), Some(_)) => "message_request",
            (Some(_), None) => "message",
            (None, Some(_)) => "request",
            (None, None) => "anonymous",
        };
        stable_key(&[
            "claude",
            &self.session_id,
            kind,
            self.message_id.as_deref().unwrap_or(""),
            self.request_id.as_deref().unwrap_or(""),
        ])
    }
}

#[derive(Clone, Debug)]
pub struct TranscriptScan {
    pub cursor: TranscriptCursor,
    pub parsed_records: usize,
    pub malformed_records: usize,
    pub oversized_records: usize,
    pub reset: bool,
}

#[derive(Clone, Debug)]
pub enum TranscriptStreamItem {
    Begin {
        descriptor: DiscoveredTranscript,
        reset: bool,
        start_cursor: TranscriptCursor,
        historical_replay: bool,
        notifications_allowed: bool,
    },
    Chunk {
        events: Vec<AgentEvent>,
        usage: Vec<UsageRecord>,
        cursor_after: TranscriptCursor,
    },
    Commit {
        final_cursor: TranscriptCursor,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum TranscriptError {
    #[error("transcript I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("transcript escaped its discovery root")]
    EscapedRoot,
    #[error("symbolic-link transcripts are not allowed")]
    Symlink,
    #[error("transcript sink failed: {0}")]
    Sink(String),
}

pub fn discover(projects_dir: &Path) -> Result<Vec<DiscoveredTranscript>, TranscriptError> {
    let canonical_root = projects_dir.canonicalize()?;
    if !canonical_root.is_dir() {
        return Err(TranscriptError::EscapedRoot);
    }
    let mut paths = Vec::new();
    for project in fs::read_dir(&canonical_root)? {
        let project = project?;
        if project.file_type()?.is_symlink()
            || !project.file_type()?.is_dir()
            || project.file_name().to_string_lossy().contains("--")
        {
            continue;
        }
        let project_path = project.path();
        ensure_contained(&project_path, &canonical_root)?;
        for entry in fs::read_dir(&project_path)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_symlink() {
                continue;
            }
            if entry.file_type()?.is_file()
                && path.extension().and_then(|v| v.to_str()) == Some("jsonl")
            {
                let canonical = ensure_contained(&path, &canonical_root)?;
                paths.push(DiscoveredTranscript {
                    path: canonical,
                    canonical_root: canonical_root.clone(),
                    family_root: project_path.canonicalize()?,
                });
                let subagents = path.with_extension("").join("subagents");
                if let Ok(entries) = fs::read_dir(subagents) {
                    for subagent in entries {
                        let subagent = subagent?;
                        if subagent.file_type()?.is_symlink() || !subagent.file_type()?.is_file() {
                            continue;
                        }
                        let subpath = subagent.path();
                        if subpath.extension().and_then(|v| v.to_str()) == Some("jsonl") {
                            let canonical = ensure_contained(&subpath, &canonical_root)?;
                            paths.push(DiscoveredTranscript {
                                path: canonical,
                                canonical_root: canonical_root.clone(),
                                family_root: project_path.canonicalize()?,
                            });
                        }
                    }
                }
            }
        }
    }
    paths.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(paths)
}

/// Streams bounded chunks synchronously. Backpressure is the sink call itself.
/// A chunk cursor may be persisted only in the same successful transaction as
/// that chunk; a sink error returns immediately and never exposes a later
/// cursor as committed.
pub fn ingest_streaming(
    descriptor: &DiscoveredTranscript,
    prior: Option<&TranscriptCursor>,
    catalog: &PriceCatalog,
    observed_at_ms: i64,
    historical_replay: bool,
    sink: &mut (impl FnMut(TranscriptStreamItem) -> Result<(), String> + Send),
) -> Result<TranscriptScan, TranscriptError> {
    let path = ensure_contained(&descriptor.path, &descriptor.canonical_root)?;
    if fs::symlink_metadata(&descriptor.path)?
        .file_type()
        .is_symlink()
    {
        return Err(TranscriptError::Symlink);
    }
    let metadata = fs::metadata(&path)?;
    let identity = file_identity(&metadata);
    let modified_at_ms = metadata
        .modified()
        .ok()
        .and_then(|v| v.duration_since(UNIX_EPOCH).ok())
        .map(|v| i64::try_from(v.as_millis()).unwrap_or(i64::MAX));
    let anchor_matches = prior.is_none_or(|cursor| {
        cursor.content_anchor.as_deref() == hash_prefix(&path, cursor.byte_offset).ok().as_deref()
    });
    let reset = prior.is_some_and(|cursor| {
        cursor.file_identity.as_deref() != Some(identity.as_str())
            || metadata.len() < cursor.byte_offset
            || !anchor_matches
    });
    let start = if reset {
        0
    } else {
        prior.map_or(0, |cursor| cursor.byte_offset)
    };
    let start_cursor = TranscriptCursor {
        file_identity: Some(identity.clone()),
        byte_offset: start,
        file_size: metadata.len(),
        modified_at_ms,
        content_anchor: Some(hash_prefix(&path, start)?),
    };
    sink(TranscriptStreamItem::Begin {
        descriptor: descriptor.clone(),
        reset,
        start_cursor,
        historical_replay,
        notifications_allowed: !historical_replay,
    })
    .map_err(TranscriptError::Sink)?;
    let file = fs::File::open(&path)?;
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(start))?;
    let mut committed = start;
    let mut line_start = start;
    let mut line = Vec::with_capacity(8192);
    let mut oversized = false;
    let mut events = Vec::with_capacity(MAX_BATCH_RECORDS);
    let mut usage = Vec::with_capacity(MAX_BATCH_RECORDS);
    let mut parsed_records = 0;
    let mut malformed_records = 0;
    let mut oversized_records = 0;
    let relative_identity = family_relative_identity(&path, &descriptor.family_root, &identity);

    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(buffer.len(), |index| index + 1);
        let content = &buffer[..newline.unwrap_or(buffer.len())];
        if !oversized {
            if line.len().saturating_add(content.len()) <= MAX_TRANSCRIPT_LINE_BYTES {
                line.extend_from_slice(content);
            } else {
                line.clear();
                oversized = true;
            }
        }
        reader.consume(consumed);
        if newline.is_none() {
            continue;
        }
        if oversized {
            oversized_records += 1;
        } else if !line.iter().all(u8::is_ascii_whitespace) {
            match serde_json::from_slice::<Value>(&line) {
                Ok(object) => {
                    if events.len().saturating_add(usage.len()).saturating_add(2)
                        > MAX_BATCH_RECORDS
                    {
                        deliver_chunk(
                            descriptor,
                            &path,
                            &identity,
                            metadata.len(),
                            modified_at_ms,
                            committed,
                            historical_replay,
                            &mut events,
                            &mut usage,
                            sink,
                        )?;
                    }
                    parsed_records += 1;
                    let timestamp = timestamp_ms(&object).unwrap_or(observed_at_ms);
                    let source_location = format!("{relative_identity}:{line_start}");
                    if let Some(event) = normalize_evidence(
                        &object,
                        EvidenceContext {
                            session_id: &descriptor.session_id(),
                            identity: &identity,
                            offset: line_start,
                            line: &line,
                            occurred_at_ms: timestamp,
                            received_at_ms: observed_at_ms,
                            modified_at_ms: modified_at_ms.unwrap_or(observed_at_ms),
                        },
                    ) {
                        events.push(event);
                    }
                    if let Some(record) = usage_record(
                        &object,
                        &path,
                        &descriptor.session_id(),
                        source_location,
                        catalog,
                        timestamp,
                    ) {
                        usage.push(record);
                    }
                }
                Err(_) => malformed_records += 1,
            }
        }
        line.clear();
        oversized = false;
        line_start = reader.stream_position()?;
        committed = line_start;
        if events.len().saturating_add(usage.len()) >= MAX_BATCH_RECORDS {
            deliver_chunk(
                descriptor,
                &path,
                &identity,
                metadata.len(),
                modified_at_ms,
                committed,
                historical_replay,
                &mut events,
                &mut usage,
                sink,
            )?;
        }
    }
    // No newline means the line remains unprocessed and will be reread.
    let cursor = TranscriptCursor {
        file_identity: Some(identity),
        byte_offset: committed,
        file_size: metadata.len(),
        modified_at_ms,
        content_anchor: Some(hash_prefix(&path, committed)?),
    };
    if !events.is_empty() || !usage.is_empty() {
        deliver_chunk(
            descriptor,
            &path,
            cursor.file_identity.as_deref().unwrap_or_default(),
            metadata.len(),
            modified_at_ms,
            committed,
            historical_replay,
            &mut events,
            &mut usage,
            sink,
        )?;
    }
    sink(TranscriptStreamItem::Commit {
        final_cursor: cursor.clone(),
    })
    .map_err(TranscriptError::Sink)?;
    Ok(TranscriptScan {
        cursor,
        parsed_records,
        malformed_records,
        oversized_records,
        reset,
    })
}

#[allow(clippy::too_many_arguments)]
fn deliver_chunk(
    descriptor: &DiscoveredTranscript,
    path: &Path,
    identity: &str,
    file_size: u64,
    modified_at_ms: Option<i64>,
    committed: u64,
    historical_replay: bool,
    events: &mut Vec<AgentEvent>,
    usage: &mut Vec<UsageRecord>,
    sink: &mut (impl FnMut(TranscriptStreamItem) -> Result<(), String> + Send),
) -> Result<(), TranscriptError> {
    let cursor = TranscriptCursor {
        file_identity: Some(identity.to_owned()),
        byte_offset: committed,
        file_size,
        modified_at_ms,
        content_anchor: Some(hash_prefix(path, committed)?),
    };
    let _ = descriptor;
    let _ = historical_replay;
    sink(TranscriptStreamItem::Chunk {
        events: std::mem::take(events),
        usage: std::mem::take(usage),
        cursor_after: cursor,
    })
    .map_err(TranscriptError::Sink)
}

pub fn dedupe_usage(records: impl IntoIterator<Item = UsageRecord>) -> Vec<UsageRecord> {
    let mut selected: HashMap<String, UsageRecord> = HashMap::new();
    for record in records {
        let key = record.dedupe_key();
        let chosen = match selected.remove(&key) {
            None => record,
            Some(previous) if record.message_id.is_none() && record.request_id.is_none() => {
                if (record.observed_at_ms, &record.source_location)
                    > (previous.observed_at_ms, &previous.source_location)
                {
                    record
                } else {
                    previous
                }
            }
            Some(previous) => prefer(previous, record),
        };
        selected.insert(key, chosen);
    }
    let mut values: Vec<_> = selected.into_values().collect();
    values.sort_by(|a, b| {
        (&a.session_id, &a.dedupe_key(), &a.source_location).cmp(&(
            &b.session_id,
            &b.dedupe_key(),
            &b.source_location,
        ))
    });
    values
}

fn prefer(a: UsageRecord, b: UsageRecord) -> UsageRecord {
    if a.is_sidechain != b.is_sidechain {
        return if a.is_sidechain { b } else { a };
    }
    if a.usage.total() != b.usage.total() {
        return if a.usage.total() > b.usage.total() {
            a
        } else {
            b
        };
    }
    if a.final_message != b.final_message {
        return if a.final_message { a } else { b };
    }
    if (a.observed_at_ms, &a.source_location) > (b.observed_at_ms, &b.source_location) {
        a
    } else {
        b
    }
}

fn usage_record(
    object: &Value,
    path: &Path,
    session_id: &str,
    source_location: String,
    catalog: &PriceCatalog,
    observed_at_ms: i64,
) -> Option<UsageRecord> {
    if object.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let message = object.get("message")?;
    let usage_value = message.get("usage")?;
    let model = message.get("model").and_then(Value::as_str).unwrap_or("");
    if matches!(model, "<synthetic>" | "synthetic") {
        return None;
    }
    let usage = extract_usage(usage_value);
    let cost = catalog.cost(usage, model);
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let request_id = text(
        object
            .get("requestId")
            .or_else(|| object.get("request_id"))
            .or_else(|| message.get("requestId"))
            .or_else(|| message.get("request_id")),
    );
    let message_id = text(message.get("id"));
    let is_sidechain = object
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let in_subagent_file = path
        .components()
        .any(|part| part.as_os_str() == "subagents");
    if is_sidechain && !in_subagent_file {
        return None;
    }
    let day = DateTime::<Utc>::from_timestamp_millis(observed_at_ms)
        .map(|v| v.with_timezone(&Local).format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());
    Some(UsageRecord {
        session_id: session_id.to_owned(),
        transcript_path: path.to_string_lossy().into_owned(),
        source_location,
        request_id,
        message_id,
        model_id: model.to_owned(),
        local_day: day,
        usage,
        cost_pico_usd: cost.pico_usd,
        cost_known: cost.known,
        observed_at_ms,
        is_sidechain,
        final_message: message.get("stop_reason").is_some_and(|v| !v.is_null()),
    })
}

struct EvidenceContext<'a> {
    session_id: &'a str,
    identity: &'a str,
    offset: u64,
    line: &'a [u8],
    occurred_at_ms: i64,
    received_at_ms: i64,
    modified_at_ms: i64,
}

fn normalize_evidence(object: &Value, context: EvidenceContext<'_>) -> Option<AgentEvent> {
    let role = object
        .get("role")
        .or_else(|| object.get("type"))
        .and_then(Value::as_str);
    let message = object.get("message").unwrap_or(object);
    let content = message.get("content");
    let last_kind = match content {
        Some(Value::Array(parts)) => parts
            .last()
            .and_then(|part| part.get("type"))
            .and_then(Value::as_str),
        Some(Value::String(_)) => Some("text"),
        _ => None,
    };
    let event = match (role, last_kind) {
        (Some("assistant"), Some("tool_use")) => "TranscriptAssistantToolUse",
        (Some("assistant"), Some("thinking")) => "TranscriptAssistantThinking",
        (_, Some("tool_result")) => "TranscriptToolResult",
        (Some("assistant"), Some("text") | None) => "TranscriptAssistantText",
        _ => return None,
    };
    let fingerprint = hex_digest(context.line);
    let key = stable_key(&[context.identity, &context.offset.to_string(), &fingerprint]);
    Some(AgentEvent {
        id: EventId(format!("transcript:{key}")),
        agent_kind: AgentKind::claude(),
        session_id: SessionId(context.session_id.to_owned()),
        source: EventSource::Transcript,
        source_event: event.to_owned(),
        occurred_at_ms: context.occurred_at_ms,
        received_at_ms: context.received_at_ms,
        sequence_no: i64::try_from(context.offset).ok(),
        dedupe_key: key,
        payload_version: 1,
        payload: if event == "TranscriptAssistantText" {
            json!({"idle_ms": context.received_at_ms.saturating_sub(context.modified_at_ms).max(0)})
        } else {
            json!({})
        },
    })
}

fn timestamp_ms(object: &Value) -> Option<i64> {
    let value = object.get("timestamp")?;
    if let Some(number) = value.as_i64() {
        return if number.unsigned_abs() < 10_000_000_000 {
            number.checked_mul(1_000)
        } else {
            Some(number)
        };
    }
    DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|value| value.timestamp_millis())
}

fn stable_key(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hash_prefix(path: &Path, offset: u64) -> Result<String, std::io::Error> {
    let mut file = fs::File::open(path)?;
    let mut remaining = offset;
    let mut buffer = [0_u8; 64 * 1024];
    let mut hasher = Sha256::new();
    while remaining > 0 {
        let wanted = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let count = file.read(&mut buffer[..wanted])?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn ensure_contained(path: &Path, root: &Path) -> Result<PathBuf, TranscriptError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(TranscriptError::Symlink);
    }
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root) {
        return Err(TranscriptError::EscapedRoot);
    }
    Ok(canonical)
}

fn family_relative_identity(path: &Path, family_root: &Path, identity: &str) -> String {
    let relative = path.strip_prefix(family_root).unwrap_or(path);
    format!("{}@{identity}", relative.to_string_lossy())
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("{}:{}", metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn file_identity(metadata: &fs::Metadata) -> String {
    format!("{}", metadata.len())
}

pub fn aggregate_by_day_model(
    records: &[UsageRecord],
) -> BTreeMap<(String, String, String), (TokenUsage, i64, bool)> {
    let mut result = BTreeMap::new();
    for record in records {
        let value = result
            .entry((
                record.local_day.clone(),
                record.session_id.clone(),
                record.model_id.clone(),
            ))
            .or_insert((
                TokenUsage {
                    input: 0,
                    output: 0,
                    cache_write: 0,
                    cache_read: 0,
                },
                0_i64,
                true,
            ));
        value.0.input = value.0.input.saturating_add(record.usage.input);
        value.0.output = value.0.output.saturating_add(record.usage.output);
        value.0.cache_write = value.0.cache_write.saturating_add(record.usage.cache_write);
        value.0.cache_read = value.0.cache_read.saturating_add(record.usage.cache_read);
        value.1 = value.1.saturating_add(record.cost_pico_usd);
        value.2 &= record.cost_known;
    }
    result
}
