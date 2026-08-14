use crate::usage::UsageTotals;
use futures_util::TryStreamExt;
use monitor_domain::{AgentEvent, EventSource};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};
use std::collections::BTreeSet;

const CURSOR_METADATA_MAGIC: &[u8; 4] = b"CMC1";
const USAGE_BACKFILL_PAGE_SIZE: i64 = 512;

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
    // Finer freshness metadata is encoded in the private cursor
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

#[derive(Default)]
struct AffectedUsageGroups {
    sessions: BTreeSet<(String, String)>,
    models: BTreeSet<(String, String, String)>,
    days: BTreeSet<(String, String)>,
}

impl AffectedUsageGroups {
    fn insert(&mut self, agent_kind: String, session_id: String, model_id: String, day: String) {
        self.sessions
            .insert((agent_kind.clone(), session_id.clone()));
        self.models
            .insert((agent_kind.clone(), session_id, model_id));
        self.days.insert((agent_kind, day));
    }

    fn extend(&mut self, other: Self) {
        self.sessions.extend(other.sessions);
        self.models.extend(other.models);
        self.days.extend(other.days);
    }
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
        let mut affected_usage_keys = BTreeSet::new();
        if file.reset {
            affected_usage_keys.extend(
                sqlx::query_scalar::<_, String>(
                    "SELECT DISTINCT dedupe_key FROM transcript_usage_stage
                      WHERE transcript_path=?1 AND staged_at_ms=-1",
                )
                .bind(&file.path)
                .fetch_all(&mut *tx)
                .await?,
            );
        }
        affected_usage_keys.extend(
            sqlx::query_scalar::<_, String>(
                "SELECT DISTINCT dedupe_key FROM transcript_usage_stage WHERE ingest_id=?1",
            )
            .bind(&file.ingest_id)
            .fetch_all(&mut *tx)
            .await?,
        );
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
        reconcile_usage_winners(&mut tx, &affected_usage_keys).await?;
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

fn empty_usage_totals() -> UsageTotals {
    UsageTotals::from_values(0, 0, 0, 0, 0, true)
}

fn usage_totals_from_row(row: &sqlx::sqlite::SqliteRow) -> UsageTotals {
    let cost_known = row.get::<i64, _>("cost_known") != 0;
    let mut usage = UsageTotals::from_values(
        row.get("input_tokens"),
        row.get("output_tokens"),
        row.get("cache_write_tokens"),
        row.get("cache_read_tokens"),
        row.get("cost_pico_usd"),
        cost_known,
    );
    if !cost_known {
        usage.unpriced_tokens = usage.tokens();
    }
    usage
}

async fn affected_usage_groups(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<AffectedUsageGroups, sqlx::Error> {
    let mut rows = sqlx::query(
        "SELECT DISTINCT u.agent_kind,u.session_id,u.model_id,u.local_day
           FROM usage_records u
           JOIN temp.cc_monitor_affected_usage_keys affected
             ON affected.dedupe_key=u.dedupe_key",
    )
    .fetch(&mut **tx);
    let mut groups = AffectedUsageGroups::default();
    while let Some(row) = rows.try_next().await? {
        groups.insert(
            row.get("agent_kind"),
            row.get("session_id"),
            row.get("model_id"),
            row.get("local_day"),
        );
    }
    Ok(groups)
}

async fn refresh_session_aggregate(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    agent_kind: &str,
    session_id: &str,
) -> Result<(), sqlx::Error> {
    let mut rows = sqlx::query(
        "SELECT input_tokens,output_tokens,cache_write_tokens,cache_read_tokens,
                cost_pico_usd,cost_known
           FROM usage_records WHERE agent_kind=?1 AND session_id=?2",
    )
    .bind(agent_kind)
    .bind(session_id)
    .fetch(&mut **tx);
    let mut total = empty_usage_totals();
    let mut found = false;
    while let Some(row) = rows.try_next().await? {
        total.add(usage_totals_from_row(&row));
        found = true;
    }
    drop(rows);
    sqlx::query(
        "DELETE FROM usage_session_aggregates
          WHERE agent_kind=?1 AND session_id=?2",
    )
    .bind(agent_kind)
    .bind(session_id)
    .execute(&mut **tx)
    .await?;
    if found {
        insert_session_aggregate(tx, agent_kind, session_id, total).await?;
    }
    Ok(())
}

async fn refresh_model_aggregate(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    agent_kind: &str,
    session_id: &str,
    model_id: &str,
) -> Result<(), sqlx::Error> {
    let mut rows = sqlx::query(
        "SELECT input_tokens,output_tokens,cache_write_tokens,cache_read_tokens,
                cost_pico_usd,cost_known
           FROM usage_records
          WHERE agent_kind=?1 AND session_id=?2 AND model_id=?3",
    )
    .bind(agent_kind)
    .bind(session_id)
    .bind(model_id)
    .fetch(&mut **tx);
    let mut total = empty_usage_totals();
    let mut found = false;
    while let Some(row) = rows.try_next().await? {
        total.add(usage_totals_from_row(&row));
        found = true;
    }
    drop(rows);
    sqlx::query(
        "DELETE FROM usage_model_aggregates
          WHERE agent_kind=?1 AND session_id=?2 AND model_id=?3",
    )
    .bind(agent_kind)
    .bind(session_id)
    .bind(model_id)
    .execute(&mut **tx)
    .await?;
    if found {
        insert_model_aggregate(tx, agent_kind, session_id, model_id, total).await?;
    }
    Ok(())
}

async fn refresh_daily_aggregate(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    agent_kind: &str,
    day: &str,
) -> Result<(), sqlx::Error> {
    let mut rows = sqlx::query(
        "SELECT input_tokens,output_tokens,cache_write_tokens,cache_read_tokens,
                cost_pico_usd,cost_known
           FROM usage_records WHERE agent_kind=?1 AND local_day=?2",
    )
    .bind(agent_kind)
    .bind(day)
    .fetch(&mut **tx);
    let mut total = empty_usage_totals();
    let mut found = false;
    while let Some(row) = rows.try_next().await? {
        total.add(usage_totals_from_row(&row));
        found = true;
    }
    drop(rows);
    sqlx::query(
        "DELETE FROM usage_daily_aggregates
          WHERE agent_kind=?1 AND local_day=?2",
    )
    .bind(agent_kind)
    .bind(day)
    .execute(&mut **tx)
    .await?;
    if found {
        insert_daily_aggregate(tx, agent_kind, day, total).await?;
    }
    Ok(())
}

async fn refresh_usage_aggregates(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    groups: AffectedUsageGroups,
) -> Result<(), sqlx::Error> {
    for (agent_kind, session_id) in groups.sessions {
        refresh_session_aggregate(tx, &agent_kind, &session_id).await?;
    }
    for (agent_kind, session_id, model_id) in groups.models {
        refresh_model_aggregate(tx, &agent_kind, &session_id, &model_id).await?;
    }
    for (agent_kind, day) in groups.days {
        refresh_daily_aggregate(tx, &agent_kind, &day).await?;
    }
    Ok(())
}

async fn insert_session_aggregate(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    agent_kind: &str,
    session_id: &str,
    total: UsageTotals,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO usage_session_aggregates (
            agent_kind,session_id,input_tokens,output_tokens,cache_write_tokens,
            cache_read_tokens,cost_pico_usd,cost_known,unpriced_tokens
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
    )
    .bind(agent_kind)
    .bind(session_id)
    .bind(total.input_tokens)
    .bind(total.output_tokens)
    .bind(total.cache_write_tokens)
    .bind(total.cache_read_tokens)
    .bind(total.cost_pico_usd)
    .bind(total.cost_known)
    .bind(total.unpriced_tokens)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_model_aggregate(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    agent_kind: &str,
    session_id: &str,
    model_id: &str,
    total: UsageTotals,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO usage_model_aggregates (
            agent_kind,session_id,model_id,input_tokens,output_tokens,
            cache_write_tokens,cache_read_tokens,cost_pico_usd,cost_known,
            unpriced_tokens
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
    )
    .bind(agent_kind)
    .bind(session_id)
    .bind(model_id)
    .bind(total.input_tokens)
    .bind(total.output_tokens)
    .bind(total.cache_write_tokens)
    .bind(total.cache_read_tokens)
    .bind(total.cost_pico_usd)
    .bind(total.cost_known)
    .bind(total.unpriced_tokens)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_daily_aggregate(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    agent_kind: &str,
    day: &str,
    total: UsageTotals,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO usage_daily_aggregates (
            agent_kind,local_day,input_tokens,output_tokens,cache_write_tokens,
            cache_read_tokens,cost_pico_usd,cost_known,unpriced_tokens
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
    )
    .bind(agent_kind)
    .bind(day)
    .bind(total.input_tokens)
    .bind(total.output_tokens)
    .bind(total.cache_write_tokens)
    .bind(total.cache_read_tokens)
    .bind(total.cost_pico_usd)
    .bind(total.cost_known)
    .bind(total.unpriced_tokens)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn reconcile_usage_winners(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    affected_keys: &BTreeSet<String>,
) -> Result<(), sqlx::Error> {
    if affected_keys.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "DROP TABLE IF EXISTS temp.cc_monitor_affected_usage_keys;
         CREATE TEMP TABLE cc_monitor_affected_usage_keys (
             dedupe_key TEXT PRIMARY KEY NOT NULL
         ) WITHOUT ROWID",
    )
    .execute(&mut **tx)
    .await?;
    let mut remaining_keys = affected_keys.iter();
    loop {
        let keys = remaining_keys.by_ref().take(500).collect::<Vec<_>>();
        if keys.is_empty() {
            break;
        }
        let mut insert = QueryBuilder::<Sqlite>::new(
            "INSERT OR IGNORE INTO temp.cc_monitor_affected_usage_keys(dedupe_key) ",
        );
        insert.push_values(&keys, |mut row, key| {
            row.push_bind(key.as_str());
        });
        insert.build().execute(&mut **tx).await?;
    }
    let mut affected_groups = affected_usage_groups(tx).await?;
    sqlx::query(
        "DELETE FROM usage_records
          WHERE dedupe_key IN (
              SELECT dedupe_key FROM temp.cc_monitor_affected_usage_keys
          )",
    )
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
                               PARTITION BY staged.dedupe_key
                               ORDER BY staged.observed_at_ms DESC,
                                        staged.source_location DESC,staged.id DESC
                           ) winner
                      FROM transcript_usage_stage staged
                      JOIN temp.cc_monitor_affected_usage_keys affected
                        ON affected.dedupe_key=staged.dedupe_key
                     WHERE staged.staged_at_ms=-1
               )
              WHERE winner=1",
    )
    .execute(&mut **tx)
    .await?;
    affected_groups.extend(affected_usage_groups(tx).await?);
    refresh_usage_aggregates(tx, affected_groups).await?;
    sqlx::query("DROP TABLE temp.cc_monitor_affected_usage_keys")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Completes the one-time v2 backfill before any desktop worker can observe
/// the new read models. The marker makes an interrupted migration restartable.
pub(crate) async fn repair_usage_aggregates(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let is_current: i64 =
        sqlx::query_scalar("SELECT is_current FROM usage_aggregate_state WHERE singleton=1")
            .fetch_one(pool)
            .await?;
    if is_current != 0 {
        return Ok(());
    }

    let mut tx = pool.begin().await?;
    let is_current: i64 =
        sqlx::query_scalar("SELECT is_current FROM usage_aggregate_state WHERE singleton=1")
            .fetch_one(&mut *tx)
            .await?;
    if is_current != 0 {
        tx.commit().await?;
        return Ok(());
    }
    sqlx::query("DELETE FROM usage_session_aggregates")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM usage_model_aggregates")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM usage_daily_aggregates")
        .execute(&mut *tx)
        .await?;
    rebuild_session_and_model_aggregates(&mut tx).await?;
    rebuild_daily_aggregates(&mut tx).await?;
    sqlx::query("UPDATE usage_aggregate_state SET is_current=1 WHERE singleton=1")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn rebuild_session_and_model_aggregates(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    let mut cursor: Option<(String, String, String, String)> = None;
    let mut session_key: Option<(String, String)> = None;
    let mut model_key: Option<(String, String, String)> = None;
    let mut session_total = empty_usage_totals();
    let mut model_total = empty_usage_totals();
    loop {
        let rows = if let Some((agent_kind, session_id, model_id, id)) = cursor.as_ref() {
            sqlx::query(
                "SELECT id,agent_kind,session_id,model_id,input_tokens,output_tokens,
                        cache_write_tokens,cache_read_tokens,cost_pico_usd,cost_known
                   FROM usage_records
                  WHERE (agent_kind,session_id,model_id,id)>(?1,?2,?3,?4)
                  ORDER BY agent_kind,session_id,model_id,id LIMIT ?5",
            )
            .bind(agent_kind)
            .bind(session_id)
            .bind(model_id)
            .bind(id)
            .bind(USAGE_BACKFILL_PAGE_SIZE)
            .fetch_all(&mut **tx)
            .await?
        } else {
            sqlx::query(
                "SELECT id,agent_kind,session_id,model_id,input_tokens,output_tokens,
                        cache_write_tokens,cache_read_tokens,cost_pico_usd,cost_known
                   FROM usage_records
                  ORDER BY agent_kind,session_id,model_id,id LIMIT ?1",
            )
            .bind(USAGE_BACKFILL_PAGE_SIZE)
            .fetch_all(&mut **tx)
            .await?
        };
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let next_session = (
                row.get::<String, _>("agent_kind"),
                row.get::<String, _>("session_id"),
            );
            let next_model = (
                next_session.0.clone(),
                next_session.1.clone(),
                row.get::<String, _>("model_id"),
            );
            if model_key.as_ref().is_some_and(|key| key != &next_model) {
                let (agent_kind, session_id, model_id) = model_key.take().unwrap();
                insert_model_aggregate(tx, &agent_kind, &session_id, &model_id, model_total)
                    .await?;
                model_total = empty_usage_totals();
            }
            if session_key.as_ref().is_some_and(|key| key != &next_session) {
                let (agent_kind, session_id) = session_key.take().unwrap();
                insert_session_aggregate(tx, &agent_kind, &session_id, session_total).await?;
                session_total = empty_usage_totals();
            }
            session_key.get_or_insert(next_session.clone());
            model_key.get_or_insert(next_model.clone());
            let usage = usage_totals_from_row(row);
            session_total.add(usage);
            model_total.add(usage);
            cursor = Some((next_model.0, next_model.1, next_model.2, row.get("id")));
        }
    }
    if let Some((agent_kind, session_id, model_id)) = model_key {
        insert_model_aggregate(tx, &agent_kind, &session_id, &model_id, model_total).await?;
    }
    if let Some((agent_kind, session_id)) = session_key {
        insert_session_aggregate(tx, &agent_kind, &session_id, session_total).await?;
    }
    Ok(())
}

async fn rebuild_daily_aggregates(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
) -> Result<(), sqlx::Error> {
    let mut cursor: Option<(String, String, String)> = None;
    let mut day_key: Option<(String, String)> = None;
    let mut day_total = empty_usage_totals();
    loop {
        let rows = if let Some((agent_kind, day, id)) = cursor.as_ref() {
            sqlx::query(
                "SELECT id,agent_kind,local_day,input_tokens,output_tokens,
                        cache_write_tokens,cache_read_tokens,cost_pico_usd,cost_known
                   FROM usage_records WHERE (agent_kind,local_day,id)>(?1,?2,?3)
                  ORDER BY agent_kind,local_day,id LIMIT ?4",
            )
            .bind(agent_kind)
            .bind(day)
            .bind(id)
            .bind(USAGE_BACKFILL_PAGE_SIZE)
            .fetch_all(&mut **tx)
            .await?
        } else {
            sqlx::query(
                "SELECT id,agent_kind,local_day,input_tokens,output_tokens,
                        cache_write_tokens,cache_read_tokens,cost_pico_usd,cost_known
                   FROM usage_records ORDER BY agent_kind,local_day,id LIMIT ?1",
            )
            .bind(USAGE_BACKFILL_PAGE_SIZE)
            .fetch_all(&mut **tx)
            .await?
        };
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let next_day = (
                row.get::<String, _>("agent_kind"),
                row.get::<String, _>("local_day"),
            );
            if day_key.as_ref().is_some_and(|key| key != &next_day) {
                let (agent_kind, day) = day_key.take().unwrap();
                insert_daily_aggregate(tx, &agent_kind, &day, day_total).await?;
                day_total = empty_usage_totals();
            }
            day_key.get_or_insert(next_day.clone());
            day_total.add(usage_totals_from_row(row));
            cursor = Some((next_day.0, next_day.1, row.get("id")));
        }
    }
    if let Some((agent_kind, day)) = day_key {
        insert_daily_aggregate(tx, &agent_kind, &day, day_total).await?;
    }
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
        let affected_usage_keys = sqlx::query_scalar::<_, String>(
            "SELECT DISTINCT dedupe_key FROM transcript_usage_stage
              WHERE transcript_path=?1 AND staged_at_ms=-1",
        )
        .bind(&path)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect::<BTreeSet<_>>();
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
        sqlx::query("DELETE FROM transcript_usage_stage WHERE transcript_path=?1")
            .bind(&path)
            .execute(&mut *tx)
            .await?;
        reconcile_usage_winners(&mut tx, &affected_usage_keys).await?;
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
