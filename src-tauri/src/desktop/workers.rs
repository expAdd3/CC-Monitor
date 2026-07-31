use super::{now_ms, settings::NotificationPolicy, DesktopState};
use monitor_engine::{BatchResult, Engine, OutboxItem, ProviderKind};
use monitor_notify::DesktopProvider;
use monitor_notify::NtfyProvider;
use sqlx::SqlitePool;
use std::{
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
    time::{Duration, SystemTime},
};
use tauri::{AppHandle, Manager};
use tokio::time::{Instant, MissedTickBehavior};

const RETENTION_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const RETENTION_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
// Hook events cross a process boundary through SQLite WAL writes. Polling file
// metadata is cheap and keeps the worst-case detection delay well below the
// former 250 ms database-poll cadence without opening a SQLite transaction.
const CHANGE_SIGNAL_POLL_INTERVAL: Duration = Duration::from_millis(50);
// Time-derived reconciliation and the shortest provider retry still need a
// bounded wake when no file changes. Five seconds matches the first retry
// deadline while reducing fully-idle Engine passes from 240 to 12 per minute.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(5);
const FALLBACK_DATABASE_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DatabaseStamp {
    database: Option<FileStamp>,
    wal: Option<FileStamp>,
}

struct DatabaseChangeSignal {
    database: PathBuf,
    observed: DatabaseStamp,
}

impl DatabaseChangeSignal {
    fn new(database: PathBuf) -> Self {
        let observed = database_stamp(&database);
        Self { database, observed }
    }

    fn changed(&mut self) -> bool {
        let current = database_stamp(&self.database);
        if current == self.observed {
            return false;
        }
        // Capture before the Engine pass. Writes made by that pass deliberately
        // cause one more wake, closing the race where a Hook commits after the
        // pending-session snapshot but before the worker goes back to sleep.
        self.observed = current;
        true
    }
}

fn database_stamp(database: &Path) -> DatabaseStamp {
    let mut wal = database.as_os_str().to_os_string();
    wal.push("-wal");
    DatabaseStamp {
        database: file_stamp(database),
        wal: file_stamp(Path::new(&wal)),
    }
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let metadata = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Some(FileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
    })
}

struct WorkWakeSchedule {
    next_maintenance: Instant,
}

impl WorkWakeSchedule {
    fn new(now: Instant) -> Self {
        Self {
            next_maintenance: now + MAINTENANCE_INTERVAL,
        }
    }

    fn should_run(&mut self, now: Instant, database_changed: bool) -> bool {
        if now >= self.next_maintenance {
            while self.next_maintenance <= now {
                self.next_maintenance += MAINTENANCE_INTERVAL;
            }
            return true;
        }
        database_changed
    }
}

fn initial_reindex_required(cursor_count: i64) -> bool {
    cursor_count == 0
}

pub(super) fn spawn_leader(app: AppHandle, state: Arc<DesktopState>) {
    spawn(app, state);
}

fn spawn(app: AppHandle, state: Arc<DesktopState>) {
    let index_app = app.clone();
    let index_state = state.clone();
    tauri::async_runtime::spawn(async move {
        let cursor_count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transcript_cursors")
            .fetch_one(&index_state.pool)
            .await;
        match cursor_count {
            Ok(cursor_count) => {
                if initial_reindex_required(cursor_count)
                    && super::indexing::begin_reindex(index_app, index_state).is_err()
                {
                    crate::logging::event("initial_index_start_failed");
                }
            }
            _ => crate::logging::event("initial_index_probe_failed"),
        }
    });

    match app.path().home_dir() {
        Ok(home) => {
            let scan_state = state.clone();
            let scan_app = app.clone();
            let scan_projects = home.join(".claude/projects");
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    if scan_state.quitting.load(Ordering::SeqCst) {
                        break;
                    }
                    if scan_state.index.lock().unwrap().state != "running" {
                        match super::indexing::scan_incremental(
                            scan_state.pool.clone(),
                            scan_projects.clone(),
                            scan_state.providers.clone(),
                        )
                        .await
                        {
                            Ok(changed) => {
                                let recovered = match record_background_success(
                                    &scan_state.pool,
                                    "incremental_index",
                                    now_ms(),
                                )
                                .await
                                {
                                    Ok(recovered) => recovered,
                                    Err(_) => {
                                        crate::logging::event("background_health_write_failed");
                                        false
                                    }
                                };
                                if changed {
                                    scan_state.invalidate_tray(&scan_app);
                                } else if recovered {
                                    scan_state.invalidate(&scan_app);
                                }
                            }
                            Err(error) => {
                                let code = incremental_index_error_code(&error);
                                crate::logging::event("incremental_index_failed");
                                let transitioned = record_background_failure(
                                    &scan_state.pool,
                                    "incremental_index",
                                    code,
                                    now_ms(),
                                )
                                .await
                                .unwrap_or_else(|_| {
                                    crate::logging::event("background_health_write_failed");
                                    false
                                });
                                if transitioned {
                                    scan_state.invalidate(&scan_app);
                                }
                            }
                        }
                    }
                }
            });
        }
        Err(_) => crate::logging::event("transcript_root_resolve_failed"),
    }

    let retention_pool = state.pool.clone();
    let retention_quitting = state.quitting.clone();
    let retention_state = state.clone();
    let retention_app = app.clone();
    tauri::async_runtime::spawn(run_retention_worker(
        retention_pool,
        retention_quitting,
        now_ms,
        move || retention_state.invalidate(&retention_app),
    ));

    tauri::async_runtime::spawn(async move {
        let engine = state.engine();
        let mut tray_fingerprint = String::new();
        let mut tray_gate = TrayRefreshGate::default();
        let startup = now_ms();
        let startup_health_changed = observe_engine_unit_result(
            &state.pool,
            "startup_reconciliation",
            reconcile_startup_with_policy(&engine, &state.providers, startup).await,
            startup,
        )
        .await;
        if startup_health_changed {
            state.invalidate(&app);
        }
        let mut database_change_signal = app
            .path()
            .app_data_dir()
            .ok()
            .map(|directory| DatabaseChangeSignal::new(directory.join("state.db")));
        let mut wake_schedule = WorkWakeSchedule::new(Instant::now());
        let mut first_pass = true;
        loop {
            if state.quitting.load(Ordering::SeqCst) {
                break;
            }
            if !first_pass {
                tokio::time::sleep(if database_change_signal.is_some() {
                    CHANGE_SIGNAL_POLL_INTERVAL
                } else {
                    FALLBACK_DATABASE_POLL_INTERVAL
                })
                .await;
                let database_changed = match database_change_signal.as_mut() {
                    Some(signal) => signal.changed(),
                    None => true,
                };
                if !wake_schedule.should_run(Instant::now(), database_changed) {
                    continue;
                }
            }
            let loop_now = now_ms();
            let processed = observe_engine_result(
                &state.pool,
                "engine_processing",
                process_pending_with_policy(&engine, &state.providers, loop_now).await,
                loop_now,
            )
            .await;
            let reconciled = observe_engine_result(
                &state.pool,
                "engine_reconciliation",
                engine.reconcile_stale_transcripts(loop_now).await,
                loop_now,
            )
            .await;
            let continue_immediately = processed.as_ref().map_or_else(
                |(result, _)| result.has_more(),
                |(result, _)| result.has_more(),
            ) || reconciled.as_ref().map_or_else(
                |(result, _)| result.has_more(),
                |(result, _)| result.has_more(),
            );
            let health_changed = processed.as_ref().map_or_else(
                |(_, transitioned)| *transitioned,
                |(_, transitioned)| *transitioned,
            ) || reconciled.as_ref().map_or_else(
                |(_, transitioned)| *transitioned,
                |(_, transitioned)| *transitioned,
            );
            let changed = processed.map_or_else(
                |(result, _)| result.changed(),
                |(result, _)| result.changed(),
            ) + reconciled.map_or_else(
                |(result, _)| result.changed(),
                |(result, _)| result.changed(),
            );
            let mut dashboard_changed = health_changed;
            let desktop = DesktopProvider::new(super::tray::TauriDesktopTransport(app.clone()));
            let desktop_delivery_at = now_ms();
            match engine
                .dispatch_one(ProviderKind::Desktop, &desktop, desktop_delivery_at)
                .await
            {
                Ok(true) => {
                    log_delivery_health(&state.pool, "desktop", desktop_delivery_at).await;
                    dashboard_changed = true;
                }
                Ok(false) => {}
                Err(_) => crate::logging::event("outbox_desktop_storage_failed"),
            }
            let ntfy_delivery_at = now_ms();
            match claim_ntfy_with_policy(&engine, &state.providers, ntfy_delivery_at).await {
                Ok(Some((item, provider))) => {
                    match engine
                        .dispatch_claimed(&item, &provider, ntfy_delivery_at)
                        .await
                    {
                        Ok(()) => {
                            log_delivery_health(&state.pool, "ntfy", ntfy_delivery_at).await;
                            dashboard_changed = true;
                        }
                        Err(_) => crate::logging::event("outbox_ntfy_storage_failed"),
                    }
                }
                Ok(None) => {}
                Err(_) => crate::logging::event("outbox_ntfy_storage_failed"),
            }
            if changed > 0 {
                state.invalidate_tray(&app);
            } else if dashboard_changed {
                state.invalidate(&app);
            }
            let tray_revision = state.tray_revision();
            let tray_now = now_ms();
            if tray_gate.needs_refresh(tray_revision, tray_now) {
                match super::queries::tray_snapshot(&state.pool).await {
                    Ok(value) => {
                        let next_fingerprint = super::tray::fingerprint(&value, now_ms());
                        if next_fingerprint == tray_fingerprint {
                            tray_gate.mark_refreshed(tray_revision, tray_now);
                        } else {
                            match super::tray::refresh(&app, &value) {
                                Ok(()) => {
                                    tray_fingerprint = next_fingerprint;
                                    tray_gate.mark_refreshed(tray_revision, tray_now);
                                }
                                Err(_) => crate::logging::event("tray_refresh_failed"),
                            }
                        }
                    }
                    Err(_) => crate::logging::event("tray_snapshot_failed"),
                }
            }
            // Delivery above is the fairness boundary: when another bounded
            // session batch remains, dispatch first, then continue without the
            // 50 ms change-signal sleep.
            first_pass = continue_immediately;
        }
    });
}

async fn claim_ntfy_with_policy(
    engine: &Engine,
    providers: &tokio::sync::RwLock<NotificationPolicy>,
    claimed_at_ms: i64,
) -> Result<Option<(OutboxItem, NtfyProvider)>, monitor_engine::EngineError> {
    let policy = providers.read().await;
    let Some(provider) = policy.ntfy_provider() else {
        return Ok(None);
    };
    Ok(engine
        .claim_next(ProviderKind::Ntfy, claimed_at_ms)
        .await?
        .map(|item| (item, provider)))
}

async fn reconcile_startup_with_policy(
    engine: &Engine,
    providers: &tokio::sync::RwLock<NotificationPolicy>,
    startup_at_ms: i64,
) -> Result<(), monitor_engine::EngineError> {
    reconcile_startup_with_policy_inner(engine, providers, startup_at_ms, None).await
}

async fn reconcile_startup_with_policy_inner(
    engine: &Engine,
    providers: &tokio::sync::RwLock<NotificationPolicy>,
    startup_at_ms: i64,
    pause_after_lock: Option<(&tokio::sync::Barrier, &tokio::sync::Barrier)>,
) -> Result<(), monitor_engine::EngineError> {
    let policy = providers.read().await;
    if let Some((locked, resume)) = pause_after_lock {
        locked.wait().await;
        resume.wait().await;
    }
    engine
        .reconcile_startup(startup_at_ms, policy.providers())
        .await
}

async fn process_pending_with_policy(
    engine: &Engine,
    providers: &Arc<tokio::sync::RwLock<NotificationPolicy>>,
    processed_at_ms: i64,
) -> Result<BatchResult, monitor_engine::BatchError> {
    let policy = providers.read().await;
    engine
        .process_pending(policy.providers(), processed_at_ms)
        .await
}

async fn run_retention_worker<Clock, Invalidate>(
    pool: SqlitePool,
    quitting: Arc<std::sync::atomic::AtomicBool>,
    clock: Clock,
    mut invalidate: Invalidate,
) where
    Clock: Fn() -> i64,
    Invalidate: FnMut(),
{
    let engine = Engine::new(pool.clone());
    let run_once = async |observed_at_ms: i64| {
        let (result, has_more) = match engine
            .retain_one_step(observed_at_ms.saturating_sub(RETENTION_AGE_MS))
            .await
        {
            Ok(step) => (Ok(()), step.has_more),
            Err(error) => (Err(error), false),
        };
        let observation =
            observe_engine_unit_result_durable(&pool, "retention_cleanup", result, observed_at_ms)
                .await;
        let continue_immediately =
            has_more && observation.task_succeeded && observation.health_persisted;
        (observation, continue_immediately)
    };
    let (startup, mut continue_immediately) = run_once(clock()).await;
    if startup.transitioned {
        invalidate();
    }
    let mut interval =
        tokio::time::interval_at(Instant::now() + RETENTION_INTERVAL, RETENTION_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        if continue_immediately {
            tokio::task::yield_now().await;
        } else {
            interval.tick().await;
        }
        if quitting.load(Ordering::SeqCst) {
            break;
        }
        let (periodic, has_more) = run_once(clock()).await;
        continue_immediately = has_more;
        if periodic.transitioned {
            invalidate();
        }
    }
}

#[derive(Default)]
pub(super) struct TrayRefreshGate {
    refreshed: Option<(u64, i64)>,
}

impl TrayRefreshGate {
    pub(super) fn needs_refresh(&self, revision: u64, now_ms: i64) -> bool {
        self.refreshed != Some((revision, now_ms.div_euclid(60_000)))
    }

    pub(super) fn mark_refreshed(&mut self, revision: u64, now_ms: i64) {
        self.refreshed = Some((revision, now_ms.div_euclid(60_000)));
    }
}

async fn log_delivery_health(pool: &SqlitePool, provider: &'static str, attempted_at_ms: i64) {
    let health = match sqlx::query_as::<_, (i64, Option<i64>)>(
        "SELECT consecutive_failures, recovered_at_ms
         FROM notification_provider_health WHERE provider=?1",
    )
    .bind(provider)
    .fetch_optional(pool)
    .await
    {
        Ok(health) => health.unwrap_or_default(),
        Err(_) => {
            crate::logging::event("provider_health_read_failed");
            return;
        }
    };
    let (failures, recovered_at_ms) = health;
    if failures > 0 {
        match provider {
            "desktop" => crate::logging::event("outbox_desktop_delivery_failed"),
            "ntfy" => crate::logging::event("outbox_ntfy_delivery_failed"),
            _ => crate::logging::event("outbox_delivery_failed"),
        }
    } else if recovered_at_ms == Some(attempted_at_ms) {
        match provider {
            "desktop" => crate::logging::event("outbox_desktop_recovered"),
            "ntfy" => crate::logging::event("outbox_ntfy_recovered"),
            _ => crate::logging::event("outbox_recovered"),
        }
    }
}

async fn observe_engine_result(
    pool: &SqlitePool,
    task: &'static str,
    result: Result<BatchResult, monitor_engine::BatchError>,
    observed_at_ms: i64,
) -> Result<(BatchResult, bool), (BatchResult, bool)> {
    match result {
        Ok(result) => {
            let health_result = if result.attempted() == 0 {
                recover_background_if_needed(pool, task, observed_at_ms).await
            } else {
                record_background_success(pool, task, observed_at_ms).await
            };
            let transitioned = match health_result {
                Ok(transitioned) => transitioned,
                Err(_) => {
                    crate::logging::event("background_health_write_failed");
                    false
                }
            };
            if result.quarantined() > 0 {
                crate::logging::event("engine_session_quarantined");
            }
            Ok((result, transitioned))
        }
        Err(error) => {
            let (partial, error) = error.into_parts();
            let code = engine_error_code(&error);
            match task {
                "engine_processing" => crate::logging::event("engine_processing_failed"),
                "engine_reconciliation" => crate::logging::event("engine_reconciliation_failed"),
                _ => crate::logging::event("background_task_failed"),
            }
            let transitioned = record_background_failure(pool, task, code, observed_at_ms)
                .await
                .unwrap_or_else(|_| {
                    crate::logging::event("background_health_write_failed");
                    false
                });
            Err((partial, transitioned))
        }
    }
}

async fn recover_background_if_needed(
    pool: &SqlitePool,
    task: &str,
    observed_at_ms: i64,
) -> Result<bool, sqlx::Error> {
    let unhealthy = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(
             SELECT 1 FROM background_task_health
              WHERE task=?1 AND consecutive_failures>0
         )",
    )
    .bind(task)
    .fetch_one(pool)
    .await?;
    if unhealthy {
        record_background_success(pool, task, observed_at_ms).await
    } else {
        Ok(false)
    }
}

async fn observe_engine_unit_result(
    pool: &SqlitePool,
    task: &'static str,
    result: Result<(), monitor_engine::EngineError>,
    observed_at_ms: i64,
) -> bool {
    observe_engine_unit_result_durable(pool, task, result, observed_at_ms)
        .await
        .transitioned
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UnitObservation {
    task_succeeded: bool,
    health_persisted: bool,
    transitioned: bool,
}

async fn observe_engine_unit_result_durable(
    pool: &SqlitePool,
    task: &'static str,
    result: Result<(), monitor_engine::EngineError>,
    observed_at_ms: i64,
) -> UnitObservation {
    match result {
        Ok(()) => match record_background_success(pool, task, observed_at_ms).await {
            Ok(transitioned) => UnitObservation {
                task_succeeded: true,
                health_persisted: true,
                transitioned,
            },
            Err(_) => {
                crate::logging::event("background_health_write_failed");
                UnitObservation {
                    task_succeeded: true,
                    health_persisted: false,
                    transitioned: false,
                }
            }
        },
        Err(error) => {
            match task {
                "startup_reconciliation" => crate::logging::event("startup_reconciliation_failed"),
                "retention_cleanup" => crate::logging::event("retention_cleanup_failed"),
                _ => crate::logging::event("background_task_failed"),
            }
            match record_background_failure(pool, task, engine_error_code(&error), observed_at_ms)
                .await
            {
                Ok(transitioned) => UnitObservation {
                    task_succeeded: false,
                    health_persisted: true,
                    transitioned,
                },
                Err(_) => {
                    crate::logging::event("background_health_write_failed");
                    UnitObservation {
                        task_succeeded: false,
                        health_persisted: false,
                        transitioned: false,
                    }
                }
            }
        }
    }
}

pub(super) async fn record_background_success(
    pool: &SqlitePool,
    task: &str,
    observed_at_ms: i64,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let transition_at: Option<i64> = sqlx::query_scalar(
        "INSERT INTO background_task_health (
            task, success_count, last_succeeded_at_ms
         ) VALUES (?1, 1, ?2)
         ON CONFLICT(task) DO UPDATE SET
            success_count = background_task_health.success_count + 1,
            recovered_at_ms = CASE
                WHEN background_task_health.consecutive_failures > 0
                THEN ?2 ELSE background_task_health.recovered_at_ms END,
            last_transition_at_ms = CASE
                WHEN background_task_health.consecutive_failures > 0
                THEN ?2 ELSE NULL END,
            consecutive_failures = 0,
            last_error_code = NULL,
            last_succeeded_at_ms = ?2
         RETURNING last_transition_at_ms",
    )
    .bind(task)
    .bind(observed_at_ms)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(transition_at == Some(observed_at_ms))
}

pub(super) async fn record_background_failure(
    pool: &SqlitePool,
    task: &str,
    code: &str,
    observed_at_ms: i64,
) -> Result<bool, sqlx::Error> {
    let code = super::sanitize_background_error_code(task, code);
    let mut tx = pool.begin().await?;
    let transition_at: Option<i64> = sqlx::query_scalar(
        "INSERT INTO background_task_health (
            task, failure_count, consecutive_failures,
            last_error_code, last_failed_at_ms, last_transition_at_ms
         ) VALUES (?1, 1, 1, ?2, ?3, ?3)
         ON CONFLICT(task) DO UPDATE SET
            failure_count = background_task_health.failure_count + 1,
            consecutive_failures = background_task_health.consecutive_failures + 1,
            last_transition_at_ms = CASE
                WHEN background_task_health.consecutive_failures = 0
                  OR background_task_health.last_error_code IS NOT ?2
                THEN ?3 ELSE NULL END,
            last_error_code = ?2,
            last_failed_at_ms = ?3
         RETURNING last_transition_at_ms",
    )
    .bind(task)
    .bind(code)
    .bind(observed_at_ms)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(transition_at == Some(observed_at_ms))
}

fn engine_error_code(error: &monitor_engine::EngineError) -> &'static str {
    match error {
        monitor_engine::EngineError::Storage(_) => "engine_storage_failed",
        monitor_engine::EngineError::InvalidEvent(_) => "engine_invalid_event",
        monitor_engine::EngineError::InvalidProjection => "engine_invalid_projection",
    }
}

fn incremental_index_error_code(error: &anyhow::Error) -> &'static str {
    let lower = error.to_string().to_ascii_lowercase();
    if lower.contains("database") || lower.contains("sqlite") {
        "index_storage_failed"
    } else if lower.contains("permission") || lower.contains("denied") {
        "index_permission_denied"
    } else if lower.contains("json") || lower.contains("parse") {
        "index_invalid_transcript"
    } else if lower.contains("i/o") || lower.contains("io error") || lower.contains("not found") {
        "index_io_failed"
    } else {
        "index_failed"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_index_is_needed_only_without_cursors() {
        assert!(initial_reindex_required(0));
        assert!(!initial_reindex_required(1));
    }

    #[test]
    fn work_wakes_for_database_changes_or_the_bounded_maintenance_deadline() {
        let start = Instant::now();
        let mut schedule = WorkWakeSchedule::new(start);
        assert!(!schedule.should_run(start + Duration::from_secs(1), false));
        assert!(schedule.should_run(start + Duration::from_secs(1), true));
        assert!(schedule.should_run(start + MAINTENANCE_INTERVAL, false));
        assert!(!schedule.should_run(
            start + MAINTENANCE_INTERVAL + Duration::from_millis(1),
            false
        ));
    }

    #[test]
    fn database_change_signal_observes_wal_creation() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("state.db");
        std::fs::write(&database, b"db").unwrap();
        let mut signal = DatabaseChangeSignal::new(database.clone());
        assert!(!signal.changed());
        let mut wal = database.into_os_string();
        wal.push("-wal");
        std::fs::write(std::path::PathBuf::from(wal), b"event").unwrap();
        assert!(signal.changed());
        assert!(!signal.changed());
    }

    #[tokio::test]
    async fn idle_success_does_not_write_background_health() {
        let directory = tempfile::tempdir().unwrap();
        let pool = monitor_storage::connect(&directory.path().join("state.db"))
            .await
            .unwrap();
        monitor_storage::migrate(&pool).await.unwrap();

        let result =
            observe_engine_result(&pool, "engine_processing", Ok(BatchResult::default()), 10).await;

        assert!(result.is_ok());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM background_task_health WHERE task='engine_processing'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
    }
}
