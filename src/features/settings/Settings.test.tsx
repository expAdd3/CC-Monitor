import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import App from "../../App";
import { api, type SettingsDto } from "../../api";
import { appTestState, savedSettings } from "../../appTestHarness";
import { validateNtfyTopic } from "./Settings";
import topicFixtures from "../../../tests/fixtures/ntfy_topics.json";

describe("Settings", () => {
  it("uses the shared ntfy topic contract", () => {
    for (const topic of topicFixtures.valid) {
      expect(validateNtfyTopic(topic), `valid topic: ${JSON.stringify(topic)}`).toBeUndefined();
    }
    for (const topic of topicFixtures.invalid) {
      expect(validateNtfyTopic(topic), `invalid topic: ${JSON.stringify(topic)}`).toBeTruthy();
    }
  });

  it("tracks dirty, pending and saved settings without duplicate saves", async () => {
    let finishSave!: () => void;
    vi.mocked(api.saveSettings).mockImplementationOnce((settings) => new Promise<SettingsDto>((resolve) => {
      finishSave = () => resolve(savedSettings(settings));
    }));
    location.hash = "settings";
    render(<App />);
    const topic = await screen.findByRole("textbox", { name: "通知主题（Topic）" });
    expect((screen.getByRole("button", { name: "保存远程通知设置" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(topic, { target: { value: "alerts" } });
    fireEvent.click(screen.getByRole("button", { name: "保存远程通知设置" }));
    expect((await screen.findByRole("button", { name: "正在保存…" }) as HTMLButtonElement).disabled).toBe(true);
    expect((topic as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole("checkbox", { name: "启用 ntfy" }) as HTMLInputElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "正在保存…" }));
    expect(api.saveSettings).toHaveBeenCalledTimes(1);
    await act(async () => { finishSave(); });
    expect(await screen.findByText("远程通知设置已保存")).toBeTruthy();
    fireEvent.change(topic, { target: { value: "changed-again" } });
    expect(screen.queryByText("远程通知设置已保存")).toBeNull();
    expect(screen.getByText("有未保存的更改")).toBeTruthy();
  });

  it("shows save errors and allows retry", async () => {
    vi.mocked(api.saveSettings).mockRejectedValueOnce(new Error("disk full"));
    location.hash = "settings";
    render(<App />);
    fireEvent.change(await screen.findByRole("textbox", { name: "通知主题（Topic）" }), { target: { value: "alerts" } });
    fireEvent.click(screen.getByRole("button", { name: "保存远程通知设置" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("设置保存失败，请重试");
    expect(alert.textContent).not.toContain("disk full");
    expect((screen.getByRole("button", { name: "保存远程通知设置" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("explains a failed autostart compensation without exposing backend details", async () => {
    vi.mocked(api.saveSettings).mockRejectedValueOnce("settings_inconsistent");
    location.hash = "settings";
    render(<App />);
    fireEvent.change(await screen.findByRole("textbox", { name: "通知主题（Topic）" }), { target: { value: "alerts" } });
    fireEvent.click(screen.getByRole("button", { name: "保存远程通知设置" }));
    expect(await screen.findByText("设置保存未完成，登录启动状态可能已更改；请重新打开设置确认")).toBeTruthy();
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
    fireEvent.click(screen.getByRole("button", { name: "保存远程通知设置" }));

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
    fireEvent.click(await screen.findByRole("button", { name: "保存远程通知设置" }));

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
    fireEvent.click(screen.getByRole("button", { name: "保存远程通知设置" }));

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
    fireEvent.click(screen.getByRole("button", { name: "保存远程通知设置" }));

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
    fireEvent.click(await screen.findByRole("button", { name: "保存远程通知设置" }));
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
    fireEvent.click(screen.getByRole("button", { name: "保存远程通知设置" }));

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
    expect(await screen.findByText("测试消息已提交，请检查接收设备")).toBeTruthy();
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

    expect(screen.queryByText("测试消息已提交，请检查接收设备")).toBeNull();
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

  it("keeps the ntfy save action inside the remote notification section", async () => {
    location.hash = "settings";
    render(<App />);
    const save = await screen.findByRole("button", { name: "保存远程通知设置" });
    const section = save.closest(".settings-group");
    expect(section?.querySelector("h2")?.textContent).toBe("远程通知");
    expect(save.closest(".settings-save-dock")).toBeNull();
  });

  it("validates and saves current ntfy fields when enabling immediately", async () => {
    vi.mocked(api.settings).mockResolvedValueOnce({
      ntfyEnabled: false, ntfyServer: "https://ntfy.example.com", ntfyTopic: "saved-topic",
      ntfyUsername: "saved-user", ntfyPasswordSet: false, autostart: false,
    });
    location.hash = "settings";
    render(<App />);
    fireEvent.change(await screen.findByRole("textbox", { name: "用户名" }), {
      target: { value: "unsaved-user" },
    });
    fireEvent.click(screen.getByRole("checkbox", { name: "启用 ntfy" }));

    await waitFor(() => expect(api.saveSettings).toHaveBeenCalledWith(expect.objectContaining({
      ntfyEnabled: true,
      ntfyTopic: "saved-topic",
      ntfyUsername: "unsaved-user",
    })));
    expect(await screen.findByText("远程通知已启用")).toBeTruthy();
    expect((screen.getByRole("textbox", { name: "用户名" }) as HTMLInputElement).value).toBe("unsaved-user");
    expect(screen.queryByText("有未保存的更改")).toBeNull();
  });

  it("keeps edits made while ntfy enable is being saved dirty", async () => {
    const initial = {
      ntfyEnabled: false, ntfyServer: "https://ntfy.example.com", ntfyTopic: "saved-topic",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    };
    let finishEnable!: () => void;
    vi.mocked(api.settings).mockResolvedValueOnce(initial);
    vi.mocked(api.saveSettings).mockImplementationOnce((settings) => new Promise((resolve) => {
      finishEnable = () => resolve(savedSettings(settings));
    }));
    location.hash = "settings";
    render(<App />);
    const topic = await screen.findByRole("textbox", { name: "通知主题（Topic）" });
    fireEvent.click(screen.getByRole("checkbox", { name: "启用 ntfy" }));
    fireEvent.change(topic, { target: { value: "new-local-edit" } });

    await act(async () => finishEnable());

    expect(await screen.findByText("远程通知已启用")).toBeTruthy();
    expect((topic as HTMLInputElement).value).toBe("new-local-edit");
    expect(screen.getByText("有未保存的更改")).toBeTruthy();
    expect((screen.getByRole("button", { name: "保存远程通知设置" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("requires a saved complete ntfy configuration before immediate enable", async () => {
    location.hash = "settings";
    render(<App />);
    const toggle = await screen.findByRole("checkbox", { name: "启用 ntfy" }) as HTMLInputElement;
    fireEvent.click(toggle);

    expect(await screen.findByText("请先补全正确的 ntfy 配置")).toBeTruthy();
    expect(api.saveSettings).not.toHaveBeenCalled();
    expect(toggle.checked).toBe(false);
  });

  it("saves autostart immediately without including unsaved ntfy edits", async () => {
    location.hash = "settings";
    render(<App />);
    fireEvent.change(await screen.findByRole("textbox", { name: "通知主题（Topic）" }), {
      target: { value: "unsaved-topic" },
    });
    fireEvent.click(screen.getByRole("checkbox", { name: "登录时启动 CC Monitor" }));

    await waitFor(() => expect(api.saveSettings).toHaveBeenCalledWith(expect.objectContaining({
      autostart: true,
      ntfyTopic: "",
    })));
    expect(await screen.findByText("登录启动设置已保存")).toBeTruthy();
    expect((screen.getByRole("textbox", { name: "通知主题（Topic）" }) as HTMLInputElement).value).toBe("unsaved-topic");
    expect(screen.getByText("有未保存的更改")).toBeTruthy();
  });

  it("keeps ntfy edits made while autostart is being saved dirty", async () => {
    let finishAutostart!: () => void;
    vi.mocked(api.saveSettings).mockImplementationOnce((settings) => new Promise((resolve) => {
      finishAutostart = () => resolve(savedSettings(settings));
    }));
    location.hash = "settings";
    render(<App />);

    fireEvent.click(await screen.findByRole("checkbox", { name: "登录时启动 CC Monitor" }));
    const topic = screen.getByRole("textbox", { name: "通知主题（Topic）" });
    fireEvent.change(topic, { target: { value: "new-local-edit" } });
    await act(async () => finishAutostart());

    expect(await screen.findByText("登录启动设置已保存")).toBeTruthy();
    expect((topic as HTMLInputElement).value).toBe("new-local-edit");
    expect(screen.getByText("有未保存的更改")).toBeTruthy();
    expect((screen.getByRole("button", { name: "保存远程通知设置" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("restores the autostart toggle and shows a safe error when immediate save fails", async () => {
    vi.mocked(api.saveSettings).mockRejectedValueOnce("settings_inconsistent");
    location.hash = "settings";
    render(<App />);
    const toggle = await screen.findByRole("checkbox", { name: "登录时启动 CC Monitor" }) as HTMLInputElement;
    fireEvent.click(toggle);

    expect(await screen.findByText("设置保存未完成，登录启动状态可能已更改；请重新打开设置确认")).toBeTruthy();
    expect(toggle.checked).toBe(false);
  });

  it("uses the sidebar collector status as a shortcut to its single-column settings section", async () => {
    appTestState.snapshot = {
      ...appTestState.snapshot,
      hook: { status: "installed", issueCode: null, version: "1" },
    };
    render(<App />);
    const collector = await screen.findByRole("button", { name: "事件采集器已安装，打开设置" });
    fireEvent.click(collector);
    expect(await screen.findByRole("heading", { name: "设置", level: 1 })).toBeTruthy();
    expect(await screen.findByRole("region", { name: "事件采集器管理" })).toBeTruthy();
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
    expect(screen.getByRole("heading", { name: "设置", level: 1 })).toBeTruthy();
    expect(alert.textContent).toContain("暂时无法读取设置，请重试");
    expect(alert.textContent).not.toContain("read failed");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByRole("textbox", { name: "通知主题（Topic）" })).toBeTruthy();
    expect(api.settings).toHaveBeenCalledTimes(2);
  });

  it("focuses the Settings heading during loading and retains focus when settings are ready", async () => {
    let resolveSettings!: (settings: Awaited<ReturnType<typeof api.settings>>) => void;
    vi.mocked(api.settings).mockReturnValueOnce(new Promise((resolve) => { resolveSettings = resolve; }));
    render(<App />);
    await screen.findByText("project-a");
    fireEvent.click(screen.getByRole("button", { name: "设置" }));
    await screen.findByText("正在读取设置…");
    const loadingHeading = screen.getByRole("heading", { name: "设置", level: 1 });
    await waitFor(() => expect(document.activeElement).toBe(loadingHeading));

    await act(async () => resolveSettings({
      ntfyEnabled: false, ntfyServer: "https://ntfy.sh", ntfyTopic: "",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    }));
    await screen.findByRole("region", { name: "事件采集器管理" });
    const readyHeading = screen.getByRole("heading", { name: "设置", level: 1 });
    expect(document.activeElement).toBe(readyHeading);
  });

  it("does not steal focus when the user moves it while Settings is loading", async () => {
    let resolveSettings!: (settings: Awaited<ReturnType<typeof api.settings>>) => void;
    vi.mocked(api.settings).mockReturnValueOnce(new Promise((resolve) => { resolveSettings = resolve; }));
    render(<App />);
    await screen.findByText("project-a");
    fireEvent.click(screen.getByRole("button", { name: "设置" }));
    await screen.findByText("正在读取设置…");
    const loadingHeading = screen.getByRole("heading", { name: "设置", level: 1 });
    await waitFor(() => expect(document.activeElement).toBe(loadingHeading));
    const diagnosticsNav = screen.getByRole("button", { name: "诊断" });
    diagnosticsNav.focus();

    await act(async () => resolveSettings({
      ntfyEnabled: false, ntfyServer: "https://ntfy.sh", ntfyTopic: "",
      ntfyUsername: "", ntfyPasswordSet: false, autostart: false,
    }));
    await screen.findByRole("heading", { name: "设置", level: 1 });
    expect(document.activeElement).toBe(diagnosticsNav);
  });

  it("persists a Settings-only no-reminder choice and hides both read-only callouts", async () => {
    let finishDefer!: () => void;
    vi.mocked(api.deferHookOnboarding).mockImplementationOnce(() => new Promise((resolve) => {
      finishDefer = () => {
        appTestState.snapshot = {
          ...appTestState.snapshot,
          revision: appTestState.snapshot.revision + 1,
          hookOnboardingDisposition: "deferred",
        };
        resolve();
      };
    }));
    location.hash = "settings";
    render(<App />);

    fireEvent.click(await screen.findByRole("button", { name: "暂不提醒" }));
    expect(api.deferHookOnboarding).toHaveBeenCalledOnce();
    expect((await screen.findByRole("button", { name: "正在保存…" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "安装事件采集器" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "安装事件采集器" }));
    expect(api.installHook).not.toHaveBeenCalled();

    await act(async () => finishDefer());
    expect(await screen.findByText("已设置暂不提醒；仍可随时在设置中安装事件采集器")).toBeTruthy();
    expect(api.snapshot).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("button", { name: "暂不提醒" })).toBeNull();
    expect((screen.getByRole("button", { name: "安装事件采集器" }) as HTMLButtonElement).disabled).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "总览" }));
    await screen.findByRole("heading", { name: "运行总览" });
    expect(screen.queryByRole("heading", { name: "事件采集器未安装" })).toBeNull();
    expect(screen.queryByRole("button", { name: "打开设置" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "诊断" }));
    await screen.findByRole("heading", { name: "诊断", level: 1 });
    expect(screen.queryByRole("heading", { name: "事件采集器未安装" })).toBeNull();
    expect(screen.queryByRole("button", { name: "打开设置" })).toBeNull();
  });

  it("installs an absent Hook and refreshes the snapshot", async () => {
    appTestState.snapshot = { ...appTestState.snapshot, hook: { status: "absent", issueCode: null, version: null } };
    location.hash = "settings";
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "安装事件采集器" }));
    expect(await screen.findByText("事件采集器已安装，实时监控已启用")).toBeTruthy();
    expect(api.installHook).toHaveBeenCalledOnce();
    expect(api.snapshot).toHaveBeenCalledTimes(2);
  });

  it("shows an incomplete Hook as repair-required and offers a repair action", async () => {
    appTestState.snapshot = {
      ...appTestState.snapshot,
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
    appTestState.snapshot = { ...appTestState.snapshot, hook: { status: "installed", issueCode: null, version: "1" } };
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
    appTestState.snapshot = { ...appTestState.snapshot, hook: { status: "installed", issueCode: null, version: "1" } };
    vi.mocked(api.uninstallHook).mockImplementationOnce(async () => {
      appTestState.snapshot = {
        ...appTestState.snapshot,
        revision: appTestState.snapshot.revision + 1,
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
    expect(screen.queryByRole("heading", { name: "事件采集器未安装" })).toBeNull();
    expect(screen.queryByRole("button", { name: "打开设置" })).toBeNull();
    expect(screen.getByRole("button", { name: "事件采集器未安装，打开设置" })).toBeTruthy();
  });

  it("serializes Hook repair and uninstall and reports successful removal", async () => {
    appTestState.snapshot = { ...appTestState.snapshot, hook: { status: "installed", issueCode: null, version: "1" } };
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
});
