//! Tauri-independent projection, delivery, and retention pipeline.

pub use monitor_domain as domain;
pub use monitor_storage as storage;

use monitor_domain::{
    reduce_at, AgentEvent, AgentKind, Confidence, EventId, EventSource, NotificationKind,
    SessionId, SessionLifecycle, TurnState,
};
use monitor_notify::{diagnostic_code, Notification, NotificationProvider, Priority};
use sqlx::{Row, SqlitePool};
use std::collections::{BTreeSet, HashMap};

const SESSION_BATCH_SIZE: i64 = 8;
pub const MAX_DELIVERY_ATTEMPTS: i64 = 3;
const RETENTION_NOTIFICATION_BATCH_SIZE: u64 = 100;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("engine storage operation failed")]
    Storage(#[from] sqlx::Error),
    #[error("stored event is invalid")]
    InvalidEvent(#[from] serde_json::Error),
    #[error("derived session projection is invalid")]
    InvalidProjection,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SessionKey {
    agent: AgentKind,
    session_id: SessionId,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BatchResult {
    attempted: BTreeSet<SessionKey>,
    succeeded: BTreeSet<SessionKey>,
    quarantined: BTreeSet<SessionKey>,
    has_more: bool,
}

impl BatchResult {
    pub fn merge(&mut self, other: Self) {
        self.has_more |= other.has_more;
        self.attempted.extend(other.attempted);
        self.succeeded.extend(other.succeeded);
        self.quarantined.extend(other.quarantined);
    }

    pub fn attempted(&self) -> usize {
        self.attempted.len()
    }

    pub fn succeeded(&self) -> usize {
        self.succeeded.len()
    }

    pub fn quarantined(&self) -> usize {
        self.quarantined.len()
    }

    pub fn changed(&self) -> usize {
        self.succeeded.len() + self.quarantined.len()
    }

    pub fn has_more(&self) -> bool {
        self.has_more
    }

    pub fn terminal_session_ids(&self) -> Vec<String> {
        self.succeeded
            .union(&self.quarantined)
            .map(|key| key.session_id.0.clone())
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct BatchError {
    partial: BatchResult,
    #[source]
    source: EngineError,
}

impl BatchError {
    fn new(partial: BatchResult, source: EngineError) -> Self {
        Self { partial, source }
    }

    pub fn partial(&self) -> &BatchResult {
        &self.partial
    }

    pub fn into_parts(self) -> (BatchResult, EngineError) {
        (self.partial, self.source)
    }

    pub fn source_error(&self) -> &EngineError {
        &self.source
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderKind {
    Desktop,
    Ntfy,
}

impl ProviderKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Ntfy => "ntfy",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboxItem {
    pub id: String,
    pub provider: ProviderKind,
    pub notification: Notification,
    pub attempt_count: i64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetentionCounts {
    pub raw_events_deleted: u64,
    pub notifications_deleted: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetentionStep {
    pub counts: RetentionCounts,
    pub has_more: bool,
}

pub struct Engine {
    pool: SqlitePool,
}

impl Engine {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn pending_sessions(&self) -> Result<Vec<(AgentKind, SessionId)>, EngineError> {
        let rows = sqlx::query(
            "SELECT agent_kind,session_id,MIN(received_at_ms) ready_at
               FROM raw_events
              WHERE processed_at_ms IS NULL AND process_error IS NULL
              GROUP BY agent_kind,session_id
              ORDER BY ready_at,agent_kind,session_id
              LIMIT ?1",
        )
        .bind(SESSION_BATCH_SIZE)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    AgentKind(row.get("agent_kind")),
                    SessionId(row.get("session_id")),
                )
            })
            .collect())
    }

    pub async fn process_pending(
        &self,
        providers: &[ProviderKind],
        processed_at_ms: i64,
    ) -> Result<BatchResult, BatchError> {
        let sessions = self
            .pending_sessions()
            .await
            .map_err(|source| BatchError::new(BatchResult::default(), source))?;
        let mut result = self
            .process_sessions(sessions, providers, processed_at_ms)
            .await?;
        result.has_more = !self
            .pending_sessions()
            .await
            .map_err(|source| BatchError::new(result.clone(), source))?
            .is_empty();
        Ok(result)
    }

    pub async fn process_sessions<I>(
        &self,
        sessions: I,
        providers: &[ProviderKind],
        processed_at_ms: i64,
    ) -> Result<BatchResult, BatchError>
    where
        I: IntoIterator<Item = (AgentKind, SessionId)>,
    {
        let mut result = BatchResult::default();
        for (agent, session_id) in sessions {
            let key = SessionKey {
                agent: agent.clone(),
                session_id: session_id.clone(),
            };
            result.attempted.insert(key.clone());
            match self
                .process_session(agent, &session_id, providers, processed_at_ms)
                .await
            {
                Ok(()) => {
                    result.succeeded.insert(key);
                }
                Err(error @ EngineError::Storage(_)) => {
                    return Err(BatchError::new(result, error));
                }
                Err(error) => {
                    self.quarantine_session(&session_id, error_code(&error), processed_at_ms)
                        .await
                        .map_err(|source| BatchError::new(result.clone(), source))?;
                    result.quarantined.insert(key);
                }
            }
        }
        Ok(result)
    }

    pub async fn process_session(
        &self,
        agent: AgentKind,
        session_id: &SessionId,
        providers: &[ProviderKind],
        processed_at_ms: i64,
    ) -> Result<(), EngineError> {
        let rows = sqlx::query(
            "SELECT id,agent_kind,session_id,source,source_event,occurred_at_ms,
                    received_at_ms,sequence_no,dedupe_key,payload_version,payload_json,
                    transcript_path,notifications_allowed
               FROM raw_events
              WHERE agent_kind=?1 AND session_id=?2
              ORDER BY occurred_at_ms,received_at_ms,dedupe_key",
        )
        .bind(&agent.0)
        .bind(&session_id.0)
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            let mut tx = self.pool.begin().await?;
            sqlx::query("DELETE FROM session_projection WHERE agent_kind=?1 AND session_id=?2")
                .bind(&agent.0)
                .bind(&session_id.0)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(());
        }
        let mut notifications_allowed = HashMap::new();
        let mut events = Vec::with_capacity(rows.len());
        let mut project_name = None;
        let mut cwd = None;
        let mut transcript_path: Option<String> = None;
        let mut client_bundle_id = None;
        for row in rows {
            let payload: serde_json::Value =
                serde_json::from_str(row.get::<&str, _>("payload_json"))?;
            for (target, key) in [
                (&mut project_name, "project_name"),
                (&mut cwd, "cwd"),
                (&mut client_bundle_id, "client_bundle_id"),
            ] {
                if let Some(value) = payload.get(key).and_then(|value| value.as_str()) {
                    if !value.is_empty() {
                        *target = Some(value.to_owned());
                    }
                }
            }
            let id: String = row.get("id");
            notifications_allowed
                .insert(id.clone(), row.get::<i64, _>("notifications_allowed") != 0);
            if row.get::<&str, _>("source") == "transcript" {
                transcript_path = row.get("transcript_path");
            }
            events.push(AgentEvent {
                id: EventId(id),
                agent_kind: AgentKind(row.get("agent_kind")),
                session_id: SessionId(row.get("session_id")),
                source: event_source(row.get("source"))?,
                source_event: row.get("source_event"),
                occurred_at_ms: row.get("occurred_at_ms"),
                received_at_ms: row.get("received_at_ms"),
                sequence_no: row.get("sequence_no"),
                dedupe_key: row.get("dedupe_key"),
                payload_version: row.get("payload_version"),
                payload,
            });
        }
        let reduction = reduce_at(events, processed_at_ms);
        let projection = reduction.projection.ok_or(EngineError::InvalidProjection)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO session_projection (
                agent_kind,session_id,lifecycle,turn_state,state_reason,state_source,
                confidence,revision,project_name,cwd,transcript_path,client_bundle_id,
                started_at_ms,changed_at_ms,last_observed_at_ms,ended_at_ms,current_turn_id
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
             ON CONFLICT(agent_kind,session_id) DO UPDATE SET
                lifecycle=excluded.lifecycle,turn_state=excluded.turn_state,
                state_reason=excluded.state_reason,state_source=excluded.state_source,
                confidence=excluded.confidence,revision=excluded.revision,
                project_name=COALESCE(excluded.project_name,session_projection.project_name),
                cwd=COALESCE(excluded.cwd,session_projection.cwd),
                transcript_path=COALESCE(excluded.transcript_path,session_projection.transcript_path),
                client_bundle_id=COALESCE(excluded.client_bundle_id,session_projection.client_bundle_id),
                started_at_ms=COALESCE(session_projection.started_at_ms,excluded.started_at_ms),
                changed_at_ms=excluded.changed_at_ms,
                last_observed_at_ms=excluded.last_observed_at_ms,
                ended_at_ms=excluded.ended_at_ms,current_turn_id=excluded.current_turn_id",
        )
        .bind(&projection.agent_kind.0)
        .bind(&projection.session_id.0)
        .bind(lifecycle_code(projection.lifecycle))
        .bind(turn_state_code(projection.turn_state))
        .bind(projection.reason.code())
        .bind(source_code(projection.source))
        .bind(confidence_code(projection.confidence))
        .bind(i64::try_from(projection.revision).unwrap_or(i64::MAX))
        .bind(project_name.as_deref())
        .bind(cwd.as_deref())
        .bind(transcript_path.as_deref())
        .bind(client_bundle_id.as_deref())
        .bind(projection.last_observed_at_ms)
        .bind(projection.changed_at_ms)
        .bind(projection.last_observed_at_ms)
        .bind((projection.lifecycle == SessionLifecycle::Ended).then_some(projection.changed_at_ms))
        .bind(projection.current_turn_id.as_ref().map(|value| value.0.as_str()))
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM turns WHERE agent_kind=?1 AND session_id=?2")
            .bind(&projection.agent_kind.0)
            .bind(&projection.session_id.0)
            .execute(&mut *tx)
            .await?;
        let turn_id = projection
            .current_turn_id
            .as_ref()
            .map(|value| value.0.clone())
            .unwrap_or_else(|| format!("{}:current", projection.session_id.0));
        sqlx::query(
            "INSERT INTO turns (
                id,agent_kind,session_id,ordinal,state,started_at_ms,changed_at_ms,finished_at_ms
             ) VALUES (?1,?2,?3,1,?4,?5,?6,?7)",
        )
        .bind(&turn_id)
        .bind(&projection.agent_kind.0)
        .bind(&projection.session_id.0)
        .bind(turn_state_code(projection.turn_state))
        .bind(projection.changed_at_ms)
        .bind(projection.changed_at_ms)
        .bind((projection.turn_state != TurnState::Running).then_some(projection.changed_at_ms))
        .execute(&mut *tx)
        .await?;
        for edge in reduction.notifications {
            if !notifications_allowed
                .get(&edge.triggering_event_id.0)
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
            for provider in providers {
                insert_notification(
                    &mut tx,
                    NotificationInsert {
                        session_id: &projection.session_id.0,
                        turn_id: &turn_id,
                        kind: edge.kind,
                        revision: edge.projection_revision,
                        provider: *provider,
                        project_name: project_name.as_deref(),
                        created_at_ms: processed_at_ms,
                    },
                )
                .await?;
            }
        }
        sqlx::query(
            "UPDATE raw_events SET processed_at_ms=?3,process_error=NULL
              WHERE agent_kind=?1 AND session_id=?2",
        )
        .bind(&agent.0)
        .bind(&session_id.0)
        .bind(processed_at_ms)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn quarantine_session(
        &self,
        session_id: &SessionId,
        code: &'static str,
        processed_at_ms: i64,
    ) -> Result<(), EngineError> {
        sqlx::query(
            "UPDATE raw_events SET process_error=?2,processed_at_ms=?3
              WHERE session_id=?1 AND processed_at_ms IS NULL",
        )
        .bind(&session_id.0)
        .bind(code)
        .bind(processed_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn reconcile_stale_transcripts(
        &self,
        observed_at_ms: i64,
    ) -> Result<BatchResult, BatchError> {
        let rows = sqlx::query(
            "SELECT agent_kind,session_id FROM session_projection
              WHERE lifecycle='active' AND state_source='transcript'
                AND turn_state='running' AND last_observed_at_ms<?1
              ORDER BY last_observed_at_ms LIMIT ?2",
        )
        .bind(observed_at_ms.saturating_sub(30_000))
        .bind(SESSION_BATCH_SIZE)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| BatchError::new(BatchResult::default(), EngineError::Storage(source)))?;
        self.process_sessions(
            rows.into_iter().map(|row| {
                (
                    AgentKind(row.get("agent_kind")),
                    SessionId(row.get("session_id")),
                )
            }),
            &[],
            observed_at_ms,
        )
        .await
    }

    pub async fn reconcile_startup(
        &self,
        observed_at_ms: i64,
        providers: &[ProviderKind],
    ) -> Result<(), EngineError> {
        sqlx::query(
            "UPDATE notification_outbox
                SET status='failed',next_attempt_at_ms=?1,last_error='delivery_interrupted'
              WHERE status='inflight'",
        )
        .bind(observed_at_ms)
        .execute(&self.pool)
        .await?;
        self.process_pending(providers, observed_at_ms)
            .await
            .map_err(|error| error.into_parts().1)?;
        Ok(())
    }

    pub async fn claim_next(
        &self,
        provider: ProviderKind,
        claimed_at_ms: i64,
    ) -> Result<Option<OutboxItem>, EngineError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT id,title,body,kind,attempt_count,session_id
               FROM notification_outbox
              WHERE provider=?1 AND status IN ('pending','failed')
                AND attempt_count<?2
                AND COALESCE(next_attempt_at_ms,-9223372036854775808)<=?3
              ORDER BY COALESCE(next_attempt_at_ms,-9223372036854775808),
                       created_at_ms,id LIMIT 1",
        )
        .bind(provider.as_str())
        .bind(MAX_DELIVERY_ATTEMPTS)
        .bind(claimed_at_ms)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let id: String = row.get("id");
        let changed = sqlx::query(
            "UPDATE notification_outbox SET status='inflight'
              WHERE id=?1 AND status IN ('pending','failed')",
        )
        .bind(&id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            tx.rollback().await?;
            return Ok(None);
        }
        let kind: String = row.get("kind");
        let session_id: String = row.get("session_id");
        let item = OutboxItem {
            id,
            provider,
            attempt_count: row.get("attempt_count"),
            notification: Notification {
                title: row.get("title"),
                body: row.get("body"),
                priority: if kind == "needs_input" {
                    Priority::High
                } else {
                    Priority::Default
                },
                tag: match kind.as_str() {
                    "needs_input" => "question",
                    "failed" => "x",
                    _ => "white_check_mark",
                }
                .into(),
                session_id: Some(session_id.clone()),
                client_bundle_id: sqlx::query_scalar(
                    "SELECT client_bundle_id FROM session_projection
                      WHERE agent_kind='claude' AND session_id=?1",
                )
                .bind(session_id)
                .fetch_optional(&mut *tx)
                .await?
                .flatten(),
            },
        };
        tx.commit().await?;
        Ok(Some(item))
    }

    pub async fn mark_sent(&self, id: &str, sent_at_ms: i64) -> Result<(), EngineError> {
        sqlx::query(
            "UPDATE notification_outbox
                SET status='sent',sent_at_ms=?2,next_attempt_at_ms=NULL,last_error=NULL
              WHERE id=?1 AND status='inflight'",
        )
        .bind(id)
        .bind(sent_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_provider_healthy(
        &self,
        provider: ProviderKind,
        observed_at_ms: i64,
    ) -> Result<(), EngineError> {
        sqlx::query(
            "INSERT INTO notification_provider_health (
                provider,consecutive_failures,last_error,last_failed_at_ms,recovered_at_ms
             ) VALUES (?1,0,NULL,NULL,?2)
             ON CONFLICT(provider) DO UPDATE SET
                recovered_at_ms=CASE WHEN consecutive_failures>0 THEN ?2
                                     ELSE recovered_at_ms END,
                consecutive_failures=0,last_error=NULL",
        )
        .bind(provider.as_str())
        .bind(observed_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_failed(
        &self,
        item: &OutboxItem,
        code: &str,
        failed_at_ms: i64,
    ) -> Result<(), EngineError> {
        let attempt = item.attempt_count + 1;
        let terminal = attempt >= MAX_DELIVERY_ATTEMPTS;
        let next = (!terminal).then_some(
            failed_at_ms
                + match attempt {
                    1 => 5_000,
                    2 => 30_000,
                    _ => 300_000,
                },
        );
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE notification_outbox
                SET status='failed',attempt_count=?2,next_attempt_at_ms=?3,last_error=?4
              WHERE id=?1 AND status='inflight'",
        )
        .bind(&item.id)
        .bind(attempt)
        .bind(next)
        .bind(diagnostic_code(code))
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO notification_provider_health (
                provider,consecutive_failures,last_error,last_failed_at_ms,recovered_at_ms
             ) VALUES (?1,1,?2,?3,NULL)
             ON CONFLICT(provider) DO UPDATE SET
                consecutive_failures=notification_provider_health.consecutive_failures+1,
                last_error=?2,last_failed_at_ms=?3",
        )
        .bind(item.provider.as_str())
        .bind(diagnostic_code(code))
        .bind(failed_at_ms)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn dispatch_one(
        &self,
        provider: ProviderKind,
        transport: &dyn NotificationProvider,
        observed_at_ms: i64,
    ) -> Result<bool, EngineError> {
        let Some(item) = self.claim_next(provider, observed_at_ms).await? else {
            return Ok(false);
        };
        self.dispatch_claimed(&item, transport, observed_at_ms)
            .await?;
        Ok(true)
    }

    pub async fn dispatch_claimed(
        &self,
        item: &OutboxItem,
        transport: &dyn NotificationProvider,
        observed_at_ms: i64,
    ) -> Result<(), EngineError> {
        match transport.send(&item.notification).await {
            Ok(()) => {
                self.mark_sent(&item.id, observed_at_ms).await?;
                self.mark_provider_healthy(item.provider, observed_at_ms)
                    .await
            }
            Err(error) => self.mark_failed(item, error.code(), observed_at_ms).await,
        }
    }

    pub async fn retain_older_than(&self, cutoff_ms: i64) -> Result<RetentionCounts, EngineError> {
        let mut counts = RetentionCounts::default();
        loop {
            let step = self.retain_one_step(cutoff_ms).await?;
            counts.raw_events_deleted += step.counts.raw_events_deleted;
            counts.notifications_deleted += step.counts.notifications_deleted;
            if !step.has_more {
                return Ok(counts);
            }
        }
    }

    pub async fn retain_one_step(&self, cutoff_ms: i64) -> Result<RetentionStep, EngineError> {
        let sessions = sqlx::query(
            "SELECT agent_kind,session_id FROM session_projection
              WHERE lifecycle='ended' AND last_observed_at_ms<?1
                AND NOT EXISTS (
                    SELECT 1 FROM notification_outbox o
                     WHERE o.agent_kind=session_projection.agent_kind
                       AND o.session_id=session_projection.session_id
                       AND (
                            o.status IN ('pending','inflight')
                            OR (o.status='failed' AND o.attempt_count<?2)
                       )
                )
              ORDER BY last_observed_at_ms LIMIT 4",
        )
        .bind(cutoff_ms)
        .bind(MAX_DELIVERY_ATTEMPTS)
        .fetch_all(&self.pool)
        .await?;
        let session_batch_full = sessions.len() == 4;
        let mut counts = RetentionCounts::default();
        let mut tx = self.pool.begin().await?;
        let terminal_notifications = sqlx::query(
            "DELETE FROM notification_outbox
              WHERE id IN (
                    SELECT id FROM notification_outbox
                     WHERE (
                            status IN ('sent','suppressed')
                            OR (status='failed' AND attempt_count>=?3)
                       )
                       AND created_at_ms<?1
                     ORDER BY created_at_ms,id
                     LIMIT ?2
              )",
        )
        .bind(cutoff_ms)
        .bind(i64::try_from(RETENTION_NOTIFICATION_BATCH_SIZE).unwrap_or(i64::MAX))
        .bind(MAX_DELIVERY_ATTEMPTS)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        counts.notifications_deleted += terminal_notifications;
        for row in sessions {
            let agent: String = row.get("agent_kind");
            let session: String = row.get("session_id");
            counts.raw_events_deleted +=
                sqlx::query("DELETE FROM raw_events WHERE agent_kind=?1 AND session_id=?2")
                    .bind(&agent)
                    .bind(&session)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
            counts.notifications_deleted += sqlx::query(
                "DELETE FROM notification_outbox WHERE agent_kind=?1 AND session_id=?2",
            )
            .bind(&agent)
            .bind(&session)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            sqlx::query("DELETE FROM session_projection WHERE agent_kind=?1 AND session_id=?2")
                .bind(&agent)
                .bind(&session)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(RetentionStep {
            counts,
            has_more: session_batch_full
                || terminal_notifications == RETENTION_NOTIFICATION_BATCH_SIZE,
        })
    }
}

struct NotificationInsert<'a> {
    session_id: &'a str,
    turn_id: &'a str,
    kind: NotificationKind,
    revision: u64,
    provider: ProviderKind,
    project_name: Option<&'a str>,
    created_at_ms: i64,
}

async fn insert_notification(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    notification: NotificationInsert<'_>,
) -> Result<(), sqlx::Error> {
    let kind_code = notification_kind_code(notification.kind);
    let label = notification.project_name.unwrap_or("Claude Code");
    let (title, body) = match notification.kind {
        NotificationKind::NeedsInput => (format!("{label} 需要输入"), "会话正在等待你的操作"),
        NotificationKind::Done => (format!("{label} 已完成"), "本轮任务已停止"),
        NotificationKind::Failed => (format!("{label} 执行失败"), "本轮任务异常结束"),
    };
    let revision = i64::try_from(notification.revision).unwrap_or(i64::MAX);
    let id = format!(
        "claude:{}:{revision}:{kind_code}:{}",
        notification.session_id,
        notification.provider.as_str()
    );
    sqlx::query(
        "INSERT OR IGNORE INTO notification_outbox (
            id,agent_kind,session_id,turn_id,projection_revision,kind,provider,
            status,title,body,created_at_ms,next_attempt_at_ms
         ) VALUES (?1,'claude',?2,?3,?4,?5,?6,'pending',?7,?8,?9,?9)",
    )
    .bind(id)
    .bind(notification.session_id)
    .bind(notification.turn_id)
    .bind(revision)
    .bind(kind_code)
    .bind(notification.provider.as_str())
    .bind(title)
    .bind(body)
    .bind(notification.created_at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn event_source(value: &str) -> Result<EventSource, EngineError> {
    match value {
        "hook" => Ok(EventSource::Hook),
        "transcript" => Ok(EventSource::Transcript),
        "recovery" => Ok(EventSource::Recovery),
        _ => Err(EngineError::InvalidProjection),
    }
}

fn source_code(value: EventSource) -> &'static str {
    match value {
        EventSource::Hook => "hook",
        EventSource::Transcript => "transcript",
        EventSource::Recovery => "recovery",
    }
}

fn lifecycle_code(value: SessionLifecycle) -> &'static str {
    match value {
        SessionLifecycle::Active => "active",
        SessionLifecycle::Ended => "ended",
    }
}

fn turn_state_code(value: TurnState) -> &'static str {
    match value {
        TurnState::Running => "running",
        TurnState::Waiting => "waiting",
        TurnState::NeedsInput => "needs_input",
        TurnState::Failed => "failed",
    }
}

fn confidence_code(value: Confidence) -> &'static str {
    match value {
        Confidence::Definitive => "definitive",
        Confidence::Inferred => "inferred",
    }
}

fn notification_kind_code(value: NotificationKind) -> &'static str {
    match value {
        NotificationKind::NeedsInput => "needs_input",
        NotificationKind::Done => "done",
        NotificationKind::Failed => "failed",
    }
}

fn error_code(error: &EngineError) -> &'static str {
    match error {
        EngineError::InvalidEvent(_) => "engine_invalid_event",
        EngineError::InvalidProjection => "engine_invalid_projection",
        EngineError::Storage(_) => "engine_storage_failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> (tempfile::TempDir, SqlitePool) {
        let directory = tempfile::tempdir().unwrap();
        let pool = monitor_storage::connect(&directory.path().join("state.db"))
            .await
            .unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        (directory, pool)
    }

    async fn insert_stop_evidence(pool: &SqlitePool, notifications_allowed: bool) {
        for (id, source_event, at) in [("prompt", "UserPromptSubmit", 1), ("stop", "Stop", 2)] {
            sqlx::query(
                "INSERT INTO raw_events (
                    id,agent_kind,session_id,source,source_event,occurred_at_ms,
                    received_at_ms,dedupe_key,payload_json,notifications_allowed
                 ) VALUES (?1,'claude','session','hook',?2,?3,?3,?1,'{}',?4)",
            )
            .bind(id)
            .bind(source_event)
            .bind(at)
            .bind(notifications_allowed)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn live_stop_projects_waiting_and_enqueues_one_notification() {
        let (_directory, pool) = pool().await;
        insert_stop_evidence(&pool, true).await;
        Engine::new(pool.clone())
            .process_session(
                AgentKind::claude(),
                &SessionId("session".into()),
                &[ProviderKind::Desktop],
                3,
            )
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT turn_state FROM session_projection WHERE session_id='session'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "waiting"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notification_outbox")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn historical_evidence_projects_state_without_notification_intent() {
        let (_directory, pool) = pool().await;
        insert_stop_evidence(&pool, false).await;
        Engine::new(pool.clone())
            .process_session(
                AgentKind::claude(),
                &SessionId("session".into()),
                &[ProviderKind::Desktop],
                3,
            )
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notification_outbox")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }

    struct FailingProvider;

    #[async_trait::async_trait]
    impl NotificationProvider for FailingProvider {
        async fn send(&self, _: &Notification) -> Result<(), monitor_notify::NotifyError> {
            Err(monitor_notify::NotifyError::Delivery("delivery_failed"))
        }
    }

    struct SuccessfulProvider;

    #[async_trait::async_trait]
    impl NotificationProvider for SuccessfulProvider {
        async fn send(&self, _: &Notification) -> Result<(), monitor_notify::NotifyError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn delivery_failure_retries_and_success_recovers_provider_health() {
        let (_directory, pool) = pool().await;
        insert_stop_evidence(&pool, true).await;
        let engine = Engine::new(pool.clone());
        engine
            .process_session(
                AgentKind::claude(),
                &SessionId("session".into()),
                &[ProviderKind::Desktop],
                3,
            )
            .await
            .unwrap();

        assert!(engine
            .dispatch_one(ProviderKind::Desktop, &FailingProvider, 10)
            .await
            .unwrap());
        assert_eq!(
            sqlx::query_as::<_, (String, i64)>(
                "SELECT status,attempt_count FROM notification_outbox"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            ("failed".into(), 1)
        );

        assert!(engine
            .dispatch_one(ProviderKind::Desktop, &SuccessfulProvider, 5_010)
            .await
            .unwrap());
        assert_eq!(
            sqlx::query_as::<_, (String, i64, Option<String>)>(
                "SELECT o.status,h.consecutive_failures,h.last_error
                   FROM notification_outbox o
                   JOIN notification_provider_health h ON h.provider=o.provider"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            ("sent".into(), 0, None)
        );
    }

    #[tokio::test]
    async fn retention_removes_old_terminal_notifications_without_removing_active_evidence() {
        let (_directory, pool) = pool().await;
        insert_stop_evidence(&pool, true).await;
        let engine = Engine::new(pool.clone());
        engine
            .process_session(
                AgentKind::claude(),
                &SessionId("session".into()),
                &[ProviderKind::Desktop],
                3,
            )
            .await
            .unwrap();
        sqlx::query(
            "UPDATE notification_outbox
                SET status='failed',attempt_count=3,created_at_ms=5",
        )
        .execute(&pool)
        .await
        .unwrap();

        let step = engine.retain_one_step(10).await.unwrap();
        assert_eq!(step.counts.notifications_deleted, 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notification_outbox")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM raw_events")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn retention_preserves_retryable_notification_and_its_session_evidence() {
        let (_directory, pool) = pool().await;
        insert_stop_evidence(&pool, true).await;
        let engine = Engine::new(pool.clone());
        engine
            .process_session(
                AgentKind::claude(),
                &SessionId("session".into()),
                &[ProviderKind::Desktop],
                3,
            )
            .await
            .unwrap();
        sqlx::query(
            "UPDATE session_projection
                SET lifecycle='ended',last_observed_at_ms=1
              WHERE session_id='session'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE notification_outbox
                SET status='failed',attempt_count=1,created_at_ms=5",
        )
        .execute(&pool)
        .await
        .unwrap();

        let step = engine.retain_one_step(10).await.unwrap();

        assert_eq!(step.counts, RetentionCounts::default());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notification_outbox")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM raw_events")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM session_projection")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }
}
