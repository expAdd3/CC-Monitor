use adapter_claude::install::{self, HookInstallation};
use async_trait::async_trait;
use std::path::Path;
use tauri::{AppHandle, Manager};
use uuid::Uuid;

#[tauri::command]
async fn install_claude_hook(app: AppHandle) -> Result<(), String> {
    let app_data = app.path().app_data_dir().map_err(safe_error)?;
    let settings = app
        .path()
        .home_dir()
        .map_err(safe_error)?
        .join(".claude/settings.json");
    let hook_source = std::env::current_exe()
        .map_err(safe_error)?
        .parent()
        .ok_or_else(|| "application executable has no parent".to_owned())?
        .join("cc-monitor-hook");
    install_hook_at(&app_data, &settings, &hook_source)
        .await
        .map_err(safe_error)
}

#[tauri::command]
async fn uninstall_claude_hook(app: AppHandle) -> Result<bool, String> {
    let app_data = app.path().app_data_dir().map_err(safe_error)?;
    let settings = app
        .path()
        .home_dir()
        .map_err(safe_error)?
        .join(".claude/settings.json");
    uninstall_hook_at(&app_data, &settings)
        .await
        .map_err(safe_error)
}

async fn install_hook_at(
    app_data: &Path,
    settings: &Path,
    hook_source: &Path,
) -> Result<(), anyhow::Error> {
    std::fs::create_dir_all(app_data)?;
    let database = app_data.join("state.db");
    let pool = monitor_storage::connect(&database).await?;
    monitor_storage::migrate(&pool).await?;

    let store = SqliteOwnershipStore {
        pool: &pool,
        database: &database,
    };
    let installation_id = store
        .load()
        .await?
        .map(|value| value.installation_id)
        .unwrap_or_else(Uuid::now_v7);
    let staged_hook = install::stage_hook(hook_source, app_data)?;
    let installation = HookInstallation {
        installation_id,
        staged_hook,
        database: database.clone(),
    };
    orchestrate_install(&store, settings, &installation).await?;
    pool.close().await;
    Ok(())
}

async fn uninstall_hook_at(app_data: &Path, settings: &Path) -> Result<bool, anyhow::Error> {
    let database = app_data.join("state.db");
    if !database.exists() {
        return Ok(false);
    }
    let pool = monitor_storage::connect(&database).await?;
    let store = SqliteOwnershipStore {
        pool: &pool,
        database: &database,
    };
    let changed = orchestrate_uninstall(&store, settings).await?;
    pool.close().await;
    Ok(changed)
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
    if let Err(error) = install::install(settings, installation) {
        if let Some(previous) = previous {
            let _ = store.put(&previous).await;
        } else {
            let _ = store.clear(installation.installation_id).await;
        }
        return Err(error.into());
    }
    Ok(())
}

async fn orchestrate_uninstall(
    store: &impl OwnershipStore,
    settings: &Path,
) -> anyhow::Result<bool> {
    let Some(installation) = store.load().await? else {
        return Ok(false);
    };
    let removed = install::uninstall(settings, &installation)?.is_some();
    // If the exact command is already absent, clearing the stale record makes
    // retrying an interrupted uninstall self-healing.
    store.clear(installation.installation_id).await?;
    Ok(removed)
}

fn safe_error(error: impl std::fmt::Display) -> String {
    let text = error.to_string();
    text.chars().take(256).collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            install_claude_hook,
            uninstall_claude_hook
        ])
        .run(tauri::generate_context!())
        .expect("failed to run CC Monitor");
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
        fail_clear: AtomicUsize,
        clear_attempts: AtomicUsize,
    }

    impl MockStore {
        fn new(record: Option<HookInstallation>) -> Self {
            Self {
                record: Mutex::new(record),
                fail_put: AtomicBool::new(false),
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
            if self.fail_put.load(Ordering::SeqCst) {
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

    #[tokio::test]
    async fn app_migrates_database_before_install_and_can_remove_its_hook() {
        let dir = tempdir().unwrap();
        let app_data = dir.path().join("app-data");
        let settings = dir.path().join(".claude/settings.json");
        let source = dir.path().join("cc-monitor-hook");
        std::fs::write(&source, b"hook").unwrap();

        install_hook_at(&app_data, &settings, &source)
            .await
            .unwrap();
        let pool = monitor_storage::connect(&app_data.join("state.db"))
            .await
            .unwrap();
        let installation_id: String =
            sqlx::query_scalar("SELECT installation_id FROM installation WHERE singleton = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        pool.close().await;
        let installed: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert!(installed["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(&installation_id));

        assert!(uninstall_hook_at(&app_data, &settings).await.unwrap());
        let removed: Value = serde_json::from_slice(&std::fs::read(settings).unwrap()).unwrap();
        assert!(removed["hooks"]["Stop"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn malformed_settings_prevent_install_without_overwrite() {
        let dir = tempdir().unwrap();
        let app_data = dir.path().join("app-data");
        let settings = dir.path().join("settings.json");
        let source = dir.path().join("cc-monitor-hook");
        std::fs::write(&source, b"hook").unwrap();
        std::fs::write(&settings, b"{bad").unwrap();

        assert!(install_hook_at(&app_data, &settings, &source)
            .await
            .is_err());
        assert_eq!(std::fs::read(settings).unwrap(), b"{bad");
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
    async fn uninstall_delete_failure_is_safe_and_retry_self_heals() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        let installation = installation(dir.path());
        install::install(&settings, &installation).unwrap();
        let store = MockStore::new(Some(installation));
        store.fail_clear.store(1, Ordering::SeqCst);

        assert!(orchestrate_uninstall(&store, &settings).await.is_err());
        let value: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert!(value["hooks"]["Stop"].as_array().unwrap().is_empty());
        assert!(store.record.lock().unwrap().is_some());

        assert!(!orchestrate_uninstall(&store, &settings).await.unwrap());
        assert!(store.record.lock().unwrap().is_none());
    }
}
