import { useEffect, useState } from "react";
import { api, type DashboardSnapshot, type IndexProgress } from "../../api";
import { useAsyncAction, type ActionState } from "../../lib/asyncState";
import { Info, PageTitle, SectionTitle } from "../../ui";
import { CollectorStatusCallout, hookVersion } from "../collector/CollectorStatusCallout";

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
  if (index.interrupted) return "历史记录扫描失败，请重试";
  const failedFiles = Math.max(0, Math.trunc(index.failedFiles));
  const quarantinedSessions = Math.max(0, Math.trunc(index.quarantinedSessions));
  if (failedFiles > 0 && quarantinedSessions > 0) {
    return `${failedFiles} 个记录文件读取失败，${quarantinedSessions} 个处理失败的会话，其他内容已完成`;
  }
  if (failedFiles > 0) return `${failedFiles} 个记录文件读取失败，其他内容已完成`;
  if (quarantinedSessions > 0) return `${quarantinedSessions} 个处理失败的会话，其他内容已完成`;
  return "历史记录扫描失败，请重试";
}

function backgroundTaskLabel(task: string) {
  return ({
    incremental_index: "增量扫描",
    engine_processing: "状态处理",
    engine_reconciliation: "校正会话状态",
    startup_reconciliation: "启动时校正状态",
    retention_cleanup: "历史清理",
  } as Record<string, string>)[task] || task;
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

export function indexStateLabel(state: DashboardSnapshot["index"]["state"]) {
  return ({ idle: "就绪", running: "扫描中", complete: "已完成", failed: "失败" } as const)[state];
}

export function Diagnostics({ snapshot, onChanged, onOpenSettings }: {
  snapshot: DashboardSnapshot;
  onChanged: () => Promise<void>;
  onOpenSettings: () => void;
}) {
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
      ...snapshot.diagnostics.backgroundHealth.map((health) => [
        `background_task=${health.task}`,
        `successes=${health.successCount}`,
        `failures=${health.failureCount}`,
        `consecutive_failures=${health.consecutiveFailures}`,
        `error_code=${health.errorCode || "none"}`,
        `last_succeeded_at_ms=${health.lastSucceededAtMs ?? "none"}`,
        `last_failed_at_ms=${health.lastFailedAtMs ?? "none"}`,
        `recovered_at_ms=${health.recoveredAtMs ?? "none"}`,
      ].join(" ")),
    ].join("\n");
    await navigator.clipboard.writeText(report);
  }, "复制失败，请检查剪贴板权限");
  const openSettings = useAsyncAction(() => api.openNotificationSettings(), "无法打开通知设置");
  const testDesktopNotification = useAsyncAction(() => api.testDesktopNotification(), desktopNotificationError);
  const cleanup = useAsyncAction(() => api.clearHistory(), "清理失败，请重试");
  const backgroundIssues = snapshot.diagnostics.backgroundHealth.filter((health) => health.consecutiveFailures > 0);
  const backgroundHealthUnknown = snapshot.diagnostics.backgroundHealth.length === 0;
  const issueCount = [
    snapshot.hook.status !== "installed",
    snapshot.index.state === "failed",
    snapshot.diagnostics.pendingEvents > 0,
    snapshot.diagnostics.quarantinedSessions > 0,
    snapshot.diagnostics.pendingNotifications > 0,
    snapshot.diagnostics.desktopFailures > 0,
    snapshot.diagnostics.ntfyFailures > 0,
    ...snapshot.diagnostics.backgroundHealth.map((health) => health.consecutiveFailures > 0),
  ].filter(Boolean).length;
  const desktopHealth = snapshot.diagnostics.desktopFailures === 0
    ? "未发现连续失败"
    : `${snapshot.diagnostics.desktopFailures} 次连续失败 · ${diagnosticDesktopIssue(snapshot.diagnostics.desktopErrorCode)}`;
  const ntfyHealth = snapshot.diagnostics.ntfyFailures === 0
    ? "未发现连续失败"
    : `${snapshot.diagnostics.ntfyFailures} 次连续失败 · ${diagnosticNtfyIssue(snapshot.diagnostics.ntfyErrorCode)}`;

  useEffect(() => {
    if (startingReindex || !reindexRunId || snapshot.index.runId !== reindexRunId) return;
    if (snapshot.index.state === "running") setReindexState({ status: "pending" });
    else if (snapshot.index.state === "complete") {
      setReindexState({ status: "success", value: `历史记录扫描完成，共处理 ${snapshot.index.completed} 个会话记录` });
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
      void onChanged();
    } catch {
      setReindexState({ status: "error", error: "历史记录扫描失败，请重试" });
    } finally {
      setStartingReindex(false);
    }
  };
  const copyDiagnostics = async () => {
    try { await copy.run(); } catch { /* feedback is rendered below */ }
  };
  const confirmCleanup = async () => {
    try {
      await cleanup.run();
      setCleanupConfirmation(false);
      void onChanged();
    } catch {
      // The action state provides the accessible error feedback.
    }
  };
  const reindexProgress = !startingReindex && snapshot.index.runId === reindexRunId;
  const reindexProgressKnown = reindexProgress && snapshot.index.total > 0;
  const reindexMessage = reindexState.status === "pending"
    ? `正在扫描历史记录…${reindexProgressKnown ? ` ${snapshot.index.completed}/${snapshot.index.total}` : ""}`
    : reindexState.status === "success" ? reindexState.value
    : reindexState.status === "error" ? reindexState.error
    : null;

  return <>
    <PageTitle title="诊断" subtitle="诊断信息不包含会话记录内容或通知凭据" />
    <section className="diagnostic-workspace" aria-labelledby="diagnostic-status-title">
      <div className="diagnostic-heading">
        <SectionTitle id="diagnostic-status-title" title="运行状态" subtitle={issueCount > 0 ? `${issueCount} 项需要处理` : backgroundHealthUnknown ? "后台任务尚无运行记录" : "未发现需要处理的异常"} action={<div className="diagnostic-action">
          <button className="secondary" disabled={copy.state.status === "pending"} aria-busy={copy.state.status === "pending"} onClick={copyDiagnostics}>{copy.state.status === "pending" ? "正在复制…" : "复制诊断信息"}</button>
          {copy.state.status !== "idle" && <span className="action-message" role={copy.state.status === "error" ? "alert" : "status"} aria-live={copy.state.status === "error" ? undefined : "polite"}>
            {copy.state.status === "pending" ? "正在复制诊断…" : copy.state.status === "success" ? "诊断信息已复制" : copy.state.status === "error" ? copy.state.error : null}
          </span>}
        </div>} />
      </div>
      <div className={`health-summary ${issueCount > 0 ? "attention" : backgroundHealthUnknown ? "unknown" : ""}`}>
        <span aria-hidden="true" />
        <div><strong>{issueCount > 0 ? `${issueCount} 项需要处理` : backgroundHealthUnknown ? "状态信息不完整" : "未发现异常"}</strong><small>{issueCount > 0 ? "请检查下方标记的组件" : backgroundHealthUnknown ? "后台任务尚无运行记录，暂时无法判断其健康状态" : "现有记录中未发现需要处理的异常"}</small></div>
      </div>
      <div className="diagnostic-grid">
        <section className="surface diagnostic-card" aria-labelledby="collector-health-title">
          <SectionTitle id="collector-health-title" title="事件采集" subtitle="事件采集器、历史记录扫描与本地事件队列" />
          <CollectorStatusCallout hook={snapshot.hook} onboardingDisposition={snapshot.hookOnboardingDisposition} location="diagnostics" onOpenSettings={onOpenSettings} />
          <Info label="事件采集器" value={hookVersion(snapshot)} />
          <Info label="历史记录扫描" value={indexStateLabel(snapshot.index.state)} />
          <Info label="等待处理的事件" value={String(snapshot.diagnostics.pendingEvents)} />
          <Info label="处理失败的会话" value={String(snapshot.diagnostics.quarantinedSessions)} />
        </section>
        <section className="surface diagnostic-card" aria-labelledby="notification-health-title">
          <SectionTitle id="notification-health-title" title="通知通道" subtitle="桌面通知与远程通知投递状态" />
          <Info label="桌面通知" value={desktopHealth} />
          <Info label="ntfy" value={ntfyHealth} />
          <Info label="待发送通知" value={String(snapshot.diagnostics.pendingNotifications)} />
          <div className="diagnostic-channel-actions" role="group" aria-label="通知诊断工具">
            <div className="diagnostic-action">
              <button className="secondary" disabled={testDesktopNotification.state.status === "pending"} aria-busy={testDesktopNotification.state.status === "pending"} onClick={() => { void testDesktopNotification.run().catch(() => undefined); }}>{testDesktopNotification.state.status === "pending" ? "正在发送…" : "测试桌面通知"}</button>
              {testDesktopNotification.state.status !== "idle" && testDesktopNotification.state.status !== "pending" && <span className="action-message" role={testDesktopNotification.state.status === "error" ? "alert" : "status"} aria-live={testDesktopNotification.state.status === "error" ? undefined : "polite"}>
                {testDesktopNotification.state.status === "success" ? "测试通知已提交，请检查通知中心" : testDesktopNotification.state.status === "error" ? testDesktopNotification.state.error : null}
              </span>}
            </div>
            <div className="diagnostic-action">
              <button className="secondary" disabled={openSettings.state.status === "pending"} aria-busy={openSettings.state.status === "pending"} onClick={() => { void openSettings.run().catch(() => undefined); }}>{openSettings.state.status === "pending" ? "正在打开…" : "打开 macOS 通知设置"}</button>
              {openSettings.state.status !== "idle" && openSettings.state.status !== "pending" && <span className="action-message" role={openSettings.state.status === "error" ? "alert" : "status"} aria-live={openSettings.state.status === "error" ? undefined : "polite"}>
                {openSettings.state.status === "success" ? "已请求打开通知设置" : openSettings.state.status === "error" ? openSettings.state.error : null}
              </span>}
            </div>
          </div>
        </section>
      </div>
      <section className="surface diagnostic-card background-health" aria-labelledby="background-health-title">
        <SectionTitle id="background-health-title" title="后台任务" subtitle={backgroundHealthUnknown ? "尚无运行记录" : backgroundIssues.length === 0 ? "未发现连续失败" : `${backgroundIssues.length} 个任务连续失败`} />
        <div className="background-health-rows">
          {snapshot.diagnostics.backgroundHealth.map((health) => <div className="health-row" key={health.task}>
            <span>{backgroundTaskLabel(health.task)}</span>
            <strong className={health.consecutiveFailures > 0 ? "attention" : ""}>{health.consecutiveFailures > 0 ? `${health.consecutiveFailures} 次连续失败` : "未发现连续失败"}</strong>
          </div>)}
          {backgroundHealthUnknown && <div className="empty compact">尚无后台任务运行记录</div>}
        </div>
        {snapshot.diagnostics.backgroundHealth.length > 0 && <details className="technical-details">
          <summary>查看技术详情</summary>
          {snapshot.diagnostics.backgroundHealth.map((health) => <Info key={health.task} label={backgroundTaskLabel(health.task)} value={[
            `成功 ${health.successCount}`,
            `失败 ${health.failureCount}`,
            `最近成功 ${healthTime(health.lastSucceededAtMs)}`,
            `最近失败 ${healthTime(health.lastFailedAtMs)}`,
            `最近恢复 ${healthTime(health.recoveredAtMs)}`,
          ].join(" · ")} />)}
        </details>}
      </section>
    </section>
    <section className="maintenance" aria-labelledby="reindex-title">
      <div className="maintenance-copy">
        <SectionTitle id="reindex-title" title="历史记录扫描" subtitle="重新读取 Claude Code 会话记录，刷新历史用量与状态。" />
        {reindexState.status !== "idle" && <div className={`operation-toast ${reindexState.status === "error" ? "failed" : ""}`} role={reindexState.status === "error" ? "alert" : "status"} aria-live={reindexState.status === "error" ? undefined : "polite"} aria-atomic="true">
          <span>{reindexMessage}</span>
          {reindexState.status === "pending" && (reindexProgressKnown
            ? <progress aria-label="历史记录扫描进度" value={snapshot.index.completed} max={snapshot.index.total} />
            : <progress aria-label="历史记录扫描进度" />)}
        </div>}
      </div>
      <div className="maintenance-actions">
        <button className="primary" disabled={reindexState.status === "pending" || snapshot.index.state === "running"} aria-busy={reindexState.status === "pending" || snapshot.index.state === "running"} onClick={reindex}>{reindexState.status === "pending" ? "正在扫描历史记录…" : "重新扫描历史记录"}</button>
      </div>
    </section>
    <section className="danger-zone" aria-label="危险操作：数据维护">
      <div className="maintenance-copy">
        <SectionTitle id="cleanup-title" title="数据维护" subtitle="清理已结束会话的原始事件与通知记录。" />
        {cleanup.state.status === "success" && <div className="operation-toast" role="status" aria-live="polite">{cleanup.state.value.rawEventsDeleted || cleanup.state.value.notificationsDeleted ? `已删除 ${cleanup.state.value.rawEventsDeleted} 条原始事件和 ${cleanup.state.value.notificationsDeleted} 条通知记录` : "没有符合条件的旧记录可清理"}</div>}
        {cleanup.state.status === "error" && <div className="operation-toast failed" role="alert">{cleanup.state.error}</div>}
      </div>
      <div className="maintenance-actions">
        {!cleanupConfirmation ? <button className="danger" disabled={cleanup.state.status === "pending"} onClick={() => { cleanup.reset(); setCleanupConfirmation(true); }}>清理已处理记录</button> : <div className="cleanup-confirmation" role="group" aria-label="确认清理历史记录">
          <p>将删除已结束会话的原始事件、会话状态，以及已发送、已抑制和重试耗尽的通知记录；活跃会话、Token 用量与每日汇总、历史扫描进度标记、待发送和仍可重试的失败通知都会保留。</p>
          <div className="cleanup-confirmation-actions">
            <button className="danger" disabled={cleanup.state.status === "pending"} aria-busy={cleanup.state.status === "pending"} onClick={confirmCleanup}>{cleanup.state.status === "pending" ? "正在清理…" : "确认清理"}</button>
            <button className="secondary" disabled={cleanup.state.status === "pending"} onClick={() => { cleanup.reset(); setCleanupConfirmation(false); }}>取消</button>
          </div>
        </div>}
      </div>
    </section>
  </>;
}
