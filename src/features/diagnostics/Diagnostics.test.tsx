import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import App from "../../App";
import { api } from "../../api";
import { appTestState, invalidateSnapshot } from "../../appTestHarness";

describe("Diagnostics", () => {
  it("reports completion after a requested reindex", async () => {
    render(<App />);
    await screen.findByText("project-a");
    location.hash = "diagnostics";
    await act(async () => {
      window.dispatchEvent(new HashChangeEvent("hashchange"));
    });
    const button = await screen.findByRole("button", { name: "重新扫描历史记录" });
    await act(async () => {
      fireEvent.click(button);
    });
    expect(await screen.findByRole("progressbar", { name: "历史记录扫描进度" })).toBeTruthy();

    appTestState.snapshot = {
      ...appTestState.snapshot,
      index: { runId: "run-a", state: "complete", completed: 12, total: 12, failedFiles: 0, quarantinedSessions: 0, interrupted: false },
    };
    await invalidateSnapshot(8);
    expect(await screen.findByText("历史记录扫描完成，共处理 12 个会话记录")).toBeTruthy();
  });

  it("copies sanitized diagnostics without paths or provider errors", async () => {
    appTestState.snapshot = {
      ...appTestState.snapshot,
      diagnostics: {
        ...appTestState.snapshot.diagnostics,
        migrationVersion: 1,
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
    const button = await screen.findByRole("button", { name: "复制诊断信息" });
    const statusRegion = screen.getByRole("region", { name: "运行状态" });
    expect(statusRegion.contains(button)).toBe(true);
    expect(statusRegion.contains(screen.getByRole("button", { name: "打开 macOS 通知设置" }))).toBe(true);
    await act(async () => {
      fireEvent.click(button);
    });
    expect(await screen.findByText("诊断信息已复制")).toBeTruthy();
    const report = vi.mocked(navigator.clipboard.writeText).mock.calls[0][0];
    expect(report).toContain("migration_version=1");
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
    appTestState.snapshot = {
      ...appTestState.snapshot,
      hook: { status: "installed", issueCode: null, version: "1" },
      diagnostics: {
        ...appTestState.snapshot.diagnostics,
        desktopFailures: 2,
        desktopErrorCode: "delivery_failed",
        ntfyFailures: 1,
        ntfyErrorCode: "sensitive_error_redacted",
      },
    };
    location.hash = "diagnostics";
    render(<App />);
    await screen.findByRole("region", { name: "事件采集" });
    expect(screen.queryByText("数据库结构版本")).toBeNull();
    expect(screen.queryByText("Hook 路径")).toBeNull();
    expect(screen.queryByText(/private desktop provider error/)).toBeNull();
    expect(screen.queryByText(/private ntfy provider error/)).toBeNull();
    expect(screen.getByText("2 次连续失败 · 通知发送失败")).toBeTruthy();
    expect(screen.getByText("1 次连续失败 · 远程通知异常")).toBeTruthy();
    expect(document.body.textContent).not.toContain("delivery_failed");
    expect(document.body.textContent).not.toContain("sensitive_error_redacted");
    expect(document.body.textContent).not.toContain("/Users/private");
  });

  it("counts pending notifications as attention instead of reporting normal", async () => {
    appTestState.snapshot = {
      ...appTestState.snapshot,
      hook: { status: "installed", issueCode: null, version: "1" },
      diagnostics: {
        ...appTestState.snapshot.diagnostics,
        pendingNotifications: 3,
        backgroundHealth: [{
          task: "incremental_index",
          successCount: 1,
          failureCount: 0,
          consecutiveFailures: 0,
          errorCode: null,
          lastSucceededAtMs: 10,
          lastFailedAtMs: null,
          recoveredAtMs: null,
        }],
      },
    };
    location.hash = "diagnostics";
    render(<App />);
    expect((await screen.findAllByText("1 项需要处理")).length).toBeGreaterThan(0);
    expect(screen.getByText("3", { selector: ".info strong" })).toBeTruthy();
    expect(screen.queryByText("未发现异常")).toBeNull();
    expect(screen.queryByText("未发现需要处理的异常")).toBeNull();
  });

  it("shows empty background health as unknown with no run records", async () => {
    appTestState.snapshot = {
      ...appTestState.snapshot,
      hook: { status: "installed", issueCode: null, version: "1" },
      diagnostics: { ...appTestState.snapshot.diagnostics, backgroundHealth: [] },
    };
    location.hash = "diagnostics";
    render(<App />);
    expect(await screen.findByText("状态信息不完整")).toBeTruthy();
    expect(screen.getByText("后台任务尚无运行记录，暂时无法判断其健康状态")).toBeTruthy();
    expect(screen.getByText("尚无后台任务运行记录")).toBeTruthy();
    expect(screen.queryByText("未发现异常")).toBeNull();
  });

  it("describes zero provider failures as no consecutive failures", async () => {
    appTestState.snapshot = {
      ...appTestState.snapshot,
      hook: { status: "installed", issueCode: null, version: "1" },
      diagnostics: {
        ...appTestState.snapshot.diagnostics,
        backgroundHealth: [{
          task: "incremental_index",
          successCount: 1,
          failureCount: 0,
          consecutiveFailures: 0,
          errorCode: null,
          lastSucceededAtMs: 10,
          lastFailedAtMs: null,
          recoveredAtMs: null,
        }],
      },
    };
    location.hash = "diagnostics";
    render(<App />);
    const notificationCard = await screen.findByRole("region", { name: "通知通道" });
    expect(within(notificationCard).getAllByText("未发现连续失败")).toHaveLength(2);
    expect(within(notificationCard).queryByText("正常")).toBeNull();
  });

  it("ignores a stale terminal reindex snapshot after a second run starts", async () => {
    vi.mocked(api.reindex)
      .mockResolvedValueOnce({ runId: "run-a" })
      .mockResolvedValueOnce({ runId: "run-b" });
    location.hash = "diagnostics";
    render(<App />);
    const button = await screen.findByRole("button", { name: "重新扫描历史记录" });
    await act(async () => { fireEvent.click(button); });
    appTestState.snapshot = { ...appTestState.snapshot, index: { runId: "run-a", state: "complete", completed: 4, total: 4, failedFiles: 0, quarantinedSessions: 0, interrupted: false } };
    await invalidateSnapshot(8);
    expect(await screen.findByText("历史记录扫描完成，共处理 4 个会话记录")).toBeTruthy();

    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "重新扫描历史记录" })); });
    appTestState.snapshot = { ...appTestState.snapshot, index: { runId: "run-a", state: "complete", completed: 4, total: 4, failedFiles: 0, quarantinedSessions: 0, interrupted: false } };
    await invalidateSnapshot(9);
    expect(screen.queryByText("历史记录扫描完成，共处理 4 个会话记录")).toBeNull();
    expect(screen.getByRole("progressbar", { name: "历史记录扫描进度" }).getAttribute("value")).toBeNull();

    appTestState.snapshot = { ...appTestState.snapshot, index: { runId: "run-b", state: "complete", completed: 8, total: 8, failedFiles: 0, quarantinedSessions: 0, interrupted: false } };
    await invalidateSnapshot(10);
    expect(await screen.findByText("历史记录扫描完成，共处理 8 个会话记录")).toBeTruthy();
  });

  it("requires confirmation and reports truthful cleanup counts", async () => {
    vi.mocked(api.clearHistory).mockResolvedValueOnce({ rawEventsDeleted: 7, notificationsDeleted: 2 });
    location.hash = "diagnostics";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "清理已处理记录" }));
    expect(screen.getByText(/活跃会话、Token 用量与每日汇总、历史扫描进度标记、待发送和仍可重试的失败通知都会保留/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(screen.queryByRole("button", { name: "确认清理" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "清理已处理记录" }));
    await act(async () => { fireEvent.click(screen.getByRole("button", { name: "确认清理" })); });
    expect(await screen.findByText("已删除 7 条原始事件和 2 条通知记录")).toBeTruthy();
  });

  it("adopts an existing reindex and reports matching progress and failure", async () => {
    appTestState.snapshot = { ...appTestState.snapshot, index: { runId: "existing", state: "running", completed: 2, total: 5, failedFiles: 0, quarantinedSessions: 0, interrupted: false } };
    location.hash = "diagnostics";
    render(<App />);
    const button = await screen.findByRole("button", { name: "正在扫描历史记录…" });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByRole("progressbar", { name: "历史记录扫描进度" }).getAttribute("value")).toBe("2");
    appTestState.snapshot = { ...appTestState.snapshot, index: { runId: "existing", state: "failed", completed: 3, total: 5, failedFiles: 1, quarantinedSessions: 2, interrupted: false } };
    await invalidateSnapshot(11);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("1 个记录文件读取失败，2 个处理失败的会话，其他内容已完成");
    expect(alert.textContent).not.toContain("bad index");
    expect(alert.textContent).not.toContain("/private/path");
  });

  it("reports reindex startup rejection", async () => {
    vi.mocked(api.reindex).mockRejectedValueOnce(new Error("cannot start"));
    location.hash = "diagnostics";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "重新扫描历史记录" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("历史记录扫描失败，请重试");
    expect(alert.textContent).not.toContain("cannot start");
  });

  it("uses a generic safe message when reindexing is interrupted", async () => {
    appTestState.snapshot = {
      ...appTestState.snapshot,
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
    await screen.findByRole("progressbar", { name: "历史记录扫描进度" });
    appTestState.snapshot = {
      ...appTestState.snapshot,
      index: {
        ...appTestState.snapshot.index,
        state: "failed",
        quarantinedSessions: 1,
        interrupted: true,
      },
    };
    await invalidateSnapshot(12);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("历史记录扫描失败，请重试");
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

  it("shows truthful feedback for diagnostic utility actions", async () => {
    let rejectCopy!: (reason: Error) => void;
    vi.mocked(navigator.clipboard.writeText).mockReturnValueOnce(new Promise((_, reject) => { rejectCopy = reject; }));
    let resolveSettings!: () => void;
    vi.mocked(api.openNotificationSettings).mockReturnValueOnce(new Promise((resolve) => { resolveSettings = resolve; }));
    location.hash = "diagnostics";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "复制诊断信息" }));
    expect((screen.getByRole("button", { name: "正在复制…" }) as HTMLButtonElement).disabled).toBe(true);
    await act(async () => rejectCopy(new Error("denied")));
    expect(await screen.findByText("复制失败，请检查剪贴板权限")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "测试桌面通知" }));
    expect(await screen.findByText("测试通知已提交，请检查通知中心")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "打开 macOS 通知设置" }));
    expect((screen.getByRole("button", { name: "正在打开…" }) as HTMLButtonElement).disabled).toBe(true);
    await act(async () => resolveSettings());
    expect(await screen.findByText("已请求打开通知设置")).toBeTruthy();
    vi.mocked(api.openNotificationSettings).mockRejectedValueOnce(new Error("unavailable"));
    fireEvent.click(screen.getByRole("button", { name: "打开 macOS 通知设置" }));
    expect(await screen.findByText("无法打开通知设置")).toBeTruthy();
  });

  it("groups diagnostic and maintenance actions in right-anchored containers", async () => {
    location.hash = "diagnostics";
    render(<App />);

    const notificationTools = await screen.findByRole("group", { name: "通知诊断工具" });
    expect(within(notificationTools).getAllByRole("button").map((button) => button.textContent)).toEqual([
      "测试桌面通知",
      "打开 macOS 通知设置",
    ]);

    const reindexButton = screen.getByRole("button", { name: "重新扫描历史记录" });
    expect(reindexButton.parentElement?.classList.contains("maintenance-actions")).toBe(true);

    const cleanupButton = screen.getByRole("button", { name: "清理已处理记录" });
    expect(cleanupButton.parentElement?.classList.contains("maintenance-actions")).toBe(true);
    fireEvent.click(cleanupButton);

    const confirmation = screen.getByRole("group", { name: "确认清理历史记录" });
    expect(confirmation.parentElement?.classList.contains("maintenance-actions")).toBe(true);
    const confirmationButtons = within(confirmation).getAllByRole("button");
    expect(confirmationButtons.map((button) => button.textContent)).toEqual(["确认清理", "取消"]);
    expect(confirmationButtons[0].parentElement?.classList.contains("cleanup-confirmation-actions")).toBe(true);
  });
});
