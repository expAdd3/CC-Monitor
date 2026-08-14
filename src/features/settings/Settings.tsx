import { type FormEvent, useCallback, useEffect, useRef, useState } from "react";
import { api, type DashboardSnapshot, type SettingsDto } from "../../api";
import { hookIssueLabel } from "../collector/CollectorStatusCallout";
import { ModelPrices } from "../pricing/ModelPrices";
import { useAsyncAction, useAsyncResource, type ActionState } from "../../lib/asyncState";
import { Loading, PageTitle } from "../../ui";

function settingsSaveError(reason: unknown): string {
  const code = typeof reason === "string"
    ? reason
    : reason instanceof Error ? reason.message : "";
  if (code === "settings_autostart_failed") return "无法更新登录启动设置，其他设置未保存";
  if (code === "settings_inconsistent") return "设置保存未完成，登录启动状态可能已更改；请重新打开设置确认";
  if (code === "ntfy_server_https_required") return "非本机 ntfy 服务器必须使用 HTTPS";
  if (code === "ntfy_credentials_https_required") return "使用用户名或密码时必须使用 HTTPS";
  return "设置保存失败，请重试";
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
  return messages[code] || "事件采集器操作失败，请重试";
}

function useHookActions(onChanged: () => Promise<void>) {
  const install = useAsyncAction(api.installHook, hookMutationError);
  const repair = useAsyncAction(api.installHook, hookMutationError);
  const uninstall = useAsyncAction(api.uninstallHook, hookMutationError);
  const defer = useAsyncAction(api.deferHookOnboarding, "事件采集器操作失败，请重试");
  const reset = useCallback(() => {
    install.reset();
    repair.reset();
    uninstall.reset();
    defer.reset();
  }, [defer.reset, install.reset, repair.reset, uninstall.reset]);
  const run = useCallback(async (action: "install" | "repair" | "uninstall" | "defer") => {
    const selected = { install, repair, uninstall, defer }[action];
    try {
      const value = await selected.run();
      await onChanged();
      return value;
    } catch (reason) {
      // Hook settings, the staged binary and SQLite cannot be one transaction.
      // Refresh before surfacing the fixed error so retry starts from truth.
      await onChanged().catch(() => undefined);
      throw reason;
    }
  }, [defer, install, onChanged, repair, uninstall]);
  const busy = [install.state, repair.state, uninstall.state, defer.state]
    .some((state) => state.status === "pending");
  return { install, repair, uninstall, defer, run, reset, busy };
}

function Feedback({ state }: { state: ActionState<string> }) {
  if (state.status === "idle") return null;
  let text: string;
  if (state.status === "pending") text = "正在保存设置…";
  else if (state.status === "success") text = state.value;
  else if (state.status === "error") text = state.error;
  else return null;
  return <div
    className={`operation-banner ${state.status === "error" ? "failed" : ""}`}
    role={state.status === "error" ? "alert" : "status"}
    aria-live={state.status === "error" ? undefined : "polite"}
    aria-atomic="true"
  >{text}</div>;
}

function Toggle({ label, checked, disabled = false, onChange }: { label: string; checked: boolean; disabled?: boolean; onChange: (value: boolean) => void }) {
  return <label className="toggle-row"><span>{label}</span><input type="checkbox" checked={checked} disabled={disabled} onChange={(event) => onChange(event.target.checked)} /><i /></label>;
}

function Field({ id, label, value, error, disabled = false, onChange, placeholder = "", type = "text" }: { id?: string; label: string; value: string; error?: string; disabled?: boolean; onChange: (value: string) => void; placeholder?: string; type?: string }) {
  const errorId = error && id ? `${id}-error` : undefined;
  return <label className="field"><span>{label}</span><input id={id} aria-label={label} type={type} value={value} disabled={disabled} placeholder={placeholder} aria-invalid={error ? "true" : undefined} aria-describedby={errorId} onChange={(event) => onChange(event.target.value)} />{error && <small id={errorId} className="field-error" role="alert">{error}</small>}</label>;
}

export function Settings({ hook, onboardingDisposition, onSaved }: {
  hook: DashboardSnapshot["hook"];
  onboardingDisposition: DashboardSnapshot["hookOnboardingDisposition"];
  onSaved: () => Promise<void>;
}) {
  const loadSettings = useCallback(() => api.settings(), []);
  const settingsResource = useAsyncResource(loadSettings, "暂时无法读取设置，请重试");
  const [settings, setSettings] = useState<SettingsDto | null>(null);
  const [saveState, setSaveState] = useState<ActionState<string>>({ status: "idle" });
  const [ntfyEnabledState, setNtfyEnabledState] = useState<ActionState<string>>({ status: "idle" });
  const [autostartState, setAutostartState] = useState<ActionState<string>>({ status: "idle" });
  const [ntfyTestState, setNtfyTestState] = useState<ActionState<string>>({ status: "idle" });
  const [dirty, setDirty] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<{ ntfyServer?: string; ntfyTopic?: string }>({});
  const [hookConfirmation, setHookConfirmation] = useState(false);
  const editGeneration = useRef(0);
  const persistedSettings = useRef<SettingsDto | null>(null);
  const ntfyTestGeneration = useRef(0);
  const hookActions = useHookActions(onSaved);

  useEffect(() => {
    if (settingsResource.state.status === "success") {
      persistedSettings.current = settingsResource.state.value;
      setSettings(settingsResource.state.value);
    }
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
    setSaveState({ status: "idle" });
    setNtfyEnabledState({ status: "idle" });
    setNtfyTestState({ status: "idle" });
  };
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!settings || !dirty || saveState.status === "pending" || ntfyEnabledState.status === "pending" || autostartState.status === "pending") return;
    const validation = validateSettings(settings);
    if (validation.ntfyServer || validation.ntfyTopic) {
      setFieldErrors(validation);
      setSaveState({ status: "idle" });
      return;
    }
    setFieldErrors({});
    setSaveState({ status: "pending" });
    const saveGeneration = editGeneration.current;
    try {
      const canonical = await api.saveSettings(settings);
      persistedSettings.current = canonical;
      if (editGeneration.current === saveGeneration) {
        setSettings(canonical);
        setDirty(false);
      }
    } catch (reason) {
      setSaveState({ status: "error", error: settingsSaveError(reason) });
      return;
    }
    setSaveState({ status: "success", value: "远程通知设置已保存" });
    void onSaved().catch(() => undefined);
  };
  const saveNtfyEnabled = async (value: boolean) => {
    if (!settings || saveState.status === "pending" || ntfyEnabledState.status === "pending" || autostartState.status === "pending") return;
    const saveGeneration = editGeneration.current;
    const previous = settings.ntfyEnabled;
    const next = value
      ? { ...settings, ntfyEnabled: true }
      : { ...(persistedSettings.current || settings), ntfyEnabled: false };
    if (value) {
      const validation = validateNtfy(next);
      if (validation.ntfyServer || validation.ntfyTopic) {
        setFieldErrors(validation);
        setNtfyEnabledState({ status: "error", error: "请先补全正确的 ntfy 配置" });
        return;
      }
    }
    setSettings((current) => current ? { ...current, ntfyEnabled: value } : current);
    setNtfyEnabledState({ status: "pending" });
    try {
      const canonical = await api.saveSettings(next);
      persistedSettings.current = canonical;
      if (value && editGeneration.current === saveGeneration) {
        setSettings(canonical);
        setDirty(false);
        setFieldErrors({});
        setSaveState({ status: "idle" });
      } else if (!dirty && editGeneration.current === saveGeneration) {
        setSettings(canonical);
      }
      setNtfyEnabledState({ status: "success", value: value ? "远程通知已启用" : "远程通知已停用" });
      void onSaved().catch(() => undefined);
    } catch (reason) {
      setSettings((current) => current ? { ...current, ntfyEnabled: previous } : current);
      setNtfyEnabledState({ status: "error", error: settingsSaveError(reason) });
    }
  };
  const saveAutostart = async (value: boolean) => {
    if (!settings || saveState.status === "pending" || ntfyEnabledState.status === "pending" || autostartState.status === "pending") return;
    const saveGeneration = editGeneration.current;
    const previous = settings.autostart;
    const next = { ...(persistedSettings.current || settings), autostart: value };
    setSettings((current) => current ? { ...current, autostart: value } : current);
    setAutostartState({ status: "pending" });
    try {
      const canonical = await api.saveSettings(next);
      persistedSettings.current = canonical;
      if (!dirty && editGeneration.current === saveGeneration) setSettings(canonical);
      setAutostartState({ status: "success", value: "登录启动设置已保存" });
      void onSaved().catch(() => undefined);
    } catch (reason) {
      setSettings((current) => current ? { ...current, autostart: previous } : current);
      setAutostartState({ status: "error", error: settingsSaveError(reason) });
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
  const deferHookReminder = async () => {
    try {
      await hookActions.run("defer");
    } catch {
      // The action state provides the accessible error feedback.
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
        setNtfyTestState({ status: "success", value: "测试消息已提交，请检查接收设备" });
      }
    } catch (reason) {
      if (ntfyTestGeneration.current === testGeneration) {
        setNtfyTestState({ status: "error", error: ntfyTestError(reason) });
      }
    }
  };

  if (!settings && settingsResource.state.status === "error") return <><PageTitle title="设置" subtitle="通知、启动与事件采集器" /><div className="error-banner" role="alert">设置加载失败：{settingsResource.state.error} <button type="button" className="secondary" onClick={settingsResource.retry}>重试</button></div></>;
  if (!settings) return <><PageTitle title="设置" subtitle="通知、启动与事件采集器" /><Loading label="正在读取设置…" /></>;

  const saving = saveState.status === "pending";
  const ntfyEnabledSaving = ntfyEnabledState.status === "pending";
  const autostartSaving = autostartState.status === "pending";
  const hookBusy = hookActions.busy;
  const hookMessage = hookActions.install.state.status === "success"
    ? "事件采集器已安装，实时监控已启用"
    : hookActions.uninstall.state.status === "success"
      ? hookActions.uninstall.state.value ? "事件采集器已卸载" : "未找到可卸载的事件采集器"
      : hookActions.install.state.status === "error" ? hookActions.install.state.error
      : hookActions.uninstall.state.status === "error" ? hookActions.uninstall.state.error : null;
  const repairMessage = hookActions.repair.state.status === "success" ? "事件采集器已修复"
    : hookActions.repair.state.status === "error" ? hookActions.repair.state.error : null;
  const deferMessage = hookActions.defer.state.status === "success"
    ? "已设置暂不提醒；仍可随时在设置中安装事件采集器"
    : hookActions.defer.state.status === "error" ? hookActions.defer.state.error : null;
  const hookDescription = hook.status === "installed"
    ? "安装完整且配置有效；可重新安装进行修复，或卸载 CC Monitor 自己的采集项。"
    : hook.status === "repair_required"
      ? `检测到不完整的安装（${hookIssueLabel(hook.issueCode)}），请修复后再继续使用事件采集。`
      : "尚未安装；安装程序只修改属于 CC Monitor 的采集项。";

  return <>
    <PageTitle title="设置" subtitle="通知、启动与事件采集器" />
    <div className="settings-layout">
      <div className="settings-main">
        <section className="settings-side" aria-label="事件采集器管理">
          <div className="hook-actions">
            <div><h2>事件采集器</h2><p>{hookDescription}</p></div>
            {!hookConfirmation ? <div>
              {hook.status === "absent" && <>
                <button type="button" className="primary" disabled={hookBusy} aria-busy={hookActions.install.state.status === "pending"} onClick={mutateHook}>{hookActions.install.state.status === "pending" ? "正在安装…" : "安装事件采集器"}</button>
                {onboardingDisposition === null && <button type="button" className="secondary" disabled={hookBusy} aria-busy={hookActions.defer.state.status === "pending"} onClick={deferHookReminder}>{hookActions.defer.state.status === "pending" ? "正在保存…" : "暂不提醒"}</button>}
              </>}
              {hook.status === "repair_required" && <button type="button" className="primary" disabled={hookBusy} aria-busy={hookActions.repair.state.status === "pending"} onClick={async () => {
                try { await hookActions.run("repair"); } catch { /* feedback is rendered below */ }
              }}>{hookActions.repair.state.status === "pending" ? "正在修复…" : "立即修复"}</button>}
              {hook.status === "installed" && <>
                <button type="button" className="secondary" disabled={hookBusy} aria-busy={hookActions.repair.state.status === "pending"} onClick={async () => {
                  try { await hookActions.run("repair"); } catch { /* feedback is rendered below */ }
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
          {deferMessage && <div className={`operation-banner ${hookActions.defer.state.status === "error" ? "failed" : ""}`} role={hookActions.defer.state.status === "error" ? "alert" : "status"} aria-live={hookActions.defer.state.status === "error" ? undefined : "polite"}>{deferMessage}</div>}
        </section>
        <form className="settings-ntfy-form" onSubmit={submit}>
          <section className="settings-group">
            <div className="ntfy-heading"><div><h2>远程通知</h2><p>非本机服务器必须使用 HTTPS；HTTP 仅用于无凭据的 localhost、127.0.0.1 或 ::1 测试服务。</p></div>
              <Toggle label="启用 ntfy" checked={settings.ntfyEnabled} disabled={saving || ntfyEnabledSaving || autostartSaving} onChange={(value) => void saveNtfyEnabled(value)} />
            </div>
            <Feedback state={ntfyEnabledState} />
            <Field id="ntfy-server" label="服务器地址" value={settings.ntfyServer} error={fieldErrors.ntfyServer} disabled={saving} placeholder="https://ntfy.sh" onChange={(value) => update({ ntfyServer: value })} />
            <Field id="ntfy-topic" label="通知主题（Topic）" value={settings.ntfyTopic} error={fieldErrors.ntfyTopic} disabled={saving} onChange={(value) => update({ ntfyTopic: value })} />
            <Field label="用户名" value={settings.ntfyUsername} disabled={saving} onChange={(value) => update({ ntfyUsername: value })} />
            <Field label="密码" type="password" value={settings.ntfyPassword || ""} placeholder={settings.ntfyPasswordSet ? "••••••••（留空保持不变）" : "可选"} disabled={saving} onChange={(value) => update({ ntfyPassword: value || undefined })} />
            <div className="ntfy-actions-row">
              <div className="ntfy-action-feedback">
                {ntfyTestState.status !== "idle" && <span className={`action-message ${ntfyTestState.status === "error" ? "failed" : ""}`} role={ntfyTestState.status === "error" ? "alert" : "status"} aria-live={ntfyTestState.status === "error" ? undefined : "polite"}>
                  {ntfyTestState.status === "pending" ? "正在连接 ntfy 服务…" : ntfyTestState.status === "success" ? ntfyTestState.value : ntfyTestState.status === "error" ? ntfyTestState.error : null}
                </span>}
                <Feedback state={saveState} />
                {dirty && saveState.status !== "pending" && <span className="unsaved-label">有未保存的更改</span>}
              </div>
              <div className="ntfy-action-buttons">
                <button type="button" className="secondary" disabled={saving || ntfyTestState.status === "pending"} aria-busy={ntfyTestState.status === "pending"} onClick={testNtfy}>{ntfyTestState.status === "pending" ? "正在发送…" : "发送测试消息"}</button>
                <button className="primary" disabled={!dirty || saving || ntfyEnabledSaving || autostartSaving} aria-busy={saving}>{saving ? "正在保存…" : "保存远程通知设置"}</button>
              </div>
            </div>
          </section>
        </form>
        <section className="settings-group">
          <div><h2>系统</h2><p>关闭窗口后菜单栏和监控引擎仍继续运行。</p></div>
          <Toggle label="登录时启动 CC Monitor" checked={settings.autostart} disabled={saving || ntfyEnabledSaving || autostartSaving} onChange={(value) => void saveAutostart(value)} />
          <Feedback state={autostartState} />
        </section>
        <ModelPrices />
      </div>
    </div>
  </>;
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
        else if (settings.ntfyUsername.length > 0 || Boolean(settings.ntfyPassword) || settings.ntfyPasswordSet) {
          errors.ntfyServer = "使用用户名或密码时必须使用 HTTPS";
        }
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
