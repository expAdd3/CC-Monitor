import { type CSSProperties, type KeyboardEvent, useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  api,
  compact,
  cost,
  decimal,
  demoSnapshot,
  tokens,
  type DashboardSnapshot,
} from "./api";
import {
  CollectorStatusCallout,
  hookStatusLabel,
  hookVersion,
} from "./features/collector/CollectorStatusCallout";
import { Diagnostics, indexStateLabel } from "./features/diagnostics/Diagnostics";
import { buildAnnualHeatmap, buildRecentHistory } from "./features/history/model";
import { Settings } from "./features/settings/Settings";
import { useAsyncResource } from "./lib/asyncState";
import { sessionStateReasonLabel } from "./sessionStateReasons";
import { Info, Loading, PageTitle } from "./ui";

const logoUrl = new URL("./assets/app_icon_color.svg", import.meta.url).href;

function focusedElement() {
  return document.activeElement instanceof HTMLElement ? document.activeElement : null;
}

function focusStillFollowsNavigation(source: HTMLElement | null) {
  const active = document.activeElement;
  if (active === source) return true;
  return active === document.body && (!source || source === document.body || !source.isConnected);
}

type Route = "dashboard" | "history" | "settings" | "diagnostics" | `session:${string}`;
type MainRoute = Exclude<Route, `session:${string}`>;

function routeTitle(route: Route) {
  if (route.startsWith("session:")) return "会话详情";
  return ({
    dashboard: "运行总览",
    history: "历史趋势",
    settings: "设置",
    diagnostics: "诊断",
  } as const)[route as MainRoute];
}

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

function useLocalMidnightRefresh(refresh: (revision: number) => Promise<void>) {
  useEffect(() => {
    let timeout: number | undefined;
    const schedule = () => {
      const now = new Date();
      const nextMidnight = new Date(now);
      nextMidnight.setHours(24, 0, 0, 0);
      timeout = window.setTimeout(() => {
        void refresh(0);
        schedule();
      }, Math.max(1, nextMidnight.getTime() - now.getTime()));
    };
    schedule();
    return () => {
      if (timeout !== undefined) window.clearTimeout(timeout);
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
          setError("暂时无法读取监控数据，请稍后重试");
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
  useLocalMidnightRefresh(refresh);
  useEffect(() => {
    const onHash = () => setRoute(routeFromHash());
    addEventListener("hashchange", onHash);
    return () => removeEventListener("hashchange", onHash);
  }, []);
  useEffect(() => {
    if (previousRoute.current === route) return;
    focusIntent.current = { route, source: focusedElement() };
    previousRoute.current = route;
    const intent = focusIntent.current;
    const heading = contentRef.current?.querySelector<HTMLElement>("h1");
    if (!intent || intent.route !== route || !heading) return;
    focusIntent.current = null;
    if (focusStillFollowsNavigation(intent.source)) heading.focus();
  }, [route]);

  return (
    <div className="app-shell">
      <a
        className="skip-link"
        href="#main-content"
        onClick={(event) => {
          event.preventDefault();
          focusIntent.current = null;
          const main = contentRef.current;
          main?.focus({ preventScroll: true });
          main?.scrollIntoView?.({ block: "start" });
        }}
      >跳到主要内容</a>
      <aside className="sidebar" aria-label="应用侧边栏">
        <div className="brand">
          <img className="brand-mark" src={logoUrl} alt="CC Monitor 闹钟终端标志" />
          <div><strong>CC Monitor</strong><small>Claude Code 会话监控</small></div>
        </div>
        <nav aria-label="主导航">
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
      <main id="main-content" className="content" aria-label="主要内容" ref={contentRef} tabIndex={-1}>
        {error && <div className="error-banner" role="alert" aria-live="assertive">监控数据刷新失败：{error}</div>}
        {loading ? <><PageTitle title={routeTitle(route)} /><Loading /></> : (
          <>
            {route === "dashboard" && <Dashboard snapshot={snapshot} />}
            {route === "history" && <History snapshot={snapshot} />}
            {route === "settings" && <Settings hook={snapshot.hook} onboardingDisposition={snapshot.hookOnboardingDisposition} onSaved={refresh} />}
            {route === "diagnostics" && <Diagnostics snapshot={snapshot} onChanged={refresh} onOpenSettings={() => navigate("settings")} />}
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

function Dashboard({ snapshot }: { snapshot: DashboardSnapshot }) {
  const cards = [
    ["运行中", snapshot.counts.running, "running"],
    ["等待中", snapshot.counts.waiting, "waiting"],
    ["需要介入", snapshot.counts.needsInput, "needs"],
    ["今日 Token", compact(tokens(snapshot.today)), "tokens"],
  ];
  return (
    <>
      <PageTitle title="运行总览" />
      <CollectorStatusCallout hook={snapshot.hook} onboardingDisposition={snapshot.hookOnboardingDisposition} location="dashboard" onOpenSettings={() => navigate("settings")} />
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
          <Info label="历史记录扫描" value={indexStateLabel(snapshot.index.state)} />
          <Info label="等待处理的事件" value={String(snapshot.diagnostics.pendingEvents)} />
          <Info label="处理失败的会话" value={String(snapshot.diagnostics.quarantinedSessions)} />
        </article>
      </section>
    </>
  );
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
  const recent = buildRecentHistory(snapshot.trends, now);
  const [activeDayKey, setActiveDayKey] = useState(() => recent.days[recent.initialIndex].day);
  const matchingActiveIndex = recent.days.findIndex((day) => day.day === activeDayKey);
  const activeIndex = matchingActiveIndex >= 0 ? matchingActiveIndex : recent.initialIndex;
  const activeDay = recent.days[activeIndex];
  useEffect(() => {
    if (activeDayKey !== activeDay.day) setActiveDayKey(activeDay.day);
  }, [activeDay.day, activeDayKey]);
  const activeCost = costWithCoverage(activeDay.costPicoUsd, activeDay.costKnown, activeDay.unpricedTokens, activeDay.tokens);
  const activeDetail = `${activeDay.day}，${compact(activeDay.tokens)} Token，${activeCost}，未计费 Token ${compact(activeDay.unpricedTokens ?? 0)}`;
  const selectWithKeyboard = (event: KeyboardEvent<HTMLDivElement>) => {
    let next = activeIndex;
    if (event.key === "ArrowLeft" || event.key === "ArrowDown") next -= 1;
    else if (event.key === "ArrowRight" || event.key === "ArrowUp") next += 1;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = recent.days.length - 1;
    else return;
    event.preventDefault();
    setActiveDayKey(recent.days[Math.min(Math.max(next, 0), recent.days.length - 1)].day);
  };
  const heatmapStyle = { "--heatmap-columns": annual.columns } as CSSProperties;
  const rangeTokens = annual.days.reduce((total, day) => total + decimal(day.tokens), 0n);
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
          <p>固定显示最近 30 个自然日；指向或选择柱形查看当天详情。</p>
        </div>
        {recent.maxTokens === 0n && <p className="recent-history-empty">最近 30 天暂无用量，仍显示完整日期范围。</p>}
        <div
          className="recent-chart"
          role="group"
          aria-roledescription="交互式柱状图"
          aria-label={`最近 30 天每日 Token 用量。当前日期：${activeDetail}`}
          aria-describedby="recent-chart-instructions"
          tabIndex={0}
          onKeyDown={selectWithKeyboard}
        >
          <span id="recent-chart-instructions" className="visually-hidden">使用左右方向键逐日浏览，Home 跳到第一天，End 跳到今天。</span>
          <div className="recent-chart-plot" aria-hidden="true">
            {recent.days.map((day, index) => {
              const value = decimal(day.tokens);
              const percent = recent.maxTokens === 0n ? 0 : Number(value * 10_000n / recent.maxTokens) / 100;
              return <span
                className={`recent-bar ${index === activeIndex ? "active" : ""} ${value === 0n ? "zero" : ""}`}
                key={day.day}
                onPointerEnter={() => setActiveDayKey(day.day)}
                onClick={() => setActiveDayKey(day.day)}
              ><i style={{ "--bar-height": `${percent}%` } as CSSProperties} /></span>;
            })}
          </div>
          <div className="recent-chart-ticks" aria-hidden="true">
            {recent.tickIndexes.map((index) => <span key={recent.days[index].day} style={{ gridColumn: index + 1 }}>{recent.days[index].day.slice(5)}</span>)}
          </div>
        </div>
        <div className="recent-day-detail" role="status" aria-live="polite" aria-atomic="true">
          <strong>{activeDay.date.getFullYear()}年{activeDay.date.getMonth() + 1}月{activeDay.date.getDate()}日</strong>
          <span><small>Token</small>{compact(activeDay.tokens)}</span>
          <span><small>费用</small>{activeCost}</span>
          <span><small>未计费 Token</small>{compact(activeDay.unpricedTokens ?? 0)}</span>
        </div>
        <table className="visually-hidden" aria-label="最近 30 天每日用量数据">
          <caption>最近 30 个自然日 Token、费用及未计费 Token</caption>
          <thead><tr><th scope="col">日期</th><th scope="col">Token</th><th scope="col">费用</th><th scope="col">未计费 Token</th></tr></thead>
          <tbody>{recent.days.map((day) => <tr key={day.day}><th scope="row">{day.day}</th><td>{compact(day.tokens)}</td><td>{costWithCoverage(day.costPicoUsd, day.costKnown, day.unpricedTokens, day.tokens)}</td><td>{compact(day.unpricedTokens ?? 0)}</td></tr>)}</tbody>
        </table>
      </section>
      <section className="history-hero" aria-labelledby="annual-history-title">
        <div className="history-hero-top">
          <div className="section-title">
          <h2 id="annual-history-title">过去一年 Token 活跃度</h2>
          <p>最近 365 天滚动区间；首尾月份仅显示区间内日期，颜色越深表示用量越高。</p>
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
              {annual.monthLabels.map((day) => (
                <span key={day.day} style={{ gridColumn: `${day.labelColumn} / span ${day.labelSpan}` }}>{day.label}</span>
              ))}
            </div>
            <div className="heatmap-weekdays" aria-hidden="true"><span>周一</span><span /><span>周三</span><span /><span>周五</span><span /><span /></div>
            <div className="heatmap-grid" role="img" aria-label={heatmapSummary}>
              {annual.days.map((day) => (
                    <span
                      key={day.day}
                      className="heatmap-cell"
                      data-level={day.level}
                      aria-hidden="true"
                      title={`${day.day}，${compact(day.tokens)} Token，${costWithCoverage(day.costPicoUsd, day.costKnown, day.unpricedTokens, day.tokens)}`}
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
  const detail = useAsyncResource(loadSession, "暂时无法读取会话详情，请返回后重试");
  const value = detail.state.status === "success" ? detail.state.value : null;
  return (
    <>
      <button className="back" onClick={() => navigate("dashboard")}>← 返回总览</button>
      <PageTitle
        title={value ? value.session.projectName || value.session.sessionId.slice(0, 16) : "会话详情"}
        subtitle={value ? `会话 #${shortSessionId(value.session.sessionId)} · ${value.session.sessionId}` : `会话 ${sessionId}`}
      />
      {detail.state.status === "error" && <div className="error-banner" role="alert" aria-live="assertive">会话详情加载失败：{detail.state.error} <button type="button" className="secondary" onClick={detail.retry}>重试</button></div>}
      {detail.state.status === "pending" && <Loading label="正在读取会话详情…" />}
      {value && (
      <div className="session-workspace">
        <aside className="session-summary" aria-label="会话摘要">
          <section>
            <h2>当前状态</h2>
            <div className="large-state"><State state={value.session.turnState} />{stateLabel(value.session.turnState)}</div>
            <Info label="原因" value={sessionStateReasonLabel(value.session.stateReason)} />
            <Info label="最近活动" value={new Date(value.session.lastObservedAtMs).toLocaleString()} />
          </section>
          <section className="session-model-usage">
            <div className="session-model-heading"><h2>模型用量</h2><p>按 Token 类型拆分</p></div>
            {value.models.length > 0 ? <div className="session-model-table-wrap">
              <table className="session-model-table">
                <caption>本次会话各模型的 Token 用量</caption>
                <thead><tr><th scope="col">模型</th><th scope="col">输入</th><th scope="col">输出</th><th scope="col">缓存读取</th><th scope="col">缓存创建</th></tr></thead>
                <tbody>{value.models.map((model) => (
                  <tr key={model.modelId}>
                    <th scope="row"><strong>{model.modelId}</strong><small>合计 {compact(model.tokens)} · {costWithCoverage(model.costPicoUsd, model.costKnown, model.unpricedTokens, model.tokens)}</small></th>
                    <td>{compact(model.inputTokens)}</td>
                    <td>{compact(model.outputTokens)}</td>
                    <td>{compact(model.cacheReadTokens)}</td>
                    <td>{compact(model.cacheWriteTokens)}</td>
                  </tr>
                ))}</tbody>
              </table>
            </div> : <Empty text="暂无模型用量" />}
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
      )}
    </>
  );
}

function Empty({ text }: { text: string }) { return <div className="empty">{text}</div>; }
function State({ state }: { state: string }) { return <span className={`state-dot ${state}`} aria-hidden="true" />; }
function stateLabel(state: string) { return ({ running: "运行中", waiting: "等待中", needs_input: "需要介入", failed: "失败" }[state] || state); }
function UsageRows({ value }: { value: DashboardSnapshot["today"] }) {
  return <>{[
    ["输入 Token", value.inputTokens], ["输出 Token", value.outputTokens],
    ["缓存读取", value.cacheReadTokens], ["缓存创建", value.cacheWriteTokens],
  ].map(([label, amount]) => <Info key={String(label)} label={String(label)} value={compact(amount)} />)}
    <Info label="费用" value={costWithCoverage(value.costPicoUsd, value.costKnown, value.unpricedTokens, tokens(value))} /></>;
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
function ago(timestamp: number) {
  const seconds = Math.max(0, Math.round((Date.now() - timestamp) / 1000));
  if (seconds < 60) return `${seconds} 秒前`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
  return `${Math.floor(seconds / 3600)} 小时前`;
}
function shortSessionId(sessionId: string) {
  return sessionId.replaceAll("-", "").slice(0, 6);
}
