use adapter_claude::install::{self, HookInstallation};
use async_trait::async_trait;
use serde::Serialize;
use sqlx::SqlitePool;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::{AppHandle, State};
use uuid::Uuid;

pub(crate) struct HookLifecycleState {
    pub(crate) lifecycle: tokio::sync::Mutex<()>,
    pool: SqlitePool,
    app_data: PathBuf,
    database: PathBuf,
    settings: PathBuf,
    hook_source: PathBuf,
}

impl HookLifecycleState {
    pub(crate) fn new(
        pool: SqlitePool,
        app_data: PathBuf,
        database: PathBuf,
        settings: PathBuf,
        hook_source: PathBuf,
    ) -> Self {
        Self {
            lifecycle: tokio::sync::Mutex::new(()),
            pool,
            app_data,
            database,
            settings,
            hook_source,
        }
    }

    pub(crate) fn verify_health(&self, ownership: Option<HookOwnershipRecord>) -> HookHealth {
        let expected_staged = match install::managed_hook_path(&self.app_data) {
            Ok(path) => path,
            Err(_) => {
                return HookHealth::repair_required(
                    ownership.map(|value| value.version),
                    "hook_managed_path_unsafe",
                );
            }
        };
        let staged_health = staged_executable_health(&expected_staged);
        let Some(ownership) = ownership else {
            return if staged_health == StagedExecutableHealth::Missing {
                HookHealth::absent()
            } else {
                HookHealth::repair_required(None, "hook_ownership_missing")
            };
        };
        let version = Some(ownership.version);
        let Ok(installation_id) = ownership.installation_id.parse() else {
            return HookHealth::repair_required(version, "hook_ownership_invalid");
        };
        let staged_hook = PathBuf::from(&ownership.hook_path);
        if staged_hook != expected_staged {
            return HookHealth::repair_required(version, "hook_ownership_path_mismatch");
        }
        match staged_health {
            StagedExecutableHealth::Missing => {
                return HookHealth::repair_required(version, "hook_binary_missing");
            }
            StagedExecutableHealth::Invalid => {
                return HookHealth::repair_required(version, "hook_binary_invalid");
            }
            StagedExecutableHealth::Ready => {}
        }
        let installation = HookInstallation {
            installation_id,
            staged_hook,
            database: self.database.clone(),
        };
        match install::installation_matches(&self.settings, &installation) {
            Ok(true) => HookHealth {
                status: HookHealthStatus::Installed,
                issue_code: None,
                version,
            },
            Ok(false) => HookHealth::repair_required(version, "hook_settings_mismatch"),
            Err(_) => HookHealth::repair_required(version, "hook_settings_unreadable"),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HookOwnershipRecord {
    pub(crate) installation_id: String,
    pub(crate) hook_path: String,
    pub(crate) version: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StagedExecutableHealth {
    Missing,
    Invalid,
    Ready,
}

fn staged_executable_health(path: &Path) -> StagedExecutableHealth {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return StagedExecutableHealth::Missing;
        }
        Err(_) => return StagedExecutableHealth::Invalid,
    };
    if !metadata.file_type().is_file() {
        return StagedExecutableHealth::Invalid;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return StagedExecutableHealth::Invalid;
        }
    }
    StagedExecutableHealth::Ready
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HookHealthStatus {
    Installed,
    RepairRequired,
    Absent,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HookHealth {
    pub(crate) status: HookHealthStatus,
    pub(crate) issue_code: Option<&'static str>,
    pub(crate) version: Option<String>,
}

impl HookHealth {
    pub(crate) fn absent() -> Self {
        Self {
            status: HookHealthStatus::Absent,
            issue_code: None,
            version: None,
        }
    }

    fn repair_required(version: Option<String>, issue_code: &'static str) -> Self {
        Self {
            status: HookHealthStatus::RepairRequired,
            issue_code: Some(issue_code),
            version,
        }
    }
}

#[tauri::command]
pub(crate) async fn install_claude_hook(
    app: AppHandle,
    state: State<'_, Arc<HookLifecycleState>>,
    desktop: State<'_, Arc<crate::desktop::DesktopState>>,
) -> Result<(), String> {
    let result = install_managed_hook(&state).await;
    // A failed post-install preference cleanup can still leave a healthy Hook.
    // Always invalidate so the next snapshot reflects the verified filesystem
    // and ownership state instead of preserving stale UI state.
    desktop.invalidate(&app);
    result.map_err(|error| match error {
        HookMutationError::Onboarding(_) => "hook_installed_onboarding_sync_failed".to_owned(),
        HookMutationError::Lifecycle(_) => crate::IpcError::HookInstall.code().to_owned(),
    })
}

#[derive(Debug)]
enum HookMutationError {
    Lifecycle(anyhow::Error),
    Onboarding(anyhow::Error),
}

impl std::fmt::Display for HookMutationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lifecycle(error) | Self::Onboarding(error) => error.fmt(formatter),
        }
    }
}

async fn install_managed_hook(state: &HookLifecycleState) -> Result<(), HookMutationError> {
    let _lifecycle_guard = state.lifecycle.lock().await;
    install_hook_at(
        &state.pool,
        &state.app_data,
        &state.database,
        &state.settings,
        &state.hook_source,
    )
    .await
    .map_err(HookMutationError::Lifecycle)?;
    crate::hook_onboarding::clear(&state.pool)
        .await
        .map_err(HookMutationError::Onboarding)
}

#[tauri::command]
pub(crate) async fn uninstall_claude_hook(
    app: AppHandle,
    state: State<'_, Arc<HookLifecycleState>>,
    desktop: State<'_, Arc<crate::desktop::DesktopState>>,
) -> Result<bool, String> {
    let result = uninstall_managed_hook_outcome(&state).await;
    // The deliberate intent and the filesystem/settings mutation cannot share
    // a transaction. Invalidation makes every partial result discoverable and
    // lets retry converge from the verified state.
    desktop.invalidate(&app);
    result
        .map(|(removed, _)| removed)
        .map_err(|error| match error {
            HookMutationError::Onboarding(_) => "hook_uninstall_intent_save_failed".to_owned(),
            HookMutationError::Lifecycle(_) => "hook_uninstall_incomplete".to_owned(),
        })
}

#[cfg(test)]
async fn uninstall_managed_hook(state: &HookLifecycleState) -> anyhow::Result<bool> {
    uninstall_managed_hook_outcome(state)
        .await
        .map(|outcome| outcome.0)
        .map_err(|error| anyhow::anyhow!("{error}"))
}

async fn uninstall_managed_hook_outcome(
    state: &HookLifecycleState,
) -> Result<(bool, bool), HookMutationError> {
    let _lifecycle_guard = state.lifecycle.lock().await;
    // Persist the user's durable intent before touching settings or the staged
    // binary. If this write fails, uninstall has not started. If a later file
    // operation fails, retry sees the same intent and safely converges.
    crate::hook_onboarding::persist(
        &state.pool,
        crate::hook_onboarding::HookOnboardingDisposition::DeliberatelyUninstalled,
    )
    .await
    .map_err(HookMutationError::Onboarding)?;
    let ownership_existed: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM installation WHERE singleton=1)")
            .fetch_one(&state.pool)
            .await
            .map_err(|error| HookMutationError::Lifecycle(error.into()))?;
    let removed = uninstall_hook_at(
        &state.pool,
        &state.app_data,
        &state.database,
        &state.settings,
    )
    .await
    .map_err(HookMutationError::Lifecycle)?;
    Ok((removed, ownership_existed))
}

async fn install_hook_at(
    pool: &SqlitePool,
    app_data: &Path,
    database: &Path,
    settings: &Path,
    hook_source: &Path,
) -> Result<(), anyhow::Error> {
    let store = SqliteOwnershipStore { pool, database };
    let installation_id = store
        .load()
        .await?
        .map(|value| value.installation_id)
        .unwrap_or_else(Uuid::now_v7);
    let rollback = StagedRollback::prepare(app_data)?;
    let staged_hook = match install::stage_hook(hook_source, app_data) {
        Ok(path) => path,
        Err(_error) => {
            return match rollback.restore() {
                Ok(()) => Err(anyhow::anyhow!("hook_stage_failed")),
                Err(_) => Err(anyhow::anyhow!(
                    "hook_stage_failed; staged_binary_rollback_failed"
                )),
            };
        }
    };
    let installation = HookInstallation {
        installation_id,
        staged_hook,
        database: database.to_owned(),
    };
    if let Err(error) = orchestrate_install(&store, settings, &installation).await {
        return match rollback.restore() {
            Ok(()) => Err(error),
            Err(_) => Err(anyhow::anyhow!("{error}; staged_binary_rollback_failed")),
        };
    }
    // Installation is already committed in both ownership and settings.
    // Failure to remove the private rollback copy is non-fatal housekeeping:
    // report a fixed diagnostic, but do not tell the UI that installation
    // failed or attempt an unsafe post-commit rollback.
    if rollback.discard().is_err() {
        crate::logging::event("hook_rollback_cleanup_failed");
    }
    Ok(())
}

async fn uninstall_hook_at(
    pool: &SqlitePool,
    app_data: &Path,
    database: &Path,
    settings: &Path,
) -> Result<bool, anyhow::Error> {
    let store = SqliteOwnershipStore { pool, database };
    orchestrate_uninstall(&store, settings, |installation| {
        remove_owned_staged_file(app_data, installation)
    })
    .await
}

#[async_trait]
trait OwnershipStore: Sync {
    async fn load(&self) -> anyhow::Result<Option<HookInstallation>>;
    async fn put(&self, installation: &HookInstallation) -> anyhow::Result<()>;
    async fn clear(&self, installation_id: Uuid) -> anyhow::Result<()>;
}

struct SqliteOwnershipStore<'a> {
    pool: &'a sqlx::SqlitePool,
    database: &'a Path,
}

#[async_trait]
impl OwnershipStore for SqliteOwnershipStore<'_> {
    async fn load(&self) -> anyhow::Result<Option<HookInstallation>> {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT installation_id, hook_path FROM installation WHERE singleton = 1",
        )
        .fetch_optional(self.pool)
        .await?;
        Ok(row.and_then(|(installation_id, hook_path)| {
            Some(HookInstallation {
                installation_id: installation_id.parse().ok()?,
                staged_hook: hook_path.into(),
                database: self.database.to_owned(),
            })
        }))
    }

    async fn put(&self, installation: &HookInstallation) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO installation (
                singleton, installation_id, hook_path, installed_at_ms, hook_version
             ) VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(singleton) DO UPDATE SET
                installation_id = excluded.installation_id,
                hook_path = excluded.hook_path,
                installed_at_ms = excluded.installed_at_ms,
                hook_version = excluded.hook_version",
        )
        .bind(installation.installation_id.to_string())
        .bind(installation.staged_hook.to_string_lossy().as_ref())
        .bind(adapter_claude::hook::now_ms())
        .bind(env!("CARGO_PKG_VERSION"))
        .execute(self.pool)
        .await?;
        Ok(())
    }

    async fn clear(&self, installation_id: Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "DELETE FROM installation
             WHERE singleton = 1 AND installation_id = ?1",
        )
        .bind(installation_id.to_string())
        .execute(self.pool)
        .await?;
        Ok(())
    }
}

async fn orchestrate_install(
    store: &impl OwnershipStore,
    settings: &Path,
    installation: &HookInstallation,
) -> anyhow::Result<()> {
    let previous = store.load().await?;
    store.put(installation).await?;
    if let Err(_error) = install::install(settings, installation) {
        let rollback = if let Some(previous) = previous {
            store.put(&previous).await
        } else {
            store.clear(installation.installation_id).await
        };
        if rollback.is_err() {
            return Err(anyhow::anyhow!(
                "hook_settings_write_failed; ownership_rollback_failed"
            ));
        }
        return Err(anyhow::anyhow!("hook_settings_write_failed"));
    }
    Ok(())
}

async fn orchestrate_uninstall(
    store: &impl OwnershipStore,
    settings: &Path,
    remove_staged: impl FnOnce(&HookInstallation) -> anyhow::Result<bool>,
) -> anyhow::Result<bool> {
    let Some(installation) = store.load().await? else {
        return Ok(false);
    };
    let removed = install::uninstall(settings, &installation)?.is_some();
    let removed_staged = remove_staged(&installation)?;
    // If the exact command is already absent, clearing the stale record makes
    // retrying an interrupted uninstall self-healing.
    store.clear(installation.installation_id).await?;
    Ok(removed || removed_staged)
}

fn remove_owned_staged_file(
    app_data: &Path,
    installation: &HookInstallation,
) -> anyhow::Result<bool> {
    let expected_lexical = app_data.join("bin/cc-monitor-hook");
    if installation.staged_hook != expected_lexical {
        return Ok(false);
    }
    let expected = install::managed_hook_path(app_data)?;
    match std::fs::remove_file(&expected) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

struct StagedRollback {
    app_data: PathBuf,
    previous: Option<PathBuf>,
}

impl StagedRollback {
    fn prepare(app_data: &Path) -> anyhow::Result<Self> {
        let target = install::managed_hook_path(app_data)?;
        let previous = if target.exists() {
            let parent = target
                .parent()
                .ok_or_else(|| anyhow::anyhow!("staged Hook has no parent"))?;
            let backup = parent.join(format!(".cc-monitor-hook.{}.rollback", Uuid::now_v7()));
            if capture_staged_rollback(&target, &backup).is_err() {
                let _ = std::fs::remove_file(&backup);
                anyhow::bail!("hook_rollback_capture_failed");
            }
            Some(backup)
        } else {
            None
        };
        Ok(Self {
            app_data: app_data.to_owned(),
            previous,
        })
    }

    fn restore(self) -> anyhow::Result<()> {
        let target = install::managed_hook_path(&self.app_data)?;
        match self.previous {
            Some(previous) => {
                validate_private_rollback(&target, &previous)?;
                std::fs::rename(previous, target)?;
                Ok(())
            }
            None => match std::fs::remove_file(target) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            },
        }
    }

    fn discard(self) -> anyhow::Result<()> {
        if let Some(previous) = self.previous {
            let target = install::managed_hook_path(&self.app_data)?;
            validate_private_rollback(&target, &previous)?;
            match std::fs::remove_file(previous) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => anyhow::bail!("staged_binary_rollback_cleanup_failed"),
            }
        }
        Ok(())
    }
}

fn capture_staged_rollback(target: &Path, backup: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(target)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("hook_managed_path_unsafe");
    }
    let mut source = std::fs::File::open(target)?;
    let mut destination = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(backup)?;
    std::io::copy(&mut source, &mut destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            backup,
            std::fs::Permissions::from_mode(metadata.permissions().mode() & 0o7777),
        )?;
    }
    #[cfg(not(unix))]
    std::fs::set_permissions(backup, metadata.permissions())?;
    destination.sync_all()?;
    Ok(())
}

fn validate_private_rollback(target: &Path, rollback: &Path) -> anyhow::Result<()> {
    if rollback.parent() != target.parent() {
        anyhow::bail!("hook_managed_path_unsafe");
    }
    let metadata = std::fs::symlink_metadata(rollback)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("hook_managed_path_unsafe");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    };
    use tempfile::tempdir;

    struct MockStore {
        record: Mutex<Option<HookInstallation>>,
        fail_put: AtomicBool,
        fail_put_on_attempt: AtomicUsize,
        put_attempts: AtomicUsize,
        fail_clear: AtomicUsize,
        clear_attempts: AtomicUsize,
    }

    impl MockStore {
        fn new(record: Option<HookInstallation>) -> Self {
            Self {
                record: Mutex::new(record),
                fail_put: AtomicBool::new(false),
                fail_put_on_attempt: AtomicUsize::new(0),
                put_attempts: AtomicUsize::new(0),
                fail_clear: AtomicUsize::new(0),
                clear_attempts: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl OwnershipStore for MockStore {
        async fn load(&self) -> anyhow::Result<Option<HookInstallation>> {
            Ok(self.record.lock().unwrap().clone())
        }

        async fn put(&self, installation: &HookInstallation) -> anyhow::Result<()> {
            let attempt = self.put_attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_put.load(Ordering::SeqCst)
                || self.fail_put_on_attempt.load(Ordering::SeqCst) == attempt
            {
                anyhow::bail!("injected put failure");
            }
            *self.record.lock().unwrap() = Some(installation.clone());
            Ok(())
        }

        async fn clear(&self, installation_id: Uuid) -> anyhow::Result<()> {
            self.clear_attempts.fetch_add(1, Ordering::SeqCst);
            if self
                .fail_clear
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                anyhow::bail!("injected clear failure");
            }
            let mut record = self.record.lock().unwrap();
            if record
                .as_ref()
                .is_some_and(|value| value.installation_id == installation_id)
            {
                *record = None;
            }
            Ok(())
        }
    }

    fn installation(root: &Path) -> HookInstallation {
        HookInstallation {
            installation_id: Uuid::now_v7(),
            staged_hook: root.join("bin/cc-monitor-hook"),
            database: root.join("state.db"),
        }
    }

    async fn migrated_pool(database: &Path) -> SqlitePool {
        let pool = monitor_storage::connect(database).await.unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        pool
    }

    async fn lifecycle_state(root: &Path) -> Arc<HookLifecycleState> {
        let app_data = root.join("app-data");
        std::fs::create_dir_all(&app_data).unwrap();
        let database = app_data.join("state.db");
        let settings = root.join(".claude/settings.json");
        let hook_source = root.join("cc-monitor-hook");
        std::fs::write(&hook_source, b"hook").unwrap();
        Arc::new(HookLifecycleState {
            lifecycle: tokio::sync::Mutex::new(()),
            pool: migrated_pool(&database).await,
            app_data,
            database,
            settings,
            hook_source,
        })
    }

    async fn assert_managed_hook_state_is_coherent(state: &HookLifecycleState) {
        let installation_id: Option<String> =
            sqlx::query_scalar("SELECT installation_id FROM installation WHERE singleton = 1")
                .fetch_optional(&state.pool)
                .await
                .unwrap();
        let settings: Value = if state.settings.exists() {
            serde_json::from_slice(&std::fs::read(&state.settings).unwrap()).unwrap()
        } else {
            serde_json::json!({})
        };
        let owned_commands = settings
            .get("hooks")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|hooks| hooks.values())
            .filter_map(Value::as_array)
            .flatten()
            .filter_map(|group| group.get("hooks").and_then(Value::as_array))
            .flatten()
            .filter_map(|entry| entry.get("command").and_then(Value::as_str))
            .filter(|command| command.contains("--installation-id"))
            .collect::<Vec<_>>();
        let staged_exists = state.app_data.join("bin/cc-monitor-hook").exists();

        match installation_id {
            Some(installation_id) => {
                assert!(staged_exists);
                assert_eq!(
                    owned_commands.len(),
                    adapter_claude::hook::HOOK_EVENTS.len()
                );
                assert!(owned_commands
                    .iter()
                    .all(|command| command.contains(&installation_id)));
            }
            None => {
                assert!(!staged_exists);
                assert!(owned_commands.is_empty());
            }
        }
    }

    async fn ownership_record(state: &HookLifecycleState) -> Option<HookOwnershipRecord> {
        sqlx::query_as::<_, (String, String, String)>(
            "SELECT installation_id, hook_path, hook_version
             FROM installation WHERE singleton = 1",
        )
        .fetch_optional(&state.pool)
        .await
        .unwrap()
        .map(
            |(installation_id, hook_path, version)| HookOwnershipRecord {
                installation_id,
                hook_path,
                version,
            },
        )
    }

    #[tokio::test]
    async fn concurrent_managed_installs_are_serial_and_idempotent() {
        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;

        let (left, right) =
            tokio::join!(install_managed_hook(&state), install_managed_hook(&state));

        left.unwrap();
        right.unwrap();
        assert_managed_hook_state_is_coherent(&state).await;
        let row_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM installation WHERE singleton = 1")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(row_count, 1);
        state.pool.close().await;
    }

    #[tokio::test]
    async fn verified_health_requires_ownership_binary_and_exact_settings() {
        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        crate::hook_onboarding::persist(
            &state.pool,
            crate::hook_onboarding::HookOnboardingDisposition::Deferred,
        )
        .await
        .unwrap();
        install_managed_hook(&state).await.unwrap();

        let health = state.verify_health(ownership_record(&state).await);
        let mut connection = state.pool.acquire().await.unwrap();

        assert_eq!(health.status, HookHealthStatus::Installed);
        assert_eq!(health.issue_code, None);
        assert_eq!(
            crate::hook_onboarding::load(&mut connection).await.unwrap(),
            None,
            "a successful install must clear an obsolete onboarding choice"
        );
        drop(connection);
        state.pool.close().await;
    }

    #[tokio::test]
    async fn installed_hook_remains_discoverable_when_onboarding_cleanup_fails() {
        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        crate::hook_onboarding::persist(
            &state.pool,
            crate::hook_onboarding::HookOnboardingDisposition::Deferred,
        )
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_onboarding_delete
             BEFORE DELETE ON settings
             WHEN OLD.key='hook_onboarding'
             BEGIN SELECT RAISE(FAIL, 'injected'); END",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        assert!(matches!(
            install_managed_hook(&state).await,
            Err(HookMutationError::Onboarding(_))
        ));
        assert_eq!(
            state.verify_health(ownership_record(&state).await).status,
            HookHealthStatus::Installed
        );
        let mut connection = state.pool.acquire().await.unwrap();
        assert_eq!(
            crate::hook_onboarding::load(&mut connection).await.unwrap(),
            Some(crate::hook_onboarding::HookOnboardingDisposition::Deferred)
        );
        drop(connection);
        state.pool.close().await;
    }

    #[tokio::test]
    async fn missing_binary_and_settings_are_repair_required() {
        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        install_managed_hook(&state).await.unwrap();
        let ownership = ownership_record(&state).await;
        std::fs::remove_file(state.app_data.join("bin/cc-monitor-hook")).unwrap();

        let missing_binary = state.verify_health(ownership.clone());
        assert_eq!(missing_binary.status, HookHealthStatus::RepairRequired);
        assert_eq!(missing_binary.issue_code, Some("hook_binary_missing"));

        install_managed_hook(&state).await.unwrap();
        std::fs::remove_file(&state.settings).unwrap();
        let missing_settings = state.verify_health(ownership_record(&state).await);
        assert_eq!(missing_settings.status, HookHealthStatus::RepairRequired);
        assert_eq!(missing_settings.issue_code, Some("hook_settings_mismatch"));
        state.pool.close().await;
    }

    #[tokio::test]
    async fn unreadable_or_malformed_settings_never_report_installed() {
        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        install_managed_hook(&state).await.unwrap();
        std::fs::write(&state.settings, b"{bad").unwrap();

        let health = state.verify_health(ownership_record(&state).await);

        assert_eq!(health.status, HookHealthStatus::RepairRequired);
        assert_eq!(health.issue_code, Some("hook_settings_unreadable"));
        state.pool.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non_executable_staged_file_is_repair_required() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        install_managed_hook(&state).await.unwrap();
        let staged = state.app_data.join("bin/cc-monitor-hook");
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)).unwrap();

        let health = state.verify_health(ownership_record(&state).await);

        assert_eq!(health.status, HookHealthStatus::RepairRequired);
        assert_eq!(health.issue_code, Some("hook_binary_invalid"));
        state.pool.close().await;
    }

    #[tokio::test]
    async fn malformed_ownership_is_repair_required() {
        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        std::fs::create_dir_all(state.app_data.join("bin")).unwrap();
        let staged = state.app_data.join("bin/cc-monitor-hook");
        std::fs::write(&staged, b"hook").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        let health = state.verify_health(Some(HookOwnershipRecord {
            installation_id: "not-a-uuid".to_owned(),
            hook_path: staged.to_string_lossy().into_owned(),
            version: "1".to_owned(),
        }));

        assert_eq!(health.status, HookHealthStatus::RepairRequired);
        assert_eq!(health.issue_code, Some("hook_ownership_invalid"));
        state.pool.close().await;
    }

    #[tokio::test]
    async fn first_install_stage_residue_is_repair_required_but_clean_state_is_absent() {
        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        assert_eq!(state.verify_health(None).status, HookHealthStatus::Absent);
        std::fs::create_dir_all(state.app_data.join("bin")).unwrap();
        let staged = state.app_data.join("bin/cc-monitor-hook");
        std::fs::write(&staged, b"partial").unwrap();

        let health = state.verify_health(None);

        assert_eq!(health.status, HookHealthStatus::RepairRequired);
        assert_eq!(health.issue_code, Some("hook_ownership_missing"));
        state.pool.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn health_rejects_a_symlinked_managed_bin() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        install_managed_hook(&state).await.unwrap();
        let ownership = ownership_record(&state).await;
        let bin = state.app_data.join("bin");
        std::fs::rename(&bin, state.app_data.join("original-bin")).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, &bin).unwrap();

        let health = state.verify_health(ownership);

        assert_eq!(health.status, HookHealthStatus::RepairRequired);
        assert_eq!(health.issue_code, Some("hook_managed_path_unsafe"));
        state.pool.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn uninstall_never_deletes_through_a_symlinked_managed_bin() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        install_managed_hook(&state).await.unwrap();
        let bin = state.app_data.join("bin");
        std::fs::rename(&bin, state.app_data.join("original-bin")).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let external_hook = outside.join("cc-monitor-hook");
        std::fs::write(&external_hook, b"external").unwrap();
        symlink(&outside, &bin).unwrap();

        assert!(uninstall_hook_at(
            &state.pool,
            &state.app_data,
            &state.database,
            &state.settings,
        )
        .await
        .is_err());
        assert_eq!(std::fs::read(external_hook).unwrap(), b"external");
        state.pool.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rollback_never_restores_through_a_symlinked_managed_bin() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        install_managed_hook(&state).await.unwrap();
        let rollback = StagedRollback::prepare(&state.app_data).unwrap();
        let bin = state.app_data.join("bin");
        std::fs::rename(&bin, state.app_data.join("original-bin")).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let external_hook = outside.join("cc-monitor-hook");
        std::fs::write(&external_hook, b"external").unwrap();
        symlink(&outside, &bin).unwrap();

        assert!(rollback.restore().is_err());
        assert_eq!(std::fs::read(external_hook).unwrap(), b"external");
        state.pool.close().await;
    }

    #[tokio::test]
    async fn concurrent_managed_install_and_uninstall_end_coherently() {
        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        install_managed_hook(&state).await.unwrap();
        std::fs::write(&state.hook_source, b"repaired-hook").unwrap();

        let (install, uninstall) =
            tokio::join!(install_managed_hook(&state), uninstall_managed_hook(&state));

        install.unwrap();
        uninstall.unwrap();
        assert_managed_hook_state_is_coherent(&state).await;
        state.pool.close().await;
    }

    #[tokio::test]
    async fn deliberate_uninstall_persists_intent_before_removing_the_hook() {
        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        install_managed_hook(&state).await.unwrap();

        assert!(uninstall_managed_hook(&state).await.unwrap());
        let mut connection = state.pool.acquire().await.unwrap();
        assert_eq!(
            crate::hook_onboarding::load(&mut connection).await.unwrap(),
            Some(crate::hook_onboarding::HookOnboardingDisposition::DeliberatelyUninstalled)
        );
        drop(connection);
        assert_managed_hook_state_is_coherent(&state).await;
        state.pool.close().await;
    }

    #[tokio::test]
    async fn uninstall_does_not_start_when_deliberate_intent_cannot_be_saved() {
        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        install_managed_hook(&state).await.unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_onboarding_insert
             BEFORE INSERT ON settings
             WHEN NEW.key='hook_onboarding'
             BEGIN SELECT RAISE(FAIL, 'injected'); END",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        assert!(matches!(
            uninstall_managed_hook_outcome(&state).await,
            Err(HookMutationError::Onboarding(_))
        ));
        assert_managed_hook_state_is_coherent(&state).await;
        assert_eq!(
            state.verify_health(ownership_record(&state).await).status,
            HookHealthStatus::Installed
        );
        state.pool.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn partial_uninstall_keeps_intent_and_retry_converges() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        install_managed_hook(&state).await.unwrap();
        let bin = state.app_data.join("bin");
        let original_bin = state.app_data.join("original-bin");
        std::fs::rename(&bin, &original_bin).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, &bin).unwrap();

        assert!(matches!(
            uninstall_managed_hook_outcome(&state).await,
            Err(HookMutationError::Lifecycle(_))
        ));
        let mut connection = state.pool.acquire().await.unwrap();
        assert_eq!(
            crate::hook_onboarding::load(&mut connection).await.unwrap(),
            Some(crate::hook_onboarding::HookOnboardingDisposition::DeliberatelyUninstalled)
        );
        drop(connection);

        std::fs::remove_file(&bin).unwrap();
        std::fs::rename(&original_bin, &bin).unwrap();
        assert!(uninstall_managed_hook(&state).await.unwrap());
        assert_managed_hook_state_is_coherent(&state).await;
        state.pool.close().await;
    }

    #[tokio::test]
    async fn managed_pool_installs_and_removes_owned_hook_and_binary() {
        let dir = tempdir().unwrap();
        let app_data = dir.path().join("app-data");
        std::fs::create_dir_all(&app_data).unwrap();
        let database = app_data.join("state.db");
        let settings = dir.path().join(".claude/settings.json");
        let source = dir.path().join("cc-monitor-hook");
        std::fs::write(&source, b"hook").unwrap();
        let pool = migrated_pool(&database).await;

        install_hook_at(&pool, &app_data, &database, &settings, &source)
            .await
            .unwrap();
        let installation_id: String =
            sqlx::query_scalar("SELECT installation_id FROM installation WHERE singleton = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        let installed: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert!(installed["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(&installation_id));
        assert!(app_data.join("bin/cc-monitor-hook").exists());

        assert!(uninstall_hook_at(&pool, &app_data, &database, &settings)
            .await
            .unwrap());
        let removed: Value = serde_json::from_slice(&std::fs::read(settings).unwrap()).unwrap();
        assert!(removed.get("hooks").is_none());
        assert!(!app_data.join("bin/cc-monitor-hook").exists());
        pool.close().await;
    }

    #[tokio::test]
    async fn malformed_settings_roll_back_new_staged_binary_without_overwrite() {
        let dir = tempdir().unwrap();
        let app_data = dir.path().join("app-data");
        std::fs::create_dir_all(&app_data).unwrap();
        let database = app_data.join("state.db");
        let settings = dir.path().join("settings.json");
        let source = dir.path().join("cc-monitor-hook");
        std::fs::write(&source, b"hook").unwrap();
        std::fs::write(&settings, b"{bad").unwrap();
        let pool = migrated_pool(&database).await;

        assert!(
            install_hook_at(&pool, &app_data, &database, &settings, &source)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(settings).unwrap(), b"{bad");
        assert!(!app_data.join("bin/cc-monitor-hook").exists());
        pool.close().await;
    }

    #[tokio::test]
    async fn failed_repair_restores_previous_staged_binary() {
        let dir = tempdir().unwrap();
        let app_data = dir.path().join("app-data");
        let bin = app_data.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let database = app_data.join("state.db");
        let settings = dir.path().join("settings.json");
        let source = dir.path().join("cc-monitor-hook");
        let staged = bin.join("cc-monitor-hook");
        std::fs::write(&source, b"new-hook").unwrap();
        std::fs::write(&staged, b"previous-hook").unwrap();
        std::fs::write(&settings, b"{bad").unwrap();
        let pool = migrated_pool(&database).await;
        let previous_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO installation (
                singleton, installation_id, hook_path, installed_at_ms, hook_version
             ) VALUES (1, ?1, ?2, 1, 'previous')",
        )
        .bind(previous_id.to_string())
        .bind(staged.to_string_lossy().as_ref())
        .execute(&pool)
        .await
        .unwrap();

        assert!(
            install_hook_at(&pool, &app_data, &database, &settings, &source)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(staged).unwrap(), b"previous-hook");
        assert_eq!(std::fs::read(settings).unwrap(), b"{bad");
        let (restored_id, restored_path): (String, String) = sqlx::query_as(
            "SELECT installation_id, hook_path
             FROM installation WHERE singleton = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(restored_id, previous_id.to_string());
        assert_eq!(
            restored_path,
            app_data.join("bin/cc-monitor-hook").to_string_lossy()
        );
        pool.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_repair_preserves_executable_mode_and_healthy_installation() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let state = lifecycle_state(dir.path()).await;
        install_managed_hook(&state).await.unwrap();
        let staged = state.app_data.join("bin/cc-monitor-hook");
        std::fs::write(&staged, b"healthy-hook").unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_file(&state.hook_source).unwrap();

        let error = install_managed_hook(&state).await.unwrap_err();

        assert_eq!(error.to_string(), "hook_stage_failed");
        assert_eq!(std::fs::read(&staged).unwrap(), b"healthy-hook");
        assert_eq!(
            std::fs::metadata(&staged).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let health = state.verify_health(ownership_record(&state).await);
        assert_eq!(health.status, HookHealthStatus::Installed);
        state.pool.close().await;
    }

    #[tokio::test]
    async fn ownership_put_failure_never_activates_settings() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, br#"{"theme":"dark"}"#).unwrap();
        let original = std::fs::read(&settings).unwrap();
        let store = MockStore::new(None);
        store.fail_put.store(true, Ordering::SeqCst);
        assert!(
            orchestrate_install(&store, &settings, &installation(dir.path()))
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(settings).unwrap(), original);
    }

    #[tokio::test]
    async fn settings_failure_attempts_ownership_cleanup() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, b"{bad").unwrap();
        let store = MockStore::new(None);
        assert!(
            orchestrate_install(&store, &settings, &installation(dir.path()))
                .await
                .is_err()
        );
        assert_eq!(store.clear_attempts.load(Ordering::SeqCst), 1);
        assert!(store.record.lock().unwrap().is_none());
        assert_eq!(std::fs::read(settings).unwrap(), b"{bad");
    }

    #[tokio::test]
    async fn settings_failure_reports_new_ownership_cleanup_failure() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, b"{bad").unwrap();
        let store = MockStore::new(None);
        store.fail_clear.store(1, Ordering::SeqCst);

        let error = orchestrate_install(&store, &settings, &installation(dir.path()))
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "hook_settings_write_failed; ownership_rollback_failed"
        );
        assert_eq!(store.clear_attempts.load(Ordering::SeqCst), 1);
        assert!(store.record.lock().unwrap().is_some());
    }

    #[tokio::test]
    async fn settings_failure_reports_previous_ownership_restore_failure() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, b"{bad").unwrap();
        let previous = installation(&dir.path().join("previous"));
        let replacement = HookInstallation {
            installation_id: previous.installation_id,
            ..installation(&dir.path().join("replacement"))
        };
        let store = MockStore::new(Some(previous));
        store.fail_put_on_attempt.store(2, Ordering::SeqCst);

        let error = orchestrate_install(&store, &settings, &replacement)
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "hook_settings_write_failed; ownership_rollback_failed"
        );
        assert_eq!(store.put_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(*store.record.lock().unwrap(), Some(replacement));
    }

    #[tokio::test]
    async fn uninstall_delete_failure_is_safe_and_retry_self_heals() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        let installation = installation(dir.path());
        install::install(&settings, &installation).unwrap();
        let store = MockStore::new(Some(installation));
        store.fail_clear.store(1, Ordering::SeqCst);

        assert!(orchestrate_uninstall(&store, &settings, |_| Ok(false))
            .await
            .is_err());
        let value: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert!(value.get("hooks").is_none());
        assert!(store.record.lock().unwrap().is_some());

        assert!(!orchestrate_uninstall(&store, &settings, |_| Ok(false))
            .await
            .unwrap());
        assert!(store.record.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn uninstall_never_deletes_unowned_staged_path_or_backup() {
        let dir = tempdir().unwrap();
        let app_data = dir.path().join("app-data");
        let settings = dir.path().join("settings.json");
        let outside = dir.path().join("other-hook");
        std::fs::write(&settings, "{}").unwrap();
        std::fs::write(&outside, b"unrelated").unwrap();
        let installation = HookInstallation {
            staged_hook: outside.clone(),
            ..installation(dir.path())
        };
        install::install(&settings, &installation).unwrap();
        let backup = settings.with_extension("json.cc-monitor-backup");
        let store = MockStore::new(Some(installation));

        assert!(orchestrate_uninstall(&store, &settings, |installation| {
            remove_owned_staged_file(&app_data, installation)
        })
        .await
        .unwrap());
        assert_eq!(std::fs::read(outside).unwrap(), b"unrelated");
        assert!(backup.exists());
    }
}
