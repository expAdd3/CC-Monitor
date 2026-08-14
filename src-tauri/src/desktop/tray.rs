use super::{DesktopState, TraySnapshot, UsageSummary};
use async_trait::async_trait;
use chrono::{Local, TimeZone};
use monitor_notify::{NotificationProvider, NotifyError};
use std::sync::{atomic::Ordering, Arc};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, Runtime, Window, WindowEvent,
};

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

pub(super) fn fingerprint(snapshot: &TraySnapshot, current_ms: i64) -> String {
    let local_day = Local
        .timestamp_millis_opt(current_ms)
        .single()
        .map(|value| value.date_naive().to_string())
        .unwrap_or_default();
    let mut value = format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}",
        local_day,
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
    let style_rows = tray_menu_rows(snapshot);
    for row in &style_rows {
        match row {
            TrayMenuRow::Item {
                id, title, enabled, ..
            } => menu.append(&MenuItem::with_id(app, id, title, *enabled, None::<&str>)?)?,
            TrayMenuRow::Separator => menu.append(&PredefinedMenuItem::separator(app)?)?,
        }
    }
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
                    crate::macos_status::style_menu_rows(&tray, style_rows);
                }
            })?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TrayMenuRow {
    Item {
        id: String,
        title: String,
        enabled: bool,
        kind: TrayMenuRowKind,
    },
    Separator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TrayMenuRowKind {
    Product,
    Status {
        urgent: bool,
    },
    Today,
    UsageWindow,
    SessionsHeading,
    Empty,
    SessionPrimary {
        state: String,
        accessibility_label: String,
    },
    SessionMeta,
    Footer,
}

impl TrayMenuRow {
    fn item(
        id: impl Into<String>,
        title: impl Into<String>,
        enabled: bool,
        kind: TrayMenuRowKind,
    ) -> Self {
        Self::Item {
            id: id.into(),
            title: title.into(),
            enabled,
            kind,
        }
    }
}

fn tray_menu_rows(snapshot: &TraySnapshot) -> Vec<TrayMenuRow> {
    let mut rows = vec![TrayMenuRow::item(
        "product-heading",
        "CC Monitor",
        false,
        TrayMenuRowKind::Product,
    )];
    if !snapshot.sessions.is_empty() {
        rows.push(TrayMenuRow::item(
            "status",
            tray_status_summary(
                snapshot.counts.running,
                snapshot.counts.waiting,
                snapshot.counts.needs_input,
                snapshot.counts.failed,
            ),
            false,
            TrayMenuRowKind::Status {
                urgent: snapshot.counts.needs_input > 0 || snapshot.counts.failed > 0,
            },
        ));
    }
    rows.push(TrayMenuRow::item(
        "today",
        tray_today_summary(&snapshot.today),
        false,
        TrayMenuRowKind::Today,
    ));
    rows.push(TrayMenuRow::item(
        "usage-window",
        tray_window_summary(&snapshot.trends),
        false,
        TrayMenuRowKind::UsageWindow,
    ));
    rows.push(TrayMenuRow::Separator);

    if snapshot.sessions.is_empty() {
        rows.push(TrayMenuRow::item(
            "empty",
            "暂无活跃会话",
            false,
            TrayMenuRowKind::Empty,
        ));
    } else {
        rows.push(TrayMenuRow::item(
            "sessions-heading",
            "最近活跃会话",
            false,
            TrayMenuRowKind::SessionsHeading,
        ));
        for session in &snapshot.sessions {
            // Project and session identifiers originate outside the app. Keep
            // control characters out of both visual and accessibility labels.
            let label = session
                .project_name
                .as_deref()
                .map(native_menu_label)
                .filter(|name| name != "—")
                .unwrap_or_else(|| "未命名会话".to_owned());
            rows.push(TrayMenuRow::item(
                format!("session:{}", session.session_id),
                tray_session_title(session, &label),
                true,
                TrayMenuRowKind::SessionPrimary {
                    state: session.turn_state.clone(),
                    accessibility_label: tray_session_accessibility_label(session, &label),
                },
            ));
            rows.push(TrayMenuRow::item(
                format!("session-meta:{}", session.session_id),
                tray_session_detail(session),
                false,
                TrayMenuRowKind::SessionMeta,
            ));
        }
    }

    rows.push(TrayMenuRow::Separator);
    rows.push(TrayMenuRow::item(
        "dashboard",
        "打开仪表盘",
        true,
        TrayMenuRowKind::Footer,
    ));
    rows.push(TrayMenuRow::item(
        "settings",
        "设置",
        true,
        TrayMenuRowKind::Footer,
    ));
    rows.push(TrayMenuRow::item(
        "quit",
        "退出 CC Monitor",
        true,
        TrayMenuRowKind::Footer,
    ));
    rows
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

pub(super) struct TauriDesktopNotifier(pub(super) AppHandle);

#[async_trait]
impl NotificationProvider for TauriDesktopNotifier {
    async fn send(&self, notification: &monitor_notify::Notification) -> Result<(), NotifyError> {
        #[cfg(target_os = "macos")]
        {
            crate::macos_notification::show(self.0.clone(), notification.clone())
                .await
                .map_err(|_| NotifyError::Delivery("delivery_failed"))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = notification;
            Err(NotifyError::Delivery("delivery_failed"))
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

fn tray_session_accessibility_label(session: &super::SessionRow, label: &str) -> String {
    format!(
        "{label}，{}，会话 {}，{} Token，费用覆盖：{}，最近活动 {}",
        tray_session_state_label(&session.turn_state),
        native_menu_label(&short_session_id(&session.session_id)),
        compact_number(total_tokens(&session.usage)),
        tray_cost_summary(&session.usage),
        activity_time(session.last_observed_at_ms),
    )
}

fn tray_session_state_label(state: &str) -> &'static str {
    match state {
        "running" => "运行中",
        "waiting" => "等待中",
        "needs_input" => "需要介入",
        "failed" => "失败",
        _ => "状态未知",
    }
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

    fn menu_item_rows(rows: Vec<TrayMenuRow>) -> Vec<(String, String, bool)> {
        rows.into_iter()
            .filter_map(|row| match row {
                TrayMenuRow::Item {
                    id, title, enabled, ..
                } => Some((id, title, enabled)),
                TrayMenuRow::Separator => None,
            })
            .collect()
    }

    fn test_session() -> super::super::SessionRow {
        super::super::SessionRow {
            session_id: "1a3aa0ff-1234".into(),
            project_name: Some("Demo".into()),
            turn_state: "waiting".into(),
            state_source: "hook".into(),
            state_reason: "turn_stopped".into(),
            changed_at_ms: 0,
            last_observed_at_ms: Local
                .with_ymd_and_hms(2024, 3, 4, 12, 0, 0)
                .single()
                .expect("test date must exist in the local timezone")
                .timestamp_millis(),
            revision: 1,
            usage: UsageSummary {
                input_tokens: 1_000,
                cost_pico_usd: 14_300_000_000,
                cost_known: true,
                ..UsageSummary::default()
            },
        }
    }

    #[test]
    fn empty_session_menu_has_one_empty_state_row() {
        let snapshot = TraySnapshot {
            counts: super::super::Counts {
                running: 2,
                ..super::super::Counts::default()
            },
            today: UsageSummary {
                cost_known: true,
                ..UsageSummary::default()
            },
            trends: Vec::new(),
            sessions: Vec::new(),
        };
        let rows = tray_menu_rows(&snapshot);
        assert!(matches!(
            rows[0],
            TrayMenuRow::Item {
                kind: TrayMenuRowKind::Product,
                ..
            }
        ));
        assert!(matches!(
            rows[1],
            TrayMenuRow::Item {
                kind: TrayMenuRowKind::Today,
                ..
            }
        ));
        assert!(matches!(
            rows[2],
            TrayMenuRow::Item {
                kind: TrayMenuRowKind::UsageWindow,
                ..
            }
        ));
        assert!(matches!(rows[3], TrayMenuRow::Separator));
        assert_eq!(
            rows[4],
            TrayMenuRow::item("empty", "暂无活跃会话", false, TrayMenuRowKind::Empty,)
        );
        assert!(matches!(rows[5], TrayMenuRow::Separator));
        assert!(matches!(
            rows[6],
            TrayMenuRow::Item {
                kind: TrayMenuRowKind::Footer,
                ..
            }
        ));

        assert_eq!(
            menu_item_rows(rows),
            vec![
                ("product-heading".into(), "CC Monitor".into(), false),
                ("today".into(), "今日 0 Token · $0.0000".into(), false),
                (
                    "usage-window".into(),
                    "近 7 天 0 Token · 30 天 0 Token".into(),
                    false,
                ),
                ("empty".into(), "暂无活跃会话".into(), false),
                ("dashboard".into(), "打开仪表盘".into(), true),
                ("settings".into(), "设置".into(), true),
                ("quit".into(), "退出 CC Monitor".into(), true),
            ]
        );
    }

    #[test]
    fn populated_session_menu_keeps_aggregate_status_and_uncounted_heading() {
        let snapshot = TraySnapshot {
            counts: super::super::Counts {
                waiting: 1,
                ..super::super::Counts::default()
            },
            today: UsageSummary {
                cost_known: true,
                ..UsageSummary::default()
            },
            trends: Vec::new(),
            sessions: vec![test_session()],
        };
        let rows = tray_menu_rows(&snapshot);
        assert!(matches!(
            rows[1],
            TrayMenuRow::Item {
                kind: TrayMenuRowKind::Status { urgent: false },
                ..
            }
        ));
        assert!(matches!(rows[4], TrayMenuRow::Separator));
        assert_eq!(
            rows[5],
            TrayMenuRow::item(
                "sessions-heading",
                "最近活跃会话",
                false,
                TrayMenuRowKind::SessionsHeading,
            )
        );
        assert!(matches!(
            &rows[6],
            TrayMenuRow::Item {
                kind: TrayMenuRowKind::SessionPrimary {
                    state,
                    accessibility_label,
                },
                ..
            } if state == "waiting"
                && accessibility_label
                    == "Demo，等待中，会话 1a3aa0，1.0k Token，费用覆盖：$0.0143，最近活动 03-04"
        ));
        assert!(matches!(
            rows[7],
            TrayMenuRow::Item {
                kind: TrayMenuRowKind::SessionMeta,
                ..
            }
        ));
        assert!(matches!(rows[8], TrayMenuRow::Separator));

        assert_eq!(
            menu_item_rows(rows),
            vec![
                ("product-heading".into(), "CC Monitor".into(), false),
                ("status".into(), "1 个会话等待中".into(), false),
                ("today".into(), "今日 0 Token · $0.0000".into(), false),
                (
                    "usage-window".into(),
                    "近 7 天 0 Token · 30 天 0 Token".into(),
                    false,
                ),
                ("sessions-heading".into(), "最近活跃会话".into(), false),
                (
                    "session:1a3aa0ff-1234".into(),
                    "Demo · #1a3aa0\t1.0k Token".into(),
                    true,
                ),
                (
                    "session-meta:1a3aa0ff-1234".into(),
                    "03-04\t$0.0143".into(),
                    false,
                ),
                ("dashboard".into(), "打开仪表盘".into(), true),
                ("settings".into(), "设置".into(), true),
                ("quit".into(), "退出 CC Monitor".into(), true),
            ]
        );
        assert_eq!(tray_status_summary(0, 0, 0, 0), "当前没有活跃会话");
        assert_eq!(tray_status_summary(0, 1, 0, 0), "1 个会话等待中");
    }

    #[test]
    fn session_menu_uses_primary_title_and_secondary_metadata() {
        let mut session = test_session();
        session.project_name = Some("A very long project name".into());
        let title = tray_session_title(&session, "A very long project name");
        let detail = tray_session_detail(&session);
        let accessibility = tray_session_accessibility_label(&session, "A very long project name");
        assert_eq!(title, "A very long project… · #1a3aa0\t1.0k Token");
        assert_eq!(detail, "03-04\t$0.0143");
        assert_eq!(
            accessibility,
            "A very long project name，等待中，会话 1a3aa0，1.0k Token，费用覆盖：$0.0143，最近活动 03-04"
        );
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
    fn fingerprint_changes_at_local_midnight_without_snapshot_changes() {
        let snapshot = TraySnapshot {
            counts: super::super::Counts::default(),
            today: UsageSummary::default(),
            trends: Vec::new(),
            sessions: Vec::new(),
        };
        let first_day = Local
            .with_ymd_and_hms(2026, 7, 31, 23, 59, 0)
            .single()
            .expect("test date must exist in the local timezone")
            .timestamp_millis();
        let next_day = Local
            .with_ymd_and_hms(2026, 8, 1, 0, 1, 0)
            .single()
            .expect("test date must exist in the local timezone")
            .timestamp_millis();
        assert_ne!(
            fingerprint(&snapshot, first_day),
            fingerprint(&snapshot, next_day)
        );
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
