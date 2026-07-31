import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { StrictMode } from "react";
import { listen } from "@tauri-apps/api/event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import topicFixtures from "../tests/fixtures/ntfy_topics.json";
import App, { buildAnnualHeatmap, calendarWeekColumn, eventLabel, indexStateLabel, validateNtfyTopic } from "./App";
import { api, compact, demoSnapshot } from "./api";

const unlistenInvalidated = vi.fn();
const unlistenNavigate = vi.fn();
const listeners = new Map<string, (event: { payload: unknown }) => void>();
let rejectedListener: string | null = null;

async function invalidate(revision: number) {
  snapshot = { ...snapshot, revision };
  await act(async () => {
    listeners.get("monitor://invalidated")?.({ payload: { revision } });
  });
}

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    if (name === rejectedListener) throw new Error("listener unavailable");
    listeners.set(name, handler);
    return name === "monitor://invalidated" ? unlistenInvalidated : unlistenNavigate;
  }),
}));

let snapshot = {
  ...demoSnapshot(),
  revision: 7,
  counts: { running: 1, waiting: 0, needsInput: 1, failed: 0 },
  activeSessionCount: 1,
  sessions: [{
    sessionId: "session-one",
    projectName: "project-a",
    turnState: "running" as const,
    stateSource: "hook" as const,
    stateReason: "tool_running",
    changedAtMs: Date.now(),
    lastObservedAtMs: Date.now(),
    revision: 2,
    usage: demoSnapshot().today,
  }],
};

vi.mock("./api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api")>();
  return {
    ...actual,
    api: {
      ...actual.api,
      snapshot: vi.fn(async () => snapshot),
      session: vi.fn(async () => { throw new Error("missing session"); }),
      reindex: vi.fn(async () => ({ runId: "run-a" })),
      clearHistory: vi.fn(async () => ({ rawEventsDeleted: 0, notificationsDeleted: 0 })),
      openNotificationSettings: vi.fn(async () => undefined),
      settings: vi.fn(async () => ({
        ntfyEnabled: false, ntfyServer: "https://ntfy.sh", ntfyTopic: "",
        ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
      })),
      saveSettings: vi.fn(async () => undefined),
      testNtfy: vi.fn(async () => undefined),
      installHook: vi.fn(async () => undefined),
      uninstallHook: vi.fn(async () => true),
      deferHookOnboarding: vi.fn(async () => undefined),
    },
  };
});

describe("App", () => {
  beforeEach(() => {
    vi.mocked(api.snapshot).mockClear();
    vi.mocked(api.session).mockReset().mockRejectedValue(new Error("missing session"));
    vi.mocked(api.reindex).mockReset().mockResolvedValue({ runId: "run-a" });
    vi.mocked(api.clearHistory).mockReset().mockResolvedValue({ rawEventsDeleted: 0, notificationsDeleted: 0 });
    vi.mocked(api.openNotificationSettings).mockReset().mockResolvedValue(undefined);
    vi.mocked(api.settings).mockReset().mockResolvedValue({
      ntfyEnabled: false, ntfyServer: "https://ntfy.sh", ntfyTopic: "",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    });
    vi.mocked(api.saveSettings).mockReset().mockResolvedValue(undefined);
    vi.mocked(api.testNtfy).mockReset().mockResolvedValue(undefined);
    vi.mocked(api.installHook).mockReset().mockResolvedValue(undefined);
    vi.mocked(api.uninstallHook).mockReset().mockResolvedValue(true);
    vi.mocked(api.deferHookOnboarding).mockReset().mockResolvedValue(undefined);
    Object.assign(navigator, {
      clipboard: { writeText: vi.fn(async () => undefined) },
    });
    snapshot = {
      ...snapshot,
      revision: 7,
      counts: { running: 1, waiting: 0, needsInput: 1, failed: 0 },
      activeSessionCount: 1,
      sessionsHasMore: false,
      sessions: [{
        sessionId: "session-one",
        projectName: "project-a",
        turnState: "running" as const,
        stateSource: "hook" as const,
        stateReason: "tool_running",
        changedAtMs: Date.now(),
        lastObservedAtMs: Date.now(),
        revision: 2,
        usage: demoSnapshot().today,
      }],
      hook: { status: "absent", issueCode: null, version: null },
      hookOnboardingDisposition: null,
      trends: [],
      diagnostics: demoSnapshot().diagnostics,
      index: { runId: null, state: "idle", completed: 0, total: 0, failedFiles: 0, quarantinedSessions: 0, interrupted: false },
    };
    location.hash = "";
    listeners.clear();
    rejectedListener = null;
    vi.mocked(listen).mockClear();
    unlistenInvalidated.mockClear();
    unlistenNavigate.mockClear();
  });
  afterEach(cleanup);

  it("uses the shared ntfy topic contract", () => {
    for (const topic of topicFixtures.valid) {
      expect(validateNtfyTopic(topic), `valid topic: ${JSON.stringify(topic)}`).toBeUndefined();
    }
    for (const topic of topicFixtures.invalid) {
      expect(validateNtfyTopic(topic), `invalid topic: ${JSON.stringify(topic)}`).toBeTruthy();
    }
  });

  it("renders snapshot view states and responds to navigation events", async () => {
    render(<App />);
    expect(await screen.findByText("project-a")).toBeTruthy();
    expect(screen.getByText("需要介入")).toBeTruthy();
    await waitFor(() => expect(listeners.has("monitor://navigate")).toBe(true));
    listeners.get("monitor://navigate")?.({ payload: "diagnostics" });
    await waitFor(() => expect(screen.getByText("数据库迁移版本")).toBeTruthy());
  });

  it("does not report a missing collector before the real snapshot is ready", async () => {
    let resolveSnapshot!: (value: typeof snapshot) => void;
    vi.mocked(api.snapshot).mockReturnValueOnce(
      new Promise((resolve) => { resolveSnapshot = resolve; }),
    );
    render(<App />);

    expect(screen.queryByText("启用实时会话监控")).toBeNull();
    expect(screen.getByRole("button", { name: "正在检测事件采集器，打开设置" })).toBeTruthy();
    await act(async () => resolveSnapshot(snapshot));
    expect(await screen.findByRole("heading", { name: "启用实时会话监控" })).toBeTruthy();
  });

  it("offers an in-context install and announces success", async () => {
    vi.mocked(api.installHook).mockImplementationOnce(async () => {
      snapshot = {
        ...snapshot,
        revision: snapshot.revision + 1,
        hook: { status: "installed", issueCode: null, version: "1" },
      };
    });
    render(<App />);

    await screen.findByRole("button", { name: "安装事件采集器" });
    fireEvent.click(screen.getByRole("button", { name: "安装事件采集器" }));
    expect(await screen.findByText("事件采集器已安装")).toBeTruthy();
    expect(screen.getByText("实时监控已启用。")).toBeTruthy();
    expect(api.installHook).toHaveBeenCalledOnce();
  });

  it("persists defer and hides only the Dashboard callout", async () => {
    vi.mocked(api.deferHookOnboarding).mockImplementationOnce(async () => {
      snapshot = {
        ...snapshot,
        revision: snapshot.revision + 1,
        hookOnboardingDisposition: "deferred",
      };
    });
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "暂不安装" }));
    await waitFor(() => expect(screen.queryByText("启用实时会话监控")).toBeNull());
    expect(api.deferHookOnboarding).toHaveBeenCalledOnce();
    expect(screen.getByRole("button", { name: "事件采集器未安装，打开设置" })).toBeTruthy();
  });

  it("keeps install failure visible with retry and recovery navigation", async () => {
    vi.mocked(api.installHook).mockRejectedValueOnce(new Error("private path"));
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "安装事件采集器" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("事件采集器操作失败，请重试");
    expect(screen.getByRole("button", { name: "安装事件采集器" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "打开设置" })).toBeTruthy();
    await waitFor(() => expect(api.snapshot).toHaveBeenCalledTimes(2));
  });

  it("shows repair details regardless of a deferred onboarding choice", async () => {
    snapshot = {
      ...snapshot,
      hook: { status: "repair_required", issueCode: "hook_settings_mismatch", version: "1" },
      hookOnboardingDisposition: "deferred",
    };
    render(<App />);

    expect(await screen.findByText(/当前配置不完整：Claude Code 设置不匹配/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "立即修复" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "查看详情" }));
    expect(await screen.findByRole("heading", { name: "诊断", level: 1 })).toBeTruthy();
  });

  it("keeps a Diagnostics recovery action after deliberate uninstall", async () => {
    snapshot = {
      ...snapshot,
      hookOnboardingDisposition: "deliberately_uninstalled",
    };
    location.hash = "diagnostics";
    render(<App />);

    expect(await screen.findByRole("heading", { name: "事件采集器未安装" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "安装事件采集器" })).toBeTruthy();
  });

  it("does not offer a no-op details action for a repair already shown in Diagnostics", async () => {
    snapshot = {
      ...snapshot,
      hook: { status: "repair_required", issueCode: "hook_settings_mismatch", version: "1" },
    };
    location.hash = "diagnostics";
    render(<App />);

    expect(await screen.findByRole("button", { name: "立即修复" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "查看详情" })).toBeNull();
  });

  it("coalesces invalidations and refuses an older snapshot completion", async () => {
    render(<App />);
    await screen.findByText("project-a");
    await waitFor(() => expect(listeners.has("monitor://invalidated")).toBe(true));

    let resolveOlder!: (value: typeof snapshot) => void;
    const older = new Promise<typeof snapshot>((resolve) => { resolveOlder = resolve; });
    const newest = {
      ...snapshot,
      revision: 22,
      sessions: [{ ...snapshot.sessions[0], projectName: "newest-project" }],
    };
    vi.mocked(api.snapshot)
      .mockReturnValueOnce(older)
      .mockResolvedValueOnce(newest);

    await act(async () => {
      listeners.get("monitor://invalidated")?.({ payload: { revision: 21 } });
      listeners.get("monitor://invalidated")?.({ payload: { revision: 22 } });
      resolveOlder({
        ...snapshot,
        revision: 21,
        sessions: [{ ...snapshot.sessions[0], projectName: "stale-project" }],
      });
      await older;
    });

    expect(await screen.findByText("newest-project")).toBeTruthy();
    expect(screen.queryByText("stale-project")).toBeNull();
    expect(api.snapshot).toHaveBeenCalledTimes(3);
  });

  it("follows a Hook mutation revision instead of publishing its in-flight old snapshot", async () => {
    snapshot = { ...snapshot, revision: 30, hook: { status: "absent", issueCode: null, version: null } };
    render(<App />);
    await screen.findByText("project-a");
    fireEvent.click(screen.getByRole("button", { name: "设置" }));
    await screen.findByRole("button", { name: "安装事件采集器" });

    let resolveOld!: (value: typeof snapshot) => void;
    const oldRead = new Promise<typeof snapshot>((resolve) => { resolveOld = resolve; });
    const installed = {
      ...snapshot,
      revision: 32,
      hook: { status: "installed" as const, issueCode: null, version: "1" },
    };
    vi.mocked(api.snapshot).mockReturnValueOnce(oldRead).mockResolvedValueOnce(installed);
    await act(async () => {
      listeners.get("monitor://invalidated")?.({ payload: { revision: 31 } });
    });
    vi.mocked(api.installHook).mockImplementationOnce(async () => {
      listeners.get("monitor://invalidated")?.({ payload: { revision: 32 } });
    });
    fireEvent.click(screen.getByRole("button", { name: "安装事件采集器" }));
    await waitFor(() => expect(api.installHook).toHaveBeenCalledOnce());
    await act(async () => {
      resolveOld({ ...snapshot, revision: 31 });
      await oldRead;
    });

    await waitFor(() => expect(api.snapshot).toHaveBeenCalledTimes(3));
    expect(await screen.findByText(/安装完整且配置有效/)).toBeTruthy();
  });

  it("does not publish a snapshot completion after unmount", async () => {
    let resolveSnapshot!: (value: typeof snapshot) => void;
    vi.mocked(api.snapshot).mockReturnValueOnce(
      new Promise((resolve) => { resolveSnapshot = resolve; }),
    );
    const rendered = render(<App />);
    await waitFor(() => expect(api.snapshot).toHaveBeenCalledOnce());
    rendered.unmount();
    await act(async () => resolveSnapshot(snapshot));
    expect(rendered.container.textContent).toBe("");
  });

  it("preserves route focus intent while the initial snapshot is still loading", async () => {
    let resolveSnapshot!: (value: typeof snapshot) => void;
    vi.mocked(api.snapshot).mockReturnValueOnce(
      new Promise((resolve) => { resolveSnapshot = resolve; }),
    );
    render(<App />);
    const settingsNav = screen.getByRole("button", { name: "设置" });
    fireEvent.click(settingsNav);
    await act(async () => resolveSnapshot(snapshot));
    const heading = await screen.findByRole("heading", { name: "设置", level: 1 });
    await waitFor(() => expect(document.activeElement).toBe(heading));
  });

  it("unsubscribes both desktop listeners on unmount", async () => {
    const rendered = render(<App />);
    await waitFor(() => expect(listeners.size).toBe(2));
    rendered.unmount();
    expect(unlistenInvalidated).toHaveBeenCalledOnce();
    expect(unlistenNavigate).toHaveBeenCalledOnce();
  });

  it("loads once and releases every listener registration under StrictMode effects", async () => {
    const rendered = render(<StrictMode><App /></StrictMode>);
    expect(await screen.findByText("project-a")).toBeTruthy();
    expect(api.snapshot).toHaveBeenCalledOnce();
    expect(listen).toHaveBeenCalledTimes(4);
    await waitFor(() => {
      expect(unlistenInvalidated).toHaveBeenCalledTimes(1);
      expect(unlistenNavigate).toHaveBeenCalledTimes(1);
    });

    rendered.unmount();
    expect(unlistenInvalidated).toHaveBeenCalledTimes(2);
    expect(unlistenNavigate).toHaveBeenCalledTimes(2);
  });

  it("keeps and releases a successful listener when the other registration fails", async () => {
    rejectedListener = "monitor://navigate";
    const rendered = render(<App />);
    await screen.findByText("project-a");
    expect(listeners.has("monitor://invalidated")).toBe(true);
    expect(listeners.has("monitor://navigate")).toBe(false);
    rendered.unmount();
    expect(unlistenInvalidated).toHaveBeenCalledOnce();
    expect(unlistenNavigate).not.toHaveBeenCalled();
  });

  it("reports completion after a requested reindex", async () => {
    render(<App />);
    await screen.findByText("project-a");
    location.hash = "diagnostics";
    await act(async () => {
      window.dispatchEvent(new HashChangeEvent("hashchange"));
    });
    const button = await screen.findByRole("button", { name: "重新索引会话记录" });
    await act(async () => {
      fireEvent.click(button);
    });
    expect(await screen.findByRole("progressbar", { name: "重新索引进度" })).toBeTruthy();

    snapshot = {
      ...snapshot,
      index: { runId: "run-a", state: "complete", completed: 12, total: 12, failedFiles: 0, quarantinedSessions: 0, interrupted: false },
    };
    await invalidate(8);
    expect(await screen.findByText("重新索引完成，共处理 12 个会话记录")).toBeTruthy();
  });

  it("copies sanitized diagnostics without paths or provider errors", async () => {
    snapshot = {
      ...snapshot,
      diagnostics: {
        ...snapshot.diagnostics,
        backgroundHealth: [{
          task: "incremental_index",
          successCount: 3,
          failureCount: 2,
          consecutiveFailures: 1,
          errorCode: "index_io_failed",
          lastSucceededAtMs: 10,
          lastFailedAtMs: 20,
          recoveredAtMs: null,
        }],
      },
    };
    location.hash = "diagnostics";
    render(<App />);
    const button = await screen.findByRole("button", { name: "复制诊断" });
    const statusRegion = screen.getByRole("region", { name: "运行状态" });
    expect(statusRegion.contains(button)).toBe(true);
    expect(statusRegion.contains(screen.getByRole("button", { name: "打开通知设置" }))).toBe(true);
    await act(async () => {
      fireEvent.click(button);
    });
    expect(await screen.findByText("诊断信息已复制")).toBeTruthy();
    const report = vi.mocked(navigator.clipboard.writeText).mock.calls[0][0];
    expect(report).toContain("migration_version=0");
    expect(report).toContain("background_task=incremental_index");
    expect(report).toContain("successes=3");
    expect(report).toContain("failures=2");
    expect(report).toContain("consecutive_failures=1");
    expect(report).toContain("error_code=index_io_failed");
    expect(report).toContain("last_succeeded_at_ms=10");
    expect(report).toContain("last_failed_at_ms=20");
    expect(report).toContain("recovered_at_ms=none");
    expect(screen.getByText(/成功 3 · 失败 2/)).toBeTruthy();
    expect(report).not.toContain("hookPath");
    expect(report).not.toContain("LastError");
  });

  it("never displays local paths or provider error details in diagnostics", async () => {
    snapshot = {
      ...snapshot,
      hook: { status: "installed", issueCode: null, version: "1" },
      diagnostics: {
        ...snapshot.diagnostics,
        desktopFailures: 2,
        desktopErrorCode: "delivery_failed",
        ntfyFailures: 1,
        ntfyErrorCode: "sensitive_error_redacted",
      },
    };
    location.hash = "diagnostics";
    render(<App />);
    await screen.findByText("数据库迁移版本");
    expect(screen.queryByText("Hook 路径")).toBeNull();
    expect(screen.queryByText(/private desktop provider error/)).toBeNull();
    expect(screen.queryByText(/private ntfy provider error/)).toBeNull();
    expect(screen.getByText("2 次连续失败 · 通知发送失败")).toBeTruthy();
    expect(screen.getByText("1 次连续失败 · 远程通知异常")).toBeTruthy();
    expect(document.body.textContent).not.toContain("delivery_failed");
    expect(document.body.textContent).not.toContain("sensitive_error_redacted");
    expect(document.body.textContent).not.toContain("/Users/private");
  });

  it("ignores a stale terminal reindex snapshot after a second run starts", async () => {
    vi.mocked(api.reindex)
      .mockResolvedValueOnce({ runId: "run-a" })
      .mockResolvedValueOnce({ runId: "run-b" });
    location.hash = "diagnostics";
    render(<App />);
    const button = await screen.findByRole("button", { name: "重新索引会话记录" });
    await act(async () => { fireEvent.click(button); });
    snapshot = { ...snapshot, index: { runId: "run-a", state: "complete", completed: 4, total: 4, failedFiles: 0, quarantinedSessions: 0, interrupted: false } };
    await invalidate(8);
    expect(await screen.findByText("重新索引完成，共处理 4 个会话记录")).toBeTruthy();

    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "重新索引会话记录" })); });
    snapshot = { ...snapshot, index: { runId: "run-a", state: "complete", completed: 4, total: 4, failedFiles: 0, quarantinedSessions: 0, interrupted: false } };
    await invalidate(9);
    expect(screen.queryByText("重新索引完成，共处理 4 个会话记录")).toBeNull();
    expect(screen.getByRole("progressbar", { name: "重新索引进度" }).getAttribute("value")).toBeNull();

    snapshot = { ...snapshot, index: { runId: "run-b", state: "complete", completed: 8, total: 8, failedFiles: 0, quarantinedSessions: 0, interrupted: false } };
    await invalidate(10);
    expect(await screen.findByText("重新索引完成，共处理 8 个会话记录")).toBeTruthy();
  });

  it("requires confirmation and reports truthful cleanup counts", async () => {
    vi.mocked(api.clearHistory).mockResolvedValueOnce({ rawEventsDeleted: 7, notificationsDeleted: 2 });
    location.hash = "diagnostics";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "清理已处理记录" }));
    expect(screen.getByText(/活跃会话、Token 用量与每日汇总、会话记录游标、待发送和仍可重试的失败通知都会保留/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(screen.queryByRole("button", { name: "确认清理" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "清理已处理记录" }));
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "确认清理" })); });
    expect(await screen.findByText("已删除 7 条原始事件和 2 条通知记录")).toBeTruthy();
  });

  it("adopts an existing reindex and reports matching progress and failure", async () => {
    snapshot = { ...snapshot, index: { runId: "existing", state: "running", completed: 2, total: 5, failedFiles: 0, quarantinedSessions: 0, interrupted: false } };
    location.hash = "diagnostics";
    render(<App />);
    const button = await screen.findByRole("button", { name: "正在重新索引…" });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByRole("progressbar", { name: "重新索引进度" }).getAttribute("value")).toBe("2");
    snapshot = { ...snapshot, index: { runId: "existing", state: "failed", completed: 3, total: 5, failedFiles: 1, quarantinedSessions: 2, interrupted: false } };
    await invalidate(11);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("1 个记录文件读取失败，2 个会话处理失败，其他内容已完成");
    expect(alert.textContent).not.toContain("bad index");
    expect(alert.textContent).not.toContain("/private/path");
  });

  it("reports reindex startup rejection", async () => {
    vi.mocked(api.reindex).mockRejectedValueOnce(new Error("cannot start"));
    location.hash = "diagnostics";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "重新索引会话记录" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("重新索引失败，请重试");
    expect(alert.textContent).not.toContain("cannot start");
  });

  it("uses a generic safe message when reindexing is interrupted", async () => {
    snapshot = {
      ...snapshot,
      index: {
        runId: "interrupted",
        state: "running",
        completed: 1,
        total: 5,
        failedFiles: 0,
        quarantinedSessions: 0,
        interrupted: false,
      },
    };
    location.hash = "diagnostics";
    render(<App />);
    await screen.findByRole("progressbar", { name: "重新索引进度" });
    snapshot = {
      ...snapshot,
      index: {
        ...snapshot.index,
        state: "failed",
        quarantinedSessions: 1,
        interrupted: true,
      },
    };
    await invalidate(12);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("重新索引失败，请重试");
    expect(alert.textContent).not.toContain("/private/path");
    expect(alert.textContent).not.toContain("1 个会话处理失败");
  });

  it("handles cleanup zero, rejection, and disables duplicate confirmation while pending", async () => {
    let resolveCleanup!: (value: { rawEventsDeleted: number; notificationsDeleted: number }) => void;
    vi.mocked(api.clearHistory).mockReturnValueOnce(new Promise((resolve) => { resolveCleanup = resolve; }));
    location.hash = "diagnostics";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "清理已处理记录" }));
    fireEvent.click(screen.getByRole("button", { name: "确认清理" }));
    expect((screen.getByRole("button", { name: "正在清理…" }) as HTMLButtonElement).disabled).toBe(true);
    expect(api.clearHistory).toHaveBeenCalledOnce();
    await act(async () => resolveCleanup({ rawEventsDeleted: 0, notificationsDeleted: 0 }));
    expect(await screen.findByText("没有符合条件的旧记录可清理")).toBeTruthy();

    vi.mocked(api.clearHistory).mockRejectedValueOnce(new Error("database busy"));
    fireEvent.click(screen.getByRole("button", { name: "清理已处理记录" }));
    fireEvent.click(screen.getByRole("button", { name: "确认清理" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("清理失败，请重试");
    expect(alert.textContent).not.toContain("database busy");
  });

  it("shows pending and error feedback for copy and notification settings", async () => {
    let rejectCopy!: (reason: Error) => void;
    vi.mocked(navigator.clipboard.writeText).mockReturnValueOnce(new Promise((_, reject) => { rejectCopy = reject; }));
    let resolveSettings!: () => void;
    vi.mocked(api.openNotificationSettings).mockReturnValueOnce(new Promise((resolve) => { resolveSettings = resolve; }));
    location.hash = "diagnostics";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "复制诊断" }));
    expect((screen.getByRole("button", { name: "正在复制…" }) as HTMLButtonElement).disabled).toBe(true);
    await act(async () => rejectCopy(new Error("denied")));
    expect(await screen.findByText("复制失败，请检查剪贴板权限")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "打开通知设置" }));
    expect((screen.getByRole("button", { name: "正在打开…" }) as HTMLButtonElement).disabled).toBe(true);
    await act(async () => resolveSettings());
    expect(await screen.findByText("通知设置已打开")).toBeTruthy();
    vi.mocked(api.openNotificationSettings).mockRejectedValueOnce(new Error("unavailable"));
    fireEvent.click(screen.getByRole("button", { name: "打开通知设置" }));
    expect(await screen.findByText("无法打开通知设置")).toBeTruthy();
  });

  it("tracks dirty, pending and saved settings without duplicate saves", async () => {
    let finishSave!: () => void;
    vi.mocked(api.saveSettings).mockImplementationOnce(() => new Promise<void>((resolve) => { finishSave = resolve; }));
    location.hash = "settings";
    render(<App />);
    const topic = await screen.findByRole("textbox", { name: "通知主题（Topic）" });
    expect((screen.getByRole("button", { name: "保存设置" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(topic, { target: { value: "alerts" } });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));
    expect((await screen.findByRole("button", { name: "正在保存…" }) as HTMLButtonElement).disabled).toBe(true);
    expect((topic as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole("checkbox", { name: "启用 ntfy" }) as HTMLInputElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "正在保存…" }));
    expect(api.saveSettings).toHaveBeenCalledTimes(1);
    await act(async () => { finishSave(); });
    expect(await screen.findByText("设置已保存")).toBeTruthy();
    fireEvent.change(topic, { target: { value: "changed-again" } });
    expect(screen.queryByText("设置已保存")).toBeNull();
    expect(screen.getByText("有未保存的更改")).toBeTruthy();
  });

  it("shows save errors and allows retry", async () => {
    vi.mocked(api.saveSettings).mockRejectedValueOnce(new Error("disk full"));
    location.hash = "settings";
    render(<App />);
    fireEvent.change(await screen.findByRole("textbox", { name: "通知主题（Topic）" }), { target: { value: "alerts" } });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("设置保存失败，请重试");
    expect(alert.textContent).not.toContain("disk full");
    expect((screen.getByRole("button", { name: "保存设置" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("keeps a successful save when canonical reload fails and retries reload separately", async () => {
    const initial = {
      ntfyEnabled: false, ntfyServer: "https://ntfy.sh", ntfyTopic: "",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    };
    vi.mocked(api.settings)
      .mockResolvedValueOnce(initial)
      .mockRejectedValueOnce(new Error("database path and password"))
      .mockResolvedValueOnce({ ...initial, ntfyTopic: "alerts" });
    location.hash = "settings";
    render(<App />);
    const topic = await screen.findByRole("textbox", { name: "通知主题（Topic）" });
    fireEvent.change(topic, { target: { value: "alerts" } });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));

    expect(await screen.findByText("设置已保存，但重新读取失败")).toBeTruthy();
    expect((topic as HTMLInputElement).value).toBe("alerts");
    expect(screen.queryByText("有未保存的更改")).toBeNull();
    expect((screen.getByRole("button", { name: "保存设置" }) as HTMLButtonElement).disabled).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "重新读取" }));
    expect(await screen.findByText("设置已保存")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "重新读取" })).toBeNull();
  });

  it("does not let a late post-save canonical reload overwrite a newer edit", async () => {
    const initial = {
      ntfyEnabled: false, ntfyServer: "https://ntfy.sh", ntfyTopic: "",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    };
    let finishReload!: (settings: typeof initial) => void;
    vi.mocked(api.settings)
      .mockResolvedValueOnce(initial)
      .mockImplementationOnce(() => new Promise((resolve) => { finishReload = resolve; }));
    location.hash = "settings";
    render(<App />);
    const topic = await screen.findByRole("textbox", { name: "通知主题（Topic）" });
    fireEvent.change(topic, { target: { value: "saved-value" } });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));
    expect(await screen.findByText("设置已保存")).toBeTruthy();

    fireEvent.change(topic, { target: { value: "new-local-edit" } });
    await act(async () => {
      finishReload({ ...initial, ntfyTopic: "canonical-value" });
    });

    expect((topic as HTMLInputElement).value).toBe("new-local-edit");
    expect(screen.getByText("有未保存的更改")).toBeTruthy();
  });

  it("does not let a late manual reload retry overwrite an intervening edit", async () => {
    const initial = {
      ntfyEnabled: false, ntfyServer: "https://ntfy.sh", ntfyTopic: "",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    };
    let finishRetry!: (settings: typeof initial) => void;
    vi.mocked(api.settings)
      .mockResolvedValueOnce(initial)
      .mockRejectedValueOnce(new Error("reload failed"))
      .mockImplementationOnce(() => new Promise((resolve) => { finishRetry = resolve; }));
    location.hash = "settings";
    render(<App />);
    const topic = await screen.findByRole("textbox", { name: "通知主题（Topic）" });
    fireEvent.change(topic, { target: { value: "saved-value" } });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));
    expect(await screen.findByText("设置已保存，但重新读取失败")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "重新读取" }));
    fireEvent.change(topic, { target: { value: "new-local-edit" } });
    await act(async () => {
      finishRetry({ ...initial, ntfyTopic: "canonical-value" });
    });

    expect((topic as HTMLInputElement).value).toBe("new-local-edit");
    expect(screen.getByText("有未保存的更改")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "重新读取" })).toBeNull();
  });

  it("ignores an old manual reload failure after a newer save reload succeeds", async () => {
    const initial = {
      ntfyEnabled: false, ntfyServer: "https://ntfy.sh", ntfyTopic: "",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    };
    let failOldRetry!: (reason: Error) => void;
    vi.mocked(api.settings)
      .mockResolvedValueOnce(initial)
      .mockRejectedValueOnce(new Error("first automatic reload failed"))
      .mockImplementationOnce(() => new Promise((_resolve, reject) => { failOldRetry = reject; }))
      .mockResolvedValueOnce({ ...initial, ntfyTopic: "new-canonical" });
    location.hash = "settings";
    render(<App />);
    const topic = await screen.findByRole("textbox", { name: "通知主题（Topic）" });
    fireEvent.change(topic, { target: { value: "first-save" } });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));
    expect(await screen.findByText("设置已保存，但重新读取失败")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "重新读取" }));
    expect((screen.getByRole("button", { name: "正在重新读取…" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(topic, { target: { value: "second-save" } });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));
    await waitFor(() => expect((topic as HTMLInputElement).value).toBe("new-canonical"));
    await act(async () => {
      failOldRetry(new Error("late failure with secret"));
    });

    expect((topic as HTMLInputElement).value).toBe("new-canonical");
    expect(screen.getByText("设置已保存")).toBeTruthy();
    expect(screen.queryByText("设置已保存，但重新读取失败")).toBeNull();
  });

  it("ignores an old manual reload success after a newer save publishes newer canonical data", async () => {
    const initial = {
      ntfyEnabled: false, ntfyServer: "https://ntfy.sh", ntfyTopic: "",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    };
    let finishOldRetry!: (settings: typeof initial) => void;
    vi.mocked(api.settings)
      .mockResolvedValueOnce(initial)
      .mockRejectedValueOnce(new Error("first automatic reload failed"))
      .mockImplementationOnce(() => new Promise((resolve) => { finishOldRetry = resolve; }))
      .mockResolvedValueOnce({ ...initial, ntfyTopic: "new-canonical" });
    location.hash = "settings";
    render(<App />);
    const topic = await screen.findByRole("textbox", { name: "通知主题（Topic）" });
    fireEvent.change(topic, { target: { value: "first-save" } });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));
    expect(await screen.findByText("设置已保存，但重新读取失败")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "重新读取" }));
    fireEvent.change(topic, { target: { value: "second-save" } });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));
    await waitFor(() => expect((topic as HTMLInputElement).value).toBe("new-canonical"));
    await act(async () => {
      finishOldRetry({ ...initial, ntfyTopic: "old-canonical" });
    });

    expect((topic as HTMLInputElement).value).toBe("new-canonical");
    expect(screen.getByText("设置已保存")).toBeTruthy();
  });

  it("explains a failed autostart compensation without exposing backend details", async () => {
    vi.mocked(api.saveSettings).mockRejectedValueOnce("settings_inconsistent");
    location.hash = "settings";
    render(<App />);
    fireEvent.change(await screen.findByRole("textbox", { name: "通知主题（Topic）" }), { target: { value: "alerts" } });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));
    expect(await screen.findByText("设置保存未完成，登录启动状态可能已更改；请重新打开设置确认")).toBeTruthy();
  });

  it("shows when ntfy activation is waiting for bounded backlog cleanup", async () => {
    vi.mocked(api.settings).mockResolvedValueOnce({
      ntfyEnabled: true, ntfyActivationPending: true,
      ntfyServer: "https://ntfy.example.com", ntfyTopic: "alerts",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    });
    location.hash = "settings";
    render(<App />);

    expect(await screen.findByText("正在清理停用期间的旧通知，完成后将自动启用。")).toBeTruthy();
  });

  it("explains an invalid ntfy server address before saving", async () => {
    vi.mocked(api.settings).mockResolvedValueOnce({
      ntfyEnabled: true, ntfyServer: "ntfy.example.com", ntfyTopic: "alerts",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    });
    location.hash = "settings";
    render(<App />);
    const server = await screen.findByRole("textbox", { name: "服务器地址" });
    fireEvent.change(server, { target: { value: "47.93.98.88:8088" } });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));

    expect(await screen.findByText("请输入以 http:// 或 https:// 开头的完整服务器地址")).toBeTruthy();
    expect(server.getAttribute("aria-invalid")).toBe("true");
    expect(api.saveSettings).not.toHaveBeenCalled();
  });

  it("requires HTTPS for non-loopback ntfy servers", async () => {
    vi.mocked(api.settings).mockResolvedValueOnce({
      ntfyEnabled: true, ntfyServer: "https://ntfy.example.com", ntfyTopic: "alerts",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    });
    location.hash = "settings";
    render(<App />);

    fireEvent.change(await screen.findByRole("textbox", { name: "服务器地址" }), {
      target: { value: "http://ntfy.example.com" },
    });
    fireEvent.click(await screen.findByRole("button", { name: "保存设置" }));

    expect(await screen.findByText("非本机 ntfy 服务器必须使用 HTTPS")).toBeTruthy();
    expect(api.saveSettings).not.toHaveBeenCalled();
  });

  it("allows credential-free HTTP only for explicit loopback addresses", async () => {
    vi.mocked(api.settings).mockResolvedValueOnce({
      ntfyEnabled: true, ntfyServer: "http://[::1]:8088", ntfyTopic: "alerts",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    });
    location.hash = "settings";
    render(<App />);

    fireEvent.change(await screen.findByRole("textbox", { name: "通知主题（Topic）" }), {
      target: { value: "local-alerts" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));

    await waitFor(() => expect(api.saveSettings).toHaveBeenCalledWith(expect.objectContaining({
      ntfyServer: "http://[::1]:8088",
      ntfyUsername: "",
    })));
  });

  it("rejects IPv4 aliases that URL parsers normalize to loopback", async () => {
    vi.mocked(api.settings).mockResolvedValueOnce({
      ntfyEnabled: true, ntfyServer: "https://ntfy.example.com", ntfyTopic: "alerts",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    });
    location.hash = "settings";
    render(<App />);

    fireEvent.change(await screen.findByRole("textbox", { name: "服务器地址" }), {
      target: { value: "http://127.1:8088" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));

    expect(await screen.findByText("非本机 ntfy 服务器必须使用 HTTPS")).toBeTruthy();
    expect(api.saveSettings).not.toHaveBeenCalled();
  });

  it("requires HTTPS when loopback ntfy has current or stored credentials", async () => {
    vi.mocked(api.settings)
      .mockResolvedValueOnce({
        ntfyEnabled: true, ntfyServer: "http://localhost:8088", ntfyTopic: "alerts",
        ntfyUsername: "monitor", ntfyPasswordSet: false, autostart: false,
      });
    location.hash = "settings";
    const first = render(<App />);

    fireEvent.change(await screen.findByRole("textbox", { name: "通知主题（Topic）" }), {
      target: { value: "first-alerts" },
    });
    fireEvent.click(await screen.findByRole("button", { name: "保存设置" }));
    expect(await screen.findByText("使用用户名或密码时必须使用 HTTPS")).toBeTruthy();
    expect(api.saveSettings).not.toHaveBeenCalled();
    first.unmount();

    vi.mocked(api.settings).mockResolvedValueOnce({
      ntfyEnabled: true, ntfyServer: "http://127.0.0.1:8088", ntfyTopic: "alerts",
      ntfyUsername: "", ntfyPasswordSet: true, autostart: false,
    });
    render(<App />);
    fireEvent.change(await screen.findByRole("textbox", { name: "通知主题（Topic）" }), {
      target: { value: "stored-alerts" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存设置" }));

    expect(await screen.findByText("使用用户名或密码时必须使用 HTTPS")).toBeTruthy();
    expect(api.saveSettings).not.toHaveBeenCalled();
  });

  it("validates and sends an ntfy test message without saving settings", async () => {
    let finishTest!: () => void;
    vi.mocked(api.testNtfy).mockImplementationOnce(() => new Promise<void>((resolve) => { finishTest = resolve; }));
    location.hash = "settings";
    render(<App />);
    const testButton = await screen.findByRole("button", { name: "发送测试消息" });
    fireEvent.click(testButton);
    expect(await screen.findByText("请输入通知主题")).toBeTruthy();
    expect(api.testNtfy).not.toHaveBeenCalled();

    fireEvent.change(screen.getByRole("textbox", { name: /^通知主题（Topic）/ }), { target: { value: "alerts" } });
    fireEvent.click(screen.getByRole("button", { name: "发送测试消息" }));
    expect((await screen.findByRole("button", { name: "正在发送…" }) as HTMLButtonElement).disabled).toBe(true);
    expect(api.testNtfy).toHaveBeenCalledWith(expect.objectContaining({ ntfyServer: "https://ntfy.sh", ntfyTopic: "alerts" }));
    expect(api.saveSettings).not.toHaveBeenCalled();
    await act(async () => finishTest());
    expect(await screen.findByText("测试消息已发送，请检查接收设备")).toBeTruthy();
  });

  it("does not let an old ntfy test completion overwrite a newer form edit", async () => {
    let finishTest!: () => void;
    vi.mocked(api.settings).mockResolvedValueOnce({
      ntfyEnabled: true, ntfyServer: "https://ntfy.example.com", ntfyTopic: "alerts",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    });
    vi.mocked(api.testNtfy).mockImplementationOnce(
      () => new Promise<void>((resolve) => { finishTest = resolve; }),
    );
    location.hash = "settings";
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "发送测试消息" }));
    expect(await screen.findByText("正在连接 ntfy 服务…")).toBeTruthy();
    fireEvent.change(screen.getByRole("textbox", { name: "用户名" }), {
      target: { value: "new-user" },
    });
    await act(async () => finishTest());

    expect(screen.queryByText("测试消息已发送，请检查接收设备")).toBeNull();
    expect(screen.queryByText("正在连接 ntfy 服务…")).toBeNull();
    expect(screen.getByText("有未保存的更改")).toBeTruthy();
  });

  it("does not let an old ntfy test failure overwrite a newer form edit", async () => {
    let failTest!: (reason: unknown) => void;
    vi.mocked(api.settings).mockResolvedValueOnce({
      ntfyEnabled: true, ntfyServer: "https://ntfy.example.com", ntfyTopic: "alerts",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    });
    vi.mocked(api.testNtfy).mockImplementationOnce(
      () => new Promise<void>((_resolve, reject) => { failTest = reject; }),
    );
    location.hash = "settings";
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "发送测试消息" }));
    expect(await screen.findByText("正在连接 ntfy 服务…")).toBeTruthy();
    fireEvent.change(screen.getByRole("textbox", { name: "通知主题（Topic）" }), {
      target: { value: "new-alerts" },
    });
    await act(async () => failTest("ntfy_permission_denied"));

    expect(screen.queryByText("当前用户没有该 Topic 的发布权限")).toBeNull();
    expect(screen.queryByText("正在连接 ntfy 服务…")).toBeNull();
    expect(screen.getByText("有未保存的更改")).toBeTruthy();
  });

  it("shows the sanitized ntfy test delivery error and allows retry", async () => {
    vi.mocked(api.settings).mockResolvedValueOnce({
      ntfyEnabled: true, ntfyServer: "https://ntfy.example.com", ntfyTopic: "alerts",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    });
    vi.mocked(api.testNtfy).mockRejectedValueOnce("ntfy_permission_denied");
    location.hash = "settings";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "发送测试消息" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("当前用户没有该 Topic 的发布权限");
    expect((screen.getByRole("button", { name: "发送测试消息" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("never renders unrecognized ntfy errors or reflected credentials", async () => {
    vi.mocked(api.settings).mockResolvedValueOnce({
      ntfyEnabled: true, ntfyServer: "https://ntfy.example.com", ntfyTopic: "alerts",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    });
    vi.mocked(api.testNtfy).mockRejectedValueOnce(
      "Authorization: Basic reflected-password <title>secret</title>",
    );
    location.hash = "settings";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "发送测试消息" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("测试消息发送失败，请检查配置后重试");
    expect(alert.textContent).not.toContain("Authorization");
    expect(alert.textContent).not.toContain("reflected-password");
    expect(alert.textContent).not.toContain("secret");
  });

  it("keeps the settings save action in a floating dock outside the Hook controls", async () => {
    location.hash = "settings";
    render(<App />);
    const save = await screen.findByRole("button", { name: "保存设置" });
    const dock = save.closest(".settings-save-dock");
    expect(dock).toBeTruthy();
    expect(dock?.children).toHaveLength(1);
    expect(save.closest(".settings-side")).toBeNull();
  });

  it("uses the sidebar collector status as a shortcut to its single-column settings section", async () => {
    snapshot = {
      ...snapshot,
      hook: { status: "installed", issueCode: null, version: "1" },
    };
    render(<App />);
    const collector = await screen.findByRole("button", { name: "事件采集器已安装，打开设置" });
    fireEvent.click(collector);
    expect(await screen.findByRole("heading", { name: "设置", level: 1 })).toBeTruthy();
    expect(screen.getByRole("region", { name: "事件采集器管理" })).toBeTruthy();
  });

  it("shows Hook management before remote notification settings", async () => {
    location.hash = "settings";
    render(<App />);
    const headings = await screen.findAllByRole("heading", { level: 2 });
    expect(headings[0].textContent).toBe("事件采集器");
    expect(headings[1].textContent).toBe("远程通知");
  });

  it("shows settings load failure and retries", async () => {
    vi.mocked(api.settings).mockRejectedValueOnce(new Error("read failed"));
    location.hash = "settings";
    render(<App />);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("暂时无法读取设置，请重试");
    expect(alert.textContent).not.toContain("read failed");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByRole("textbox", { name: "通知主题（Topic）" })).toBeTruthy();
    expect(api.settings).toHaveBeenCalledTimes(2);
  });

  it("focuses the Settings heading when its delayed resource becomes ready", async () => {
    let resolveSettings!: (settings: Awaited<ReturnType<typeof api.settings>>) => void;
    vi.mocked(api.settings).mockReturnValueOnce(new Promise((resolve) => { resolveSettings = resolve; }));
    render(<App />);
    await screen.findByText("project-a");
    fireEvent.click(screen.getByRole("button", { name: "设置" }));
    await screen.findByText("正在读取设置…");

    await act(async () => resolveSettings({
      ntfyEnabled: false, ntfyServer: "https://ntfy.sh", ntfyTopic: "",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    }));
    const heading = await screen.findByRole("heading", { name: "设置", level: 1 });
    expect(document.activeElement).toBe(heading);
  });

  it("does not steal focus when the user moves it while Settings is loading", async () => {
    let resolveSettings!: (settings: Awaited<ReturnType<typeof api.settings>>) => void;
    vi.mocked(api.settings).mockReturnValueOnce(new Promise((resolve) => { resolveSettings = resolve; }));
    render(<App />);
    await screen.findByText("project-a");
    fireEvent.click(screen.getByRole("button", { name: "设置" }));
    await screen.findByText("正在读取设置…");
    const diagnosticsNav = screen.getByRole("button", { name: "诊断" });
    diagnosticsNav.focus();

    await act(async () => resolveSettings({
      ntfyEnabled: false, ntfyServer: "https://ntfy.sh", ntfyTopic: "",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    }));
    await screen.findByRole("heading", { name: "设置", level: 1 });
    expect(document.activeElement).toBe(diagnosticsNav);
  });

  it("installs an absent Hook and refreshes the snapshot", async () => {
    snapshot = { ...snapshot, hook: { status: "absent", issueCode: null, version: null } };
    location.hash = "settings";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "安装事件采集器" }));
    expect(await screen.findByText("事件采集器已安装，实时监控已启用")).toBeTruthy();
    expect(api.installHook).toHaveBeenCalledOnce();
    expect(api.snapshot).toHaveBeenCalledTimes(2);
  });

  it("shows an incomplete Hook as repair-required and offers a repair action", async () => {
    snapshot = {
      ...snapshot,
      hook: {
        status: "repair_required",
        issueCode: "hook_settings_mismatch",
        version: "1",
      },
    };
    location.hash = "settings";
    render(<App />);

    expect(await screen.findByText(/Claude Code 设置不匹配/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "立即修复" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "安装事件采集器" })).toBeNull();
    expect(screen.queryByRole("button", { name: "卸载事件采集器" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "立即修复" }));
    expect(await screen.findByText("事件采集器已修复")).toBeTruthy();
    expect(api.installHook).toHaveBeenCalledOnce();
  });

  it("confirms Hook uninstall, supports cancel, and reports an absent Hook honestly", async () => {
    snapshot = { ...snapshot, hook: { status: "installed", issueCode: null, version: "1" } };
    vi.mocked(api.uninstallHook).mockResolvedValueOnce(false);
    location.hash = "settings";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "卸载事件采集器" }));
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(api.uninstallHook).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "卸载事件采集器" }));
    fireEvent.click(screen.getByRole("button", { name: "确认卸载" }));
    expect(await screen.findByText("未找到可卸载的事件采集器")).toBeTruthy();
    expect(api.snapshot).toHaveBeenCalledTimes(2);
  });

  it("keeps a deliberate uninstall from reopening the Dashboard prompt", async () => {
    snapshot = { ...snapshot, hook: { status: "installed", issueCode: null, version: "1" } };
    vi.mocked(api.uninstallHook).mockImplementationOnce(async () => {
      snapshot = {
        ...snapshot,
        revision: snapshot.revision + 1,
        hook: { status: "absent", issueCode: null, version: null },
        hookOnboardingDisposition: "deliberately_uninstalled",
      };
      return true;
    });
    location.hash = "settings";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "卸载事件采集器" }));
    fireEvent.click(screen.getByRole("button", { name: "确认卸载" }));
    expect(await screen.findByText("事件采集器已卸载")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "总览" }));
    await screen.findByRole("heading", { name: "运行总览" });
    expect(screen.queryByText(/数据修订/)).toBeNull();
    expect(screen.queryByText("启用实时会话监控")).toBeNull();
  });

  it("serializes Hook repair and uninstall and reports successful removal", async () => {
    snapshot = { ...snapshot, hook: { status: "installed", issueCode: null, version: "1" } };
    let finishRepair!: () => void;
    vi.mocked(api.installHook).mockReturnValueOnce(new Promise((resolve) => { finishRepair = resolve; }));
    location.hash = "settings";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "重新安装事件采集器" }));
    expect((screen.getByRole("button", { name: "卸载事件采集器" }) as HTMLButtonElement).disabled).toBe(true);
    await act(async () => finishRepair());
    fireEvent.click(screen.getByRole("button", { name: "卸载事件采集器" }));
    fireEvent.click(screen.getByRole("button", { name: "确认卸载" }));
    expect(await screen.findByText("事件采集器已卸载")).toBeTruthy();
  });

  it("renders the bundled mascot and localized dashboard vocabulary", async () => {
    snapshot = {
      ...snapshot,
      hook: { status: "installed", issueCode: null, version: null },
      index: { runId: "done", state: "complete", completed: 3, total: 3, failedFiles: 0, quarantinedSessions: 0, interrupted: false },
    };
    render(<App />);
    const logo = await screen.findByRole("img", { name: "CC Monitor 闹钟终端标志" });
    const logoSource = (logo as HTMLImageElement).src;
    expect(
      logoSource.startsWith("data:image/svg+xml")
      || new URL(logoSource).pathname.endsWith("/src/assets/app_icon_color.svg"),
    ).toBe(true);
    expect(screen.getByText("1 个会话")).toBeTruthy();
    expect(screen.getByText(/状态：运行中/)).toBeTruthy();
    expect(screen.getByText(/正在执行工具/)).toBeTruthy();
    expect(screen.getByText("已完成")).toBeTruthy();
    expect(screen.getByText("已安装（版本未知）")).toBeTruthy();
    expect(screen.queryByText("vnull")).toBeNull();
    expect(screen.getByText("输入 Token")).toBeTruthy();
    expect(screen.getByText("缓存读取")).toBeTruthy();
  });

  it("discloses when the active-session list is capped", async () => {
    snapshot = {
      ...snapshot,
      activeSessionCount: 125,
      sessionsHasMore: true,
    };
    render(<App />);
    expect(
      await screen.findByText("显示最近 1 个，共 125 个"),
    ).toBeTruthy();
  });

  it("renders session usage above Number precision without rounding it first", async () => {
    const precise = "9007199254740993";
    snapshot = {
      ...snapshot,
      today: { ...snapshot.today, inputTokens: precise },
    };
    render(<App />);
    expect((await screen.findAllByText(compact(precise))).length).toBeGreaterThan(0);
  });

  it("describes cleanup scope truthfully before confirmation", async () => {
    location.hash = "diagnostics";
    render(<App />);
    expect(await screen.findByText("清理已结束会话的原始事件与终态通知记录。")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "清理已处理记录" }));
    expect(screen.getByText(/将删除已结束会话的原始事件、会话状态，以及已发送、已抑制和重试耗尽的通知记录/)).toBeTruthy();
    expect(screen.getByText(/活跃会话、Token 用量与每日汇总/)).toBeTruthy();
    const dangerZone = screen.getByRole("region", { name: "危险操作：数据维护" });
    expect(dangerZone.querySelector("#cleanup-title")?.textContent).toBe("数据维护");
  });

  it("maps every index status to localized visible copy", () => {
    expect(indexStateLabel("idle")).toBe("就绪");
    expect(indexStateLabel("running")).toBe("索引中");
    expect(indexStateLabel("complete")).toBe("已完成");
    expect(indexStateLabel("failed")).toBe("失败");
  });

  it("labels transcript events instead of collapsing them into a generic update", () => {
    expect(eventLabel("TranscriptAssistantText")).toBe("助手输出");
    expect(eventLabel("TranscriptAssistantToolUse")).toBe("助手调用工具");
    expect(eventLabel("TranscriptToolResult")).toBe("工具返回结果");
    expect(eventLabel("FutureEvent")).toBe("未知事件（FutureEvent）");
  });

  it("exposes each history day and its token total to assistive technology", async () => {
    snapshot = {
      ...snapshot,
      trends: [{ day: "2026-07-28", tokens: 1234, costPicoUsd: 0, costKnown: false }],
    };
    location.hash = "history";
    render(<App />);
    const history = await screen.findByRole("list", { name: "每日 Token 用量" });
    expect(history.querySelector('[role="listitem"]')?.getAttribute("aria-label")).toBe("2026-07-28，1234 Token，费用待定");
  });

  it("gap-fills exactly 365 local calendar days across month and leap-day boundaries", () => {
    const annual = buildAnnualHeatmap([
      { day: "2024-02-29", tokens: 100, costPicoUsd: 0, costKnown: false },
      { day: "2024-03-02", tokens: 500, costPicoUsd: 2_000_000_000, costKnown: true },
    ], new Date(2024, 2, 1));

    expect(annual.days).toHaveLength(365);
    expect(annual.days[0].day).toBe("2023-03-03");
    expect(annual.days.at(-1)?.day).toBe("2024-03-01");
    expect(annual.days.find((day) => day.day === "2024-02-29")?.tokens).toBe("100");
    expect(annual.days.find((day) => day.day === "2024-02-28")?.tokens).toBe("0");
    expect(annual.monthLabels.map((day) => day.day)).toContain("2024-02-01");
    expect(annual.monthLabels.map((day) => day.day)).toContain("2024-03-01");
  });

  it("assigns distinct consecutive week columns across spring and autumn DST boundaries", () => {
    const springStart = new Date(2026, 2, 2);
    const autumnStart = new Date(2026, 9, 26);
    // US spring-forward: Mar 2 and Mar 9 remain seven calendar days apart.
    expect(calendarWeekColumn(new Date(2026, 2, 9), springStart)).toBe(
      calendarWeekColumn(springStart, springStart) + 1,
    );
    // US fall-back: Oct 26 and Nov 2 likewise occupy consecutive columns.
    expect(calendarWeekColumn(new Date(2026, 10, 2), autumnStart)).toBe(
      calendarWeekColumn(autumnStart, autumnStart) + 1,
    );
  });

  it("uses five visual activity levels while ignoring out-of-window history", () => {
    const annual = buildAnnualHeatmap([
      { day: "2025-06-30", tokens: 1, costPicoUsd: 0, costKnown: true },
      { day: "2026-07-27", tokens: 25, costPicoUsd: 0, costKnown: true },
      { day: "2026-07-28", tokens: 50, costPicoUsd: 0, costKnown: true },
      { day: "2026-07-29", tokens: 75, costPicoUsd: 0, costKnown: true },
      { day: "2026-07-30", tokens: 100, costPicoUsd: 0, costKnown: true },
      { day: "2026-07-31", tokens: 1000, costPicoUsd: 0, costKnown: true },
    ], new Date(2026, 6, 30));

    expect(annual.days.find((day) => day.day === "2025-07-31")?.level).toBe(0);
    expect(annual.days.find((day) => day.day === "2026-07-27")?.level).toBe(1);
    expect(annual.days.find((day) => day.day === "2026-07-28")?.level).toBe(2);
    expect(annual.days.find((day) => day.day === "2026-07-29")?.level).toBe(3);
    expect(annual.days.find((day) => day.day === "2026-07-30")?.level).toBe(4);
    expect(annual.days.some((day) => day.day === "2026-07-31")).toBe(false);
  });

  it("renders one accessible annual History heatmap without range controls", async () => {
    snapshot = {
      ...snapshot,
      trends: [
        { day: "2026-07-28", tokens: 1234, costPicoUsd: 9_000_000_000, costKnown: false, unpricedTokens: 1234 },
        { day: "2026-06-01", tokens: 10, costPicoUsd: 1_000_000_000, costKnown: true },
      ],
    };
    location.hash = "history";
    render(<App />);
    const grid = await screen.findByRole("list", { name: /过去一年 Token 活跃度/ });
    const cells = grid.querySelectorAll(".heatmap-cell");
    expect(cells).toHaveLength(365);
    expect(grid.getAttribute("aria-label")).toContain("$0.0100 · 1234 未计");
    expect(Array.from(cells).every((cell) => !cell.hasAttribute("tabindex"))).toBe(true);
    expect(Array.from(cells).every((cell) => cell.getAttribute("role") === "listitem")).toBe(true);
    expect(within(grid).getByRole("listitem", { name: /2026-07-28，1234 Token，费用待定/ })).toBeTruthy();
    expect(screen.getByLabelText("活跃度图例：从少到多")).toBeTruthy();
    expect(screen.getByText("过去一年 Token")).toBeTruthy();
    expect(screen.getByText("未计费 Token")).toBeTruthy();
    expect(screen.queryByLabelText("活跃度时间范围")).toBeNull();
    expect(screen.getByText("最近 30 天明细")).toBeTruthy();
  });

  it("builds a complete annual heatmap from the snapshot data", () => {
    const trends = [{ day: "2026-07-30", tokens: 10, costPicoUsd: 0, costKnown: true }];
    const annual = buildAnnualHeatmap(trends, new Date(2026, 6, 30));
    expect(annual.days).toHaveLength(365);
    expect(annual.columns).toBe(53);
    expect(annual.days.at(-1)?.day).toBe("2026-07-30");
  });

  it("keeps the detail chart honestly limited to the recent 30 local days", async () => {
    snapshot = {
      ...snapshot,
      trends: [
        { day: "2026-06-01", tokens: 900, costPicoUsd: 0, costKnown: true },
        { day: "2026-07-28", tokens: 12, costPicoUsd: 0, costKnown: true },
      ],
    };
    location.hash = "history";
    render(<App />);
    const history = await screen.findByRole("list", { name: "每日 Token 用量" });
    expect(history.querySelectorAll('[role="listitem"]')).toHaveLength(1);
    expect(history.textContent).toContain("07-28");
    expect(history.textContent).not.toContain("06-01");
  });

  it("announces initial loading and snapshot failures", async () => {
    let rejectSnapshot!: (reason: Error) => void;
    vi.mocked(api.snapshot).mockReturnValueOnce(new Promise((_, reject) => { rejectSnapshot = reject; }));
    render(<App />);
    const loadingStatus = screen.getByRole("status");
    expect(loadingStatus.getAttribute("aria-live")).toBe("polite");
    await act(async () => rejectSnapshot(new Error("snapshot unavailable")));
    const alert = await screen.findByRole("alert");
    expect(alert.getAttribute("aria-live")).toBe("assertive");
    expect(alert.textContent).toContain("暂时无法读取监控数据，请稍后重试");
    expect(alert.textContent).not.toContain("snapshot unavailable");
  });

  it("announces session loading and errors", async () => {
    let rejectSession!: (reason: Error) => void;
    vi.mocked(api.session).mockReturnValueOnce(new Promise((_, reject) => { rejectSession = reject; }));
    location.hash = "session:missing";
    render(<App />);
    const detailLoading = await screen.findByText("正在读取会话详情…");
    expect(detailLoading.getAttribute("role")).toBe("status");
    await act(async () => rejectSession(new Error("session unavailable")));
    const alert = await screen.findByRole("alert");
    expect(alert.getAttribute("aria-live")).toBe("assertive");
    expect(alert.textContent).toContain("暂时无法读取会话详情，请返回后重试");
    expect(alert.textContent).not.toContain("session unavailable");
  });

  it("retries a failed session resource and renders the successful detail", async () => {
    vi.mocked(api.session)
      .mockRejectedValueOnce(new Error("temporarily unavailable"))
      .mockResolvedValueOnce({
        session: { ...snapshot.sessions[0], sessionId: "retry-session", projectName: "retry-project" },
        models: [],
        events: [],
      });
    location.hash = "session:retry-session";
    render(<App />);
    const retry = await screen.findByRole("button", { name: "重试" });
    fireEvent.click(retry);

    expect(await screen.findByRole("heading", { name: "retry-project", level: 1 })).toBeTruthy();
    expect(api.session).toHaveBeenCalledTimes(2);
  });

  it("ignores a stale session completion after navigating to another session", async () => {
    let resolveFirst!: (detail: Awaited<ReturnType<typeof api.session>>) => void;
    let resolveSecond!: (detail: Awaited<ReturnType<typeof api.session>>) => void;
    vi.mocked(api.session)
      .mockReturnValueOnce(new Promise((resolve) => { resolveFirst = resolve; }))
      .mockReturnValueOnce(new Promise((resolve) => { resolveSecond = resolve; }));
    location.hash = "session:first";
    render(<App />);
    await screen.findByText("正在读取会话详情…");

    location.hash = "session:second";
    await act(async () => window.dispatchEvent(new HashChangeEvent("hashchange")));
    await act(async () => resolveSecond({
      session: { ...snapshot.sessions[0], sessionId: "second", projectName: "second-project" },
      models: [],
      events: [],
    }));
    expect(await screen.findByRole("heading", { name: "second-project", level: 1 })).toBeTruthy();

    await act(async () => resolveFirst({
      session: { ...snapshot.sessions[0], sessionId: "first", projectName: "stale-project" },
      models: [],
      events: [],
    }));
    expect(screen.queryByRole("heading", { name: "stale-project", level: 1 })).toBeNull();
    expect(screen.getByRole("heading", { name: "second-project", level: 1 })).toBeTruthy();
  });

  it("focuses a delayed session heading after in-app navigation and exposes timeline list semantics", async () => {
    let resolveSession!: (detail: Awaited<ReturnType<typeof api.session>>) => void;
    vi.mocked(api.session).mockReturnValueOnce(new Promise((resolve) => { resolveSession = resolve; }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /project-a/ }));
    await screen.findByText("正在读取会话详情…");

    await act(async () => resolveSession({
      session: snapshot.sessions[0],
      models: [],
      events: [
        { sourceEvent: "PreToolUse", source: "hook", occurredAtMs: 1 },
        { sourceEvent: "Stop", source: "transcript", occurredAtMs: 2 },
      ],
    }));

    const heading = await screen.findByRole("heading", { name: "project-a", level: 1 });
    await waitFor(() => expect(document.activeElement).toBe(heading));
    const timeline = screen.getByRole("list", { name: "会话事件历史" });
    expect(timeline.querySelectorAll('[role="listitem"], li')).toHaveLength(2);
  });

  it("does not steal focus moved to the sidebar while a session is loading", async () => {
    let resolveSession!: (detail: Awaited<ReturnType<typeof api.session>>) => void;
    vi.mocked(api.session).mockReturnValueOnce(new Promise((resolve) => { resolveSession = resolve; }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /project-a/ }));
    await screen.findByText("正在读取会话详情…");
    const settingsNav = screen.getByRole("button", { name: "设置" });
    settingsNav.focus();

    await act(async () => resolveSession({
      session: snapshot.sessions[0],
      models: [],
      events: [],
    }));
    await screen.findByRole("heading", { name: "project-a", level: 1 });
    expect(document.activeElement).toBe(settingsNav);
  });

  it("does not steal focus when a session route is opened initially", async () => {
    vi.mocked(api.session).mockResolvedValueOnce({
      session: snapshot.sessions[0],
      models: [],
      events: [],
    });
    location.hash = "session:session-one";
    render(<App />);
    const heading = await screen.findByRole("heading", { name: "project-a", level: 1 });
    expect(document.activeElement).not.toBe(heading);
  });

  it("marks active navigation and moves focus after route changes", async () => {
    render(<App />);
    await screen.findByText("project-a");
    expect(screen.getByRole("button", { name: "总览" }).getAttribute("aria-current")).toBe("page");
    fireEvent.click(screen.getByRole("button", { name: "设置" }));
    const heading = await screen.findByRole("heading", { name: "设置", level: 1 });
    await waitFor(() => expect(document.activeElement).toBe(heading));
    expect(screen.getByRole("button", { name: "设置" }).getAttribute("aria-current")).toBe("page");
  });
});
