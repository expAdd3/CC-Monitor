import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { listen } from "@tauri-apps/api/event";
import { StrictMode } from "react";
import { describe, expect, it, vi } from "vitest";
import App, { eventLabel } from "./App";
import { api, compact } from "./api";
import { appTestState } from "./appTestHarness";
import { indexStateLabel } from "./features/diagnostics/Diagnostics";

describe("App shell and session details", () => {
  it("renders snapshot view states and responds to navigation events", async () => {
    render(<App />);
    expect(await screen.findByText("project-a")).toBeTruthy();
    expect(screen.getByText("需要介入")).toBeTruthy();
    await waitFor(() => expect(appTestState.listeners.has("monitor://navigate")).toBe(true));
    appTestState.listeners.get("monitor://navigate")?.({ payload: "diagnostics" });
    const collectorRegion = await screen.findByRole("region", { name: "事件采集" });
    expect(screen.getByText("等待处理的事件")).toBeTruthy();
    expect(screen.queryByText("待处理事件")).toBeNull();
    expect(within(collectorRegion).queryByText("查看技术详情")).toBeNull();
    expect(within(collectorRegion).queryByText("数据库结构版本")).toBeNull();
    const collectorChildren = Array.from(collectorRegion.children);
    expect(collectorChildren.at(-1)?.classList.contains("info")).toBe(true);
    expect(collectorChildren.at(-1)?.textContent).toContain("处理失败的会话");
    expect(document.body.textContent).not.toContain("重新索引");
    expect(document.body.textContent).not.toContain("索引中");
  });

  it("provides a skip link and named application landmarks", async () => {
    render(<App />);
    await screen.findByRole("heading", { name: "运行总览", level: 1 });

    const skipLink = screen.getByRole("link", { name: "跳到主要内容" });
    expect(skipLink.getAttribute("href")).toBe("#main-content");
    expect(screen.getByRole("complementary", { name: "应用侧边栏" })).toBeTruthy();
    expect(screen.getByRole("navigation", { name: "主导航" })).toBeTruthy();
    const main = screen.getByRole("main", { name: "主要内容" });
    expect(main.id).toBe("main-content");
    expect(within(main).getAllByRole("heading", { level: 1 })).toHaveLength(1);
  });

  it("moves focus to main content without changing the current hash route", async () => {
    location.hash = "settings";
    render(<App />);
    await screen.findByRole("region", { name: "事件采集器管理" });
    const main = screen.getByRole("main", { name: "主要内容" });

    fireEvent.click(screen.getByRole("link", { name: "跳到主要内容" }));

    expect(location.hash).toBe("#settings");
    expect(document.activeElement).toBe(main);
    expect(screen.getByRole("heading", { name: "设置", level: 1 })).toBeTruthy();
  });

  it("does not report a missing collector before the real snapshot is ready", async () => {
    let resolveSnapshot!: (value: typeof appTestState.snapshot) => void;
    vi.mocked(api.snapshot).mockReturnValueOnce(
      new Promise((resolve) => { resolveSnapshot = resolve; }),
    );
    render(<App />);

    expect(screen.queryByText("事件采集器未安装")).toBeNull();
    expect(screen.getByRole("button", { name: "正在检测事件采集器，打开设置" })).toBeTruthy();
    await act(async () => resolveSnapshot(appTestState.snapshot));
    expect(await screen.findByRole("heading", { name: "事件采集器未安装" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "打开设置" })).toBeTruthy();
  });

  it("routes collector setup to Settings without mutating from Dashboard", async () => {
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "打开设置" }));
    expect(await screen.findByRole("heading", { name: "设置", level: 1 })).toBeTruthy();
    expect(api.installHook).not.toHaveBeenCalled();
  });

  it("suppresses the Dashboard callout after a deferred onboarding choice", async () => {
    appTestState.snapshot = { ...appTestState.snapshot, hookOnboardingDisposition: "deferred" };
    render(<App />);

    await screen.findByText("project-a");
    expect(screen.queryByRole("heading", { name: "事件采集器未安装" })).toBeNull();
    expect(screen.queryByRole("button", { name: "打开设置" })).toBeNull();
    expect(screen.getByRole("button", { name: "事件采集器未安装，打开设置" })).toBeTruthy();
    expect(api.installHook).not.toHaveBeenCalled();
  });

  it("does not expose collector install or repair actions on Dashboard", async () => {
    render(<App />);

    await screen.findByRole("heading", { name: "事件采集器未安装" });
    expect(screen.queryByRole("button", { name: "安装事件采集器" })).toBeNull();
    expect(screen.queryByRole("button", { name: "立即修复" })).toBeNull();
    expect(api.installHook).not.toHaveBeenCalled();
  });

  it("shows repair details regardless of a deferred onboarding choice", async () => {
    appTestState.snapshot = {
      ...appTestState.snapshot,
      hook: { status: "repair_required", issueCode: "hook_settings_mismatch", version: "1" },
      hookOnboardingDisposition: "deferred",
    };
    render(<App />);

    expect(await screen.findByText(/当前配置不完整：Claude Code 设置不匹配/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "立即修复" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "打开设置" }));
    expect(await screen.findByRole("heading", { name: "设置", level: 1 })).toBeTruthy();
    expect(api.installHook).not.toHaveBeenCalled();
  });

  it("suppresses the Diagnostics callout after deliberate uninstall", async () => {
    appTestState.snapshot = {
      ...appTestState.snapshot,
      hookOnboardingDisposition: "deliberately_uninstalled",
    };
    location.hash = "diagnostics";
    render(<App />);

    await screen.findByRole("heading", { name: "诊断", level: 1 });
    expect(screen.queryByRole("heading", { name: "事件采集器未安装" })).toBeNull();
    expect(screen.queryByRole("button", { name: "打开设置" })).toBeNull();
    expect(screen.getByRole("button", { name: "事件采集器未安装，打开设置" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "安装事件采集器" })).toBeNull();
    expect(api.installHook).not.toHaveBeenCalled();
  });

  it("routes a repair-required Diagnostics status to Settings without repairing in place", async () => {
    appTestState.snapshot = {
      ...appTestState.snapshot,
      hook: { status: "repair_required", issueCode: "hook_settings_mismatch", version: "1" },
    };
    location.hash = "diagnostics";
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "打开设置" }));
    expect(await screen.findByRole("heading", { name: "设置", level: 1 })).toBeTruthy();
    expect(api.installHook).not.toHaveBeenCalled();
  });

  it("coalesces invalidations and refuses an older snapshot completion", async () => {
    render(<App />);
    await screen.findByText("project-a");
    await waitFor(() => expect(appTestState.listeners.has("monitor://invalidated")).toBe(true));

    let resolveOlder!: (value: typeof appTestState.snapshot) => void;
    const older = new Promise<typeof appTestState.snapshot>((resolve) => { resolveOlder = resolve; });
    const newest = {
      ...appTestState.snapshot,
      revision: 22,
      sessions: [{ ...appTestState.snapshot.sessions[0], projectName: "newest-project" }],
    };
    vi.mocked(api.snapshot)
      .mockReturnValueOnce(older)
      .mockResolvedValueOnce(newest);

    await act(async () => {
      appTestState.listeners.get("monitor://invalidated")?.({ payload: { revision: 21 } });
      appTestState.listeners.get("monitor://invalidated")?.({ payload: { revision: 22 } });
      resolveOlder({
        ...appTestState.snapshot,
        revision: 21,
        sessions: [{ ...appTestState.snapshot.sessions[0], projectName: "stale-project" }],
      });
      await older;
    });

    expect(await screen.findByText("newest-project")).toBeTruthy();
    expect(screen.queryByText("stale-project")).toBeNull();
    expect(api.snapshot).toHaveBeenCalledTimes(3);
  });

  it("refreshes the snapshot when an idle window crosses local midnight", async () => {
    vi.useFakeTimers();
    let rendered: ReturnType<typeof render> | undefined;
    try {
      vi.setSystemTime(new Date(2026, 7, 2, 23, 59, 59, 900));
      rendered = render(<App />);
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(api.snapshot).toHaveBeenCalledTimes(1);

      appTestState.snapshot = {
        ...appTestState.snapshot,
        today: { ...appTestState.snapshot.today, inputTokens: 42 },
      };
      await act(async () => {
        await vi.advanceTimersByTimeAsync(100);
      });

      expect(api.snapshot).toHaveBeenCalledTimes(2);
      expect(screen.getAllByText("42").length).toBeGreaterThan(0);
    } finally {
      rendered?.unmount();
      vi.useRealTimers();
    }
  });

  it("follows a Hook mutation revision instead of publishing its in-flight old snapshot", async () => {
    appTestState.snapshot = { ...appTestState.snapshot, revision: 30, hook: { status: "absent", issueCode: null, version: null } };
    render(<App />);
    await screen.findByText("project-a");
    fireEvent.click(screen.getByRole("button", { name: "设置" }));
    await screen.findByRole("button", { name: "安装事件采集器" });

    let resolveOld!: (value: typeof appTestState.snapshot) => void;
    const oldRead = new Promise<typeof appTestState.snapshot>((resolve) => { resolveOld = resolve; });
    const installed = {
      ...appTestState.snapshot,
      revision: 32,
      hook: { status: "installed" as const, issueCode: null, version: "1" },
    };
    vi.mocked(api.snapshot).mockReturnValueOnce(oldRead).mockResolvedValueOnce(installed);
    await act(async () => {
      appTestState.listeners.get("monitor://invalidated")?.({ payload: { revision: 31 } });
    });
    vi.mocked(api.installHook).mockImplementationOnce(async () => {
      appTestState.listeners.get("monitor://invalidated")?.({ payload: { revision: 32 } });
    });
    fireEvent.click(screen.getByRole("button", { name: "安装事件采集器" }));
    await waitFor(() => expect(api.installHook).toHaveBeenCalledOnce());
    await act(async () => {
      resolveOld({ ...appTestState.snapshot, revision: 31 });
      await oldRead;
    });

    await waitFor(() => expect(api.snapshot).toHaveBeenCalledTimes(3));
    expect(await screen.findByText(/安装完整且配置有效/)).toBeTruthy();
  });

  it("does not publish a snapshot completion after unmount", async () => {
    let resolveSnapshot!: (value: typeof appTestState.snapshot) => void;
    vi.mocked(api.snapshot).mockReturnValueOnce(
      new Promise((resolve) => { resolveSnapshot = resolve; }),
    );
    const rendered = render(<App />);
    await waitFor(() => expect(api.snapshot).toHaveBeenCalledOnce());
    rendered.unmount();
    await act(async () => resolveSnapshot(appTestState.snapshot));
    expect(rendered.container.textContent).toBe("");
  });

  it("focuses the requested route during initial loading and renders it when the snapshot is ready", async () => {
    let resolveSnapshot!: (value: typeof appTestState.snapshot) => void;
    vi.mocked(api.snapshot).mockReturnValueOnce(
      new Promise((resolve) => { resolveSnapshot = resolve; }),
    );
    render(<App />);
    const settingsNav = screen.getByRole("button", { name: "设置" });
    fireEvent.click(settingsNav);
    const loadingHeading = await screen.findByRole("heading", { name: "设置", level: 1 });
    await waitFor(() => expect(document.activeElement).toBe(loadingHeading));

    await act(async () => resolveSnapshot(appTestState.snapshot));
    await screen.findByRole("region", { name: "事件采集器管理" });
    expect(screen.getByRole("heading", { name: "设置", level: 1 })).toBeTruthy();
    expect(screen.getByRole("button", { name: "设置" }).getAttribute("aria-current")).toBe("page");
  });

  it("unsubscribes both desktop listeners on unmount", async () => {
    const rendered = render(<App />);
    await waitFor(() => expect(appTestState.listeners.size).toBe(2));
    rendered.unmount();
    expect(appTestState.unlistenInvalidated).toHaveBeenCalledOnce();
    expect(appTestState.unlistenNavigate).toHaveBeenCalledOnce();
  });

  it("loads once and releases every listener registration under StrictMode effects", async () => {
    const rendered = render(<StrictMode><App /></StrictMode>);
    expect(await screen.findByText("project-a")).toBeTruthy();
    expect(api.snapshot).toHaveBeenCalledOnce();
    expect(listen).toHaveBeenCalledTimes(4);
    await waitFor(() => {
      expect(appTestState.unlistenInvalidated).toHaveBeenCalledTimes(1);
      expect(appTestState.unlistenNavigate).toHaveBeenCalledTimes(1);
    });

    rendered.unmount();
    expect(appTestState.unlistenInvalidated).toHaveBeenCalledTimes(2);
    expect(appTestState.unlistenNavigate).toHaveBeenCalledTimes(2);
  });

  it("keeps and releases a successful listener when the other registration fails", async () => {
    appTestState.rejectedListener = "monitor://navigate";
    const rendered = render(<App />);
    await screen.findByText("project-a");
    expect(appTestState.listeners.has("monitor://invalidated")).toBe(true);
    expect(appTestState.listeners.has("monitor://navigate")).toBe(false);
    rendered.unmount();
    expect(appTestState.unlistenInvalidated).toHaveBeenCalledOnce();
    expect(appTestState.unlistenNavigate).not.toHaveBeenCalled();
  });

  it("renders the bundled mascot and localized dashboard vocabulary", async () => {
    appTestState.snapshot = {
      ...appTestState.snapshot,
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
    expect(screen.getByText("缓存创建")).toBeTruthy();
    expect(screen.queryByText("缓存写入")).toBeNull();
  });

  it("discloses when the active-session list is capped", async () => {
    appTestState.snapshot = {
      ...appTestState.snapshot,
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
    appTestState.snapshot = {
      ...appTestState.snapshot,
      today: { ...appTestState.snapshot.today, inputTokens: precise },
    };
    render(<App />);
    expect((await screen.findAllByText(compact(precise))).length).toBeGreaterThan(0);
  });

  it("describes cleanup scope truthfully before confirmation", async () => {
    location.hash = "diagnostics";
    render(<App />);
    expect(await screen.findByText("清理已结束会话的原始事件与通知记录。")).toBeTruthy();
    expect(document.body.textContent).not.toContain("终态通知记录");
    fireEvent.click(screen.getByRole("button", { name: "清理已处理记录" }));
    expect(screen.getByText(/将删除已结束会话的原始事件、会话状态，以及已发送、已抑制和重试耗尽的通知记录/)).toBeTruthy();
    expect(screen.getByText(/活跃会话、Token 用量与每日汇总/)).toBeTruthy();
    const dangerZone = screen.getByRole("region", { name: "危险操作：数据维护" });
    expect(dangerZone.querySelector("#cleanup-title")?.textContent).toBe("数据维护");
  });

  it("maps every index status to localized visible copy", () => {
    expect(indexStateLabel("idle")).toBe("就绪");
    expect(indexStateLabel("running")).toBe("扫描中");
    expect(indexStateLabel("complete")).toBe("已完成");
    expect(indexStateLabel("failed")).toBe("失败");
  });

  it("labels transcript events instead of collapsing them into a generic update", () => {
    expect(eventLabel("TranscriptAssistantText")).toBe("助手输出");
    expect(eventLabel("TranscriptAssistantToolUse")).toBe("助手调用工具");
    expect(eventLabel("TranscriptToolResult")).toBe("工具返回结果");
    expect(eventLabel("FutureEvent")).toBe("未知事件（FutureEvent）");
  });

  it("announces initial loading and snapshot failures", async () => {
    let rejectSnapshot!: (reason: Error) => void;
    vi.mocked(api.snapshot).mockReturnValueOnce(new Promise((_, reject) => { rejectSnapshot = reject; }));
    render(<App />);
    expect(screen.getByRole("heading", { name: "运行总览", level: 1 })).toBeTruthy();
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
    expect(screen.getByRole("heading", { name: "会话详情", level: 1 })).toBeTruthy();
    expect(detailLoading.getAttribute("role")).toBe("status");
    await act(async () => rejectSession(new Error("session unavailable")));
    const alert = await screen.findByRole("alert");
    expect(screen.getByRole("heading", { name: "会话详情", level: 1 })).toBeTruthy();
    expect(alert.getAttribute("aria-live")).toBe("assertive");
    expect(alert.textContent).toContain("暂时无法读取会话详情，请返回后重试");
    expect(alert.textContent).not.toContain("session unavailable");
  });

  it("retries a failed session resource and renders the successful detail", async () => {
    vi.mocked(api.session)
      .mockRejectedValueOnce(new Error("temporarily unavailable"))
      .mockResolvedValueOnce({
        session: { ...appTestState.snapshot.sessions[0], sessionId: "retry-session", projectName: "retry-project" },
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
      session: { ...appTestState.snapshot.sessions[0], sessionId: "second", projectName: "second-project" },
      models: [],
      events: [],
    }));
    expect(await screen.findByRole("heading", { name: "second-project", level: 1 })).toBeTruthy();

    await act(async () => resolveFirst({
      session: { ...appTestState.snapshot.sessions[0], sessionId: "first", projectName: "stale-project" },
      models: [],
      events: [],
    }));
    expect(screen.queryByRole("heading", { name: "stale-project", level: 1 })).toBeNull();
    expect(screen.getByRole("heading", { name: "second-project", level: 1 })).toBeTruthy();
  });

  it("focuses the session heading during loading, retains focus, and exposes timeline list semantics", async () => {
    let resolveSession!: (detail: Awaited<ReturnType<typeof api.session>>) => void;
    vi.mocked(api.session).mockReturnValueOnce(new Promise((resolve) => { resolveSession = resolve; }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /project-a/ }));
    await screen.findByText("正在读取会话详情…");
    const loadingHeading = screen.getByRole("heading", { name: "会话详情", level: 1 });
    await waitFor(() => expect(document.activeElement).toBe(loadingHeading));

    await act(async () => resolveSession({
      session: appTestState.snapshot.sessions[0],
      models: [],
      events: [
        { sourceEvent: "PreToolUse", source: "hook", occurredAtMs: 1 },
        { sourceEvent: "Stop", source: "transcript", occurredAtMs: 2 },
      ],
    }));

    const readyHeading = await screen.findByRole("heading", { name: "project-a", level: 1 });
    expect(document.activeElement).toBe(readyHeading);
    const timeline = screen.getByRole("list", { name: "会话事件历史" });
    expect(timeline.querySelectorAll('[role="listitem"], li')).toHaveLength(2);
  });

  it("shows every token dimension for each model in session details", async () => {
    vi.mocked(api.session).mockResolvedValueOnce({
      session: appTestState.snapshot.sessions[0],
      models: [{
        modelId: "claude-sonnet-test",
        inputTokens: "11",
        outputTokens: "22",
        cacheReadTokens: "33",
        cacheWriteTokens: "44",
        tokens: "110",
        costPicoUsd: "1000000000",
        costKnown: true,
        unpricedTokens: "0",
      }],
      events: [],
    });
    location.hash = `session:${appTestState.snapshot.sessions[0].sessionId}`;
    render(<App />);

    const table = await screen.findByRole("table", { name: "本次会话各模型的 Token 用量" });
    expect(await screen.findByRole("columnheader", { name: "输入" })).toBeTruthy();
    expect(screen.getByRole("columnheader", { name: "输出" })).toBeTruthy();
    expect(screen.getByRole("columnheader", { name: "缓存读取" })).toBeTruthy();
    expect(screen.getByRole("columnheader", { name: "缓存创建" })).toBeTruthy();
    expect(within(table).getByRole("rowheader", { name: /claude-sonnet-test/ })).toBeTruthy();
    const row = screen.getByRole("row", { name: /claude-sonnet-test/ });
    expect(row.textContent).toContain("11");
    expect(row.textContent).toContain("22");
    expect(row.textContent).toContain("33");
    expect(row.textContent).toContain("44");
  });

  it("does not steal focus moved to the sidebar while a session is loading", async () => {
    let resolveSession!: (detail: Awaited<ReturnType<typeof api.session>>) => void;
    vi.mocked(api.session).mockReturnValueOnce(new Promise((resolve) => { resolveSession = resolve; }));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: /project-a/ }));
    await screen.findByText("正在读取会话详情…");
    const loadingHeading = screen.getByRole("heading", { name: "会话详情", level: 1 });
    await waitFor(() => expect(document.activeElement).toBe(loadingHeading));
    const settingsNav = screen.getByRole("button", { name: "设置" });
    settingsNav.focus();

    await act(async () => resolveSession({
      session: appTestState.snapshot.sessions[0],
      models: [],
      events: [],
    }));
    await screen.findByRole("heading", { name: "project-a", level: 1 });
    expect(document.activeElement).toBe(settingsNav);
  });

  it("does not steal focus when a session route is opened initially", async () => {
    vi.mocked(api.session).mockResolvedValueOnce({
      session: appTestState.snapshot.sessions[0],
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
