use adapter_claude::terminal_identity::for_bundle_id;
use monitor_notify::Notification;
use std::path::Path;
use tauri::AppHandle;

pub fn initialize() {
    tauri::async_runtime::spawn(async {
        // macOS presents the prompt only while the authorization state is
        // undetermined. Repeating this call after a decision is harmless and
        // lets the OS preserve the user's choice without app-side state.
        if request_authorization().await.is_err() {
            crate::logging::event("notification_permission_request_failed");
        }
    });
}

pub async fn request_authorization() -> Result<(), &'static str> {
    let executable =
        std::env::current_exe().map_err(|_| "desktop_notification_requires_app_bundle")?;
    if !is_app_bundle_executable(&executable) {
        return Err("desktop_notification_requires_app_bundle");
    }
    match notify_rust::request_auth().await {
        Ok(granted) => authorization_result(granted),
        Err(_) => Err("desktop_notification_permission_request_failed"),
    }
}

fn is_app_bundle_executable(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "Contents")
        && path
            .components()
            .any(|component| component.as_os_str() == "MacOS")
        && path
            .ancestors()
            .any(|ancestor| ancestor.extension().is_some_and(|value| value == "app"))
}

fn authorization_result(granted: bool) -> Result<(), &'static str> {
    granted
        .then_some(())
        .ok_or("desktop_notification_permission_denied")
}

pub async fn show(app: AppHandle, notification: Notification) -> Result<(), String> {
    let title = notification.title.clone();
    let body = notification.body.clone();
    let action_label = if notification
        .client_bundle_id
        .as_deref()
        .and_then(for_bundle_id)
        .is_some()
    {
        "回到终端"
    } else {
        "查看会话"
    };
    let handle = tauri::async_runtime::spawn_blocking(move || {
        let mut builder = notify_rust::Notification::new();
        builder
            .summary(&title)
            .body(&body)
            .action("open_session", action_label);
        builder
            .show()
            .map_err(super::fixed_error(super::IpcError::DesktopDelivery))
    })
    .await
    .map_err(super::fixed_error(super::IpcError::DesktopDelivery))??;

    tauri::async_runtime::spawn_blocking(move || {
        handle.wait_for_action(move |action| {
            if is_activation_action(action) {
                route_click(&app, notification.session_id, notification.client_bundle_id);
            }
        });
    });
    Ok(())
}

fn route_click(app: &AppHandle, session_id: Option<String>, bundle_id: Option<String>) {
    let route = session_id
        .as_deref()
        .map(|value| format!("session:{value}"))
        .unwrap_or_else(|| "dashboard".to_owned());
    let _ = super::desktop::show_dashboard(app.clone(), Some(route));

    if let Some(bundle_id) = bundle_id.as_deref() {
        activate_terminal(bundle_id);
    }
}

fn is_activation_action(action: &str) -> bool {
    matches!(action, "default" | "open_session")
}

fn activate_terminal(value: &str) {
    let Some(identity) = for_bundle_id(value) else {
        return;
    };
    let activated = std::process::Command::new("/usr/bin/open")
        .args(["-b", identity.canonical_bundle_id()])
        .status()
        .is_ok_and(|status| status.success());
    if !activated && std::path::Path::new(identity.application_path()).exists() {
        let _ = std::process::Command::new("/usr/bin/open")
            .args(["-a", identity.application_path()])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::is_activation_action;

    #[test]
    fn authorization_requires_an_app_bundle_executable() {
        assert!(super::is_app_bundle_executable(std::path::Path::new(
            "/Applications/CC Monitor.app/Contents/MacOS/cc-monitor"
        )));
        assert!(!super::is_app_bundle_executable(std::path::Path::new(
            "/tmp/cc-monitor"
        )));
    }

    #[test]
    fn notification_body_and_explicit_return_action_both_route_clicks() {
        assert!(is_activation_action("default"));
        assert!(is_activation_action("open_session"));
        assert!(!is_activation_action("__closed"));
    }

    #[test]
    fn denied_notification_authorization_has_a_fixed_user_safe_error() {
        assert_eq!(
            super::authorization_result(false),
            Err("desktop_notification_permission_denied")
        );
    }
}
