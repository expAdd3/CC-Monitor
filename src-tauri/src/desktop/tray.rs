use super::{DesktopState, TraySnapshot, UsageSummary};
use async_trait::async_trait;
use chrono::{Local, TimeZone};
use monitor_notify::DesktopTransport;
use std::sync::{atomic::Ordering, Arc};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, Runtime, Window, WindowEvent,
};
#[cfg(not(target_os = "macos"))]
use tauri_plugin_notification::NotificationExt as _;

pub(super) fn build_initial(app: &AppHandle) -> tauri::Result<()> {
    let menu = Menu::new(app)?;
    let loading = MenuItem::with_id(app, "status", "正在载入…", false, None::<&str>)?;
    let dashboard = MenuItem::with_id(app, "dashboard", "打开仪表盘", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 CC Monitor", true, None::<&str>)?;
    menu.append(&loading)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&dashboard)?;
    menu.append(&settings)?;
    menu.append(&quit)?;
    TrayIconBuilder::with_id("main")
        .icon(
            app.default_window_icon()
                .expect("application icon must exist")
                .clone(),
        )
        .icon_as_template(true)
        .tooltip("CC Monitor")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "dashboard" => {
                let _ = show_window(app, Some("dashboard"));
            }
            "settings" => {
                let _ = show_window(app, Some("settings"));
            }
            "quit" => {
                if let Some(state) = app.try_state::<Arc<DesktopState>>() {
                    state.quitting.store(true, Ordering::SeqCst);
                }
                app.exit(0);
            }
            id if id.starts_with("session:") => {
                let _ = show_window(app, Some(id));
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

pub(super) fn handle_window_event(window: &Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        let quitting = window
            .app_handle()
            .try_state::<Arc<DesktopState>>()
            .is_some_and(|state| state.quitting.load(Ordering::SeqCst));
        if !quitting {
            api.prevent_close();
            let _ = window.hide();
            #[cfg(target_os = "macos")]
            let _ = window
                .app_handle()
                .set_activation_policy(tauri::ActivationPolicy::Accessory);
        }
    }
}

pub(super) fn fingerprint(snapshot: &TraySnapshot, _current_ms: i64) -> String {
    let mut value = format!(
        "{}:{}:{}:{}:{}:{}:{}:{}",
        snapshot.counts.running,
        snapshot.counts.waiting,
        snapshot.counts.needs_input,
        snapshot.counts.failed,
        total_tokens(&snapshot.today),
        snapshot.today.cost_pico_usd,
        snapshot.today.cost_known,
        snapshot.today.unpriced_tokens
    );
    for day in &snapshot.trends {
        value.push_str(&format!(
            "|{}:{}:{}:{}:{}",
            day.day, day.tokens, day.cost_pico_usd, day.cost_known, day.unpriced_tokens
        ));
    }
    for session in &snapshot.sessions {
        value.push_str(&format!(
            "|{}:{}:{}:{}:{}:{}:{}:{}",
            session.session_id,
            session.project_name.as_deref().unwrap_or_default(),
            session.turn_state,
            session.last_observed_at_ms,
            total_tokens(&session.usage),
            session.usage.cost_pico_usd,
            session.usage.cost_known,
            session.usage.unpriced_tokens
        ));
    }
    value
}

pub(super) fn refresh<R: Runtime>(
    app: &AppHandle<R>,
    snapshot: &TraySnapshot,
) -> tauri::Result<()> {
    let menu = Menu::new(app)?;
    #[cfg(target_os = "macos")]
    let mut session_rows = Vec::new();
    menu.append(&MenuItem::with_id(
        app,
        "product-heading",
        "CC Monitor",
        false,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "status",
        tray_status_summary(
            snapshot.counts.running,
            snapshot.counts.waiting,
            snapshot.counts.needs_input,
            snapshot.counts.failed,
        ),
        false,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "today",
        tray_today_summary(&snapshot.today),
        false,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "usage-window",
        tray_window_summary(&snapshot.trends),
        false,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        "sessions-heading",
        format!("最近活跃会话（{}）", snapshot.sessions.len()),
        false,
        None::<&str>,
    )?)?;
    if snapshot.sessions.is_empty() {
        menu.append(&MenuItem::with_id(
            app,
            "empty",
            "暂无活跃会话",
            false,
            None::<&str>,
        )?)?;
    } else {
        for session in &snapshot.sessions {
            // Project and session identifiers originate outside the app. Keep
            // control characters out of AppKit menu and attributed-string
            // titles so one hostile label cannot create visual menu rows.
            let label = session
                .project_name
                .as_deref()
                .map(native_menu_label)
                .filter(|name| name != "—")
                .unwrap_or_else(|| "未命名会话".to_owned());
            let title = tray_session_title(session, &label);
            let detail = tray_session_detail(session);
            #[cfg(target_os = "macos")]
            session_rows.push(crate::macos_status::SessionMenuRow {
                primary: title.clone(),
                detail: detail.clone(),
                state: session.turn_state.clone(),
            });
            menu.append(&MenuItem::with_id(
                app,
                format!("session:{}", session.session_id),
                title,
                true,
                None::<&str>,
            )?)?;
            menu.append(&MenuItem::with_id(
                app,
                format!("session-meta:{}", session.session_id),
                detail,
                false,
                None::<&str>,
            )?)?;
        }
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        "dashboard",
        "打开仪表盘",
        true,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "settings",
        "设置",
        true,
        None::<&str>,
    )?)?;
    menu.append(&MenuItem::with_id(
        app,
        "quit",
        "退出 CC Monitor",
        true,
        None::<&str>,
    )?)?;
    if let Some(tray) = app.tray_by_id("main") {
        tray.set_title(None::<&str>)?;
        tray.set_tooltip(Some(&format!(
            "CC Monitor · {} running · {} waiting · {} needs input · {} tokens today",
            snapshot.counts.running,
            snapshot.counts.waiting,
            snapshot.counts.needs_input,
            compact_number(total_tokens(&snapshot.today))
        )))?;
        tray.set_menu(Some(menu))?;
        #[cfg(target_os = "macos")]
        {
            let app = app.clone();
            let running = snapshot.counts.running;
            let waiting = snapshot.counts.waiting;
            let needs_input = snapshot.counts.needs_input;
            app.clone().run_on_main_thread(move || {
                if let Some(tray) = app.tray_by_id("main") {
                    crate::macos_status::update(&tray, running, waiting, needs_input);
                    crate::macos_status::style_menu_rows(&tray, session_rows);
                }
            })?;
        }
    }
    Ok(())
}

pub(super) fn show_window(app: &AppHandle, route: Option<&str>) -> tauri::Result<()> {
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Regular)?;
    if let Some(window) = app.get_webview_window("dashboard") {
        window.show()?;
        window.unminimize()?;
        window.set_focus()?;
        if let Some(route) = route {
            let _ = app.emit("monitor://navigate", route.to_owned());
        }
    }
    Ok(())
}

pub(super) struct TauriDesktopTransport(pub(super) AppHandle);

#[async_trait]
impl DesktopTransport for TauriDesktopTransport {
    async fn show(&self, notification: &monitor_notify::Notification) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            crate::macos_notification::show(self.0.clone(), notification.clone()).await
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.0
                .notification()
                .builder()
                .title(&notification.title)
                .body(&notification.body)
                .show()
                .map_err(super::super::fixed_error(
                    super::super::IpcError::DesktopDelivery,
                ))
        }
    }
}

pub(super) fn total_tokens(usage: &UsageSummary) -> i64 {
    usage
        .input_tokens
        .saturating_add(usage.output_tokens)
        .saturating_add(usage.cache_write_tokens)
        .saturating_add(usage.cache_read_tokens)
}

pub(super) fn compact_number(value: i64) -> String {
    if value >= 1_000_000 {
        format_decimal(value, 1_000_000, "m")
    } else if value >= 1_000 {
        format_decimal(value, 1_000, "k")
    } else {
        value.to_string()
    }
}

pub(super) fn format_cost(pico_usd: i64, known: bool) -> String {
    if !known {
        return "费用待定".to_owned();
    }
    // One displayed unit is $0.0001, or 100 million pico-USD. Divide before
    // rounding instead of multiplying the input so even a saturated database
    // total remains representable here.
    const PICO_PER_FOUR_DECIMALS: i64 = 100_000_000;
    let scaled = pico_usd / PICO_PER_FOUR_DECIMALS
        + i64::from(pico_usd % PICO_PER_FOUR_DECIMALS >= PICO_PER_FOUR_DECIMALS / 2);
    format!("${}.{:04}", scaled / 10_000, scaled % 10_000)
}

pub(super) fn tray_cost_summary(usage: &UsageSummary) -> String {
    let tokens = total_tokens(usage);
    if usage.unpriced_tokens > 0 && usage.unpriced_tokens >= tokens && tokens > 0 {
        return "费用待定".to_owned();
    }
    if usage.unpriced_tokens > 0 {
        return format!(
            "{} · {} 未计",
            format_cost(usage.cost_pico_usd, true),
            compact_number(usage.unpriced_tokens)
        );
    }
    format_cost(usage.cost_pico_usd, usage.cost_known)
}

fn tray_session_title(session: &super::SessionRow, label: &str) -> String {
    format!(
        "{} · #{}\t{} Token",
        truncate_menu_label(label, 20),
        native_menu_label(&short_session_id(&session.session_id)),
        compact_number(total_tokens(&session.usage)),
    )
}

fn truncate_menu_label(value: &str, width: usize) -> String {
    let mut characters = value.chars();
    let visible: String = characters.by_ref().take(width).collect();
    if characters.next().is_some() && width > 1 {
        format!("{}…", visible.chars().take(width - 1).collect::<String>())
    } else {
        visible
    }
}

fn tray_session_detail(session: &super::SessionRow) -> String {
    format!(
        "{}\t{}",
        activity_time(session.last_observed_at_ms),
        tray_cost_summary(&session.usage),
    )
}

pub(super) fn tray_today_summary(usage: &UsageSummary) -> String {
    let tokens = total_tokens(usage);
    let cost = if usage.unpriced_tokens > 0 && usage.unpriced_tokens >= tokens && tokens > 0 {
        "费用待定".to_owned()
    } else if usage.unpriced_tokens > 0 {
        format!(
            "{} · {} 未计",
            format_cost(usage.cost_pico_usd, true),
            compact_number(usage.unpriced_tokens)
        )
    } else {
        format_cost(usage.cost_pico_usd, usage.cost_known)
    };
    format!("今日 {} Token · {}", compact_number(tokens), cost)
}

pub(super) fn tray_window_summary(trends: &[super::TrendDay]) -> String {
    let seven_start = Local::now().date_naive() - chrono::Duration::days(6);
    let mut seven_tokens = 0_i64;
    let mut thirty_tokens = 0_i64;
    for day in trends {
        thirty_tokens = thirty_tokens.saturating_add(day.tokens);
        if day.day.as_str() >= seven_start.to_string().as_str() {
            seven_tokens = seven_tokens.saturating_add(day.tokens);
        }
    }
    format!(
        "近 7 天 {} Token · 30 天 {} Token",
        compact_number(seven_tokens),
        compact_number(thirty_tokens)
    )
}

fn format_decimal(value: i64, divisor: i64, suffix: &str) -> String {
    let whole = value / divisor;
    let tenth = value % divisor * 10 / divisor;
    format!("{whole}.{tenth}{suffix}")
}

pub(super) fn tray_status_summary(
    running: i64,
    waiting: i64,
    needs_input: i64,
    failed: i64,
) -> String {
    if needs_input > 0 {
        return format!("{needs_input} 个会话需要介入");
    }
    if failed > 0 {
        return format!("{failed} 个会话失败");
    }
    let active = running.saturating_add(waiting);
    if active == 0 {
        "当前没有活跃会话".to_owned()
    } else if running > 0 && waiting > 0 {
        format!("{running} 个运行中 · {waiting} 个等待中")
    } else if running > 0 {
        format!("{running} 个会话运行中")
    } else {
        format!("{waiting} 个会话等待中")
    }
}

pub(super) fn short_session_id(session_id: &str) -> String {
    session_id
        .chars()
        .filter(|value| *value != '-')
        .take(6)
        .collect()
}

pub(super) fn native_menu_label(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "—".to_owned()
    } else {
        collapsed
    }
}

fn activity_time(observed_ms: i64) -> String {
    let Some(observed) = Local.timestamp_millis_opt(observed_ms).single() else {
        return "—".to_owned();
    };
    if observed.date_naive() == Local::now().date_naive() {
        observed.format("%H:%M").to_string()
    } else {
        observed.format("%m-%d").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_menu_uses_primary_title_and_secondary_metadata() {
        let session = super::super::SessionRow {
            session_id: "1a3aa0ff-1234".into(),
            project_name: Some("A very long project name".into()),
            turn_state: "waiting".into(),
            state_source: "hook".into(),
            state_reason: "turn_stopped".into(),
            changed_at_ms: 0,
            last_observed_at_ms: 0,
            revision: 1,
            usage: UsageSummary {
                input_tokens: 1_000,
                cost_pico_usd: 14_300_000_000,
                cost_known: true,
                ..UsageSummary::default()
            },
        };
        let title = tray_session_title(&session, "A very long project name");
        let detail = tray_session_detail(&session);
        assert_eq!(title, "A very long project… · #1a3aa0\t1.0k Token");
        assert_eq!(detail, "01-01\t$0.0143");
        assert!(!title.contains('\n'));
        assert!(!detail.contains('\n'));
    }

    #[test]
    fn fingerprint_changes_when_rendered_cost_coverage_changes() {
        let mut snapshot = TraySnapshot {
            counts: super::super::Counts::default(),
            today: UsageSummary {
                input_tokens: 10,
                cost_pico_usd: 5,
                cost_known: true,
                ..UsageSummary::default()
            },
            trends: Vec::new(),
            sessions: Vec::new(),
        };
        let priced = fingerprint(&snapshot, 0);
        snapshot.today.cost_known = false;
        snapshot.today.unpriced_tokens = 10;
        assert_ne!(priced, fingerprint(&snapshot, 0));
    }

    #[test]
    fn window_summary_keeps_seven_and_thirty_day_usage_on_one_row() {
        let today = Local::now().date_naive();
        let trends = vec![
            super::super::TrendDay {
                day: today.to_string(),
                tokens: 2_000,
                cost_pico_usd: 0,
                cost_known: true,
                unpriced_tokens: 0,
            },
            super::super::TrendDay {
                day: (today - chrono::Duration::days(10)).to_string(),
                tokens: 3_000,
                cost_pico_usd: 0,
                cost_known: true,
                unpriced_tokens: 0,
            },
        ];
        assert_eq!(
            tray_window_summary(&trends),
            "近 7 天 2.0k Token · 30 天 5.0k Token"
        );
    }
}
