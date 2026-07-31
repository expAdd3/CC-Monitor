use monitor_domain::{AgentEvent, EventSource};
use sqlx::{Row, SqlitePool};
use std::collections::BTreeSet;

const CURSOR_METADATA_MAGIC: &[u8; 4] = b"CMC1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredUsage {
    pub id: String,
    pub session_id: String,
    pub transcript_path: String,
    pub source_location: String,
    pub request_id: Option<String>,
    pub message_id: Option<String>,
    pub model_id: String,
    pub local_day: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_read_tokens: i64,
    pub cost_pico_usd: i64,
    pub cost_known: bool,
    pub dedupe_key: String,
    pub observed_at_ms: i64,
    pub is_sidechain: bool,
    pub final_message: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TranscriptCursorPosition {
    pub file_identity: Option<String>,
    pub byte_offset: i64,
    pub file_size: i64,
    pub modified_at_ms: Option<i64>,
    // Finer freshness metadata is encoded in the existing v13 private cursor
    // blob; it never contains transcript text.
    pub modified_at_ns: Option<i64>,
    pub content_anchor: Option<String>,
    pub hash_checkpoint: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoredTranscriptCursor {
    pub transcript_path: String,
    pub file_identity: Option<String>,
    pub byte_offset: i64,
    pub file_size: i64,
    pub modified_at_ms: Option<i64>,
    pub modified_at_ns: Option<i64>,
    pub content_anchor: Option<String>,
    pub hash_checkpoint: Option<Vec<u8>>,
    pub last_scanned_at_ms: i64,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug)]
struct IngestFile {
    ingest_id: String,
    path: String,
    session_id: String,
    reset: bool,
    notifications_allowed: bool,
    sessions: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TranscriptPublish {
    pub published: bool,
    pub sessions: Vec<String>,
}

/// Stages bounded parser chunks and atomically replaces or appends one
/// transcript at commit. Incomplete staging is invisible and is discarded by
/// the next begin for the same path.
pub struct TranscriptIngestRepository {
    pool: SqlitePool,
    current: Option<IngestFile>,
}

impl TranscriptIngestRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            current: None,
        }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn begin(
        &mut self,
        path: String,
        session_id: String,
        reset: bool,
        notifications_allowed: bool,
        _cursor: &TranscriptCursorPosition,
        scanned_at_ms: i64,
    ) -> Result<(), sqlx::Error> {
        let ingest_id = uuid::Uuid::now_v7().to_string();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM transcript_event_stage
              WHERE transcript_path=?1 AND staged_at_ms>0",
        )
        .bind(&path)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM transcript_usage_stage
              WHERE transcript_path=?1 AND staged_at_ms>0",
        )
        .bind(&path)
        .execute(&mut *tx)
        .await?;
        let stale_before = scanned_at_ms.saturating_sub(24 * 60 * 60 * 1_000);
        sqlx::query(
            "DELETE FROM transcript_event_stage
              WHERE staged_at_ms>0 AND staged_at_ms<?1",
        )
        .bind(stale_before)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM transcript_usage_stage
              WHERE staged_at_ms>0 AND staged_at_ms<?1",
        )
        .bind(stale_before)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.current = Some(IngestFile {
            ingest_id,
            path,
            session_id,
            reset,
            notifications_allowed,
            sessions: BTreeSet::new(),
        });
        Ok(())
    }

    pub async fn chunk(
        &mut self,
        events: Vec<AgentEvent>,
        usage: Vec<StoredUsage>,
        _cursor: &TranscriptCursorPosition,
        scanned_at_ms: i64,
    ) -> Result<(), sqlx::Error> {
        let file = self
            .current
            .as_mut()
            .ok_or_else(|| sqlx::Error::Protocol("index chunk without begin".into()))?;
        let mut tx = self.pool.begin().await?;
        for event in events {
            if event.source != EventSource::Transcript {
                return Err(sqlx::Error::Protocol(
                    "transcript stage received non-transcript event".into(),
                ));
            }
            file.sessions.insert(event.session_id.0.clone());
            sqlx::query(
                "INSERT OR REPLACE INTO transcript_event_stage (
                    ingest_id,transcript_path,id,agent_kind,session_id,source_event,
                    occurred_at_ms,received_at_ms,sequence_no,dedupe_key,payload_version,
                    payload_json,notifications_allowed,staged_at_ms
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            )
            .bind(&file.ingest_id)
            .bind(&file.path)
            .bind(event.id.0)
            .bind(event.agent_kind.0)
            .bind(event.session_id.0)
            .bind(event.source_event)
            .bind(event.occurred_at_ms)
            .bind(event.received_at_ms)
            .bind(event.sequence_no)
            .bind(event.dedupe_key)
            .bind(event.payload_version)
            .bind(event.payload.to_string())
            .bind(file.notifications_allowed)
            // Positive values are private, uncommitted staging timestamps.
            // `-1` is reserved below for durable committed candidates.
            .bind(scanned_at_ms.max(1))
            .execute(&mut *tx)
            .await?;
        }
        for record in usage {
            file.sessions.insert(record.session_id.clone());
            sqlx::query(
                "INSERT OR REPLACE INTO transcript_usage_stage (
                    ingest_id,transcript_path,id,session_id,source_location,request_id,
                    message_id,model_id,local_day,input_tokens,output_tokens,
                    cache_write_tokens,cache_read_tokens,cost_pico_usd,cost_known,
                    dedupe_key,observed_at_ms,staged_at_ms,is_sidechain,final_message
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,
                           ?16,?17,?18,?19,?20)",
            )
            .bind(&file.ingest_id)
            .bind(&file.path)
            .bind(record.id)
            .bind(record.session_id)
            .bind(record.source_location)
            .bind(record.request_id)
            .bind(record.message_id)
            .bind(record.model_id)
            .bind(record.local_day)
            .bind(record.input_tokens)
            .bind(record.output_tokens)
            .bind(record.cache_write_tokens)
            .bind(record.cache_read_tokens)
            .bind(record.cost_pico_usd)
            .bind(record.cost_known)
            .bind(record.dedupe_key)
            .bind(record.observed_at_ms)
            .bind(scanned_at_ms.max(1))
            .bind(record.is_sidechain)
            .bind(record.final_message)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }

    pub async fn commit(
        &mut self,
        cursor: &TranscriptCursorPosition,
        scanned_at_ms: i64,
    ) -> Result<TranscriptPublish, sqlx::Error> {
        let file = self
            .current
            .take()
            .ok_or_else(|| sqlx::Error::Protocol("index commit without begin".into()))?;
        let mut sessions = file.sessions.clone();
        sessions.insert(file.session_id.clone());
        let mut tx = self.pool.begin().await?;
        if file.reset {
            let old = sqlx::query(
                "SELECT DISTINCT session_id FROM raw_events
                  WHERE source='transcript' AND transcript_path=?1
                 UNION
                 SELECT DISTINCT session_id FROM transcript_usage_stage
                  WHERE transcript_path=?1 AND staged_at_ms=-1",
            )
            .bind(&file.path)
            .fetch_all(&mut *tx)
            .await?;
            sessions.extend(old.into_iter().map(|row| row.get::<String, _>(0)));
            sqlx::query("DELETE FROM raw_events WHERE source='transcript' AND transcript_path=?1")
                .bind(&file.path)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM usage_records WHERE transcript_path=?1")
                .bind(&file.path)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "DELETE FROM transcript_usage_stage
                  WHERE transcript_path=?1 AND staged_at_ms=-1",
            )
            .bind(&file.path)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "INSERT OR IGNORE INTO raw_events (
                id,agent_kind,session_id,source,source_event,occurred_at_ms,
                received_at_ms,sequence_no,dedupe_key,payload_version,payload_json,
                transcript_path,notifications_allowed
             )
             SELECT id,agent_kind,session_id,'transcript',source_event,occurred_at_ms,
                    received_at_ms,sequence_no,dedupe_key,payload_version,payload_json,
                    transcript_path,notifications_allowed
               FROM transcript_event_stage WHERE ingest_id=?1",
        )
        .bind(&file.ingest_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE transcript_usage_stage SET staged_at_ms=-1 WHERE ingest_id=?1")
            .bind(&file.ingest_id)
            .execute(&mut *tx)
            .await?;
        rebuild_usage_winners(&mut tx).await?;
        let cursor_metadata = encode_cursor_metadata(cursor);
        sqlx::query(
            "INSERT INTO transcript_cursors (
                transcript_path,file_identity,byte_offset,file_size,modified_at_ms,
                partial_line,last_scanned_at_ms,last_error,content_anchor
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,NULL,?8)
             ON CONFLICT(transcript_path) DO UPDATE SET
                file_identity=excluded.file_identity,byte_offset=excluded.byte_offset,
                file_size=excluded.file_size,modified_at_ms=excluded.modified_at_ms,
                partial_line=excluded.partial_line,last_scanned_at_ms=excluded.last_scanned_at_ms,
                last_error=NULL,content_anchor=excluded.content_anchor",
        )
        .bind(&file.path)
        .bind(&cursor.file_identity)
        .bind(cursor.byte_offset)
        .bind(cursor.file_size)
        .bind(cursor.modified_at_ms)
        .bind(cursor_metadata)
        .bind(scanned_at_ms)
        .bind(&cursor.content_anchor)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM transcript_event_stage WHERE ingest_id=?1")
            .bind(&file.ingest_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(TranscriptPublish {
            published: true,
            sessions: sessions.into_iter().collect(),
        })
    }
}

async fn rebuild_usage_winners(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM usage_records")
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "INSERT INTO usage_records (
                id,agent_kind,session_id,transcript_path,source_location,request_id,
                message_id,model_id,local_day,input_tokens,output_tokens,
                cache_write_tokens,cache_read_tokens,cost_pico_usd,cost_known,
                dedupe_key,observed_at_ms,is_sidechain,final_message
             )
             SELECT id,'claude',session_id,transcript_path,source_location,request_id,
                    message_id,model_id,local_day,input_tokens,output_tokens,
                    cache_write_tokens,cache_read_tokens,cost_pico_usd,cost_known,
                    dedupe_key,observed_at_ms,is_sidechain,final_message
               FROM (
                    SELECT staged.*,
                           ROW_NUMBER() OVER (
                               PARTITION BY dedupe_key
                               ORDER BY observed_at_ms DESC,source_location DESC,id DESC
                           ) winner
                      FROM transcript_usage_stage staged
                     WHERE staged_at_ms=-1
               )
              WHERE winner=1",
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn encode_cursor_metadata(cursor: &TranscriptCursorPosition) -> Option<Vec<u8>> {
    if cursor.modified_at_ns.is_none() && cursor.hash_checkpoint.is_none() {
        return None;
    }
    let checkpoint = cursor.hash_checkpoint.as_deref().unwrap_or_default();
    let checkpoint_len = u32::try_from(checkpoint.len()).ok()?;
    let mut encoded = Vec::with_capacity(17 + checkpoint.len());
    encoded.extend_from_slice(CURSOR_METADATA_MAGIC);
    encoded.push(u8::from(cursor.modified_at_ns.is_some()));
    encoded.extend_from_slice(&cursor.modified_at_ns.unwrap_or_default().to_le_bytes());
    encoded.extend_from_slice(&checkpoint_len.to_le_bytes());
    encoded.extend_from_slice(checkpoint);
    Some(encoded)
}

fn decode_cursor_metadata(value: Option<Vec<u8>>) -> (Option<i64>, Option<Vec<u8>>) {
    let Some(value) = value else {
        return (None, None);
    };
    if value.len() < 17 || &value[..4] != CURSOR_METADATA_MAGIC {
        return (None, None);
    }
    let modified_at_ns = (value[4] != 0).then(|| {
        i64::from_le_bytes(
            value[5..13]
                .try_into()
                .expect("fixed cursor metadata width"),
        )
    });
    let checkpoint_len = u32::from_le_bytes(
        value[13..17]
            .try_into()
            .expect("fixed cursor metadata width"),
    ) as usize;
    let Some(end) = 17_usize.checked_add(checkpoint_len) else {
        return (None, None);
    };
    if end != value.len() {
        return (None, None);
    }
    let hash_checkpoint = (checkpoint_len > 0).then(|| value[17..end].to_vec());
    (modified_at_ns, hash_checkpoint)
}

pub async fn load_cursor(
    pool: &SqlitePool,
    transcript_path: &str,
) -> Result<Option<StoredTranscriptCursor>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT transcript_path,file_identity,byte_offset,file_size,modified_at_ms,
                partial_line,content_anchor,last_scanned_at_ms,last_error
           FROM transcript_cursors WHERE transcript_path=?1",
    )
    .bind(transcript_path)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| {
        let (modified_at_ns, hash_checkpoint) = decode_cursor_metadata(row.get("partial_line"));
        StoredTranscriptCursor {
            transcript_path: row.get("transcript_path"),
            file_identity: row.get("file_identity"),
            byte_offset: row.get("byte_offset"),
            file_size: row.get("file_size"),
            modified_at_ms: row.get("modified_at_ms"),
            modified_at_ns,
            content_anchor: row.get("content_anchor"),
            hash_checkpoint,
            last_scanned_at_ms: row.get("last_scanned_at_ms"),
            last_error: row.get("last_error"),
        }
    }))
}

/// Removes evidence for transcript files no longer present and returns the
/// affected sessions for deterministic reducer recomputation.
pub async fn remove_missing_transcripts(
    pool: &SqlitePool,
    present_paths: &BTreeSet<String>,
) -> Result<Vec<String>, sqlx::Error> {
    let paths = sqlx::query_scalar::<_, String>(
        "SELECT transcript_path FROM transcript_cursors ORDER BY transcript_path",
    )
    .fetch_all(pool)
    .await?;
    let mut sessions = BTreeSet::new();
    for path in paths {
        if present_paths.contains(&path) {
            continue;
        }
        let mut tx = pool.begin().await?;
        let rows = sqlx::query(
            "SELECT DISTINCT session_id FROM raw_events
              WHERE source='transcript' AND transcript_path=?1
             UNION
             SELECT DISTINCT session_id FROM transcript_usage_stage
              WHERE transcript_path=?1 AND staged_at_ms=-1",
        )
        .bind(&path)
        .fetch_all(&mut *tx)
        .await?;
        sessions.extend(rows.into_iter().map(|row| row.get::<String, _>(0)));
        sqlx::query("DELETE FROM raw_events WHERE source='transcript' AND transcript_path=?1")
            .bind(&path)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM usage_records WHERE transcript_path=?1")
            .bind(&path)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM transcript_usage_stage WHERE transcript_path=?1")
            .bind(&path)
            .execute(&mut *tx)
            .await?;
        rebuild_usage_winners(&mut tx).await?;
        sqlx::query("DELETE FROM transcript_cursors WHERE transcript_path=?1")
            .bind(&path)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(sessions.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use monitor_domain::{AgentKind, EventId, SessionId};
    use serde_json::json;

    #[tokio::test]
    async fn failed_ingest_is_invisible_and_retry_replaces_the_file_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::connect(&directory.path().join("state.db"))
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        let mut repository = TranscriptIngestRepository::new(pool.clone());
        repository
            .begin(
                "/tmp/session.jsonl".into(),
                "session".into(),
                true,
                false,
                &TranscriptCursorPosition::default(),
                1,
            )
            .await
            .unwrap();
        repository
            .chunk(
                vec![AgentEvent {
                    id: EventId("event".into()),
                    agent_kind: AgentKind::claude(),
                    session_id: SessionId("session".into()),
                    source: EventSource::Transcript,
                    source_event: "TranscriptAssistantText".into(),
                    occurred_at_ms: 1,
                    received_at_ms: 1,
                    sequence_no: None,
                    dedupe_key: "event".into(),
                    payload_version: 1,
                    payload: json!({}),
                }],
                Vec::new(),
                &TranscriptCursorPosition::default(),
                1,
            )
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM raw_events")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        repository
            .commit(&TranscriptCursorPosition::default(), 1)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM raw_events")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }
}
