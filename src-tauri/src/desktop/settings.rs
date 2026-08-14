use super::{now_ms, DesktopState};
use monitor_notify::{Notification, NotificationProvider, NtfyConfig, NtfyProvider, Priority};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::sync::Arc;
use tauri::{AppHandle, State};

const NTFY_TEST_BODY: &str = "测试消息已提交，请检查接收设备是否收到。";
use tauri_plugin_autostart::ManagerExt as _;

const SETTINGS_AUTOSTART_FAILED: &str = "settings_autostart_failed";
const SETTINGS_STORAGE_FAILED: &str = "settings_storage_failed";
const SETTINGS_INCONSISTENT: &str = "settings_inconsistent";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettingsDto {
    pub(super) ntfy_enabled: bool,
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
    Ok(settings)
}

pub(super) async fn save_settings(
    app: AppHandle,
    settings: SettingsDto,
    state: State<'_, Arc<DesktopState>>,
) -> Result<SettingsDto, String> {
    validate_settings(&settings)?;
    let store = SqliteSettingsStore {
        pool: state.pool.clone(),
    };
    let mut saved = save_settings_consistently(
        &store,
        &TauriAutostart { app: app.clone() },
        &state.providers,
        settings,
        now_ms(),
    )
    .await
    .map_err(SettingsSaveError::code)?;
    state.invalidate(&app);
    saved.ntfy_password_set = saved
        .ntfy_password
        .as_ref()
        .is_some_and(|value| !value.is_empty());
    saved.ntfy_password = None;
    Ok(saved)
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
) -> Result<SettingsDto, SettingsSaveError> {
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
    // Provider readers cover projection and delivery work. Taking the writer
    // before persistence makes the durable settings/suppression transaction
    // and the in-memory policy switch one observable operation to workers.
    let mut provider_policy = providers.write().await;
    if store.persist(&desired, updated_at_ms).await.is_err() {
        if desired.autostart != previous_autostart
            && autostart.set_enabled(previous_autostart).is_err()
        {
            return Err(SettingsSaveError::Inconsistent);
        }
        return Err(SettingsSaveError::Storage);
    }
    *provider_policy = next_policy;
    Ok(desired)
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
            body: NTFY_TEST_BODY.to_owned(),
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

pub(super) fn notification_policy(
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

    struct BlockingStore {
        previous: SettingsDto,
        persist_entered: Arc<tokio::sync::Barrier>,
        persist_resume: Arc<tokio::sync::Barrier>,
    }

    #[async_trait::async_trait]
    impl SettingsStore for BlockingStore {
        async fn load(&self) -> Result<SettingsDto, ()> {
            Ok(self.previous.clone())
        }

        async fn persist(&self, _: &SettingsDto, _: i64) -> Result<(), ()> {
            self.persist_entered.wait().await;
            self.persist_resume.wait().await;
            Ok(())
        }
    }

    struct DisabledAutostart;

    impl AutostartControl for DisabledAutostart {
        fn is_enabled(&self) -> Result<bool, ()> {
            Ok(false)
        }

        fn set_enabled(&self, enabled: bool) -> Result<(), ()> {
            (!enabled).then_some(()).ok_or(())
        }
    }

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

    #[test]
    fn test_notification_copy_requires_receiver_verification() {
        assert!(NTFY_TEST_BODY.contains("检查接收设备"));
        assert!(!NTFY_TEST_BODY.contains("正常接收"));
        assert!(!NTFY_TEST_BODY.contains("配置有效"));
    }

    #[tokio::test]
    async fn provider_writer_covers_settings_persistence_and_policy_publication() {
        let persist_entered = Arc::new(tokio::sync::Barrier::new(2));
        let persist_resume = Arc::new(tokio::sync::Barrier::new(2));
        let store = Arc::new(BlockingStore {
            previous: SettingsDto::default(),
            persist_entered: persist_entered.clone(),
            persist_resume: persist_resume.clone(),
        });
        let providers = Arc::new(tokio::sync::RwLock::new(NotificationPolicy::desktop_only()));
        let desired = SettingsDto {
            ntfy_enabled: true,
            ntfy_server: "https://ntfy.sh".into(),
            ntfy_topic: "cc-monitor-test".into(),
            ..SettingsDto::default()
        };
        let save_store = store.clone();
        let save_providers = providers.clone();
        let save = tokio::spawn(async move {
            save_settings_consistently(
                save_store.as_ref(),
                &DisabledAutostart,
                save_providers.as_ref(),
                desired,
                1,
            )
            .await
        });

        persist_entered.wait().await;
        assert!(
            providers.try_read().is_err(),
            "workers must not observe a policy between persistence and publication"
        );
        persist_resume.wait().await;
        save.await.unwrap().unwrap();
        assert!(providers.read().await.ntfy_provider().is_some());
    }
}
