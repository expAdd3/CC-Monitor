use crate::pricing::{extract_usage, PriceCatalog, TokenUsage};
use chrono::{DateTime, Local, Utc};
use monitor_domain::{AgentEvent, AgentKind, EventId, EventSource, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, BufRead, BufReader, Seek, SeekFrom},
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
    pub modified_at_ns: Option<i64>,
    pub content_anchor: Option<String>,
    pub hash_checkpoint: Option<Vec<u8>>,
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
    /// Bytes read from the transcript during this scan, including prefix
    /// verification. A trusted unchanged fast path reports zero.
    pub bytes_read: u64,
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
    #[error("transcript is not a regular file")]
    NonRegular,
    #[error("transcript sink failed: {0}")]
    Sink(String),
}

pub fn discover(projects_dir: &Path) -> Result<Vec<DiscoveredTranscript>, TranscriptError> {
    let mut paths = Vec::new();
    visit_discovered(projects_dir, |descriptor| {
        paths.push(descriptor);
        Ok(())
    })?;
    paths.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(paths)
}

/// Visits discovered transcripts without retaining the complete discovery
/// result. Callers that need a durable snapshot can persist fixed-size pages
/// from this callback.
pub fn visit_discovered(
    projects_dir: &Path,
    mut visitor: impl FnMut(DiscoveredTranscript) -> Result<(), TranscriptError>,
) -> Result<(), TranscriptError> {
    let canonical_root = projects_dir.canonicalize()?;
    if !canonical_root.is_dir() {
        return Err(TranscriptError::EscapedRoot);
    }
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
                visitor(DiscoveredTranscript {
                    path: canonical,
                    canonical_root: canonical_root.clone(),
                    family_root: project_path.canonicalize()?,
                })?;
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
                            visitor(DiscoveredTranscript {
                                path: canonical,
                                canonical_root: canonical_root.clone(),
                                family_root: project_path.canonicalize()?,
                            })?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
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
    ingest_streaming_with_freshness(
        descriptor,
        prior,
        catalog,
        observed_at_ms,
        historical_replay,
        true,
        sink,
    )
}

/// Incremental scanner variant that may trust an exact opened-file freshness
/// token between periodic full prefix-anchor audits. The fast path emits no
/// stream items and performs no transcript reads.
pub fn ingest_streaming_with_freshness(
    descriptor: &DiscoveredTranscript,
    prior: Option<&TranscriptCursor>,
    catalog: &PriceCatalog,
    observed_at_ms: i64,
    historical_replay: bool,
    full_anchor_audit: bool,
    sink: &mut (impl FnMut(TranscriptStreamItem) -> Result<(), String> + Send),
) -> Result<TranscriptScan, TranscriptError> {
    let (path, file, metadata) = open_verified_transcript(descriptor)?;
    let identity = file_identity(&metadata);
    let modified_at_ms = metadata
        .modified()
        .ok()
        .and_then(|v| v.duration_since(UNIX_EPOCH).ok())
        .map(|v| i64::try_from(v.as_millis()).unwrap_or(i64::MAX));
    let modified_at_ns = metadata_modified_at_ns(&metadata);
    if !full_anchor_audit {
        if let Some(cursor) = prior.filter(|cursor| {
            cursor.file_identity.as_deref() == Some(identity.as_str())
                && cursor.file_size == metadata.len()
                && cursor.byte_offset <= cursor.file_size
                && cursor.modified_at_ns.is_some()
                && cursor.modified_at_ns == modified_at_ns
        }) {
            return Ok(TranscriptScan {
                cursor: cursor.clone(),
                parsed_records: 0,
                malformed_records: 0,
                oversized_records: 0,
                reset: false,
                bytes_read: 0,
            });
        }
    }
    let cursor_source = CursorSource {
        identity: &identity,
        file_size: metadata.len(),
        modified_at_ms,
        modified_at_ns,
    };
    let checkpoint_append = !full_anchor_audit
        && prior.is_some_and(|cursor| {
            cursor.file_identity.as_deref() == Some(identity.as_str())
                && metadata.len() > cursor.file_size
                && cursor.file_size >= cursor.byte_offset
        });
    let mut bytes_read = 0_u64;
    let prior_anchor = prior.and_then(|cursor| {
        if cursor.file_identity.as_deref() != Some(identity.as_str())
            || metadata.len() < cursor.byte_offset
        {
            return None;
        }
        if checkpoint_append {
            if let Some((anchor, checkpoint_bytes_read)) = PrefixAnchor::from_checkpoint(
                &file,
                cursor.hash_checkpoint.as_deref()?,
                cursor.byte_offset,
                cursor.content_anchor.as_deref()?,
            ) {
                bytes_read = bytes_read.saturating_add(checkpoint_bytes_read);
                return Some(anchor);
            }
        }
        let anchor = PrefixAnchor::from_file(&file, cursor.byte_offset).ok()?;
        bytes_read = bytes_read.saturating_add(anchor.bytes_hashed);
        Some(anchor)
    });
    let anchor_matches = prior.is_none_or(|cursor| {
        prior_anchor.as_ref().map(PrefixAnchor::digest).as_deref()
            == cursor.content_anchor.as_deref()
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
    let mut scan_anchor = if reset {
        PrefixAnchor::default()
    } else {
        prior_anchor.unwrap_or_default()
    };
    let mut committed_anchor = scan_anchor.clone();
    debug_assert_eq!(committed_anchor.bytes_hashed, start);
    let start_cursor = TranscriptCursor {
        file_identity: Some(identity.clone()),
        byte_offset: start,
        file_size: metadata.len(),
        modified_at_ms,
        modified_at_ns,
        content_anchor: Some(committed_anchor.digest()),
        hash_checkpoint: Some(committed_anchor.checkpoint()),
    };
    sink(TranscriptStreamItem::Begin {
        descriptor: descriptor.clone(),
        reset,
        start_cursor,
        historical_replay,
        notifications_allowed: !historical_replay && !reset,
    })
    .map_err(TranscriptError::Sink)?;
    let mut reader = BufReader::new(&file);
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
        scan_anchor.update(&buffer[..consumed]);
        bytes_read = bytes_read.saturating_add(consumed as u64);
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
                            &cursor_source,
                            committed,
                            &committed_anchor,
                            &mut events,
                            &mut usage,
                            sink,
                        )?;
                    }
                    parsed_records += 1;
                    let timestamp = timestamp_ms(&object).unwrap_or(observed_at_ms);
                    let (event_timestamp, received_at_ms) = if historical_replay {
                        let historical =
                            trusted_historical_event_time_ms(timestamp, observed_at_ms);
                        // The reducer applies the ordinary receipt-time trust
                        // window again. Historical replay therefore uses the
                        // validated source time as its logical receipt time;
                        // the scan time remains cursor/staging metadata.
                        (historical, historical)
                    } else {
                        (
                            monitor_domain::trusted_event_time_ms(timestamp, observed_at_ms),
                            observed_at_ms,
                        )
                    };
                    let source_location = format!("{relative_identity}:{line_start}");
                    if let Some(event) = normalize_evidence(
                        &object,
                        EvidenceContext {
                            session_id: &descriptor.session_id(),
                            identity: &identity,
                            offset: line_start,
                            line: &line,
                            occurred_at_ms: event_timestamp,
                            received_at_ms,
                            modified_at_ms: modified_at_ms.unwrap_or(observed_at_ms),
                        },
                    ) {
                        events.push(event);
                    }
                    // Usage day buckets must never be placed in the future.
                    // Transcript evidence retains its separately governed event
                    // timestamp, while the derived usage projection clamps an
                    // implausible future source time to this scan's observation.
                    let usage_timestamp = if historical_replay {
                        event_timestamp
                    } else {
                        timestamp.min(observed_at_ms)
                    };
                    if let Some(record) = usage_record(
                        &object,
                        &path,
                        &descriptor.session_id(),
                        source_location,
                        catalog,
                        usage_timestamp,
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
        committed_anchor = scan_anchor.clone();
        debug_assert_eq!(committed_anchor.bytes_hashed, committed);
        if events.len().saturating_add(usage.len()) >= MAX_BATCH_RECORDS {
            deliver_chunk(
                &cursor_source,
                committed,
                &committed_anchor,
                &mut events,
                &mut usage,
                sink,
            )?;
        }
    }
    // No newline means the line remains unprocessed and will be reread.
    let cursor = TranscriptCursor {
        file_identity: Some(identity.clone()),
        byte_offset: committed,
        file_size: metadata.len(),
        modified_at_ms,
        modified_at_ns,
        content_anchor: Some(committed_anchor.digest()),
        hash_checkpoint: Some(committed_anchor.checkpoint()),
    };
    if !events.is_empty() || !usage.is_empty() {
        deliver_chunk(
            &cursor_source,
            committed,
            &committed_anchor,
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
        bytes_read,
    })
}

struct CursorSource<'a> {
    identity: &'a str,
    file_size: u64,
    modified_at_ms: Option<i64>,
    modified_at_ns: Option<i64>,
}

fn deliver_chunk(
    source: &CursorSource<'_>,
    committed: u64,
    anchor: &PrefixAnchor,
    events: &mut Vec<AgentEvent>,
    usage: &mut Vec<UsageRecord>,
    sink: &mut (impl FnMut(TranscriptStreamItem) -> Result<(), String> + Send),
) -> Result<(), TranscriptError> {
    debug_assert_eq!(anchor.bytes_hashed, committed);
    let cursor = TranscriptCursor {
        file_identity: Some(source.identity.to_owned()),
        byte_offset: committed,
        file_size: source.file_size,
        modified_at_ms: source.modified_at_ms,
        modified_at_ns: source.modified_at_ns,
        content_anchor: Some(anchor.digest()),
        hash_checkpoint: Some(anchor.checkpoint()),
    };
    sink(TranscriptStreamItem::Chunk {
        events: std::mem::take(events),
        usage: std::mem::take(usage),
        cursor_after: cursor,
    })
    .map_err(TranscriptError::Sink)
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

fn trusted_historical_event_time_ms(source_at_ms: i64, scanned_at_ms: i64) -> i64 {
    const EARLIEST_PLAUSIBLE_TRANSCRIPT_MS: i64 = 946_684_800_000; // 2000-01-01
    if source_at_ms >= EARLIEST_PLAUSIBLE_TRANSCRIPT_MS
        && source_at_ms <= scanned_at_ms.saturating_add(monitor_domain::EVENT_TIME_TRUST_WINDOW_MS)
    {
        source_at_ms
    } else {
        scanned_at_ms
    }
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

const SHA256_CHECKPOINT_MAGIC: &[u8; 4] = b"CCS2";
const SHA256_CHECKPOINT_VERSION: u8 = 1;
const SHA256_CHECKPOINT_LEN: usize = 4 + 1 + 8 + 32;
const SHA256_INITIAL_STATE: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

#[derive(Clone)]
struct PrefixAnchor {
    state: [u32; 8],
    tail: [u8; 64],
    tail_len: usize,
    bytes_hashed: u64,
}

impl Default for PrefixAnchor {
    fn default() -> Self {
        Self {
            state: SHA256_INITIAL_STATE,
            tail: [0; 64],
            tail_len: 0,
            bytes_hashed: 0,
        }
    }
}

impl PrefixAnchor {
    fn update(&mut self, bytes: &[u8]) {
        self.bytes_hashed = self
            .bytes_hashed
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        let mut remaining = bytes;
        if self.tail_len > 0 {
            let copy = remaining.len().min(64 - self.tail_len);
            self.tail[self.tail_len..self.tail_len + copy].copy_from_slice(&remaining[..copy]);
            self.tail_len += copy;
            remaining = &remaining[copy..];
            if self.tail_len == 64 {
                self.compress_block(self.tail);
                self.tail = [0; 64];
                self.tail_len = 0;
            }
            if remaining.is_empty() {
                return;
            }
        }
        let mut chunks = remaining.chunks_exact(64);
        for chunk in &mut chunks {
            let mut block = [0_u8; 64];
            block.copy_from_slice(chunk);
            self.compress_block(block);
        }
        let remainder = chunks.remainder();
        self.tail[..remainder.len()].copy_from_slice(remainder);
        self.tail_len = remainder.len();
    }

    fn digest(&self) -> String {
        let mut value = self.clone();
        let bit_len = value.bytes_hashed.wrapping_mul(8);
        let mut padding = [0_u8; 128];
        padding[0] = 0x80;
        let padding_len = if value.tail_len < 56 {
            56 - value.tail_len
        } else {
            120 - value.tail_len
        };
        value.update(&padding[..padding_len]);
        value.update(&bit_len.to_be_bytes());
        debug_assert_eq!(value.tail_len, 0);
        value
            .state
            .iter()
            .map(|part| format!("{part:08x}"))
            .collect::<Vec<_>>()
            .concat()
    }

    fn checkpoint(&self) -> Vec<u8> {
        let mut value = Vec::with_capacity(SHA256_CHECKPOINT_LEN);
        value.extend_from_slice(SHA256_CHECKPOINT_MAGIC);
        value.push(SHA256_CHECKPOINT_VERSION);
        let checkpoint_offset = self.bytes_hashed.saturating_sub(self.tail_len as u64);
        value.extend_from_slice(&checkpoint_offset.to_be_bytes());
        for part in self.state {
            value.extend_from_slice(&part.to_be_bytes());
        }
        value
    }

    fn from_checkpoint(
        file: &fs::File,
        value: &[u8],
        expected_len: u64,
        expected_digest: &str,
    ) -> Option<(Self, u64)> {
        if value.len() != SHA256_CHECKPOINT_LEN
            || &value[..4] != SHA256_CHECKPOINT_MAGIC
            || value[4] != SHA256_CHECKPOINT_VERSION
        {
            return None;
        }
        let checkpoint_offset = u64::from_be_bytes(value[5..13].try_into().ok()?);
        let mut state = [0_u32; 8];
        for (index, part) in state.iter_mut().enumerate() {
            let start = 13 + index * 4;
            *part = u32::from_be_bytes(value[start..start + 4].try_into().ok()?);
        }
        if checkpoint_offset > expected_len
            || checkpoint_offset % 64 != 0
            || expected_len.saturating_sub(checkpoint_offset) >= 64
        {
            return None;
        }
        let mut checkpoint = Self {
            state,
            tail: [0; 64],
            tail_len: 0,
            bytes_hashed: checkpoint_offset,
        };
        let tail_len = usize::try_from(expected_len - checkpoint_offset).ok()?;
        let mut tail = [0_u8; 63];
        read_exact_at(file, &mut tail[..tail_len], checkpoint_offset).ok()?;
        checkpoint.update(&tail[..tail_len]);
        (checkpoint.digest() == expected_digest).then_some((checkpoint, tail_len as u64))
    }

    fn compress_block(&mut self, block: [u8; 64]) {
        let block = sha2::digest::generic_array::GenericArray::clone_from_slice(&block);
        sha2::compress256(&mut self.state, &[block]);
    }

    #[cfg(unix)]
    fn from_file(file: &fs::File, offset: u64) -> Result<Self, std::io::Error> {
        use std::os::unix::fs::FileExt;

        let mut anchor = Self::default();
        let mut remaining = offset;
        let mut position = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        while remaining > 0 {
            let wanted =
                usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
            let count = file.read_at(&mut buffer[..wanted], position)?;
            if count == 0 {
                break;
            }
            anchor.update(&buffer[..count]);
            remaining -= count as u64;
            position = position.saturating_add(count as u64);
        }
        Ok(anchor)
    }

    #[cfg(not(unix))]
    fn from_file(file: &fs::File, offset: u64) -> Result<Self, std::io::Error> {
        let mut clone = file.try_clone()?;
        let original_position = clone.stream_position()?;
        clone.seek(SeekFrom::Start(0))?;
        let mut anchor = Self::default();
        let mut remaining = offset;
        let mut buffer = [0_u8; 64 * 1024];
        while remaining > 0 {
            let wanted =
                usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
            let count = std::io::Read::read(&mut clone, &mut buffer[..wanted])?;
            if count == 0 {
                break;
            }
            anchor.update(&buffer[..count]);
            remaining -= count as u64;
        }
        // Some platforms share a cursor between cloned file handles. Restore it
        // before returning so anchor verification cannot perturb stream parsing.
        clone.seek(SeekFrom::Start(original_position))?;
        Ok(anchor)
    }
}

#[cfg(unix)]
fn read_exact_at(file: &fs::File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;

    while !buffer.is_empty() {
        let count = file.read_at(buffer, offset)?;
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        offset = offset.saturating_add(count as u64);
        buffer = &mut buffer[count..];
    }
    Ok(())
}

#[cfg(not(unix))]
fn read_exact_at(file: &fs::File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    let mut clone = file.try_clone()?;
    let original_position = clone.stream_position()?;
    clone.seek(SeekFrom::Start(offset))?;
    std::io::Read::read_exact(&mut clone, buffer)?;
    clone.seek(SeekFrom::Start(original_position))?;
    Ok(())
}

fn open_verified_transcript(
    descriptor: &DiscoveredTranscript,
) -> Result<(PathBuf, fs::File, fs::Metadata), TranscriptError> {
    // Validate the namespace before opening, then open without following the
    // final component. A second namespace+identity check binds the validated
    // path to the already-open handle, so later replacements cannot redirect
    // this scan.
    ensure_contained(&descriptor.path, &descriptor.canonical_root)?;
    let file = open_read_nofollow(&descriptor.path).map_err(|error| {
        if fs::symlink_metadata(&descriptor.path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            TranscriptError::Symlink
        } else {
            TranscriptError::Io(error)
        }
    })?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(TranscriptError::NonRegular);
    }
    let path = ensure_contained(&descriptor.path, &descriptor.canonical_root)?;
    let path_metadata = fs::metadata(&path)?;
    if file_identity(&metadata) != file_identity(&path_metadata) {
        return Err(TranscriptError::EscapedRoot);
    }
    Ok((path, file, metadata))
}

#[cfg(unix)]
fn open_read_nofollow(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_read_nofollow(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().read(true).open(path)
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

#[cfg(unix)]
fn metadata_modified_at_ns(metadata: &fs::Metadata) -> Option<i64> {
    use std::os::unix::fs::MetadataExt;

    metadata
        .mtime()
        .checked_mul(1_000_000_000)
        .and_then(|seconds| seconds.checked_add(metadata.mtime_nsec()))
}

#[cfg(not(unix))]
fn metadata_modified_at_ns(metadata: &fs::Metadata) -> Option<i64> {
    let modified = metadata.modified().ok()?;
    match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_nanos()).ok(),
        Err(error) => i64::try_from(error.duration().as_nanos())
            .ok()
            .and_then(i64::checked_neg),
    }
}

#[cfg(not(unix))]
fn file_identity(metadata: &fs::Metadata) -> String {
    portable_file_identity(
        metadata.len(),
        metadata.modified().ok(),
        metadata.created().ok(),
        metadata.permissions().readonly(),
    )
}

#[cfg(any(not(unix), test))]
fn portable_file_identity(
    len: u64,
    modified: Option<std::time::SystemTime>,
    created: Option<std::time::SystemTime>,
    readonly: bool,
) -> String {
    fn time_key(value: Option<std::time::SystemTime>) -> String {
        value.map_or_else(
            || "unknown".to_owned(),
            |time| match time.duration_since(UNIX_EPOCH) {
                Ok(duration) => format!("+{}", duration.as_nanos()),
                Err(error) => format!("-{}", error.duration().as_nanos()),
            },
        )
    }

    format!(
        "len={len};modified={};created={};readonly={readonly}",
        time_key(modified),
        time_key(created)
    )
}

#[cfg(test)]
mod portable_identity_tests {
    use super::{portable_file_identity, PrefixAnchor};
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        time::{Duration, UNIX_EPOCH},
    };
    use tempfile::tempdir;

    #[test]
    fn portable_identity_distinguishes_same_length_replacements_when_metadata_changes() {
        let first = portable_file_identity(
            128,
            Some(UNIX_EPOCH + Duration::from_secs(10)),
            Some(UNIX_EPOCH + Duration::from_secs(5)),
            false,
        );
        let modified = portable_file_identity(
            128,
            Some(UNIX_EPOCH + Duration::from_secs(11)),
            Some(UNIX_EPOCH + Duration::from_secs(5)),
            false,
        );
        let recreated = portable_file_identity(
            128,
            Some(UNIX_EPOCH + Duration::from_secs(10)),
            Some(UNIX_EPOCH + Duration::from_secs(6)),
            false,
        );

        assert_ne!(first, modified);
        assert_ne!(first, recreated);
        assert!(first.contains("len=128"));
    }

    #[test]
    fn prefix_anchor_hashes_each_byte_once_as_chunks_advance() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("large.jsonl");
        let bytes: Vec<u8> = (0..1_048_576).map(|index| (index % 251) as u8).collect();
        fs::write(&path, &bytes).unwrap();
        let file = fs::File::open(path).unwrap();
        let resume_offset = 262_144_u64;
        let mut anchor = PrefixAnchor::from_file(&file, resume_offset).unwrap();

        for chunk in bytes[resume_offset as usize..].chunks(4_093) {
            anchor.update(chunk);
        }

        assert_eq!(anchor.bytes_hashed, bytes.len() as u64);
        assert_eq!(
            anchor.digest(),
            format!("{:x}", Sha256::digest(&bytes)),
            "the resume prefix and each newly parsed byte contribute exactly once"
        );
    }
}
