//! Tauri-independent projection, delivery, and retention pipeline.

pub use monitor_domain as domain;
pub use monitor_storage as storage;

use monitor_domain::{
    reduce_at, AgentEvent, AgentKind, Confidence, EventId, EventSource, NotificationKind,
    SessionId, SessionLifecycle, TurnState,
};
use monitor_notify::{diagnostic_code, Notification, NotificationProvider, Priority};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool};
use std::collections::{BTreeSet, HashMap, HashSet};

const SESSION_BATCH_SIZE: i64 = 8;
pub const MAX_DELIVERY_ATTEMPTS: i64 = 3;
const RETENTION_NOTIFICATION_BATCH_SIZE: u64 = 100;
// Only the desktop process owns projection reduction. All Engine handles in
// that process share this lock, so transcript publication and the ordinary
// worker cannot commit stale snapshots over one another while the Hook remains
// free to append evidence through its separate short-lived process.
static REDUCER_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

    pub fn quarantined(&self) -> usize {
        self.quarantined.len()
    }

    pub fn quarantined_session_ids(&self) -> impl Iterator<Item = &str> {
        self.quarantined.iter().map(|key| key.session_id.0.as_str())
    }

    pub fn changed(&self) -> usize {
        self.succeeded.len() + self.quarantined.len()
    }

    pub fn has_more(&self) -> bool {
        self.has_more
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
                Err(_) => {
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
        self.process_session_inner(agent, session_id, providers, processed_at_ms, None)
            .await
    }

    async fn process_session_inner(
        &self,
        agent: AgentKind,
        session_id: &SessionId,
        providers: &[ProviderKind],
        processed_at_ms: i64,
        pause_after_snapshot: Option<(&tokio::sync::Barrier, &tokio::sync::Barrier)>,
    ) -> Result<(), EngineError> {
        let _reducer_guard = REDUCER_LOCK.lock().await;
        let rows = sqlx::query(
            "SELECT id,agent_kind,session_id,source,source_event,occurred_at_ms,
                    received_at_ms,sequence_no,dedupe_key,payload_version,payload_json,
                    transcript_path,notifications_allowed,processed_at_ms,process_error
               FROM raw_events
              WHERE agent_kind=?1 AND session_id=?2 AND process_error IS NULL
              ORDER BY occurred_at_ms,received_at_ms,dedupe_key",
        )
        .bind(&agent.0)
        .bind(&session_id.0)
        .fetch_all(&self.pool)
        .await?;
        if let Some((snapshot_loaded, resume)) = pause_after_snapshot {
            snapshot_loaded.wait().await;
            resume.wait().await;
        }
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
        let event_ids = rows
            .iter()
            .map(|row| row.get::<String, _>("id"))
            .collect::<Vec<_>>();
        let pending_event_ids = rows
            .iter()
            .filter(|row| {
                row.get::<Option<i64>, _>("processed_at_ms").is_none()
                    && row.get::<Option<String>, _>("process_error").is_none()
            })
            .map(|row| row.get::<String, _>("id"))
            .collect::<HashSet<_>>();
        let mut notifications_allowed = HashMap::new();
        let mut events = Vec::with_capacity(rows.len());
        let mut project_name = None;
        let mut cwd = None;
        let mut transcript_path: Option<String> = None;
        let mut client_bundle_id = None;
        for row in rows {
            let id: String = row.get("id");
            let payload: serde_json::Value =
                match serde_json::from_str(row.get::<&str, _>("payload_json")) {
                    Ok(payload) => payload,
                    Err(source) => {
                        let error = EngineError::InvalidEvent(source);
                        self.quarantine_events(
                            std::slice::from_ref(&id),
                            error_code(&error),
                            processed_at_ms,
                        )
                        .await?;
                        return Err(error);
                    }
                };
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
            notifications_allowed
                .insert(id.clone(), row.get::<i64, _>("notifications_allowed") != 0);
            if row.get::<&str, _>("source") == "transcript" {
                transcript_path = row.get("transcript_path");
            }
            let source = match event_source(row.get("source")) {
                Ok(source) => source,
                Err(error) => {
                    self.quarantine_events(
                        std::slice::from_ref(&id),
                        error_code(&error),
                        processed_at_ms,
                    )
                    .await?;
                    return Err(error);
                }
            };
            events.push(AgentEvent {
                id: EventId(id),
                agent_kind: AgentKind(row.get("agent_kind")),
                session_id: SessionId(row.get("session_id")),
                source,
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
        let Some(projection) = reduction.projection else {
            let error = EngineError::InvalidProjection;
            self.quarantine_events(&event_ids, error_code(&error), processed_at_ms)
                .await?;
            return Err(error);
        };
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
            if !pending_event_ids.contains(&edge.triggering_event_id.0)
                || !notifications_allowed
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
                        triggering_event_id: &edge.triggering_event_id.0,
                        provider: *provider,
                        project_name: project_name.as_deref(),
                        created_at_ms: processed_at_ms,
                    },
                )
                .await?;
            }
        }
        update_event_snapshot(&mut tx, &event_ids, processed_at_ms, None).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn quarantine_events(
        &self,
        event_ids: &[String],
        code: &'static str,
        processed_at_ms: i64,
    ) -> Result<(), EngineError> {
        let mut tx = self.pool.begin().await?;
        for event_ids in event_ids.chunks(500) {
            let mut query = QueryBuilder::<Sqlite>::new(
                "UPDATE raw_events SET processed_at_ms=COALESCE(processed_at_ms,",
            );
            query.push_bind(processed_at_ms);
            query.push("),process_error=");
            query.push_bind(code);
            query.push(" WHERE id IN (");
            {
                let mut ids = query.separated(",");
                for id in event_ids {
                    ids.push_bind(id);
                }
            }
            query.push(")");
            query.build().execute(&mut *tx).await?;
        }
        tx.commit().await?;
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
        let mut tx = self.pool.begin().await?;
        mark_sent_in_transaction(&mut tx, id, sent_at_ms).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn mark_provider_healthy(
        &self,
        provider: ProviderKind,
        observed_at_ms: i64,
    ) -> Result<(), EngineError> {
        let mut tx = self.pool.begin().await?;
        mark_provider_healthy_in_transaction(&mut tx, provider, observed_at_ms).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn mark_delivery_succeeded(
        &self,
        item: &OutboxItem,
        observed_at_ms: i64,
    ) -> Result<(), EngineError> {
        let mut tx = self.pool.begin().await?;
        mark_sent_in_transaction(&mut tx, &item.id, observed_at_ms).await?;
        mark_provider_healthy_in_transaction(&mut tx, item.provider, observed_at_ms).await?;
        tx.commit().await?;
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
            Ok(()) => self.mark_delivery_succeeded(item, observed_at_ms).await,
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
        // Retention replaces the same session-owned rows as the reducer. Sharing
        // its lock prevents a reducer snapshot loaded before deletion from
        // recreating a projection after retention commits.
        let _reducer_guard = REDUCER_LOCK.lock().await;
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
        // The first DELETE upgrades this deferred transaction to the single
        // SQLite writer before eligible sessions are selected. A Hook event
        // committed before that point is visible to the pending-event guard;
        // one arriving afterwards cannot be inserted until this transaction
        // commits, so it cannot be caught by the broad evidence cleanup below.
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
                AND NOT EXISTS (
                    SELECT 1 FROM raw_events e
                     WHERE e.agent_kind=session_projection.agent_kind
                       AND e.session_id=session_projection.session_id
                       AND e.processed_at_ms IS NULL
                )
              ORDER BY last_observed_at_ms LIMIT 4",
        )
        .bind(cutoff_ms)
        .bind(MAX_DELIVERY_ATTEMPTS)
        .fetch_all(&mut *tx)
        .await?;
        let session_batch_full = sessions.len() == 4;
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

async fn update_event_snapshot(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    event_ids: &[String],
    processed_at_ms: i64,
    process_error: Option<&str>,
) -> Result<(), sqlx::Error> {
    if event_ids.is_empty() {
        return Ok(());
    }
    for event_ids in event_ids.chunks(500) {
        let mut query = QueryBuilder::<Sqlite>::new("UPDATE raw_events SET processed_at_ms=");
        query.push_bind(processed_at_ms);
        query.push(",process_error=");
        query.push_bind(process_error);
        query.push(" WHERE processed_at_ms IS NULL AND id IN (");
        {
            let mut ids = query.separated(",");
            for id in event_ids {
                ids.push_bind(id);
            }
        }
        query.push(")");
        query.build().execute(&mut **tx).await?;
    }
    Ok(())
}

async fn mark_sent_in_transaction(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    id: &str,
    sent_at_ms: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE notification_outbox
            SET status='sent',sent_at_ms=?2,next_attempt_at_ms=NULL,last_error=NULL
          WHERE id=?1 AND status='inflight'",
    )
    .bind(id)
    .bind(sent_at_ms)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn mark_provider_healthy_in_transaction(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    provider: ProviderKind,
    observed_at_ms: i64,
) -> Result<(), sqlx::Error> {
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
    .execute(&mut **tx)
    .await?;
    Ok(())
}

struct NotificationInsert<'a> {
    session_id: &'a str,
    turn_id: &'a str,
    kind: NotificationKind,
    revision: u64,
    triggering_event_id: &'a str,
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
        "claude:{}:{kind_code}:{}",
        notification.triggering_event_id,
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
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT id FROM notification_outbox")
                .fetch_one(&pool)
                .await
                .unwrap(),
            "claude:stop:done:desktop",
            "notification identity must follow the triggering event, not a replay revision",
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

    #[tokio::test]
    async fn historical_insertion_does_not_replay_a_processed_notification_edge() {
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
            "INSERT INTO raw_events (
                id,agent_kind,session_id,source,source_event,occurred_at_ms,
                received_at_ms,dedupe_key,payload_json,transcript_path,
                notifications_allowed
             ) VALUES (
                'historical','claude','session','transcript',
                'TranscriptAssistantThinking',0,4,'historical','{}',
                '/fixture/session.jsonl',0
             )",
        )
        .execute(&pool)
        .await
        .unwrap();

        engine
            .process_session(
                AgentKind::claude(),
                &SessionId("session".into()),
                &[ProviderKind::Desktop],
                5,
            )
            .await
            .unwrap();

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM session_projection")
                .fetch_one(&pool)
                .await
                .unwrap(),
            3,
            "historical evidence still participates in projection replay",
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notification_outbox")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1,
            "the previously processed Stop edge must not be enqueued at its shifted revision",
        );
    }

    #[tokio::test]
    async fn provider_change_does_not_backfill_processed_notification_edges() {
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

        engine
            .process_session(
                AgentKind::claude(),
                &SessionId("session".into()),
                &[ProviderKind::Desktop, ProviderKind::Ntfy],
                4,
            )
            .await
            .unwrap();

        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM notification_outbox WHERE provider='desktop'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1,
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM notification_outbox WHERE provider='ntfy'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0,
        );
    }

    #[tokio::test]
    async fn deleted_outbox_history_is_not_recreated_without_a_new_trigger() {
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
        sqlx::query("DELETE FROM notification_outbox")
            .execute(&pool)
            .await
            .unwrap();

        engine
            .process_session(
                AgentKind::claude(),
                &SessionId("session".into()),
                &[ProviderKind::Desktop],
                4,
            )
            .await
            .unwrap();

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notification_outbox")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0,
        );
    }

    #[tokio::test]
    async fn concurrent_projection_commit_cannot_be_overwritten_by_an_older_snapshot() {
        let (_directory, pool) = pool().await;
        sqlx::query(
            "INSERT INTO raw_events (
                id,agent_kind,session_id,source,source_event,occurred_at_ms,
                received_at_ms,dedupe_key,payload_json,notifications_allowed
             ) VALUES (
                'prompt','claude','session','hook','UserPromptSubmit',1,1,
                'prompt','{}',0
             )",
        )
        .execute(&pool)
        .await
        .unwrap();

        let snapshot_loaded = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let resume_snapshot = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let older_pool = pool.clone();
        let older_loaded = snapshot_loaded.clone();
        let older_resume = resume_snapshot.clone();
        let older = tokio::spawn(async move {
            Engine::new(older_pool)
                .process_session_inner(
                    AgentKind::claude(),
                    &SessionId("session".into()),
                    &[],
                    3,
                    Some((&older_loaded, &older_resume)),
                )
                .await
        });
        snapshot_loaded.wait().await;

        // Publish newer evidence after the first reducer loaded its snapshot.
        // The Hook and transcript writers do not take REDUCER_LOCK, so evidence
        // collection remains unblocked while projection reducers are serialized.
        sqlx::query(
            "INSERT INTO raw_events (
                id,agent_kind,session_id,source,source_event,occurred_at_ms,
                received_at_ms,dedupe_key,payload_json,notifications_allowed
             ) VALUES ('stop','claude','session','hook','Stop',2,2,'stop','{}',0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let newer_pool = pool.clone();
        let mut newer = tokio::spawn(async move {
            Engine::new(newer_pool)
                .process_session(AgentKind::claude(), &SessionId("session".into()), &[], 3)
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut newer)
                .await
                .is_err(),
            "a newer reducer must wait until the older snapshot commits"
        );

        resume_snapshot.wait().await;
        older.await.unwrap().unwrap();
        newer.await.unwrap().unwrap();

        assert_eq!(
            sqlx::query_as::<_, (String, i64)>(
                "SELECT turn_state,revision FROM session_projection WHERE session_id='session'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            ("waiting".into(), 2),
            "projection must include evidence committed before its serialized snapshot",
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<i64>>(
                "SELECT processed_at_ms FROM raw_events WHERE id='stop'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            Some(3),
        );
        assert!(Engine::new(pool.clone())
            .pending_sessions()
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn event_arriving_after_projection_snapshot_remains_pending() {
        let (_directory, pool) = pool().await;
        insert_stop_evidence(&pool, false).await;
        sqlx::query(
            "CREATE TRIGGER insert_event_after_projection
             AFTER INSERT ON session_projection
             BEGIN
                 INSERT INTO raw_events (
                     id,agent_kind,session_id,source,source_event,occurred_at_ms,
                     received_at_ms,dedupe_key,payload_json,notifications_allowed
                 ) VALUES (
                     'late','claude','session','hook','UserPromptSubmit',4,4,
                     'late','{}',1
                 );
             END",
        )
        .execute(&pool)
        .await
        .unwrap();

        let engine = Engine::new(pool.clone());
        engine
            .process_session(AgentKind::claude(), &SessionId("session".into()), &[], 3)
            .await
            .unwrap();

        assert_eq!(
            sqlx::query_scalar::<_, Option<i64>>(
                "SELECT processed_at_ms FROM raw_events WHERE id='late'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            None,
            "an event outside the reducer snapshot must remain pending",
        );
        assert_eq!(
            engine.pending_sessions().await.unwrap(),
            vec![(AgentKind::claude(), SessionId("session".into()))],
        );
    }

    #[tokio::test]
    async fn quarantined_bad_row_does_not_poison_later_valid_evidence() {
        let (_directory, pool) = pool().await;
        let mut connection = pool.acquire().await.unwrap();
        sqlx::query("PRAGMA ignore_check_constraints=ON")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO raw_events (
                id,agent_kind,session_id,source,source_event,occurred_at_ms,
                received_at_ms,dedupe_key,payload_json,notifications_allowed
             ) VALUES
                ('bad','claude','session','hook','UserPromptSubmit',1,1,
                 'bad','{',0),
                ('valid','claude','session','hook','UserPromptSubmit',2,2,
                 'valid','{}',0)",
        )
        .execute(&mut *connection)
        .await
        .unwrap();
        drop(connection);

        let engine = Engine::new(pool.clone());
        assert!(matches!(
            engine
                .process_session(AgentKind::claude(), &SessionId("session".into()), &[], 3)
                .await,
            Err(EngineError::InvalidEvent(_)),
        ));
        assert_eq!(
            sqlx::query_as::<_, (Option<i64>, Option<String>)>(
                "SELECT processed_at_ms,process_error FROM raw_events WHERE id='bad'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            (Some(3), Some("engine_invalid_event".into())),
        );
        assert_eq!(
            sqlx::query_as::<_, (Option<i64>, Option<String>)>(
                "SELECT processed_at_ms,process_error FROM raw_events WHERE id='valid'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            (None, None),
            "a sibling valid event must remain pending rather than being quarantined",
        );

        engine
            .process_session(AgentKind::claude(), &SessionId("session".into()), &[], 4)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_as::<_, (String, i64)>(
                "SELECT turn_state,revision FROM session_projection WHERE session_id='session'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            ("running".into(), 1),
        );
        assert_eq!(
            sqlx::query_as::<_, (Option<i64>, Option<String>)>(
                "SELECT processed_at_ms,process_error FROM raw_events WHERE id='valid'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            (Some(4), None),
        );
        assert!(engine.pending_sessions().await.unwrap().is_empty());
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
    async fn successful_delivery_state_rolls_back_when_health_update_fails() {
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
            "CREATE TRIGGER reject_provider_health
             BEFORE INSERT ON notification_provider_health
             BEGIN
                 SELECT RAISE(FAIL, 'injected health failure');
             END",
        )
        .execute(&pool)
        .await
        .unwrap();

        let error = engine
            .dispatch_one(ProviderKind::Desktop, &SuccessfulProvider, 10)
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Storage(_)));
        assert_eq!(
            sqlx::query_as::<_, (String, Option<i64>)>(
                "SELECT status,sent_at_ms FROM notification_outbox"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            ("inflight".into(), None),
            "outbox sent state and provider health must commit atomically",
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM notification_provider_health")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0,
        );
    }

    #[tokio::test]
    async fn retention_cannot_delete_beneath_an_inflight_reducer_snapshot() {
        let (_directory, pool) = pool().await;
        for (id, source_event, at) in [("prompt", "UserPromptSubmit", 1), ("end", "SessionEnd", 2)]
        {
            sqlx::query(
                "INSERT INTO raw_events (
                    id,agent_kind,session_id,source,source_event,occurred_at_ms,
                    received_at_ms,dedupe_key,payload_json,notifications_allowed
                 ) VALUES (?1,'claude','session','hook',?2,?3,?3,?1,'{}',0)",
            )
            .bind(id)
            .bind(source_event)
            .bind(at)
            .execute(&pool)
            .await
            .unwrap();
        }
        Engine::new(pool.clone())
            .process_session(AgentKind::claude(), &SessionId("session".into()), &[], 3)
            .await
            .unwrap();

        let snapshot_loaded = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let resume_snapshot = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let reducer_pool = pool.clone();
        let reducer_loaded = snapshot_loaded.clone();
        let reducer_resume = resume_snapshot.clone();
        let reducer = tokio::spawn(async move {
            Engine::new(reducer_pool)
                .process_session_inner(
                    AgentKind::claude(),
                    &SessionId("session".into()),
                    &[],
                    4,
                    Some((&reducer_loaded, &reducer_resume)),
                )
                .await
        });
        snapshot_loaded.wait().await;

        let retention_pool = pool.clone();
        let mut retention =
            tokio::spawn(async move { Engine::new(retention_pool).retain_one_step(10).await });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut retention)
                .await
                .is_err(),
            "retention must wait until the reducer has published its snapshot",
        );

        resume_snapshot.wait().await;
        reducer.await.unwrap().unwrap();
        let step = retention.await.unwrap().unwrap();
        assert_eq!(step.counts.raw_events_deleted, 2);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM session_projection")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0,
            "retention must not leave a projection resurrected from deleted evidence",
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM raw_events")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0,
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

    #[tokio::test]
    async fn retention_rechecks_a_session_that_resumes_after_candidate_selection() {
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
            "UPDATE notification_outbox SET status='sent',created_at_ms=5
              WHERE session_id='session'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER resume_during_retention
             AFTER DELETE ON notification_outbox
             WHEN OLD.session_id='session'
             BEGIN
                 UPDATE session_projection
                    SET lifecycle='active',turn_state='running',last_observed_at_ms=20
                  WHERE agent_kind='claude' AND session_id='session';
                 INSERT INTO raw_events (
                     id,agent_kind,session_id,source,source_event,occurred_at_ms,
                     received_at_ms,dedupe_key,payload_json,notifications_allowed
                 ) VALUES (
                     'resumed','claude','session','hook','UserPromptSubmit',20,20,
                     'resumed','{}',1
                 );
             END",
        )
        .execute(&pool)
        .await
        .unwrap();

        engine.retain_one_step(10).await.unwrap();

        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT lifecycle FROM session_projection WHERE session_id='session'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            "active",
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<i64>>(
                "SELECT processed_at_ms FROM raw_events WHERE id='resumed'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            None,
        );
    }
}
