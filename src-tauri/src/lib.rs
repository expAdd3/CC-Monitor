#![warn(unreachable_pub)]

mod desktop;
mod hook_lifecycle;
mod hook_onboarding;
mod logging;
#[cfg(target_os = "macos")]
mod macos_notification;
#[cfg(target_os = "macos")]
mod macos_status;

use std::sync::Arc;
use tauri::Manager;

#[derive(Clone, Copy)]
enum IpcError {
    DashboardRead,
    SessionInvalid,
    SessionRead,
    DashboardOpen,
    Cleanup,
    NotificationSettingsOpen,
    SettingsStorage,
    SettingsAutostart,
    NtfySettingsRead,
    ReindexStart,
    ReindexAlreadyRunning,
    DesktopDelivery,
    HookInstall,
    HookOnboarding,
}

impl IpcError {
    fn code(self) -> &'static str {
        match self {
            Self::DashboardRead => "dashboard_read_failed",
            Self::SessionInvalid => "session_id_invalid",
            Self::SessionRead => "session_read_failed",
            Self::DashboardOpen => "dashboard_open_failed",
            Self::Cleanup => "cleanup_failed",
            Self::NotificationSettingsOpen => "notification_settings_open_failed",
            Self::SettingsStorage => "settings_storage_failed",
            Self::SettingsAutostart => "settings_autostart_failed",
            Self::NtfySettingsRead => "ntfy_settings_read_failed",
            Self::ReindexStart => "reindex_start_failed",
            Self::ReindexAlreadyRunning => "reindex_already_running",
            Self::DesktopDelivery => "desktop_delivery_failed",
            Self::HookInstall => "hook_install_failed",
            Self::HookOnboarding => "hook_onboarding_failed",
        }
    }
}

fn fixed_error<E>(code: IpcError) -> impl FnOnce(E) -> String {
    move |_| code.code().to_owned()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // This must stay first: later plugins and setup may touch application
        // state, while a second process must stop before either can run.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let _ = desktop::show_window(app, None);
        }))
        .setup(|app| {
            #[cfg(desktop)]
            app.handle().plugin(tauri_plugin_autostart::init(
                tauri_plugin_autostart::MacosLauncher::LaunchAgent,
                None,
            ))?;
            let app_data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&app_data)?;
            let database = app_data.join("state.db");
            let settings = app.path().home_dir()?.join(".claude/settings.json");
            let hook_source = std::env::current_exe()?
                .parent()
                .ok_or_else(|| anyhow::anyhow!("application executable has no parent"))?
                .join("cc-monitor-hook");
            logging::initialize(&app.path().app_log_dir()?)?;
            let pool = tauri::async_runtime::block_on(async {
                let pool = monitor_storage::connect_and_migrate(&database).await?;
                Ok::<_, anyhow::Error>(pool)
            })
            .map_err(|_| {
                logging::event("database_startup_failed");
                anyhow::anyhow!("database_startup_failed")
            })?;
            let hook_lifecycle = Arc::new(hook_lifecycle::HookLifecycleState::new(
                pool.clone(),
                app_data,
                database,
                settings,
                hook_source,
            ));
            app.manage(hook_lifecycle.clone());
            tauri::async_runtime::block_on(desktop::initialize(
                app.handle(),
                pool,
                hook_lifecycle,
            ))?;
            Ok(())
        })
        .on_window_event(desktop::handle_window_event)
        .invoke_handler(tauri::generate_handler![
            hook_lifecycle::install_claude_hook,
            hook_lifecycle::uninstall_claude_hook,
            desktop::defer_hook_onboarding,
            desktop::get_dashboard_snapshot,
            desktop::get_session_detail,
            desktop::get_settings,
            desktop::save_settings,
            desktop::pricing::list_model_prices,
            desktop::pricing::save_model_price,
            desktop::pricing::delete_model_price,
            desktop::test_ntfy,
            desktop::show_dashboard,
            desktop::clear_completed_history,
            desktop::reindex_transcripts,
            desktop::open_notification_settings,
            desktop::test_desktop_notification
        ])
        .run(tauri::generate_context!())
        .expect("failed to run CC Monitor");
}

#[cfg(test)]
mod tests {
    #[test]
    fn second_instance_reuses_the_shared_window_activation_path() {
        let source = include_str!("lib.rs");
        let callback = source
            .split_once("tauri_plugin_single_instance::init")
            .expect("single-instance plugin must be configured")
            .1
            .split_once(".setup(")
            .expect("single-instance plugin must precede setup")
            .0;

        assert!(callback.contains("desktop::show_window(app, None)"));
        assert!(!callback.contains("get_webview_window"));
    }

    #[test]
    fn every_fixed_ipc_error_rejects_underlying_text() {
        let secret = "/Users/private/transcript.jsonl: database failed";
        for code in [
            super::IpcError::DashboardRead,
            super::IpcError::SessionInvalid,
            super::IpcError::SessionRead,
            super::IpcError::DashboardOpen,
            super::IpcError::Cleanup,
            super::IpcError::NotificationSettingsOpen,
            super::IpcError::SettingsStorage,
            super::IpcError::SettingsAutostart,
            super::IpcError::NtfySettingsRead,
            super::IpcError::ReindexStart,
            super::IpcError::ReindexAlreadyRunning,
            super::IpcError::DesktopDelivery,
            super::IpcError::HookInstall,
            super::IpcError::HookOnboarding,
        ] {
            let mapped = super::fixed_error(code)(anyhow::anyhow!(secret));
            assert_eq!(mapped, code.code());
            assert!(!mapped.contains(secret));
        }
    }
}
