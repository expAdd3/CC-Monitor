import type { DashboardSnapshot } from "../../api";

type Hook = DashboardSnapshot["hook"];

export function hookStatusLabel(status: Hook["status"]) {
  return ({ installed: "已安装", repair_required: "需修复", absent: "未安装" } as const)[status];
}

export function hookIssueLabel(issue: string | null) {
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

export function hookVersion(snapshot: Pick<DashboardSnapshot, "hook">) {
  if (snapshot.hook.status === "repair_required") return "需要修复";
  if (snapshot.hook.status === "absent") return "未安装";
  return snapshot.hook.version ? `版本 ${snapshot.hook.version}` : "已安装（版本未知）";
}

export function CollectorStatusCallout({
  hook,
  onboardingDisposition,
  location,
  onOpenSettings,
}: {
  hook: Hook;
  onboardingDisposition: DashboardSnapshot["hookOnboardingDisposition"];
  location: "dashboard" | "diagnostics";
  onOpenSettings: () => void;
}) {
  if (hook.status === "installed") return null;
  if (hook.status === "absent" && onboardingDisposition !== null) return null;

  const repairingRequired = hook.status === "repair_required";
  const title = repairingRequired ? "事件采集器需要修复" : "事件采集器未安装";
  const body = repairingRequired
    ? `当前配置不完整：${hookIssueLabel(hook.issueCode)}。修复前，部分会话状态可能无法及时更新。`
    : "安装事件采集器后，CC Monitor 才能及时识别会话开始、完成和需要介入。它只管理由 CC Monitor 添加的 Claude Code 配置。";
  return <section
    className={`hook-callout ${repairingRequired ? "warning" : ""} ${location === "diagnostics" ? "compact" : ""}`}
    aria-labelledby={`hook-callout-title-${location}`}
    aria-describedby={`hook-callout-body-${location}`}
  >
    <span className="hook-callout-mark" aria-hidden="true">{repairingRequired ? "!" : "●"}</span>
    <div className="hook-callout-copy">
      <h2 id={`hook-callout-title-${location}`}>{title}</h2>
      <p id={`hook-callout-body-${location}`}>{body}</p>
    </div>
    <div className="hook-callout-actions">
      <button type="button" className="secondary" onClick={onOpenSettings}>打开设置</button>
    </div>
  </section>;
}
