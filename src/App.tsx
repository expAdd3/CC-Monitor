import { FormEvent, type CSSProperties, useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  api,
  compact,
  cost,
  decimal,
  demoSnapshot,
  tokens,
  type DashboardSnapshot,
  type IndexProgress,
  type SettingsDto,
} from "./api";
import { sessionStateReasonLabel } from "./sessionStateReasons";

const logoUrl = new URL("./assets/app_icon_color.svg", import.meta.url).href;

type ActionState<T> = { status: "idle" | "pending" } | { status: "success"; value: T } | { status: "error"; error: string };
type ResourceState<T> = { status: "pending" } | { status: "success"; value: T } | { status: "error"; error: string };
type ErrorContext =
  | "snapshot" | "session" | "settings-load" | "settings-save"
  | "hook" | "hook-repair" | "reindex" | "cleanup"
  | "clipboard" | "notification-settings" | "desktop-notification";

function userSafeError(context: ErrorContext) {
  const messages: Record<ErrorContext, string> = {
    snapshot: "暂时无法读取监控数据，请稍后重试",
    session: "暂时无法读取会话详情，请返回后重试",
    "settings-load": "暂时无法读取设置，请重试",
    "settings-save": "设置保存失败，请重试",
    hook: "事件采集器操作失败，请重试",
    "hook-repair": "事件采集器修复失败，请重试",
    reindex: "重新索引失败，请重试",
    cleanup: "清理失败，请重试",
    clipboard: "复制失败，请检查剪贴板权限",
    "notification-settings": "无法打开通知设置",
    "desktop-notification": "桌面通知测试失败，请检查通知权限",
  };
  return messages[context];
}

function settingsSaveError(reason: unknown): string {
  const code = typeof reason === "string"
    ? reason
    : reason instanceof Error ? reason.message : "";
  if (code === "settings_autostart_failed") {
    return "无法更新登录启动设置，其他设置未保存";
  }
  if (code === "settings_inconsistent") {
    return "设置保存未完成，登录启动状态可能已更改；请重新打开设置确认";
  }
  if (code === "ntfy_server_https_required") {
    return "非本机 ntfy 服务器必须使用 HTTPS";
  }
  if (code === "ntfy_credentials_https_required") {
    return "使用用户名或密码时必须使用 HTTPS";
  }
  return userSafeError("settings-save");
}

function ntfyTestError(reason: unknown): string {
  const code = typeof reason === "string"
    ? reason
    : reason instanceof Error ? reason.message : "";
  const messages: Record<string, string> = {
    ntfy_auth_failed: "认证失败，请检查用户名和密码",
    ntfy_permission_denied: "当前用户没有该 Topic 的发布权限",
    ntfy_not_found: "服务器地址或 Topic 不存在",
    ntfy_rate_limited: "发送过于频繁，请稍后重试",
    ntfy_server_failed: "ntfy 服务器暂时不可用，请稍后重试",
    ntfy_http_failed: "ntfy 服务器拒绝了请求",
    ntfy_network_timeout: "连接 ntfy 服务器超时",
    ntfy_network_connect: "无法连接到 ntfy 服务器",
    ntfy_network_tls: "ntfy 服务器的安全连接验证失败",
    ntfy_network_request: "发送 ntfy 请求失败",
    ntfy_server_https_required: "非本机 ntfy 服务器必须使用 HTTPS",
    ntfy_credentials_https_required: "使用用户名或密码时必须使用 HTTPS",
  };
  return messages[code] || "测试消息发送失败，请检查配置后重试";
}

function hookMutationError(reason: unknown): string {
  const code = typeof reason === "string"
    ? reason
    : reason instanceof Error ? reason.message : "";
  const messages: Record<string, string> = {
    hook_installed_onboarding_sync_failed: "事件采集器已安装，但引导状态同步失败；已重新检测当前状态",
    hook_uninstall_intent_save_failed: "无法保存卸载选择，尚未开始卸载；请重试",
    hook_uninstall_incomplete: "卸载未完整完成；已重新检测当前状态，请重试",
  };
  return messages[code] || userSafeError("hook");
}

function desktopNotificationError(reason: unknown): string {
  const code = typeof reason === "string"
    ? reason
    : reason instanceof Error ? reason.message : "";
  const messages: Record<string, string> = {
    desktop_notification_requires_app_bundle: "请从 CC Monitor.app 启动应用，不能直接运行 target/release/cc-monitor",
    desktop_notification_permission_denied: "通知权限已被拒绝，请在系统设置中为 CC Monitor 打开通知",
    desktop_notification_permission_request_failed: "macOS 未能注册 CC Monitor 的通知权限",
    desktop_delivery_failed: "macOS 拒绝了桌面通知投递，请检查通知设置",
  };
  return messages[code] || "桌面通知测试失败，请检查通知权限";
}

function reindexFailureMessage(index: IndexProgress): string {
  if (index.interrupted) return userSafeError("reindex");
  const failedFiles = Math.max(0, Math.trunc(index.failedFiles));
  const quarantinedSessions = Math.max(0, Math.trunc(index.quarantinedSessions));
  if (failedFiles > 0 && quarantinedSessions > 0) {
    return `${failedFiles} 个记录文件读取失败，${quarantinedSessions} 个会话处理失败，其他内容已完成`;
  }
  if (failedFiles > 0) {
    return `${failedFiles} 个记录文件读取失败，其他内容已完成`;
  }
  if (quarantinedSessions > 0) {
    return `${quarantinedSessions} 个会话处理失败，其他内容已完成`;
  }
  return userSafeError("reindex");
}

function useAsyncAction<T>(action: () => Promise<T>, errorContext: ErrorContext, errorMessage?: (reason: unknown) => string) {
  const [state, setState] = useState<ActionState<T>>({ status: "idle" });
  const id = useRef(0);
  const run = useCallback(async () => {
    const current = ++id.current;
    setState({ status: "pending" });
    try {
      const value = await action();
      if (id.current === current) setState({ status: "success", value });
      return value;
    } catch (reason) {
      if (id.current === current) setState({ status: "error", error: errorMessage?.(reason) || userSafeError(errorContext) });
      throw reason;
    }
  }, [action, errorContext, errorMessage]);
  const reset = useCallback(() => { id.current++; setState({ status: "idle" }); }, []);
  return { state, run, reset };
}

function useHookActions(onChanged: () => Promise<void>) {
  const install = useAsyncAction(api.installHook, "hook", hookMutationError);
  const repair = useAsyncAction(api.installHook, "hook-repair", hookMutationError);
  const uninstall = useAsyncAction(api.uninstallHook, "hook", hookMutationError);
  const defer = useAsyncAction(api.deferHookOnboarding, "hook");
  const reset = useCallback(() => {
    install.reset();
    repair.reset();
    uninstall.reset();
    defer.reset();
  }, [defer.reset, install.reset, repair.reset, uninstall.reset]);
  const run = useCallback(async (
    action: "install" | "repair" | "uninstall" | "defer",
  ) => {
    const selected = { install, repair, uninstall, defer }[action];
    try {
      const value = await selected.run();
      await onChanged();
      return value;
    } catch (reason) {
      // Hook settings, the staged binary and SQLite cannot be one transaction.
      // A failed command may therefore have a safe, observable partial result.
      // Refresh before surfacing the fixed error so retry starts from truth.
      await onChanged().catch(() => undefined);
      throw reason;
    }
  }, [defer, install, onChanged, repair, uninstall]);
  const busy = [install.state, repair.state, uninstall.state, defer.state]
    .some((state) => state.status === "pending");
  return { install, repair, uninstall, defer, run, reset, busy };
}

function HookRecoveryCallout({
  hook,
  disposition,
  onChanged,
  location,
}: {
  hook: DashboardSnapshot["hook"];
  disposition: DashboardSnapshot["hookOnboardingDisposition"];
  onChanged: () => Promise<void>;
  location: "dashboard" | "diagnostics";
}) {
  const actions = useHookActions(onChanged);
  const installing = actions.install.state.status === "pending";
  const repairing = actions.repair.state.status === "pending";
  const success = actions.install.state.status === "success"
    || actions.repair.state.status === "success";
  const failed = actions.install.state.status === "error"
    ? actions.install.state.error
    : actions.repair.state.status === "error"
      ? actions.repair.state.error
      : actions.defer.state.status === "error"
        ? actions.defer.state.error
        : null;

  useEffect(() => {
    if (!success) return;
    const timeout = window.setTimeout(actions.reset, 1800);
    return () => window.clearTimeout(timeout);
  }, [actions.reset, success]);

  if (success) {
    return (
      <section className="hook-callout success" role="status" aria-live="polite">
        <span className="hook-callout-mark" aria-hidden="true">✓</span>
        <div><h2>事件采集器已安装</h2><p>实时监控已启用。</p></div>
      </section>
    );
  }
  if (hook.status === "installed") return null;
  if (
    location === "dashboard"
    && hook.status === "absent"
    && disposition !== null
  ) return null;

  const repairingRequired = hook.status === "repair_required";
  const title = repairingRequired
    ? "事件采集器需要修复"
    : location === "dashboard" ? "启用实时会话监控" : "事件采集器未安装";
  const body = repairingRequired
    ? `当前配置不完整：${hookIssueLabel(hook.issueCode)}。修复前，部分会话状态可能无法及时更新。`
    : "安装事件采集器后，CC Monitor 才能及时识别会话开始、完成和需要介入。它只管理由 CC Monitor 添加的 Claude Code 配置。";
  const primaryAction = repairingRequired ? "repair" : "install";
  const primaryState = repairingRequired ? actions.repair.state : actions.install.state;

  return (
    <section
      className={`hook-callout ${repairingRequired ? "warning" : ""} ${location === "diagnostics" ? "compact" : ""}`}
      aria-labelledby={`hook-callout-title-${location}`}
      aria-describedby={`hook-callout-body-${location}`}
      aria-busy={installing || repairing}
    >
      <span className="hook-callout-mark" aria-hidden="true">{repairingRequired ? "!" : "●"}</span>
      <div className="hook-callout-copy">
        <h2 id={`hook-callout-title-${location}`}>{title}</h2>
        <p id={`hook-callout-body-${location}`}>{body}</p>
        {failed && <p className="hook-callout-error" role="alert">{failed}</p>}
      </div>
      <div className="hook-callout-actions">
        <button
          type="button"
          className="primary"
          disabled={actions.busy}
          aria-busy={primaryState.status === "pending"}
          onClick={() => actions.run(primaryAction).catch(() => undefined)}
        >
          {installing ? "正在安装…" : repairing ? "正在修复…" : repairingRequired ? "立即修复" : "安装事件采集器"}
        </button>
        {location === "dashboard" && !repairingRequired && (
          <button
            type="button"
            className="secondary"
            disabled={actions.busy}
            aria-busy={actions.defer.state.status === "pending"}
            onClick={() => actions.run("defer").catch(() => undefined)}
          >
            {actions.defer.state.status === "pending" ? "正在保存…" : "暂不安装"}
          </button>
        )}
        {repairingRequired && location === "dashboard" && (
          <button type="button" className="secondary" disabled={actions.busy} onClick={() => navigate("diagnostics")}>
            查看详情
          </button>
        )}
        {failed && location === "dashboard" && (
          <button type="button" className="secondary" disabled={actions.busy} onClick={() => navigate("settings")}>
            打开设置
          </button>
        )}
      </div>
    </section>
  );
}

function useAsyncResource<T>(load: () => Promise<T>, errorContext: ErrorContext) {
  const [state, setState] = useState<ResourceState<T>>({ status: "pending" });
  const [attempt, setAttempt] = useState(0);
  const id = useRef(0);
  useEffect(() => {
    const current = ++id.current;
    setState({ status: "pending" });
    load()
      .then((value) => {
        if (id.current === current) setState({ status: "success", value });
      })
      .catch(() => {
        if (id.current === current) setState({ status: "error", error: userSafeError(errorContext) });
      });
    return () => {
      if (id.current === current) id.current++;
    };
  }, [attempt, errorContext, load]);
  const retry = useCallback(() => setAttempt((value) => value + 1), []);
  return { state, retry };
}

function focusedElement() {
  return document.activeElement instanceof HTMLElement ? document.activeElement : null;
}

function focusStillFollowsNavigation(source: HTMLElement | null) {
  const active = document.activeElement;
  if (active === source) return true;
  return active === document.body && (!source || source === document.body || !source.isConnected);
}

type Route = "dashboard" | "history" | "settings" | "diagnostics" | `session:${string}`;

function routeFromHash(): Route {
  const value = location.hash.slice(1);
  if (value.startsWith("session:")) return value as Route;
  if (["history", "settings", "diagnostics"].includes(value)) return value as Route;
  return "dashboard";
}

function navigate(route: Route) {
  location.hash = route;
}

function useDesktopEvents(refresh: (revision: number) => void) {
  useEffect(() => {
    let alive = true;
    const cleanups: UnlistenFn[] = [];
    Promise.allSettled([
      listen<{ revision: number }>("monitor://invalidated", (event) => refresh(event.payload.revision)),
      listen<string>("monitor://navigate", (event) => {
        if (event.payload.startsWith("session:")) navigate(event.payload as Route);
        else navigate(event.payload as Route);
      }),
    ])
      .then((results) => {
        const items = results
          .filter((result): result is PromiseFulfilledResult<UnlistenFn> => result.status === "fulfilled")
          .map((result) => result.value);
        if (alive) {
          cleanups.push(...items);
          // Register first, then read. This closes the startup gap where a
          // commit could otherwise land between the initial read and listener
          // installation without a later refresh.
          refresh(0);
        }
        else items.forEach((cleanup) => cleanup());
      });
    return () => {
      alive = false;
      cleanups.forEach((cleanup) => cleanup());
    };
  }, [refresh]);
}

export default function App() {
  const [route, setRoute] = useState<Route>(routeFromHash);
  const [snapshot, setSnapshot] = useState<DashboardSnapshot>(demoSnapshot);
  const [loading, setLoading] = useState(true);
  const [snapshotReady, setSnapshotReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const contentRef = useRef<HTMLElement>(null);
  const previousRoute = useRef<Route>(route);
  const focusIntent = useRef<{ route: Route; source: HTMLElement | null } | null>(null);
  const displayedRevision = useRef(0);
  const requestedRevision = useRef(0);
  const refreshGeneration = useRef(0);
  const refreshInFlight = useRef<Promise<void> | null>(null);
  const mounted = useRef(true);

  const refresh = useCallback((minimumRevision = 0) => {
    requestedRevision.current = Math.max(requestedRevision.current, minimumRevision);
    if (refreshInFlight.current) return refreshInFlight.current;

    const generation = ++refreshGeneration.current;
    const request = (async () => {
      for (;;) {
        const targetAtStart = requestedRevision.current;
        try {
          const value = await api.snapshot();
          if (!mounted.current || refreshGeneration.current !== generation) return;
          const newestTarget = requestedRevision.current;
          if (
            value.revision >= targetAtStart
            && value.revision >= newestTarget
            && value.revision >= displayedRevision.current
          ) {
            displayedRevision.current = value.revision;
            setSnapshot(value);
            setSnapshotReady(true);
            setError(null);
            setLoading(false);
          }
          // An invalidation that arrived during this request is coalesced into
          // one follow-up read. A response older than an already-known target
          // is ignored rather than overwriting the displayed snapshot.
          if (newestTarget > targetAtStart) {
            await Promise.resolve();
            continue;
          }
          return;
        } catch {
          if (!mounted.current || refreshGeneration.current !== generation) return;
          if (requestedRevision.current > targetAtStart) {
            await Promise.resolve();
            continue;
          }
          setError(userSafeError("snapshot"));
          setLoading(false);
          return;
        }
      }
    })();
    refreshInFlight.current = request;
    void request.finally(() => {
      if (refreshInFlight.current === request) refreshInFlight.current = null;
    });
    return request;
  }, []);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      refreshGeneration.current++;
      refreshInFlight.current = null;
    };
  }, []);
  useDesktopEvents(refresh);
  useEffect(() => {
    const onHash = () => setRoute(routeFromHash());
    addEventListener("hashchange", onHash);
    return () => removeEventListener("hashchange", onHash);
  }, []);
  useEffect(() => {
    if (previousRoute.current === route) return;
    focusIntent.current = { route, source: focusedElement() };
    previousRoute.current = route;
    const focusWhenReady = () => {
      const intent = focusIntent.current;
      if (!intent || intent.route !== route) return;
      const heading = contentRef.current?.querySelector<HTMLElement>("h1");
      if (!heading) return;
      focusIntent.current = null;
      if (focusStillFollowsNavigation(intent.source)) heading.focus();
    };
    focusWhenReady();
    const observer = new MutationObserver(focusWhenReady);
    if (contentRef.current) observer.observe(contentRef.current, { childList: true, subtree: true });
    return () => observer.disconnect();
  }, [route]);

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <img className="brand-mark" src={logoUrl} alt="CC Monitor 闹钟终端标志" />
          <div><strong>CC Monitor</strong><small>Claude Code 会话监控</small></div>
        </div>
        <nav>
          <Nav route="dashboard" current={route}>总览</Nav>
          <Nav route="history" current={route}>历史趋势</Nav>
          <Nav route="settings" current={route}>设置</Nav>
          <Nav route="diagnostics" current={route}>诊断</Nav>
        </nav>
        <button type="button" className="sidebar-foot" aria-label={snapshotReady ? `事件采集器${hookStatusLabel(snapshot.hook.status)}，打开设置` : "正在检测事件采集器，打开设置"} onClick={() => navigate("settings")}>
          <span className={`health-dot ${!snapshotReady ? "pending" : snapshot.hook.status === "installed" ? "ok" : snapshot.hook.status === "repair_required" ? "warning" : ""}`} />
          <span>事件采集器</span>
          <strong className={!snapshotReady ? "pending" : snapshot.hook.status}>{snapshotReady ? hookStatusLabel(snapshot.hook.status) : "检测中"}</strong>
        </button>
      </aside>
      <main className="content" ref={contentRef}>
        {error && <div className="error-banner" role="alert" aria-live="assertive">监控数据刷新失败：{error}</div>}
        {loading ? <Loading /> : (
          <>
            {route === "dashboard" && <Dashboard snapshot={snapshot} onChanged={refresh} />}
            {route === "history" && <History snapshot={snapshot} />}
            {route === "settings" && <Settings hook={snapshot.hook} onSaved={refresh} />}
            {route === "diagnostics" && <Diagnostics snapshot={snapshot} onChanged={refresh} />}
            {route.startsWith("session:") && (
              <Session key={route} sessionId={route.slice(8)} />
            )}
          </>
        )}
      </main>
    </div>
  );
}

function Nav({ route, current, children }: { route: Route; current: Route; children: string }) {
  const active = current === route || (route === "dashboard" && current.startsWith("session:"));
  return <button className={active ? "active" : ""} aria-current={active ? "page" : undefined} onClick={() => navigate(route)}>{children}</button>;
}

function Dashboard({ snapshot, onChanged }: { snapshot: DashboardSnapshot; onChanged: () => Promise<void> }) {
  const cards = [
    ["运行中", snapshot.counts.running, "running"],
    ["等待中", snapshot.counts.waiting, "waiting"],
    ["需要介入", snapshot.counts.needsInput, "needs"],
    ["今日 Token", compact(tokens(snapshot.today)), "tokens"],
  ];
  return (
    <>
      <PageTitle title="运行总览" />
      <HookRecoveryCallout
        hook={snapshot.hook}
        disposition={snapshot.hookOnboardingDisposition}
        onChanged={onChanged}
        location="dashboard"
      />
      <section className="metric-grid">
        {cards.map(([label, value, kind]) => (
          <article className={`metric ${kind}`} key={String(label)}>
            <span>{label}</span><strong>{value}</strong>
          </article>
        ))}
      </section>
      <section className="overview-stage" aria-labelledby="active-sessions-title">
        <div className="stage-heading">
          <div><h2 id="active-sessions-title">活跃会话</h2><p>24 小时内有活动</p></div>
          <span>
            {snapshot.sessionsHasMore
              ? `显示最近 ${snapshot.sessions.length} 个，共 ${snapshot.activeSessionCount} 个`
              : `${snapshot.activeSessionCount} 个会话`}
          </span>
        </div>
        {snapshot.sessions.length === 0 ? <Empty text="暂无活跃 Claude Code 会话" /> : (
          <div className="session-list">
            {snapshot.sessions.map((session) => (
              <button key={session.sessionId} onClick={() => navigate(`session:${session.sessionId}`)}>
                <State state={session.turnState} />
                <div className="session-copy">
                  <strong>{session.projectName || session.sessionId.slice(0, 12)} <span className="session-ref">#{shortSessionId(session.sessionId)}</span></strong>
                  <small>状态：{stateLabel(session.turnState)} · {sessionStateReasonLabel(session.stateReason)} · {ago(session.lastObservedAtMs)}</small>
                </div>
                <div className="session-usage">
                  <strong>{compact(tokens(session.usage))}</strong>
                  <small>{costWithCoverage(session.usage.costPicoUsd, session.usage.costKnown, session.usage.unpricedTokens, tokens(session.usage))}</small>
                </div>
                <span className="chevron">›</span>
              </button>
            ))}
          </div>
        )}
      </section>
      <section className="overview-support" aria-label="今日用量与采集状态">
        <article className="overview-summary">
          <h2>今日用量</h2>
          <UsageRows value={snapshot.today} />
        </article>
        <article className="overview-summary">
          <h2>采集状态</h2>
          <Info label="事件采集器" value={hookVersion(snapshot)} />
          <Info label="历史索引" value={indexStateLabel(snapshot.index.state)} />
          <Info label="待处理事件" value={String(snapshot.diagnostics.pendingEvents)} />
          <Info label="隔离会话" value={String(snapshot.diagnostics.quarantinedSessions)} />
        </article>
      </section>
    </>
  );
}

type TrendDay = DashboardSnapshot["trends"][number];
type HeatmapDay = TrendDay & { date: Date; level: number; column: number; row: number };

function localDayKey(date: Date) {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

function localDateFromKey(value: string) {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!match) return null;
  const date = new Date(Number(match[1]), Number(match[2]) - 1, Number(match[3]));
  return localDayKey(date) === value ? date : null;
}

function addLocalDays(date: Date, days: number) {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate() + days);
}

function mondayOf(date: Date) {
  return addLocalDays(date, -((date.getDay() + 6) % 7));
}

function safeTokens(value: TrendDay["tokens"]) {
  return decimal(value);
}

function daySerial(date: Date) {
  return Math.floor(Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / 86_400_000);
}

export function calendarWeekColumn(date: Date, gridStart: Date) {
  return Math.floor((daySerial(date) - daySerial(gridStart)) / 7) + 1;
}

export function buildAnnualHeatmap(trends: TrendDay[], now = new Date()) {
  return buildHeatmap(trends, 365, now);
}

function buildHeatmap(trends: TrendDay[], daysInRange: number, now = new Date()) {
  const end = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const range = Math.max(1, Math.trunc(daysInRange));
  const start = addLocalDays(end, -(range - 1));
  const byDay = new Map<string, TrendDay>();
  for (const trend of trends) {
    const date = localDateFromKey(trend.day);
    if (date && date >= start && date <= end) {
      byDay.set(trend.day, { ...trend, tokens: safeTokens(trend.tokens).toString() });
    }
  }
  const days = Array.from({ length: range }, (_, index) => {
    const date = addLocalDays(start, index);
    const day = byDay.get(localDayKey(date));
    return {
      date,
      day: localDayKey(date),
      tokens: day?.tokens ?? "0",
      costPicoUsd: day?.costPicoUsd ?? "0",
      costKnown: day?.costKnown ?? true,
      unpricedTokens: day?.unpricedTokens ?? "0",
    };
  });
  const max = days.reduce((value, day) => value > safeTokens(day.tokens) ? value : safeTokens(day.tokens), 0n);
  const gridStart = mondayOf(start);
  const heatmapDays: HeatmapDay[] = days.map((day) => ({
    ...day,
    level: safeTokens(day.tokens) === 0n || max === 0n ? 0 : Number((safeTokens(day.tokens) * 4n + max - 1n) / max),
    column: calendarWeekColumn(day.date, gridStart),
    row: (day.date.getDay() + 6) % 7 + 1,
  }));
  const monthLabels = heatmapDays.filter((day, index) => index === 0 || day.date.getMonth() !== heatmapDays[index - 1].date.getMonth());
  return {
    days: heatmapDays,
    monthLabels,
    columns: Math.max(...heatmapDays.map((day) => day.column)),
  };
}

function costWithCoverage(costPicoUsd: string | number, costKnown: boolean, unpricedTokens?: string | number, totalTokens?: string | number | bigint) {
  const unknown = decimal(unpricedTokens ?? 0);
  const total = typeof totalTokens === "bigint" ? totalTokens : decimal(totalTokens ?? 0);
  if (unknown > 0n && total > 0n && unknown >= total) return "费用待定";
  if (unknown > 0n) return `${cost(costPicoUsd, true)} · ${compact(unknown)} 未计`;
  return costKnown ? cost(costPicoUsd, true) : "费用待定";
}

function History({ snapshot }: { snapshot: DashboardSnapshot }) {
  const now = new Date();
  const annual = buildAnnualHeatmap(snapshot.trends, now);
  const detailEnd = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const detailStart = addLocalDays(detailEnd, -29);
  const detailDays = snapshot.trends.filter((day) => {
    const date = localDateFromKey(day.day);
    return date !== null && date >= detailStart && date <= detailEnd;
  });
  const max = detailDays.reduce((value, day) => value > safeTokens(day.tokens) ? value : safeTokens(day.tokens), 1n);
  const heatmapStyle = { "--heatmap-columns": annual.columns } as CSSProperties;
  const rangeTokens = annual.days.reduce((total, day) => total + safeTokens(day.tokens), 0n);
  const rangeCostKnown = annual.days.every((day) => day.costKnown);
  const rangeCost = annual.days.reduce((total, day) => total + decimal(day.costPicoUsd), 0n);
  const rangeUnpriced = annual.days.reduce((total, day) => total + decimal(day.unpricedTokens ?? "0"), 0n);
  const heatmapSummary = `过去一年 Token 活跃度，共 ${compact(rangeTokens.toString())} Token，${costWithCoverage(rangeCost.toString(), rangeCostKnown, rangeUnpriced.toString(), rangeTokens)}。颜色由浅到深表示当天 Token 用量由少到多。`;
  return (
    <>
      <PageTitle title="历史趋势" subtitle="按本地日期汇总 Token 用量与费用" />
      <section className="history-stage history-recent" aria-labelledby="recent-history-title">
        <div className="section-title">
          <h2 id="recent-history-title">最近 30 天明细</h2>
          <p>费用显示已可计算部分；未计费 Token 表示缺少定价。</p>
        </div>
        {detailDays.length === 0 ? <Empty text="最近 30 天暂无用量" /> : (
          <div className="bars" role="list" aria-label="每日 Token 用量">
            {detailDays.map((day) => (
              <div className="bar-column" role="listitem" aria-label={`${day.day}，${compact(day.tokens)} Token，${costWithCoverage(day.costPicoUsd, day.costKnown, day.unpricedTokens, day.tokens)}`} key={day.day}>
                <div className="bar-plot">
                  <div className="bar-value">{compact(day.tokens)}</div>
                  <div className="bar" style={{ height: `${Math.max(4, Number(safeTokens(day.tokens) * 12000n / max) / 100)}px` }} />
                </div>
                <small>{day.day.slice(5)}</small>
                <span className="bar-cost">
                  <span>{cost(day.costPicoUsd, day.costKnown || decimal(day.unpricedTokens ?? 0) > 0n)}</span>
                  {decimal(day.unpricedTokens ?? 0) > 0n && <em>未计 {compact(day.unpricedTokens ?? 0)}</em>}
                </span>
              </div>
            ))}
          </div>
        )}
      </section>
      <section className="history-hero" aria-labelledby="annual-history-title">
        <div className="history-hero-top">
          <div className="section-title">
            <h2 id="annual-history-title">过去一年 Token 活跃度</h2>
            <p>按天汇总；颜色越深表示当天 Token 使用量越高。</p>
          </div>
          <div className="history-total" aria-label="过去一年用量汇总">
            <div><span>过去一年 Token</span><strong>{compact(rangeTokens.toString())}</strong></div>
            <div><span>已估算费用</span><strong className="known">{cost(rangeCost.toString(), true)}</strong></div>
            <div><span>未计费 Token</span><strong className={rangeUnpriced > 0n ? "unknown" : ""}>{compact(rangeUnpriced)}</strong></div>
          </div>
        </div>
        <div className="heatmap-scroll">
          <div className="heatmap-layout" style={heatmapStyle}>
            <div className="heatmap-corner" aria-hidden="true" />
            <div className="heatmap-months" aria-hidden="true">
              {annual.monthLabels.map((day) => <span key={day.day} style={{ gridColumnStart: day.column }}>{day.date.getMonth() + 1}月</span>)}
            </div>
            <div className="heatmap-weekdays" aria-hidden="true"><span>周一</span><span /><span>周三</span><span /><span>周五</span><span /><span /></div>
            <div className="heatmap-grid" role="list" aria-label={heatmapSummary}>
              {annual.days.map((day) => (
                    <span
                      key={day.day}
                      className="heatmap-cell"
                      data-level={day.level}
                      role="listitem"
                      aria-label={`${day.day}，${compact(day.tokens)} Token，${costWithCoverage(day.costPicoUsd, day.costKnown, day.unpricedTokens, day.tokens)}`}
                      style={{ gridColumnStart: day.column, gridRowStart: day.row }}
                    />
              ))}
            </div>
          </div>
        </div>
        <div className="heatmap-legend" aria-label="活跃度图例：从少到多">
          <span>较少</span>{[0, 1, 2, 3, 4].map((level) => <i key={level} data-level={level} aria-hidden="true" />)}<span>较多</span>
        </div>
      </section>
    </>
  );
}

function Session({ sessionId }: { sessionId: string }) {
  const loadSession = useCallback(() => api.session(sessionId), [sessionId]);
  const detail = useAsyncResource(loadSession, "session");
  if (detail.state.status === "error") return <div className="error-banner" role="alert" aria-live="assertive">会话详情加载失败：{detail.state.error} <button type="button" className="secondary" onClick={detail.retry}>重试</button></div>;
  if (detail.state.status === "pending") return <Loading label="正在读取会话详情…" />;
  const value = detail.state.value;
  return (
    <>
      <button className="back" onClick={() => navigate("dashboard")}>← 返回总览</button>
      <PageTitle
        title={value.session.projectName || value.session.sessionId.slice(0, 16)}
        subtitle={`会话 #${shortSessionId(value.session.sessionId)} · ${value.session.sessionId}`}
      />
      <div className="session-workspace">
        <aside className="session-summary" aria-label="会话摘要">
          <section>
            <h2>当前状态</h2>
            <div className="large-state"><State state={value.session.turnState} />{stateLabel(value.session.turnState)}</div>
            <Info label="原因" value={sessionStateReasonLabel(value.session.stateReason)} />
            <Info label="最近活动" value={new Date(value.session.lastObservedAtMs).toLocaleString()} />
          </section>
          <section>
            <h2>模型用量</h2>
            {value.models.map((model) => (
              <Info key={model.modelId} label={model.modelId} value={`${compact(model.tokens)} · ${costWithCoverage(model.costPicoUsd, model.costKnown, model.unpricedTokens, model.tokens)}`} />
            ))}
            {!value.models.length && <Empty text="暂无模型用量" />}
          </section>
        </aside>
        <section className="session-timeline" aria-labelledby="event-history-title">
          <div className="stage-heading"><div><h2 id="event-history-title">事件历史</h2><p>最近 100 条</p></div></div>
          <ul className="timeline" aria-label="会话事件历史">
            {value.events.map((event, index) => (
              <li key={`${event.occurredAtMs}-${index}`}>
                <span /><div><strong>{eventLabel(event.sourceEvent)}</strong><small>{sourceLabel(event.source)} · {new Date(event.occurredAtMs).toLocaleString()}</small></div>
              </li>
            ))}
          </ul>
        </section>
      </div>
    </>
  );
}

function Settings({ hook, onSaved }: { hook: DashboardSnapshot["hook"]; onSaved: () => Promise<void> }) {
  const loadSettings = useCallback(() => api.settings(), []);
  const settingsResource = useAsyncResource(loadSettings, "settings-load");
  const [settings, setSettings] = useState<SettingsDto | null>(null);
  const [saveState, setSaveState] = useState<ActionState<string>>({ status: "idle" });
  const [ntfyTestState, setNtfyTestState] = useState<ActionState<string>>({ status: "idle" });
  const [reloadFailed, setReloadFailed] = useState(false);
  const [reloadPending, setReloadPending] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<{ ntfyServer?: string; ntfyTopic?: string }>({});
  const [hookConfirmation, setHookConfirmation] = useState(false);
  const editGeneration = useRef(0);
  const reloadRequestGeneration = useRef(0);
  const ntfyTestGeneration = useRef(0);
  const hookActions = useHookActions(onSaved);
  useEffect(() => {
    if (settingsResource.state.status === "success") setSettings(settingsResource.state.value);
  }, [settingsResource.state]);
  const update = (patch: Partial<SettingsDto>) => {
    editGeneration.current += 1;
    ntfyTestGeneration.current += 1;
    setSettings((current) => current ? ({ ...current, ...patch }) : current);
    setFieldErrors((current) => ({
      ...current,
      ...("ntfyServer" in patch ? { ntfyServer: undefined } : {}),
      ...("ntfyTopic" in patch ? { ntfyTopic: undefined } : {}),
      ...(patch.ntfyEnabled === false ? { ntfyServer: undefined, ntfyTopic: undefined } : {}),
    }));
    setDirty(true);
    setReloadFailed(false);
    setReloadPending(false);
    setSaveState({ status: "idle" });
    setNtfyTestState({ status: "idle" });
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!settings || !dirty || saveState.status === "pending") return;
    const validation = validateSettings(settings);
    if (validation.ntfyServer || validation.ntfyTopic) {
      setFieldErrors(validation);
      setSaveState({ status: "idle" });
      return;
    }
    setFieldErrors({});
    setSaveState({ status: "pending" });
    try {
      await api.saveSettings(settings);
    } catch (reason) {
      setSaveState({ status: "error", error: settingsSaveError(reason) });
      return;
    }
    setDirty(false);
    setReloadFailed(false);
    setSaveState({ status: "success", value: "设置已保存" });
    void onSaved().catch(() => undefined);
    const reloadGeneration = editGeneration.current;
    const requestGeneration = ++reloadRequestGeneration.current;
    setReloadPending(true);
    try {
      const canonical = await api.settings();
      if (editGeneration.current === reloadGeneration
        && reloadRequestGeneration.current === requestGeneration) {
        setSettings(canonical);
        setReloadPending(false);
      }
    } catch {
      if (editGeneration.current === reloadGeneration
        && reloadRequestGeneration.current === requestGeneration) {
        setReloadPending(false);
        setReloadFailed(true);
        setSaveState({ status: "success", value: "设置已保存，但重新读取失败" });
      }
    }
  };
  const retrySettingsReload = async () => {
    if (reloadPending) return;
    const reloadGeneration = editGeneration.current;
    const requestGeneration = ++reloadRequestGeneration.current;
    setReloadPending(true);
    try {
      const canonical = await api.settings();
      if (editGeneration.current === reloadGeneration
        && reloadRequestGeneration.current === requestGeneration) {
        setSettings(canonical);
        setReloadPending(false);
        setReloadFailed(false);
        setSaveState({ status: "success", value: "设置已保存" });
      }
    } catch {
      if (editGeneration.current === reloadGeneration
        && reloadRequestGeneration.current === requestGeneration) {
        setReloadPending(false);
        setReloadFailed(true);
      }
    }
  };
  const mutateHook = async () => {
    try {
      const result = hook.status === "installed"
        ? await hookActions.run("uninstall")
        : await hookActions.run("install");
      setHookConfirmation(false);
      return result;
    } catch {
      return undefined;
    }
  };
  const testNtfy = async () => {
    if (!settings || ntfyTestState.status === "pending") return;
    const validation = validateNtfy(settings);
    if (validation.ntfyServer || validation.ntfyTopic) {
      setFieldErrors(validation);
      setNtfyTestState({ status: "error", error: "请先补全正确的 ntfy 配置" });
      return;
    }
    setFieldErrors({});
    const testGeneration = ++ntfyTestGeneration.current;
    setNtfyTestState({ status: "pending" });
    try {
      await api.testNtfy(settings);
      if (ntfyTestGeneration.current === testGeneration) {
        setNtfyTestState({ status: "success", value: "测试消息已发送，请检查接收设备" });
      }
    } catch (reason) {
      if (ntfyTestGeneration.current === testGeneration) {
        setNtfyTestState({ status: "error", error: ntfyTestError(reason) });
      }
    }
  };
  if (!settings && settingsResource.state.status === "error") return <div className="error-banner" role="alert">设置加载失败：{settingsResource.state.error} <button type="button" className="secondary" onClick={settingsResource.retry}>重试</button></div>;
  if (!settings) return <Loading label="正在读取设置…" />;
  const saving = saveState.status === "pending";
  const hookBusy = hookActions.busy;
  const hookMessage = hookActions.install.state.status === "success"
    ? "事件采集器已安装，实时监控已启用"
    : hookActions.uninstall.state.status === "success"
      ? hookActions.uninstall.state.value ? "事件采集器已卸载" : "未找到可卸载的事件采集器"
      : hookActions.install.state.status === "error" ? hookActions.install.state.error
      : hookActions.uninstall.state.status === "error" ? hookActions.uninstall.state.error : null;
  const repairMessage = hookActions.repair.state.status === "success" ? "事件采集器已修复"
    : hookActions.repair.state.status === "error" ? hookActions.repair.state.error : null;
  const hookDescription = hook.status === "installed"
    ? "安装完整且配置有效；可重新安装进行修复，或卸载 CC Monitor 自己的采集项。"
    : hook.status === "repair_required"
      ? `检测到不完整的安装（${hookIssueLabel(hook.issueCode)}），请修复后再继续使用事件采集。`
      : "尚未安装；安装程序只修改属于 CC Monitor 的采集项。";
  return (
    <>
      <PageTitle title="设置" subtitle="通知、启动与事件采集器" />
      <form className="settings-layout" onSubmit={submit}>
        <div className="settings-main">
          <section className="settings-side" aria-label="事件采集器管理">
            <div className="hook-actions">
            <div><h2>事件采集器</h2><p>{hookDescription}</p></div>
            {!hookConfirmation ? <div>
              {hook.status === "absent" && <button type="button" className="primary" disabled={hookBusy} aria-busy={hookActions.install.state.status === "pending"} onClick={mutateHook}>{hookActions.install.state.status === "pending" ? "正在安装…" : "安装事件采集器"}</button>}
              {hook.status === "repair_required" && <button type="button" className="primary" disabled={hookBusy} aria-busy={hookActions.repair.state.status === "pending"} onClick={async () => {
                try {
                  await hookActions.run("repair");
                } catch {
                  // The action state provides the accessible error feedback.
                }
              }}>{hookActions.repair.state.status === "pending" ? "正在修复…" : "立即修复"}</button>}
              {hook.status === "installed" && <>
                <button type="button" className="secondary" disabled={hookBusy} aria-busy={hookActions.repair.state.status === "pending"} onClick={async () => {
                  try {
                    await hookActions.run("repair");
                  } catch {
                    // The action state provides the accessible error feedback.
                  }
                }}>{hookActions.repair.state.status === "pending" ? "正在修复…" : "重新安装事件采集器"}</button>
                <button type="button" className="danger" disabled={hookBusy} onClick={() => { hookActions.reset(); setHookConfirmation(true); }}>卸载事件采集器</button>
              </>}
            </div> : <div className="hook-confirmation" role="group" aria-label="确认卸载事件采集器">
              <span>确认卸载 CC Monitor 的事件采集器？</span>
              <button type="button" className="danger" disabled={hookBusy} aria-busy={hookActions.uninstall.state.status === "pending"} onClick={mutateHook}>{hookActions.uninstall.state.status === "pending" ? "正在卸载…" : "确认卸载"}</button>
              <button type="button" className="secondary" disabled={hookBusy} onClick={() => { hookActions.reset(); setHookConfirmation(false); }}>取消</button>
            </div>}
            </div>
            {hookMessage && <div className={`operation-banner ${hookActions.install.state.status === "error" || hookActions.uninstall.state.status === "error" ? "failed" : ""}`} role={hookActions.install.state.status === "error" || hookActions.uninstall.state.status === "error" ? "alert" : "status"} aria-live={hookActions.install.state.status === "error" || hookActions.uninstall.state.status === "error" ? undefined : "polite"}>{hookMessage}</div>}
            {repairMessage && <div className={`operation-banner ${hookActions.repair.state.status === "error" ? "failed" : ""}`} role={hookActions.repair.state.status === "error" ? "alert" : "status"} aria-live={hookActions.repair.state.status === "error" ? undefined : "polite"}>{repairMessage}</div>}
          </section>
          <section className="settings-group">
          <div><h2>远程通知</h2><p>非本机服务器必须使用 HTTPS；HTTP 仅用于无凭据的 localhost、127.0.0.1 或 ::1 测试服务。</p></div>
          <Toggle label="启用 ntfy" checked={settings.ntfyEnabled} disabled={saving} onChange={(value) => update({ ntfyEnabled: value })} />
          {settings.ntfyActivationPending && (
            <p className="muted" role="status" aria-live="polite">
              正在清理停用期间的旧通知，完成后将自动启用。
            </p>
          )}
          <Field id="ntfy-server" label="服务器地址" value={settings.ntfyServer} error={fieldErrors.ntfyServer} disabled={saving} placeholder="https://ntfy.sh" onChange={(value) => update({ ntfyServer: value })} />
          <Field id="ntfy-topic" label="通知主题（Topic）" value={settings.ntfyTopic} error={fieldErrors.ntfyTopic} disabled={saving} onChange={(value) => update({ ntfyTopic: value })} />
          <Field label="用户名" value={settings.ntfyUsername} disabled={saving} onChange={(value) => update({ ntfyUsername: value })} />
          <Field
            label="密码"
            type="password"
            value={settings.ntfyPassword || ""}
            placeholder={settings.ntfyPasswordSet ? "••••••••（留空保持不变）" : "可选"}
            disabled={saving}
            onChange={(value) => update({ ntfyPassword: value || undefined })}
          />
          <div className="ntfy-test-row">
            <button type="button" className="secondary" disabled={saving || ntfyTestState.status === "pending"} aria-busy={ntfyTestState.status === "pending"} onClick={testNtfy}>
              {ntfyTestState.status === "pending" ? "正在发送…" : "发送测试消息"}
            </button>
            {ntfyTestState.status !== "idle" && (
              <span className={`action-message ${ntfyTestState.status === "error" ? "failed" : ""}`} role={ntfyTestState.status === "error" ? "alert" : "status"} aria-live={ntfyTestState.status === "error" ? undefined : "polite"}>
                {ntfyTestState.status === "pending" ? "正在连接 ntfy 服务…" : ntfyTestState.status === "success" ? ntfyTestState.value : ntfyTestState.status === "error" ? ntfyTestState.error : null}
              </span>
            )}
          </div>
          </section>
          <section className="settings-group">
          <div><h2>系统</h2><p>关闭窗口后菜单栏和监控引擎仍继续运行。</p></div>
          <Toggle label="登录时启动 CC Monitor" checked={settings.autostart} disabled={saving} onChange={(value) => update({ autostart: value })} />
          </section>
        </div>
        <div className="settings-save-dock">
          <Feedback state={saveState} />
          {reloadFailed && <button type="button" className="secondary" disabled={reloadPending} aria-busy={reloadPending} onClick={retrySettingsReload}>{reloadPending ? "正在重新读取…" : "重新读取"}</button>}
          {dirty && saveState.status !== "pending" && <span className="unsaved-label">有未保存的更改</span>}
          <button className="primary" disabled={!dirty || saveState.status === "pending"} aria-busy={saveState.status === "pending"}>{saveState.status === "pending" ? "正在保存…" : "保存设置"}</button>
        </div>
      </form>
    </>
  );
}

function Diagnostics({ snapshot, onChanged }: { snapshot: DashboardSnapshot; onChanged: () => Promise<void> }) {
  const initialIndex = snapshot.index.state === "running" && snapshot.index.runId
    ? { runId: snapshot.index.runId, state: { status: "pending" } as ActionState<string> }
    : { runId: null, state: { status: "idle" } as ActionState<string> };
  const [reindexRunId, setReindexRunId] = useState<string | null>(initialIndex.runId);
  const [reindexState, setReindexState] = useState<ActionState<string>>(initialIndex.state);
  const [startingReindex, setStartingReindex] = useState(false);
  const [cleanupConfirmation, setCleanupConfirmation] = useState(false);
  const copy = useAsyncAction(async () => {
    const report = [
      "CC Monitor diagnostics",
      `generated_at=${new Date().toISOString()}`,
      `revision=${snapshot.revision}`,
      `migration_version=${snapshot.diagnostics.migrationVersion}`,
      `index_state=${snapshot.index.state}`,
      `index_progress=${snapshot.index.completed}/${snapshot.index.total}`,
      `hook_status=${snapshot.hook.status}`,
      `hook_issue_code=${snapshot.hook.issueCode || "none"}`,
      `hook_version=${snapshot.hook.version || "unknown"}`,
      `pending_events=${snapshot.diagnostics.pendingEvents}`,
      `quarantined_sessions=${snapshot.diagnostics.quarantinedSessions}`,
      `pending_notifications=${snapshot.diagnostics.pendingNotifications}`,
      `desktop_failures=${snapshot.diagnostics.desktopFailures}`,
      `desktop_error_code=${snapshot.diagnostics.desktopErrorCode || "none"}`,
      `ntfy_failures=${snapshot.diagnostics.ntfyFailures}`,
      `ntfy_error_code=${snapshot.diagnostics.ntfyErrorCode || "none"}`,
      ...snapshot.diagnostics.backgroundHealth.map((health) =>
        [
          `background_task=${health.task}`,
          `successes=${health.successCount}`,
          `failures=${health.failureCount}`,
          `consecutive_failures=${health.consecutiveFailures}`,
          `error_code=${health.errorCode || "none"}`,
          `last_succeeded_at_ms=${health.lastSucceededAtMs ?? "none"}`,
          `last_failed_at_ms=${health.lastFailedAtMs ?? "none"}`,
          `recovered_at_ms=${health.recoveredAtMs ?? "none"}`,
        ].join(" ")
      ),
    ].join("\n");
    await navigator.clipboard.writeText(report);
  }, "clipboard");
  const openSettings = useAsyncAction(() => api.openNotificationSettings(), "notification-settings");
  const testDesktopNotification = useAsyncAction(() => api.testDesktopNotification(), "desktop-notification", desktopNotificationError);
  const cleanup = useAsyncAction(() => api.clearHistory(), "cleanup");
  const backgroundIssues = snapshot.diagnostics.backgroundHealth.filter((health) => health.consecutiveFailures > 0);
  const issueCount = [
    snapshot.hook.status !== "installed",
    snapshot.index.state === "failed",
    snapshot.diagnostics.pendingEvents > 0,
    snapshot.diagnostics.quarantinedSessions > 0,
    snapshot.diagnostics.desktopFailures > 0,
    snapshot.diagnostics.ntfyFailures > 0,
    ...snapshot.diagnostics.backgroundHealth.map((health) => health.consecutiveFailures > 0),
  ].filter(Boolean).length;
  const desktopHealth = snapshot.diagnostics.desktopFailures === 0
    ? "正常"
    : `${snapshot.diagnostics.desktopFailures} 次连续失败 · ${diagnosticDesktopIssue(snapshot.diagnostics.desktopErrorCode)}`;
  const ntfyHealth = snapshot.diagnostics.ntfyFailures === 0
    ? "正常"
    : `${snapshot.diagnostics.ntfyFailures} 次连续失败 · ${diagnosticNtfyIssue(snapshot.diagnostics.ntfyErrorCode)}`;
  useEffect(() => {
    if (startingReindex || !reindexRunId || snapshot.index.runId !== reindexRunId) return;
    if (snapshot.index.state === "running") {
      setReindexState({ status: "pending" });
    } else if (snapshot.index.state === "complete") {
      setReindexState({ status: "success", value: `重新索引完成，共处理 ${snapshot.index.completed} 个会话记录` });
    } else if (snapshot.index.state === "failed") {
      setReindexState({ status: "error", error: reindexFailureMessage(snapshot.index) });
    }
  }, [reindexRunId, snapshot.index, startingReindex]);
  useEffect(() => {
    if (openSettings.state.status !== "success") return;
    const timeout = window.setTimeout(openSettings.reset, 3000);
    return () => window.clearTimeout(timeout);
  }, [openSettings.state.status, openSettings.reset]);

  const reindex = async () => {
    setStartingReindex(true);
    setReindexState({ status: "pending" });
    try {
      const { runId } = await api.reindex();
      setReindexRunId(runId);
      onChanged();
    } catch {
      setReindexState({ status: "error", error: userSafeError("reindex") });
    } finally {
      setStartingReindex(false);
    }
  };

  const copyDiagnostics = async () => {
    try {
      await copy.run();
    } catch {
      // The action state provides the accessible error feedback.
    }
  };

  const confirmCleanup = async () => {
    try {
      await cleanup.run();
      setCleanupConfirmation(false);
      onChanged();
    } catch {
      // The action state provides the accessible error feedback.
    }
  };
  const reindexProgress = !startingReindex && snapshot.index.runId === reindexRunId;
  const reindexProgressKnown = reindexProgress && snapshot.index.total > 0;
  const reindexMessage = reindexState.status === "pending"
    ? `正在重新索引…${reindexProgressKnown ? ` ${snapshot.index.completed}/${snapshot.index.total}` : ""}`
    : reindexState.status === "success" ? reindexState.value
    : reindexState.status === "error" ? reindexState.error
    : null;

  return (
    <>
      <PageTitle title="诊断" subtitle="诊断信息不包含会话记录内容或通知凭据" />
      <section className="diagnostic-workspace" aria-labelledby="diagnostic-status-title">
        <div className="diagnostic-heading">
          <SectionTitle id="diagnostic-status-title" title="运行状态" subtitle={issueCount === 0 ? "所有本地组件运行正常" : `${issueCount} 项需要处理`} />
          <div className="diagnostic-action">
            <button className="secondary" disabled={copy.state.status === "pending"} aria-busy={copy.state.status === "pending"} onClick={copyDiagnostics}>{copy.state.status === "pending" ? "正在复制…" : "复制诊断"}</button>
            {copy.state.status !== "idle" && (
              <span className="action-message" role={copy.state.status === "error" ? "alert" : "status"} aria-live={copy.state.status === "error" ? undefined : "polite"}>
                {copy.state.status === "pending" ? "正在复制诊断…" : copy.state.status === "success" ? "诊断信息已复制" : copy.state.status === "error" ? copy.state.error : null}
              </span>
            )}
          </div>
        </div>
        <div className={`health-summary ${issueCount > 0 ? "attention" : ""}`}>
          <span aria-hidden="true" />
          <div><strong>{issueCount === 0 ? "运行正常" : `${issueCount} 项需要处理`}</strong><small>{issueCount === 0 ? "事件、通知和后台任务均未发现异常" : "请检查下方标记的组件"}</small></div>
        </div>
        <div className="diagnostic-grid">
          <section className="surface diagnostic-card" aria-labelledby="collector-health-title">
            <SectionTitle id="collector-health-title" title="事件采集" subtitle="事件采集器、索引与本地事件队列" />
            <HookRecoveryCallout
              hook={snapshot.hook}
              disposition={snapshot.hookOnboardingDisposition}
              onChanged={onChanged}
              location="diagnostics"
            />
            <Info label="事件采集器" value={hookVersion(snapshot)} />
            <Info label="历史索引" value={indexStateLabel(snapshot.index.state)} />
            <Info label="待处理事件" value={String(snapshot.diagnostics.pendingEvents)} />
            <Info label="隔离会话" value={String(snapshot.diagnostics.quarantinedSessions)} />
            <Info label="数据库迁移版本" value={String(snapshot.diagnostics.migrationVersion)} />
          </section>
          <section className="surface diagnostic-card" aria-labelledby="notification-health-title">
            <SectionTitle id="notification-health-title" title="通知通道" subtitle="桌面通知与远程通知投递状态" />
            <Info label="桌面通知" value={desktopHealth} />
            <Info label="ntfy" value={ntfyHealth} />
            <Info label="待发送通知" value={String(snapshot.diagnostics.pendingNotifications)} />
            <div className="diagnostic-channel-actions" role="group" aria-label="通知诊断工具">
              <div className="diagnostic-action">
                <button className="secondary" disabled={testDesktopNotification.state.status === "pending"} aria-busy={testDesktopNotification.state.status === "pending"} onClick={() => { testDesktopNotification.run().catch(() => undefined); }}>{testDesktopNotification.state.status === "pending" ? "正在发送…" : "测试桌面通知"}</button>
                {testDesktopNotification.state.status !== "idle" && testDesktopNotification.state.status !== "pending" && (
                  <span className="action-message" role={testDesktopNotification.state.status === "error" ? "alert" : "status"} aria-live={testDesktopNotification.state.status === "error" ? undefined : "polite"}>
                    {testDesktopNotification.state.status === "success" ? "桌面通知已发送" : testDesktopNotification.state.status === "error" ? testDesktopNotification.state.error : null}
                  </span>
                )}
              </div>
              <div className="diagnostic-action">
                <button className="secondary" disabled={openSettings.state.status === "pending"} aria-busy={openSettings.state.status === "pending"} onClick={() => { openSettings.run().catch(() => undefined); }}>{openSettings.state.status === "pending" ? "正在打开…" : "打开通知设置"}</button>
                {openSettings.state.status !== "idle" && openSettings.state.status !== "pending" && (
                  <span className="action-message" role={openSettings.state.status === "error" ? "alert" : "status"} aria-live={openSettings.state.status === "error" ? undefined : "polite"}>
                    {openSettings.state.status === "success" ? "通知设置已打开" : openSettings.state.status === "error" ? openSettings.state.error : null}
                  </span>
                )}
              </div>
            </div>
          </section>
        </div>
        <section className="surface diagnostic-card background-health" aria-labelledby="background-health-title">
          <SectionTitle id="background-health-title" title="后台任务" subtitle={backgroundIssues.length === 0 ? "所有任务运行正常" : `${backgroundIssues.length} 个任务连续失败`} />
          <div className="background-health-rows">
            {snapshot.diagnostics.backgroundHealth.map((health) => (
              <div className="health-row" key={health.task}>
                <span>{backgroundTaskLabel(health.task)}</span>
                <strong className={health.consecutiveFailures > 0 ? "attention" : ""}>{health.consecutiveFailures > 0 ? `${health.consecutiveFailures} 次连续失败` : "正常"}</strong>
              </div>
            ))}
            {snapshot.diagnostics.backgroundHealth.length === 0 && <div className="empty compact">暂无后台任务记录</div>}
          </div>
          {snapshot.diagnostics.backgroundHealth.length > 0 && (
            <details className="technical-details">
              <summary>查看技术详情</summary>
              {snapshot.diagnostics.backgroundHealth.map((health) => (
                <Info
                  key={health.task}
                  label={backgroundTaskLabel(health.task)}
                  value={[
                    `成功 ${health.successCount}`,
                    `失败 ${health.failureCount}`,
                    `最近成功 ${healthTime(health.lastSucceededAtMs)}`,
                    `最近失败 ${healthTime(health.lastFailedAtMs)}`,
                    `最近恢复 ${healthTime(health.recoveredAtMs)}`,
                  ].join(" · ")}
                />
              ))}
            </details>
          )}
        </section>
      </section>
      <section className="maintenance" aria-labelledby="reindex-title">
        <div className="maintenance-copy">
          <SectionTitle id="reindex-title" title="历史会话索引" subtitle="重新读取 Claude Code 会话记录，刷新历史用量与状态。" />
          {reindexState.status !== "idle" && (
            <div className={`operation-toast ${reindexState.status === "error" ? "failed" : ""}`} role={reindexState.status === "error" ? "alert" : "status"} aria-live={reindexState.status === "error" ? undefined : "polite"} aria-atomic="true">
              <span>{reindexMessage}</span>
              {reindexState.status === "pending" && (reindexProgressKnown
                ? <progress aria-label="重新索引进度" value={snapshot.index.completed} max={snapshot.index.total} />
                : <progress aria-label="重新索引进度" />)}
            </div>
          )}
        </div>
        <button className="primary" disabled={reindexState.status === "pending" || snapshot.index.state === "running"} aria-busy={reindexState.status === "pending" || snapshot.index.state === "running"} onClick={reindex}>
          {reindexState.status === "pending" ? "正在重新索引…" : "重新索引会话记录"}
        </button>
      </section>
      <section className="danger-zone" aria-label="危险操作：数据维护">
        <div>
          <SectionTitle id="cleanup-title" title="数据维护" subtitle="清理已结束会话的原始事件与终态通知记录。" />
          {cleanup.state.status === "success" && <div className="operation-toast" role="status" aria-live="polite">{cleanup.state.value.rawEventsDeleted || cleanup.state.value.notificationsDeleted ? `已删除 ${cleanup.state.value.rawEventsDeleted} 条原始事件和 ${cleanup.state.value.notificationsDeleted} 条通知记录` : "没有符合条件的旧记录可清理"}</div>}
          {cleanup.state.status === "error" && <div className="operation-toast failed" role="alert">{cleanup.state.error}</div>}
        </div>
        {!cleanupConfirmation ? <button className="danger" disabled={cleanup.state.status === "pending"} onClick={() => { cleanup.reset(); setCleanupConfirmation(true); }}>清理已处理记录</button> : (
          <div className="cleanup-confirmation" role="group" aria-label="确认清理历史记录">
            <p>将删除已结束会话的原始事件、会话状态，以及已发送、已抑制和重试耗尽的通知记录；活跃会话、Token 用量与每日汇总、会话记录游标、待发送和仍可重试的失败通知都会保留。</p>
            <button className="danger" disabled={cleanup.state.status === "pending"} aria-busy={cleanup.state.status === "pending"} onClick={confirmCleanup}>{cleanup.state.status === "pending" ? "正在清理…" : "确认清理"}</button>
            <button className="secondary" disabled={cleanup.state.status === "pending"} onClick={() => { cleanup.reset(); setCleanupConfirmation(false); }}>取消</button>
          </div>
        )}
      </section>
    </>
  );
}

function Feedback({ state }: { state: ActionState<string> }) {
  if (state.status === "idle") return null;
  let text: string;
  if (state.status === "pending") text = "正在保存设置…";
  else if (state.status === "success") text = state.value;
  else if (state.status === "error") text = state.error;
  else return null;
  return <div className={`operation-banner ${state.status === "error" ? "failed" : ""}`} role={state.status === "error" ? "alert" : "status"} aria-live={state.status === "error" ? undefined : "polite"} aria-atomic="true">{text}</div>;
}
function PageTitle({ title, subtitle, headingRef }: { title: string; subtitle?: string; headingRef?: (heading: HTMLHeadingElement | null) => void }) {
  return <header className="page-title"><div><h1 ref={headingRef} tabIndex={-1}>{title}</h1>{subtitle && <p>{subtitle}</p>}</div></header>;
}
function SectionTitle({ id, title, subtitle }: { id: string; title: string; subtitle: string }) {
  return <div className="section-title"><h2 id={id}>{title}</h2><p>{subtitle}</p></div>;
}
function Loading({ label = "正在读取本地监控数据…" }: { label?: string }) { return <div className="loading" role="status" aria-live="polite">{label}</div>; }
function Empty({ text }: { text: string }) { return <div className="empty">{text}</div>; }
function State({ state }: { state: string }) { return <span className={`state-dot ${state}`} aria-hidden="true" />; }
function stateLabel(state: string) { return ({ running: "运行中", waiting: "等待中", needs_input: "需要介入", failed: "失败" }[state] || state); }
function backgroundTaskLabel(task: string) {
  return (({
    incremental_index: "增量索引",
    engine_processing: "状态处理",
    engine_reconciliation: "状态校正",
    startup_reconciliation: "启动校正",
    retention_cleanup: "历史清理",
  } as Record<string, string>)[task] || task);
}
function healthTime(value: number | null) {
  return value === null ? "—" : new Date(value).toLocaleString();
}
function diagnosticDesktopIssue(code: string | null) {
  return ({
    delivery_failed: "通知发送失败",
    desktop_delivery_failed: "通知发送失败",
    permission_denied: "通知权限未开启",
    desktop_notification_permission_denied: "通知权限未开启",
    permission_request_failed: "通知权限申请失败",
    desktop_notification_permission_request_failed: "通知权限申请失败",
    requires_app_bundle: "需要从应用程序启动",
    desktop_notification_requires_app_bundle: "需要从应用程序启动",
  } as Record<string, string>)[code || ""] || "桌面通知异常";
}
function diagnosticNtfyIssue(code: string | null) {
  return ({
    auth_failed: "认证失败",
    ntfy_auth_failed: "认证失败",
    permission_denied: "发布权限不足",
    ntfy_permission_denied: "发布权限不足",
    not_found: "服务或 Topic 不存在",
    ntfy_not_found: "服务或 Topic 不存在",
    rate_limited: "发送频率受限",
    ntfy_rate_limited: "发送频率受限",
    server_failed: "服务器异常",
    ntfy_server_failed: "服务器异常",
    network_timeout: "连接超时",
    ntfy_network_timeout: "连接超时",
    network_connect: "无法连接服务器",
    ntfy_network_connect: "无法连接服务器",
    network_tls: "安全连接验证失败",
    ntfy_network_tls: "安全连接验证失败",
  } as Record<string, string>)[code || ""] || "远程通知异常";
}
function Info({ label, value }: { label: string; value: string }) { return <div className="info"><span>{label}</span><strong>{value}</strong></div>; }
function UsageRows({ value }: { value: DashboardSnapshot["today"] }) {
  return <>{[
    ["输入 Token", value.inputTokens], ["输出 Token", value.outputTokens],
    ["缓存读取", value.cacheReadTokens], ["缓存写入", value.cacheWriteTokens],
  ].map(([label, amount]) => <Info key={String(label)} label={String(label)} value={compact(amount)} />)}
    <Info label="费用" value={costWithCoverage(value.costPicoUsd, value.costKnown, value.unpricedTokens, tokens(value))} /></>;
}
function hookVersion(snapshot: DashboardSnapshot) {
  if (snapshot.hook.status === "repair_required") return "需要修复";
  if (snapshot.hook.status === "absent") return "未安装";
  return snapshot.hook.version ? `版本 ${snapshot.hook.version}` : "已安装（版本未知）";
}
function hookStatusLabel(status: DashboardSnapshot["hook"]["status"]) {
  return status === "installed" ? "已安装" : status === "repair_required" ? "需修复" : "未安装";
}
function hookIssueLabel(issue: string | null) {
  const labels: Record<string, string> = {
    hook_ownership_missing: "安装记录缺失",
    hook_ownership_invalid: "安装记录无效",
    hook_ownership_path_mismatch: "程序路径不匹配",
    hook_binary_missing: "采集程序缺失",
    hook_binary_invalid: "采集程序不可执行",
    hook_managed_path_unsafe: "托管程序目录不安全",
    hook_settings_mismatch: "Claude Code 设置不匹配",
    hook_settings_unreadable: "Claude Code 设置无法读取",
  };
  return issue ? labels[issue] || "安装状态异常" : "安装状态异常";
}
export function indexStateLabel(state: DashboardSnapshot["index"]["state"]) {
  return ({ idle: "就绪", running: "索引中", complete: "已完成", failed: "失败" } as const)[state];
}
export function eventLabel(event: string) {
  const labels: Record<string, string> = {
    SessionStart: "会话开始",
    SessionEnd: "会话结束",
    StopFailure: "会话执行失败",
    PreToolUse: "工具调用开始",
    PostToolUse: "工具调用完成",
    Notification: "收到通知事件",
    Stop: "本轮完成",
    UserPromptSubmit: "用户提交消息",
    TranscriptAssistantToolUse: "助手调用工具",
    TranscriptAssistantThinking: "助手思考",
    TranscriptToolResult: "工具返回结果",
    TranscriptAssistantText: "助手输出",
  };
  return labels[event] || `未知事件（${event || "未命名"}）`;
}
function sourceLabel(source: string) {
  return source === "hook" ? "事件采集器" : source === "transcript" ? "会话记录" : "状态恢复";
}
function Toggle({ label, checked, disabled = false, onChange }: { label: string; checked: boolean; disabled?: boolean; onChange: (value: boolean) => void }) {
  return <label className="toggle-row"><span>{label}</span><input type="checkbox" checked={checked} disabled={disabled} onChange={(event) => onChange(event.target.checked)} /><i /></label>;
}
function Field({ id, label, value, error, disabled = false, onChange, placeholder = "", type = "text" }: { id?: string; label: string; value: string; error?: string; disabled?: boolean; onChange: (value: string) => void; placeholder?: string; type?: string }) {
  const errorId = error && id ? `${id}-error` : undefined;
  return <label className="field"><span>{label}</span><input id={id} type={type} value={value} disabled={disabled} placeholder={placeholder} aria-invalid={error ? "true" : undefined} aria-describedby={errorId} onChange={(event) => onChange(event.target.value)} />{error && <small id={errorId} className="field-error" role="alert">{error}</small>}</label>;
}

function validateSettings(settings: SettingsDto) {
  if (!settings.ntfyEnabled) return {};
  return validateNtfy(settings);
}
function validateNtfy(settings: SettingsDto) {
  const errors: { ntfyServer?: string; ntfyTopic?: string } = {};
  const server = settings.ntfyServer.trim();
  if (!server) errors.ntfyServer = "请输入服务器地址";
  else if (!/^https?:\/\//i.test(server)) errors.ntfyServer = "请输入以 http:// 或 https:// 开头的完整服务器地址";
  else {
    try {
      const url = new URL(server);
      if (!url.hostname) errors.ntfyServer = "服务器地址缺少有效的主机名";
      else if (url.username || url.password) errors.ntfyServer = "服务器地址中不要包含用户名或密码，请填写下方独立字段";
      else if (url.search || url.hash) errors.ntfyServer = "服务器地址中不要包含查询参数或 # 片段";
      else if (url.protocol === "http:") {
        const authority = server.slice(server.indexOf("://") + 3).split(/[/?#]/, 1)[0];
        const sourceHost = authority.startsWith("[")
          ? authority.slice(1, authority.indexOf("]"))
          : authority.includes(":") ? authority.slice(0, authority.lastIndexOf(":")) : authority;
        const loopback = ["localhost", "127.0.0.1", "::1"].includes(sourceHost.toLowerCase());
        if (!loopback) errors.ntfyServer = "非本机 ntfy 服务器必须使用 HTTPS";
        else if (
          settings.ntfyUsername.length > 0
          || Boolean(settings.ntfyPassword)
          || settings.ntfyPasswordSet
        ) errors.ntfyServer = "使用用户名或密码时必须使用 HTTPS";
      }
    } catch {
      errors.ntfyServer = "服务器地址格式不正确，请检查地址和端口";
    }
  }
  errors.ntfyTopic = validateNtfyTopic(settings.ntfyTopic);
  return errors;
}
export function validateNtfyTopic(topic: string) {
  if (!topic) return "请输入通知主题";
  if (!/^[A-Za-z0-9_-]{1,64}$/.test(topic)) {
    return "通知主题只能包含字母、数字、下划线和连字符，长度为 1-64";
  }
  return undefined;
}
function ago(timestamp: number) {
  const seconds = Math.max(0, Math.round((Date.now() - timestamp) / 1000));
  if (seconds < 60) return `${seconds} 秒前`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
  return `${Math.floor(seconds / 3600)} 小时前`;
}
function shortSessionId(sessionId: string) {
  return sessionId.replaceAll("-", "").slice(0, 6);
}
