use super::{now_ms, DesktopState};
use monitor_notify::{Notification, NotificationProvider, NtfyConfig, NtfyProvider, Priority};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt as _;

const SETTINGS_AUTOSTART_FAILED: &str = "settings_autostart_failed";
const SETTINGS_STORAGE_FAILED: &str = "settings_storage_failed";
const SETTINGS_INCONSISTENT: &str = "settings_inconsistent";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDto {
    pub(super) ntfy_enabled: bool,
    #[serde(default)]
    pub(super) ntfy_activation_pending: bool,
    pub(super) ntfy_server: String,
    pub(super) ntfy_topic: String,
    pub(super) ntfy_username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) ntfy_password: Option<String>,
    pub(super) ntfy_password_set: bool,
    pub(super) autostart: bool,
}

#[derive(Clone)]
pub(super) struct NotificationPolicy {
    providers: Vec<monitor_engine::ProviderKind>,
    ntfy: Option<NtfyProvider>,
}

impl NotificationPolicy {
    pub(super) fn desktop_only() -> Self {
        Self {
            providers: vec![monitor_engine::ProviderKind::Desktop],
            ntfy: None,
        }
    }

    pub(super) fn providers(&self) -> &[monitor_engine::ProviderKind] {
        &self.providers
    }

    pub(super) fn ntfy_provider(&self) -> Option<NtfyProvider> {
        self.ntfy.clone()
    }
}

pub(super) async fn get_settings(
    app: AppHandle,
    state: State<'_, Arc<DesktopState>>,
) -> Result<SettingsDto, String> {
    let mut settings = load_settings(&state.pool)
        .await
        .map_err(super::super::fixed_error(
            super::super::IpcError::SettingsStorage,
        ))?;
    settings.autostart = app
        .autolaunch()
        .is_enabled()
        .map_err(super::super::fixed_error(
            super::super::IpcError::SettingsAutostart,
        ))?;
    settings.ntfy_password_set = settings
        .ntfy_password
        .as_ref()
        .is_some_and(|value| !value.is_empty());
    settings.ntfy_password = None;
    settings.ntfy_activation_pending = false;
    Ok(settings)
}

pub(super) async fn save_settings(
    app: AppHandle,
    settings: SettingsDto,
    state: State<'_, Arc<DesktopState>>,
) -> Result<(), String> {
    validate_settings(&settings)?;
    let store = SqliteSettingsStore {
        pool: state.pool.clone(),
    };
    save_settings_consistently(
        &store,
        &TauriAutostart { app: app.clone() },
        &state.providers,
        settings,
        now_ms(),
    )
    .await
    .map_err(SettingsSaveError::code)?;
    state.invalidate(&app);
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingsSaveError {
    Autostart,
    Storage,
    Inconsistent,
    InvalidConfig(&'static str),
}

impl SettingsSaveError {
    fn code(self) -> String {
        match self {
            Self::Autostart => SETTINGS_AUTOSTART_FAILED,
            Self::Storage => SETTINGS_STORAGE_FAILED,
            Self::Inconsistent => SETTINGS_INCONSISTENT,
            Self::InvalidConfig(code) => code,
        }
        .to_owned()
    }
}

trait AutostartControl: Send + Sync {
    fn is_enabled(&self) -> Result<bool, ()>;
    fn set_enabled(&self, enabled: bool) -> Result<(), ()>;
}

struct TauriAutostart {
    app: AppHandle,
}

impl AutostartControl for TauriAutostart {
    fn is_enabled(&self) -> Result<bool, ()> {
        self.app.autolaunch().is_enabled().map_err(|_| ())
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), ()> {
        if enabled {
            self.app.autolaunch().enable()
        } else {
            self.app.autolaunch().disable()
        }
        .map_err(|_| ())
    }
}

#[async_trait::async_trait]
trait SettingsStore: Send + Sync {
    async fn load(&self) -> Result<SettingsDto, ()>;
    async fn persist(&self, settings: &SettingsDto, updated_at_ms: i64) -> Result<(), ()>;
}

struct SqliteSettingsStore {
    pool: SqlitePool,
}

#[async_trait::async_trait]
impl SettingsStore for SqliteSettingsStore {
    async fn load(&self) -> Result<SettingsDto, ()> {
        load_settings(&self.pool).await.map_err(|_| ())
    }

    async fn persist(&self, settings: &SettingsDto, updated_at_ms: i64) -> Result<(), ()> {
        persist_settings(&self.pool, settings, updated_at_ms)
            .await
            .map_err(|_| ())
    }
}

async fn save_settings_consistently(
    store: &dyn SettingsStore,
    autostart: &dyn AutostartControl,
    providers: &tokio::sync::RwLock<NotificationPolicy>,
    mut desired: SettingsDto,
    updated_at_ms: i64,
) -> Result<(), SettingsSaveError> {
    let previous = store.load().await.map_err(|_| SettingsSaveError::Storage)?;
    let previous_autostart = autostart
        .is_enabled()
        .map_err(|_| SettingsSaveError::Autostart)?;
    if desired.ntfy_password.is_none() {
        desired.ntfy_password = previous.ntfy_password;
    }
    desired.ntfy_password_set = desired
        .ntfy_password
        .as_ref()
        .is_some_and(|value| !value.is_empty());
    let next_policy = notification_policy(&desired)
        .map_err(|error| SettingsSaveError::InvalidConfig(error.code()))?;

    if desired.autostart != previous_autostart {
        autostart
            .set_enabled(desired.autostart)
            .map_err(|_| SettingsSaveError::Autostart)?;
    }
    if store.persist(&desired, updated_at_ms).await.is_err() {
        if desired.autostart != previous_autostart
            && autostart.set_enabled(previous_autostart).is_err()
        {
            return Err(SettingsSaveError::Inconsistent);
        }
        return Err(SettingsSaveError::Storage);
    }
    *providers.write().await = next_policy;
    Ok(())
}

pub(super) async fn test_ntfy(
    mut settings: SettingsDto,
    state: State<'_, Arc<DesktopState>>,
) -> Result<(), String> {
    settings.ntfy_enabled = true;
    validate_settings(&settings)?;
    if settings.ntfy_password.is_none() {
        settings.ntfy_password = load_settings(&state.pool)
            .await
            .map_err(super::super::fixed_error(
                super::super::IpcError::NtfySettingsRead,
            ))?
            .ntfy_password;
    }
    ntfy_provider(&settings)
        .map_err(|error| error.code().to_owned())?
        .send(&Notification {
            title: "CC Monitor 测试通知".to_owned(),
            body: "ntfy 通知配置有效，可以正常接收消息。".to_owned(),
            priority: Priority::Default,
            tag: "white_check_mark".to_owned(),
            session_id: None,
            client_bundle_id: None,
        })
        .await
        .map_err(|error| error.code().to_owned())
}

pub(super) async fn load_settings(pool: &SqlitePool) -> anyhow::Result<SettingsDto> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value_json FROM settings WHERE key='desktop'")
            .fetch_optional(pool)
            .await?;
    Ok(value
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default())
}

pub(super) async fn load_notification_policy_read_only(
    pool: &SqlitePool,
) -> anyhow::Result<NotificationPolicy> {
    notification_policy(&load_settings(pool).await?).map_err(Into::into)
}

async fn persist_settings(
    pool: &SqlitePool,
    settings: &SettingsDto,
    updated_at_ms: i64,
) -> anyhow::Result<()> {
    let value = serde_json::to_string(settings)?;
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO settings(key,value_json,updated_at_ms)
         VALUES('desktop',?1,?2)
         ON CONFLICT(key) DO UPDATE SET
            value_json=excluded.value_json,updated_at_ms=excluded.updated_at_ms",
    )
    .bind(value)
    .bind(updated_at_ms)
    .execute(&mut *tx)
    .await?;
    if !settings.ntfy_enabled {
        sqlx::query(
            "UPDATE notification_outbox
                SET status='suppressed',next_attempt_at_ms=NULL,last_error='ntfy_disabled'
              WHERE provider='ntfy' AND status IN ('pending','failed','inflight')",
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub(super) fn validate_settings(settings: &SettingsDto) -> Result<(), String> {
    if settings.ntfy_enabled {
        ntfy_provider(settings).map_err(|error| error.code().to_owned())?;
    }
    for (code, value) in [
        ("ntfy_server_too_long", &settings.ntfy_server),
        ("ntfy_topic_too_long", &settings.ntfy_topic),
        ("ntfy_username_too_long", &settings.ntfy_username),
    ] {
        if value.len() > 2048 {
            return Err(code.to_owned());
        }
    }
    if settings
        .ntfy_password
        .as_ref()
        .is_some_and(|value| value.len() > 4096)
    {
        return Err("ntfy_password_too_long".to_owned());
    }
    Ok(())
}

pub(super) fn ntfy_provider(
    settings: &SettingsDto,
) -> Result<NtfyProvider, monitor_notify::NotifyError> {
    NtfyProvider::new(NtfyConfig {
        server: settings.ntfy_server.clone(),
        topic: settings.ntfy_topic.clone(),
        username: settings.ntfy_username.clone(),
        password: settings.ntfy_password.clone().unwrap_or_default(),
    })
}

fn notification_policy(
    settings: &SettingsDto,
) -> Result<NotificationPolicy, monitor_notify::NotifyError> {
    let ntfy = settings
        .ntfy_enabled
        .then(|| ntfy_provider(settings))
        .transpose()?;
    let mut providers = vec![monitor_engine::ProviderKind::Desktop];
    if ntfy.is_some() {
        providers.push(monitor_engine::ProviderKind::Ntfy);
    }
    Ok(NotificationPolicy { providers, ntfy })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_ntfy_does_not_require_configuration() {
        assert!(validate_settings(&SettingsDto::default()).is_ok());
    }

    #[test]
    fn enabled_ntfy_requires_a_valid_endpoint_and_topic() {
        let settings = SettingsDto {
            ntfy_enabled: true,
            ntfy_server: "https://ntfy.sh".into(),
            ntfy_topic: "cc-monitor-test".into(),
            ..SettingsDto::default()
        };
        assert!(validate_settings(&settings).is_ok());
    }
}
