use super::{
    now_ms, BackgroundHealth, Counts, DashboardSnapshot, Diagnostics, HistoryEvent, IndexProgress,
    ModelUsage, SessionDetail, SessionRow, TraySnapshot, TrendDay, UsageSummary, ACTIVE_WINDOW_MS,
    DASHBOARD_SESSION_LIMIT, TRAY_SESSION_LIMIT,
};
use crate::hook_lifecycle::{HookLifecycleState, HookOwnershipRecord};
use chrono::{Duration as ChronoDuration, Local};
use futures_util::TryStreamExt;
use monitor_engine::MAX_DELIVERY_ATTEMPTS;
use monitor_notify::diagnostic_code;
use monitor_storage::UsageTotals;
use sqlx::{Row, SqliteConnection, SqlitePool};
use std::collections::BTreeMap;

const ACTIVE_SESSION_WINDOW: &str =
    "p.lifecycle='active' AND p.last_observed_at_ms >= ?1 AND p.last_observed_at_ms <= ?2";

const SESSION_SUMMARY_SELECT: &str = "
    SELECT p.session_id,p.project_name,p.turn_state,p.state_reason,
           p.state_source,p.changed_at_ms,p.last_observed_at_ms,p.revision,
           (SELECT json_extract(h.payload_json,'$.cwd')
              FROM raw_events h
             WHERE h.agent_kind=p.agent_kind AND h.session_id=p.session_id
               AND h.source='hook' AND json_extract(h.payload_json,'$.cwd') IS NOT NULL
             ORDER BY h.received_at_ms DESC,h.id DESC LIMIT 1) latest_cwd,
           (SELECT t.transcript_path
              FROM raw_events t
             WHERE t.agent_kind=p.agent_kind AND t.session_id=p.session_id
               AND t.source='transcript' AND t.transcript_path IS NOT NULL
             ORDER BY t.received_at_ms DESC,t.id DESC LIMIT 1) latest_transcript_path
      FROM session_projection p";

pub(super) async fn snapshot<F>(
    pool: &SqlitePool,
    sample_revision: F,
    hook_lifecycle: &HookLifecycleState,
) -> anyhow::Result<DashboardSnapshot>
where
    F: FnOnce() -> u64,
{
    snapshot_impl(pool, sample_revision, Some(hook_lifecycle)).await
}

pub(super) async fn tray_snapshot(pool: &SqlitePool) -> anyhow::Result<TraySnapshot> {
    let mut transaction = pool.begin().await?;
    let value = tray_snapshot_in_transaction(&mut transaction).await?;
    transaction.commit().await?;
    Ok(value)
}

async fn tray_snapshot_in_transaction(
    connection: &mut SqliteConnection,
) -> anyhow::Result<TraySnapshot> {
    let now = now_ms();
    let cutoff = now - ACTIVE_WINDOW_MS;
    let day = Local::now().date_naive();
    let (counts, _) = active_session_counts(connection, cutoff, now).await?;
    let rows = sqlx::query(&format!(
        "{SESSION_SUMMARY_SELECT}
         WHERE p.agent_kind='claude' AND {ACTIVE_SESSION_WINDOW}
         ORDER BY p.last_observed_at_ms DESC,p.session_id
         LIMIT ?3"
    ))
    .bind(cutoff)
    .bind(now)
    .bind(TRAY_SESSION_LIMIT)
    .fetch_all(&mut *connection)
    .await?;
    let mut sessions = Vec::new();
    for row in rows {
        let (mut session, labels) = session_from_database_row(&row);
        session.project_name = resolve_project_name(labels);
        sessions.push(session);
    }
    let mut by_session = BTreeMap::new();
    let usage_sql = format!(
        "WITH visible_sessions AS (
             SELECT p.agent_kind,p.session_id
               FROM session_projection p
              WHERE p.agent_kind='claude' AND {ACTIVE_SESSION_WINDOW}
              ORDER BY p.last_observed_at_ms DESC,p.session_id
              LIMIT ?3
         )
         SELECT u.session_id,
                u.input_tokens,u.output_tokens,u.cache_write_tokens,
                u.cache_read_tokens,u.cost_pico_usd,u.cost_known
           FROM visible_sessions p
           JOIN usage_records u
             ON u.agent_kind=p.agent_kind AND u.session_id=p.session_id"
    );
    let mut usage_rows = sqlx::query(&usage_sql)
        .bind(cutoff)
        .bind(now)
        .bind(TRAY_SESSION_LIMIT)
        .fetch(&mut *connection);
    while let Some(row) = usage_rows.try_next().await? {
        add_usage(
            &mut by_session,
            row.get("session_id"),
            usage_from_record_row(&row),
        );
    }
    drop(usage_rows);
    for session in &mut sessions {
        session.usage = usage_summary(
            by_session
                .get(&session.session_id)
                .copied()
                .unwrap_or_else(empty_usage),
        );
    }
    let rows = sqlx::query(
        "SELECT local_day,input_tokens,output_tokens,cache_write_tokens,
                cache_read_tokens,cost_pico_usd,cost_known
           FROM usage_records
          WHERE local_day >= ?1 AND local_day <= ?2",
    )
    .bind((day - ChronoDuration::days(29)).to_string())
    .bind(day.to_string())
    .fetch_all(&mut *connection)
    .await?;
    let mut days = BTreeMap::new();
    for row in &rows {
        add_usage(&mut days, row.get("local_day"), usage_from_record_row(row));
    }
    let today = usage_summary(
        days.get(&day.to_string())
            .copied()
            .unwrap_or_else(empty_usage),
    );
    let trends = days
        .into_iter()
        .map(|(day, usage)| TrendDay {
            day,
            tokens: usage.tokens(),
            cost_pico_usd: usage.cost_pico_usd,
            cost_known: usage.cost_known,
            unpriced_tokens: usage.unpriced_tokens,
        })
        .collect();
    Ok(TraySnapshot {
        counts,
        today,
        trends,
        sessions,
    })
}

#[cfg(test)]
pub(super) async fn snapshot_for_test<F>(
    pool: &SqlitePool,
    sample_revision: F,
) -> anyhow::Result<DashboardSnapshot>
where
    F: FnOnce() -> u64,
{
    snapshot_impl(pool, sample_revision, None).await
}

async fn snapshot_impl<F>(
    pool: &SqlitePool,
    sample_revision: F,
    hook_lifecycle: Option<&HookLifecycleState>,
) -> anyhow::Result<DashboardSnapshot>
where
    F: FnOnce() -> u64,
{
    // State changes commit before they advance the in-memory revision. Sampling
    // first means the SQLite view pinned below is at least as new as the
    // advertised revision; a concurrent commit can only make the response
    // conservatively labelled, never falsely label old data as new.
    let revision = sample_revision();
    let mut transaction = pool.begin().await?;
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sqlite_master")
        .fetch_one(&mut *transaction)
        .await?;
    let hook_onboarding_disposition = crate::hook_onboarding::load(&mut transaction).await?;
    let mut value = snapshot_in_transaction(&mut transaction, revision).await?;
    value.snapshot.hook_onboarding_disposition = hook_onboarding_disposition;
    transaction.commit().await?;
    resolve_session_labels(&mut value);
    if let Some(hook_lifecycle) = hook_lifecycle {
        value.snapshot.hook = hook_lifecycle.verify_health(value.hook_ownership);
    }
    Ok(value.snapshot)
}

struct PendingSnapshot {
    snapshot: DashboardSnapshot,
    labels: Vec<SessionLabelEvidence>,
    hook_ownership: Option<HookOwnershipRecord>,
}

struct SessionLabelEvidence {
    project_name: Option<String>,
    cwd: Option<String>,
    transcript_path: Option<String>,
}

async fn snapshot_in_transaction(
    connection: &mut SqliteConnection,
    revision: u64,
) -> anyhow::Result<PendingSnapshot> {
    let read_at_ms = now_ms();
    let local_day = Local::now().date_naive();
    let cutoff = read_at_ms - ACTIVE_WINDOW_MS;
    let (counts, active_session_count) =
        active_session_counts(connection, cutoff, read_at_ms).await?;
    let rows = sqlx::query(&format!(
        "{SESSION_SUMMARY_SELECT}
         WHERE p.agent_kind='claude' AND {ACTIVE_SESSION_WINDOW}
         ORDER BY p.last_observed_at_ms DESC,p.session_id
         LIMIT ?3"
    ))
    .bind(cutoff)
    .bind(read_at_ms)
    .bind(DASHBOARD_SESSION_LIMIT)
    .fetch_all(&mut *connection)
    .await?;
    let mut pending_sessions: Vec<_> = rows.iter().map(session_from_database_row).collect();
    let usage_sql = format!(
        "SELECT u.session_id,
                u.input_tokens,u.output_tokens,u.cache_write_tokens,
                u.cache_read_tokens,u.cost_pico_usd,u.cost_known
           FROM (
                 SELECT p.agent_kind,p.session_id
                   FROM session_projection p
                  WHERE p.agent_kind='claude' AND {ACTIVE_SESSION_WINDOW}
                  ORDER BY p.last_observed_at_ms DESC,p.session_id
                  LIMIT ?3
           ) p
           JOIN usage_records u
             ON u.agent_kind=p.agent_kind AND u.session_id=p.session_id"
    );
    let mut usage_rows = sqlx::query(&usage_sql)
        .bind(cutoff)
        .bind(read_at_ms)
        .bind(DASHBOARD_SESSION_LIMIT)
        .fetch(&mut *connection);
    let mut session_usage = BTreeMap::<String, UsageTotals>::new();
    while let Some(row) = usage_rows.try_next().await? {
        add_usage(
            &mut session_usage,
            row.get("session_id"),
            usage_from_record_row(&row),
        );
    }
    drop(usage_rows);
    for (session, _) in &mut pending_sessions {
        session.usage = usage_summary(
            session_usage
                .get(&session.session_id)
                .copied()
                .unwrap_or_else(empty_usage),
        );
    }
    let sessions: Vec<_> = pending_sessions
        .iter()
        .map(|(session, _)| session.clone())
        .collect();
    let labels = pending_sessions
        .into_iter()
        .map(|(_, labels)| labels)
        .collect();
    let today = local_day.to_string();
    let range_start = local_day - ChronoDuration::days(364);
    let trend_rows = sqlx::query(
        "SELECT local_day,input_tokens,output_tokens,cache_write_tokens,
                cache_read_tokens,cost_pico_usd,cost_known
           FROM usage_records
          WHERE local_day >= ?1 AND local_day <= ?2",
    )
    // The snapshot supplies a sparse annual usage projection. Consumers that
    // need a shorter window (such as the tray's 7/30-day summaries) apply
    // their own local-day cutoff to this coherent transaction result.
    .bind(range_start.to_string())
    .bind(&today)
    .fetch_all(&mut *connection)
    .await?;
    let mut daily_usage = BTreeMap::<String, UsageTotals>::new();
    for row in &trend_rows {
        add_usage(
            &mut daily_usage,
            row.get("local_day"),
            usage_from_record_row(row),
        );
    }
    let installation = sqlx::query(
        "SELECT installation_id,hook_path,hook_version
         FROM installation WHERE singleton=1",
    )
    .fetch_optional(&mut *connection)
    .await?;
    let hook_ownership = installation.as_ref().map(|row| HookOwnershipRecord {
        installation_id: row.get("installation_id"),
        hook_path: row.get("hook_path"),
        version: row.get("hook_version"),
    });
    let migration_version: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(version),0) FROM _sqlx_migrations")
            .fetch_one(&mut *connection)
            .await?;
    let pending_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM raw_events
         WHERE processed_at_ms IS NULL AND process_error IS NULL",
    )
    .fetch_one(&mut *connection)
    .await?;
    let quarantined_sessions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (
             SELECT DISTINCT agent_kind, session_id FROM raw_events
             WHERE process_error IS NOT NULL
         )",
    )
    .fetch_one(&mut *connection)
    .await?;
    let pending_notifications: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM notification_outbox
          WHERE status IN ('pending','inflight')
             OR (status='failed' AND attempt_count<?1)",
    )
    .bind(MAX_DELIVERY_ATTEMPTS)
    .fetch_one(&mut *connection)
    .await?;
    let ntfy = sqlx::query(
        "SELECT consecutive_failures,last_error,recovered_at_ms
         FROM notification_provider_health WHERE provider='ntfy'",
    )
    .fetch_optional(&mut *connection)
    .await?;
    let desktop = sqlx::query(
        "SELECT consecutive_failures,last_error
         FROM notification_provider_health WHERE provider='desktop'",
    )
    .fetch_optional(&mut *connection)
    .await?;
    let background_rows = sqlx::query(
        "SELECT task, success_count, failure_count, consecutive_failures,
                last_error_code, last_succeeded_at_ms, last_failed_at_ms,
                recovered_at_ms
         FROM background_task_health
         ORDER BY task",
    )
    .fetch_all(&mut *connection)
    .await?;
    let background_health = background_rows
        .into_iter()
        .map(|row| {
            let task: String = row.get("task");
            let stored_code: Option<String> = row.get("last_error_code");
            BackgroundHealth {
                error_code: stored_code
                    .as_deref()
                    .map(|value| super::sanitize_background_error_code(&task, value).to_owned()),
                task,
                success_count: row.get("success_count"),
                failure_count: row.get("failure_count"),
                consecutive_failures: row.get("consecutive_failures"),
                last_succeeded_at_ms: row.get("last_succeeded_at_ms"),
                last_failed_at_ms: row.get("last_failed_at_ms"),
                recovered_at_ms: row.get("recovered_at_ms"),
            }
        })
        .collect();
    Ok(PendingSnapshot {
        snapshot: DashboardSnapshot {
            revision,
            counts,
            active_session_count,
            sessions_has_more: active_session_count > sessions.len() as i64,
            today: usage_summary(daily_usage.get(&today).copied().unwrap_or_else(empty_usage)),
            trends: daily_usage
                .into_iter()
                .map(|(day, usage)| TrendDay {
                    day,
                    tokens: usage.tokens(),
                    cost_pico_usd: usage.cost_pico_usd,
                    cost_known: usage.cost_known,
                    unpriced_tokens: usage.unpriced_tokens,
                })
                .collect(),
            sessions,
            hook: crate::hook_lifecycle::HookHealth::absent(),
            hook_onboarding_disposition: None,
            index: IndexProgress::default(),
            diagnostics: Diagnostics {
                migration_version,
                pending_events,
                quarantined_sessions,
                pending_notifications,
                desktop_failures: desktop
                    .as_ref()
                    .map_or(0, |row| row.get("consecutive_failures")),
                desktop_error_code: desktop
                    .as_ref()
                    .and_then(|row| row.get::<Option<String>, _>("last_error"))
                    .as_deref()
                    .map(diagnostic_code)
                    .map(str::to_owned),
                ntfy_failures: ntfy
                    .as_ref()
                    .map_or(0, |row| row.get("consecutive_failures")),
                ntfy_error_code: ntfy
                    .as_ref()
                    .and_then(|row| row.get::<Option<String>, _>("last_error"))
                    .as_deref()
                    .map(diagnostic_code)
                    .map(str::to_owned),
                ntfy_recovered_at_ms: ntfy.as_ref().and_then(|row| row.get("recovered_at_ms")),
                background_health,
            },
        },
        labels,
        hook_ownership,
    })
}

async fn active_session_counts(
    connection: &mut SqliteConnection,
    cutoff: i64,
    now: i64,
) -> anyhow::Result<(Counts, i64)> {
    let row = sqlx::query(&format!(
        "SELECT
             COUNT(*) total,
             COALESCE(SUM(p.turn_state='running'),0) running,
             COALESCE(SUM(p.turn_state='waiting'),0) waiting,
             COALESCE(SUM(p.turn_state='needs_input'),0) needs_input,
             COALESCE(SUM(p.turn_state='failed'),0) failed
           FROM session_projection p
          WHERE p.agent_kind='claude' AND {ACTIVE_SESSION_WINDOW}"
    ))
    .bind(cutoff)
    .bind(now)
    .fetch_one(&mut *connection)
    .await?;
    Ok((
        Counts {
            running: row.get("running"),
            waiting: row.get("waiting"),
            needs_input: row.get("needs_input"),
            failed: row.get("failed"),
        },
        row.get("total"),
    ))
}

pub(super) async fn session_detail(
    pool: &SqlitePool,
    session_id: &str,
) -> anyhow::Result<SessionDetail> {
    let mut transaction = pool.begin().await?;
    let mut pending_session = session_summary(&mut transaction, session_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("session not found"))?;
    let mut model_rows = sqlx::query(
        "SELECT model_id,input_tokens,output_tokens,cache_write_tokens,
                cache_read_tokens,cost_pico_usd,cost_known
           FROM usage_records
          WHERE agent_kind='claude' AND session_id=?1",
    )
    .bind(session_id)
    .fetch(&mut *transaction);
    let mut model_usage = BTreeMap::<String, UsageTotals>::new();
    let mut session_usage = empty_usage();
    while let Some(row) = model_rows.try_next().await? {
        let usage = usage_from_record_row(&row);
        session_usage.add(usage);
        add_usage(&mut model_usage, row.get("model_id"), usage);
    }
    drop(model_rows);
    pending_session.0.usage = usage_summary(session_usage);
    let mut models: Vec<_> = model_usage
        .into_iter()
        .map(|(model_id, usage)| ModelUsage {
            model_id,
            tokens: usage.tokens(),
            cost_pico_usd: usage.cost_pico_usd,
            cost_known: usage.cost_known,
            unpriced_tokens: usage.unpriced_tokens,
        })
        .collect();
    models.sort_by(|left, right| {
        right
            .tokens
            .cmp(&left.tokens)
            .then_with(|| left.model_id.cmp(&right.model_id))
    });
    let events = sqlx::query(
        "SELECT source_event,source,occurred_at_ms FROM raw_events
         WHERE agent_kind='claude' AND session_id=?1
         ORDER BY occurred_at_ms DESC,received_at_ms DESC,id DESC LIMIT 100",
    )
    .bind(session_id)
    .fetch_all(&mut *transaction)
    .await?
    .into_iter()
    .map(|row| HistoryEvent {
        source_event: row.get("source_event"),
        source: row.get("source"),
        occurred_at_ms: row.get("occurred_at_ms"),
    })
    .collect();
    transaction.commit().await?;
    Ok(SessionDetail {
        session: resolve_session_label(pending_session.0, pending_session.1),
        models,
        events,
    })
}

async fn session_summary(
    connection: &mut SqliteConnection,
    session_id: &str,
) -> anyhow::Result<Option<(SessionRow, SessionLabelEvidence)>> {
    let row = sqlx::query(&format!(
        "{SESSION_SUMMARY_SELECT}
         WHERE p.agent_kind='claude' AND p.session_id=?1"
    ))
    .bind(session_id)
    .fetch_optional(&mut *connection)
    .await?;
    let Some(row) = row.as_ref() else {
        return Ok(None);
    };
    Ok(Some(session_from_database_row(row)))
}

fn session_from_database_row(row: &sqlx::sqlite::SqliteRow) -> (SessionRow, SessionLabelEvidence) {
    let project_name = row.get::<Option<String>, _>("project_name");
    (
        SessionRow {
            session_id: row.get("session_id"),
            project_name: None,
            turn_state: row.get("turn_state"),
            state_source: row.get("state_source"),
            state_reason: row.get("state_reason"),
            changed_at_ms: row.get("changed_at_ms"),
            last_observed_at_ms: row.get("last_observed_at_ms"),
            revision: row.get("revision"),
            usage: usage_summary(empty_usage()),
        },
        SessionLabelEvidence {
            project_name,
            cwd: row.get("latest_cwd"),
            transcript_path: row.get("latest_transcript_path"),
        },
    )
}

fn resolve_session_labels(pending: &mut PendingSnapshot) {
    for (session, labels) in pending
        .snapshot
        .sessions
        .iter_mut()
        .zip(pending.labels.drain(..))
    {
        session.project_name = resolve_project_name(labels);
    }
}

fn resolve_session_label(mut session: SessionRow, labels: SessionLabelEvidence) -> SessionRow {
    session.project_name = resolve_project_name(labels);
    session
}

fn resolve_project_name(labels: SessionLabelEvidence) -> Option<String> {
    labels
        .project_name
        .or_else(|| labels.cwd.as_deref().and_then(path_label))
        .or_else(|| {
            labels
                .transcript_path
                .as_deref()
                .and_then(transcript_project_label)
        })
}

fn path_label(value: &str) -> Option<String> {
    std::path::Path::new(value)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub(super) fn transcript_project_label(value: &str) -> Option<String> {
    let path = std::path::Path::new(value);
    let parent = path.parent()?;
    let project = if parent.file_name().and_then(|value| value.to_str()) == Some("subagents") {
        parent.parent()?.parent()?
    } else {
        parent
    };
    let encoded = project
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())?;
    project_slug_from_encoded(encoded)
}

fn project_slug_from_encoded(encoded: &str) -> Option<String> {
    let parts: Vec<_> = encoded.split('-').filter(|part| !part.is_empty()).collect();
    let project = match parts.as_slice() {
        ["Users" | "home", _user, "code", "projects", rest @ ..]
        | ["Users" | "home", _user, "projects", rest @ ..] => rest,
        // Outside the conventional project roots, the encoded directory name
        // does not preserve component boundaries versus literal hyphens.
        // Expose only the conservative final slug instead of guessing and
        // leaking the user's volume or custom directory hierarchy.
        [.., basename] => std::slice::from_ref(basename),
        [] => return None,
    };
    (!project.is_empty()).then(|| project.join("-"))
}

fn empty_usage() -> UsageTotals {
    UsageTotals::from_values(0, 0, 0, 0, 0, true)
}

fn usage_from_record_row(row: &sqlx::sqlite::SqliteRow) -> UsageTotals {
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
fn add_usage(rows: &mut BTreeMap<String, UsageTotals>, key: String, usage: UsageTotals) {
    rows.entry(key).or_insert_with(empty_usage).add(usage);
}

fn usage_summary(usage: UsageTotals) -> UsageSummary {
    UsageSummary {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cost_pico_usd: usage.cost_pico_usd,
        cost_known: usage.cost_known,
        unpriced_tokens: usage.unpriced_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_labels_are_bounded_and_filesystem_independent() {
        let cases = [
            ("/root/-/session.jsonl", None),
            (
                "/root/-Users-me-code-projects-deep-missing-project/session.jsonl",
                Some("deep-missing-project"),
            ),
            (
                "/root/-Users-me-code-projects-你好-世界/session.jsonl",
                Some("你好-世界"),
            ),
            (
                "/root/-Users-me-code-projects-CC-Monitor/session/subagents/agent.jsonl",
                Some("CC-Monitor"),
            ),
            (
                "/root/-Volumes-Company-Secret-Team-产品/session.jsonl",
                Some("产品"),
            ),
            (
                "/root/-custom-private-hierarchy-工作台/session.jsonl",
                Some("工作台"),
            ),
        ];
        for (path, expected) in cases {
            assert_eq!(transcript_project_label(path).as_deref(), expected);
        }
    }

    #[tokio::test]
    async fn equal_receipt_times_choose_latest_source_by_descending_event_id() {
        let directory = tempfile::tempdir().unwrap();
        let pool = monitor_storage::connect(&directory.path().join("state.db"))
            .await
            .unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        let now = now_ms();
        sqlx::query(
            "INSERT INTO session_projection (
                agent_kind,session_id,lifecycle,turn_state,state_reason,
                state_source,confidence,revision,changed_at_ms,last_observed_at_ms
             ) VALUES ('claude','tie','active','waiting','stop',
                'hook','definitive',1,?1,?1)",
        )
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO session_projection (
                agent_kind,session_id,lifecycle,turn_state,state_reason,
                state_source,confidence,revision,changed_at_ms,last_observed_at_ms
             ) VALUES ('claude','tie-transcript','active','waiting','transcript_idle',
                'transcript','inferred',1,?1,?1)",
        )
        .bind(now - 1)
        .execute(&pool)
        .await
        .unwrap();
        for (id, cwd) in [("event-a", "/work/older"), ("event-z", "/work/newer")] {
            sqlx::query(
                "INSERT INTO raw_events (
                    id,agent_kind,session_id,source,source_event,occurred_at_ms,
                    received_at_ms,dedupe_key,payload_json
                 ) VALUES (?1,'claude','tie','hook','Stop',?2,?2,?1,?3)",
            )
            .bind(id)
            .bind(now)
            .bind(serde_json::json!({ "cwd": cwd }).to_string())
            .execute(&pool)
            .await
            .unwrap();
        }
        for (id, path) in [
            ("transcript-a", "/root/-custom-secret-older/session.jsonl"),
            ("transcript-z", "/root/-custom-secret-newer/session.jsonl"),
        ] {
            sqlx::query(
                "INSERT INTO raw_events (
                    id,agent_kind,session_id,source,transcript_path,source_event,
                    occurred_at_ms,received_at_ms,dedupe_key,payload_json
                 ) VALUES (?1,'claude','tie-transcript','transcript',?3,
                    'TranscriptAssistantText',?2,?2,?1,'{}')",
            )
            .bind(id)
            .bind(now)
            .bind(path)
            .execute(&pool)
            .await
            .unwrap();
        }

        let dashboard = snapshot_for_test(&pool, || 1).await.unwrap();
        assert_eq!(dashboard.sessions[0].project_name.as_deref(), Some("newer"));
        assert_eq!(
            dashboard.sessions[1].project_name.as_deref(),
            Some("newer"),
            "Transcript ties must use the same stable ID ordering"
        );
    }

    #[tokio::test]
    async fn active_read_models_are_bounded_but_counts_remain_complete() {
        let directory = tempfile::tempdir().unwrap();
        let pool = monitor_storage::connect(&directory.path().join("state.db"))
            .await
            .unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        let now = now_ms();
        let mut transaction = pool.begin().await.unwrap();
        for index in 0..105_i64 {
            sqlx::query(
                "INSERT INTO session_projection (
                    agent_kind,session_id,lifecycle,turn_state,state_reason,
                    state_source,confidence,revision,changed_at_ms,last_observed_at_ms
                 ) VALUES ('claude',?1,'active','waiting','stop',
                    'hook','definitive',1,?2,?2)",
            )
            .bind(format!("session-{index:03}"))
            .bind(now - index)
            .execute(&mut *transaction)
            .await
            .unwrap();
        }
        transaction.commit().await.unwrap();

        let dashboard = snapshot_for_test(&pool, || 1).await.unwrap();
        assert_eq!(dashboard.counts.waiting, 105);
        assert_eq!(dashboard.active_session_count, 105);
        assert!(dashboard.sessions_has_more);
        assert_eq!(dashboard.sessions.len(), DASHBOARD_SESSION_LIMIT as usize);
        assert_eq!(dashboard.sessions[0].session_id, "session-000");
        assert_eq!(dashboard.sessions[99].session_id, "session-099");

        let tray = tray_snapshot(&pool).await.unwrap();
        assert_eq!(tray.counts.waiting, 105);
        assert_eq!(tray.sessions.len(), TRAY_SESSION_LIMIT as usize);
        assert_eq!(tray.sessions[0].session_id, "session-000");
        assert_eq!(tray.sessions[7].session_id, "session-007");
    }

    #[tokio::test]
    async fn usage_read_models_saturate_in_rust_without_sqlite_sum_overflow() {
        let directory = tempfile::tempdir().unwrap();
        let pool = monitor_storage::connect(&directory.path().join("state.db"))
            .await
            .unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        let now = now_ms();
        sqlx::query(
            "INSERT INTO session_projection (
                agent_kind,session_id,lifecycle,turn_state,state_reason,
                state_source,confidence,revision,changed_at_ms,last_observed_at_ms
             ) VALUES ('claude','large','active','waiting','stop',
                'hook','definitive',1,?1,?1)",
        )
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
        let day = Local::now().date_naive().to_string();
        for (id, known) in [("usage-a", true), ("usage-b", false)] {
            sqlx::query(
                "INSERT INTO usage_records (
                    id,agent_kind,session_id,transcript_path,source_location,
                    model_id,local_day,input_tokens,output_tokens,
                    cache_write_tokens,cache_read_tokens,cost_pico_usd,
                    cost_known,dedupe_key,observed_at_ms
                 ) VALUES (?1,'claude','large','/tmp/large.jsonl',?1,
                    'large-model',?2,?3,0,0,0,?3,?4,?1,?5)",
            )
            .bind(id)
            .bind(&day)
            .bind(i64::MAX - 10)
            .bind(known)
            .bind(now)
            .execute(&pool)
            .await
            .unwrap();
        }

        let dashboard = snapshot_for_test(&pool, || 1).await.unwrap();
        assert_eq!(dashboard.today.input_tokens, i64::MAX);
        assert_eq!(dashboard.today.cost_pico_usd, i64::MAX);
        assert_eq!(dashboard.today.unpriced_tokens, i64::MAX - 10);
        assert!(!dashboard.today.cost_known);
        assert_eq!(dashboard.sessions[0].usage.input_tokens, i64::MAX);

        let detail = session_detail(&pool, "large").await.unwrap();
        assert_eq!(detail.session.usage.input_tokens, i64::MAX);
        assert_eq!(detail.models[0].tokens, i64::MAX);
    }

    #[tokio::test]
    async fn diagnostics_exclude_exhausted_failures_from_pending_notifications() {
        let directory = tempfile::tempdir().unwrap();
        let pool = monitor_storage::connect(&directory.path().join("state.db"))
            .await
            .unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        for (revision, status, attempts) in [
            (1, "pending", 0),
            (2, "inflight", 0),
            (3, "failed", 2),
            (4, "failed", 3),
        ] {
            sqlx::query(
                "INSERT INTO notification_outbox (
                    id,agent_kind,session_id,turn_id,projection_revision,
                    kind,provider,status,title,body,created_at_ms,attempt_count
                 ) VALUES (?1,'claude','session','turn',?2,'done','desktop',
                    ?3,'title','body',1,?4)",
            )
            .bind(format!("notification-{revision}"))
            .bind(revision)
            .bind(status)
            .bind(attempts)
            .execute(&pool)
            .await
            .unwrap();
        }

        let dashboard = snapshot_for_test(&pool, || 1).await.unwrap();

        assert_eq!(dashboard.diagnostics.pending_notifications, 3);
    }
}
