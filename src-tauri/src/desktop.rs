mod indexing;
mod queries;
mod settings;
mod tray;
mod workers;

use crate::hook_lifecycle::{HookHealth, HookLifecycleState};
use monitor_engine::{Engine, ProviderKind};
use monitor_notify::{Notification, Priority};
use serde::Serialize;
use sqlx::SqlitePool;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Emitter, Manager, State, Window, WindowEvent};

pub use indexing::ReindexStarted;
pub use settings::SettingsDto;

const ACTIVE_WINDOW_MS: i64 = 24 * 60 * 60 * 1_000;
const DASHBOARD_SESSION_LIMIT: i64 = 100;
const TRAY_SESSION_LIMIT: i64 = 8;
fn serialize_i64_as_decimal<S>(value: &i64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&value.to_string())
}

pub struct DesktopState {
    pub pool: SqlitePool,
    hook_lifecycle: Arc<HookLifecycleState>,
    providers: Arc<tokio::sync::RwLock<settings::NotificationPolicy>>,
    revision: AtomicU64,
    tray_revision: AtomicU64,
    pub(super) quitting: Arc<AtomicBool>,
    pub(super) index: Mutex<IndexProgress>,
    pub(super) indexing: Arc<tokio::sync::Mutex<()>>,
}

impl DesktopState {
    fn new(
        pool: SqlitePool,
        hook_lifecycle: Arc<HookLifecycleState>,
        providers: settings::NotificationPolicy,
    ) -> Self {
        Self {
            pool,
            hook_lifecycle,
            providers: Arc::new(tokio::sync::RwLock::new(providers)),
            revision: AtomicU64::new(1),
            tray_revision: AtomicU64::new(1),
            quitting: Arc::new(AtomicBool::new(false)),
            index: Mutex::new(IndexProgress::default()),
            indexing: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(super) fn engine(&self) -> Engine {
        Engine::new(self.pool.clone())
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    pub(super) fn tray_revision(&self) -> u64 {
        self.tray_revision.load(Ordering::SeqCst)
    }

    pub(crate) fn invalidate(&self, app: &AppHandle) {
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = app.emit("monitor://invalidated", RevisionEvent { revision });
    }

    pub(super) fn invalidate_tray(&self, app: &AppHandle) {
        self.tray_revision.fetch_add(1, Ordering::SeqCst);
        self.invalidate(app);
    }

    pub(super) async fn snapshot(&self) -> anyhow::Result<DashboardSnapshot> {
        let _lifecycle_guard = self.hook_lifecycle.lifecycle.lock().await;
        let snapshot =
            queries::snapshot(&self.pool, || self.revision(), &self.hook_lifecycle).await?;
        Ok(snapshot)
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RevisionEvent {
    revision: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSnapshot {
    revision: u64,
    counts: Counts,
    active_session_count: i64,
    sessions_has_more: bool,
    today: UsageSummary,
    trends: Vec<TrendDay>,
    sessions: Vec<SessionRow>,
    hook: HookHealth,
    hook_onboarding_disposition: Option<crate::hook_onboarding::HookOnboardingDisposition>,
    pub(super) index: IndexProgress,
    diagnostics: Diagnostics,
}

#[derive(Clone, Debug)]
pub(super) struct TraySnapshot {
    counts: Counts,
    today: UsageSummary,
    trends: Vec<TrendDay>,
    sessions: Vec<SessionRow>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Counts {
    running: i64,
    waiting: i64,
    needs_input: i64,
    failed: i64,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UsageSummary {
    #[serde(serialize_with = "serialize_i64_as_decimal")]
    input_tokens: i64,
    #[serde(serialize_with = "serialize_i64_as_decimal")]
    output_tokens: i64,
    #[serde(serialize_with = "serialize_i64_as_decimal")]
    cache_write_tokens: i64,
    #[serde(serialize_with = "serialize_i64_as_decimal")]
    cache_read_tokens: i64,
    #[serde(serialize_with = "serialize_i64_as_decimal")]
    cost_pico_usd: i64,
    cost_known: bool,
    #[serde(serialize_with = "serialize_i64_as_decimal")]
    unpriced_tokens: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TrendDay {
    day: String,
    #[serde(serialize_with = "serialize_i64_as_decimal")]
    tokens: i64,
    #[serde(serialize_with = "serialize_i64_as_decimal")]
    cost_pico_usd: i64,
    cost_known: bool,
    #[serde(serialize_with = "serialize_i64_as_decimal")]
    unpriced_tokens: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionRow {
    session_id: String,
    project_name: Option<String>,
    turn_state: String,
    state_source: String,
    state_reason: String,
    changed_at_ms: i64,
    last_observed_at_ms: i64,
    revision: i64,
    usage: UsageSummary,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IndexProgress {
    run_id: Option<String>,
    state: String,
    completed: i64,
    total: i64,
    failed_files: i64,
    quarantined_sessions: i64,
    interrupted: bool,
}

impl Default for IndexProgress {
    fn default() -> Self {
        Self {
            run_id: None,
            state: "idle".to_owned(),
            completed: 0,
            total: 0,
            failed_files: 0,
            quarantined_sessions: 0,
            interrupted: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupResult {
    raw_events_deleted: u64,
    notifications_deleted: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Diagnostics {
    migration_version: i64,
    pending_events: i64,
    quarantined_sessions: i64,
    pending_notifications: i64,
    desktop_failures: i64,
    desktop_error_code: Option<String>,
    ntfy_failures: i64,
    ntfy_error_code: Option<String>,
    ntfy_recovered_at_ms: Option<i64>,
    background_health: Vec<BackgroundHealth>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackgroundHealth {
    task: String,
    success_count: i64,
    failure_count: i64,
    consecutive_failures: i64,
    error_code: Option<String>,
    last_succeeded_at_ms: Option<i64>,
    last_failed_at_ms: Option<i64>,
    recovered_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDetail {
    session: SessionRow,
    models: Vec<ModelUsage>,
    events: Vec<HistoryEvent>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelUsage {
    model_id: String,
    #[serde(serialize_with = "serialize_i64_as_decimal")]
    tokens: i64,
    #[serde(serialize_with = "serialize_i64_as_decimal")]
    cost_pico_usd: i64,
    cost_known: bool,
    #[serde(serialize_with = "serialize_i64_as_decimal")]
    unpriced_tokens: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryEvent {
    source_event: String,
    source: String,
    occurred_at_ms: i64,
}

#[tauri::command]
pub async fn get_dashboard_snapshot(
    state: State<'_, Arc<DesktopState>>,
) -> Result<DashboardSnapshot, String> {
    let mut value = state
        .snapshot()
        .await
        .map_err(super::fixed_error(super::IpcError::DashboardRead))?;
    value.index = state.index.lock().unwrap().clone();
    Ok(value)
}

#[tauri::command]
pub async fn get_session_detail(
    session_id: String,
    state: State<'_, Arc<DesktopState>>,
) -> Result<SessionDetail, String> {
    if session_id.is_empty() || session_id.len() > 512 {
        return Err(super::IpcError::SessionInvalid.code().to_owned());
    }
    queries::session_detail(&state.pool, &session_id)
        .await
        .map_err(super::fixed_error(super::IpcError::SessionRead))
}

#[tauri::command]
pub async fn get_settings(
    app: AppHandle,
    state: State<'_, Arc<DesktopState>>,
) -> Result<SettingsDto, String> {
    settings::get_settings(app, state).await
}

#[tauri::command]
pub async fn defer_hook_onboarding(
    app: AppHandle,
    state: State<'_, Arc<DesktopState>>,
) -> Result<(), String> {
    crate::hook_onboarding::persist(
        &state.pool,
        crate::hook_onboarding::HookOnboardingDisposition::Deferred,
    )
    .await
    .map_err(super::fixed_error(super::IpcError::HookOnboarding))?;
    state.invalidate(&app);
    Ok(())
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    settings: SettingsDto,
    state: State<'_, Arc<DesktopState>>,
) -> Result<(), String> {
    settings::save_settings(app, settings, state).await
}

#[tauri::command]
pub async fn test_ntfy(
    settings: SettingsDto,
    state: State<'_, Arc<DesktopState>>,
) -> Result<(), String> {
    settings::test_ntfy(settings, state).await
}

#[tauri::command]
pub fn show_dashboard(app: AppHandle, route: Option<String>) -> Result<(), String> {
    tray::show_window(&app, route.as_deref())
        .map_err(super::fixed_error(super::IpcError::DashboardOpen))
}

#[tauri::command]
pub async fn clear_completed_history(
    app: AppHandle,
    state: State<'_, Arc<DesktopState>>,
) -> Result<CleanupResult, String> {
    let counts = state
        .engine()
        .retain_older_than(now_ms())
        .await
        .map_err(super::fixed_error(super::IpcError::Cleanup))?;
    state.invalidate(&app);
    Ok(CleanupResult {
        raw_events_deleted: counts.raw_events_deleted,
        notifications_deleted: counts.notifications_deleted,
    })
}

#[tauri::command]
pub async fn reindex_transcripts(
    app: AppHandle,
    state: State<'_, Arc<DesktopState>>,
) -> Result<ReindexStarted, String> {
    indexing::reindex_transcripts(app, state).await
}

#[tauri::command]
pub fn open_notification_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg("x-apple.systempreferences:com.apple.Notifications-Settings.extension")
            .spawn()
            .map_err(super::fixed_error(
                super::IpcError::NotificationSettingsOpen,
            ))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn test_desktop_notification(
    app: AppHandle,
    state: State<'_, Arc<DesktopState>>,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        super::macos_notification::request_authorization()
            .await
            .map_err(str::to_owned)?;
        super::macos_notification::show(
            app.clone(),
            Notification {
                title: "CC Monitor".to_owned(),
                body: "桌面通知已启用".to_owned(),
                priority: Priority::Default,
                tag: "test".to_owned(),
                session_id: None,
                client_bundle_id: None,
            },
        )
        .await?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = &app;
    }

    state
        .engine()
        .mark_provider_healthy(ProviderKind::Desktop, now_ms())
        .await
        .map_err(super::fixed_error(super::IpcError::DesktopDelivery))?;
    state.invalidate(&app);
    Ok(())
}

pub async fn initialize(
    app: &AppHandle,
    pool: SqlitePool,
    hook_lifecycle: Arc<HookLifecycleState>,
) -> tauri::Result<()> {
    let providers = match settings::load_notification_policy_read_only(&pool).await {
        Ok(providers) => providers,
        Err(_) => {
            crate::logging::event("notification_policy_load_failed");
            settings::NotificationPolicy::desktop_only()
        }
    };
    let state = Arc::new(DesktopState::new(pool, hook_lifecycle, providers));
    app.manage(state.clone());
    #[cfg(target_os = "macos")]
    super::macos_notification::initialize();
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory)?;
    tray::build_initial(app)?;
    workers::spawn_leader(app.clone(), state);
    Ok(())
}

pub fn handle_window_event(window: &Window, event: &WindowEvent) {
    tray::handle_window_event(window, event);
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn sanitize_background_error_code(task: &str, value: &str) -> &'static str {
    match (task, value) {
        ("incremental_index", "index_storage_failed") => "index_storage_failed",
        ("incremental_index", "index_permission_denied") => "index_permission_denied",
        ("incremental_index", "index_invalid_transcript") => "index_invalid_transcript",
        ("incremental_index", "index_io_failed") => "index_io_failed",
        ("incremental_index", "index_failed") => "index_failed",
        (
            "engine_processing"
            | "engine_reconciliation"
            | "startup_reconciliation"
            | "retention_cleanup",
            "engine_storage_failed",
        ) => "engine_storage_failed",
        (
            "engine_processing"
            | "engine_reconciliation"
            | "startup_reconciliation"
            | "retention_cleanup",
            "engine_invalid_event",
        ) => "engine_invalid_event",
        ("engine_processing" | "engine_reconciliation", "engine_invalid_projection") => {
            "engine_invalid_projection"
        }
        ("incremental_index", _) => "index_failed",
        _ => "engine_failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_background_errors_are_sanitized() {
        assert_eq!(
            sanitize_background_error_code("incremental_index", "secret"),
            "index_failed"
        );
    }
}
