import { invoke } from "@tauri-apps/api/core";

export type Decimal = string | number;

export type UsageSummary = {
  inputTokens: Decimal;
  outputTokens: Decimal;
  cacheWriteTokens: Decimal;
  cacheReadTokens: Decimal;
  costPicoUsd: Decimal;
  costKnown: boolean;
  unpricedTokens?: Decimal;
};

export type SessionRow = {
  sessionId: string;
  projectName: string | null;
  turnState: "running" | "waiting" | "needs_input" | "failed";
  stateSource: "hook" | "transcript" | "recovery";
  stateReason: string;
  changedAtMs: number;
  lastObservedAtMs: number;
  revision: number;
  usage: UsageSummary;
};

export type DashboardSnapshot = {
  revision: number;
  counts: { running: number; waiting: number; needsInput: number; failed: number };
  activeSessionCount: number;
  sessionsHasMore: boolean;
  today: UsageSummary;
  trends: { day: string; tokens: Decimal; costPicoUsd: Decimal; costKnown: boolean; unpricedTokens?: Decimal }[];
  sessions: SessionRow[];
  hook: {
    status: "installed" | "repair_required" | "absent";
    issueCode: string | null;
    version: string | null;
  };
  hookOnboardingDisposition: "deferred" | "deliberately_uninstalled" | null;
  index: IndexProgress;
  diagnostics: {
    migrationVersion: number;
    pendingEvents: number;
    quarantinedSessions: number;
    pendingNotifications: number;
    desktopFailures: number;
    desktopErrorCode: string | null;
    ntfyFailures: number;
    ntfyErrorCode: string | null;
    ntfyRecoveredAtMs: number | null;
    backgroundHealth: {
      task: string;
      successCount: number;
      failureCount: number;
      consecutiveFailures: number;
      errorCode: string | null;
      lastSucceededAtMs: number | null;
      lastFailedAtMs: number | null;
      recoveredAtMs: number | null;
    }[];
  };
};
export type IndexProgress = {
  runId: string | null;
  state: "idle" | "running" | "complete" | "failed";
  completed: number;
  total: number;
  failedFiles: number;
  quarantinedSessions: number;
  interrupted: boolean;
};
export type CleanupResult = { rawEventsDeleted: number; notificationsDeleted: number };
export type ReindexStarted = { runId: string };
export type ModelPrice = {
  modelId: string;
  inputCostPerMillion: string;
  outputCostPerMillion: string;
  cacheWriteCostPerMillion: string;
  cacheReadCostPerMillion: string;
};
export type SaveModelPrice = ModelPrice;

export type SessionDetail = {
  session: SessionRow;
  models: {
    modelId: string;
    inputTokens: Decimal;
    outputTokens: Decimal;
    cacheWriteTokens: Decimal;
    cacheReadTokens: Decimal;
    tokens: Decimal;
    costPicoUsd: Decimal;
    costKnown: boolean;
    unpricedTokens?: Decimal;
  }[];
  events: { sourceEvent: string; source: string; occurredAtMs: number }[];
};

export type SettingsDto = {
  ntfyEnabled: boolean;
  ntfyServer: string;
  ntfyTopic: string;
  ntfyUsername: string;
  ntfyPassword?: string;
  ntfyPasswordSet: boolean;
  autostart: boolean;
};

export const emptyUsage = (): UsageSummary => ({
  inputTokens: 0,
  outputTokens: 0,
  cacheWriteTokens: 0,
  cacheReadTokens: 0,
  costPicoUsd: 0,
  costKnown: true,
  unpricedTokens: 0,
});

export const demoSnapshot = (): DashboardSnapshot => ({
  revision: 0,
  counts: { running: 0, waiting: 0, needsInput: 0, failed: 0 },
  activeSessionCount: 0,
  sessionsHasMore: false,
  today: emptyUsage(),
  trends: [],
  sessions: [],
  hook: {
    status: "absent",
    issueCode: null,
    version: null,
  },
  hookOnboardingDisposition: null,
  index: {
    runId: null,
    state: "idle",
    completed: 0,
    total: 0,
    failedFiles: 0,
    quarantinedSessions: 0,
    interrupted: false,
  },
  diagnostics: {
    migrationVersion: 0,
    pendingEvents: 0,
    quarantinedSessions: 0,
    pendingNotifications: 0,
    desktopFailures: 0,
    desktopErrorCode: null,
    ntfyFailures: 0,
    ntfyErrorCode: null,
    ntfyRecoveredAtMs: null,
    backgroundHealth: [],
  },
});

export const api = {
  snapshot: () => invoke<DashboardSnapshot>("get_dashboard_snapshot"),
  session: (sessionId: string) =>
    invoke<SessionDetail>("get_session_detail", { sessionId }),
  settings: () => invoke<SettingsDto>("get_settings"),
  saveSettings: (settings: SettingsDto) => invoke<SettingsDto>("save_settings", { settings }),
  modelPrices: () => invoke<ModelPrice[]>("list_model_prices"),
  saveModelPrice: (price: SaveModelPrice) => invoke<ModelPrice>("save_model_price", { price }),
  deleteModelPrice: (modelId: string) => invoke<string>("delete_model_price", { modelId }),
  testNtfy: (settings: SettingsDto) => invoke<void>("test_ntfy", { settings }),
  installHook: () => invoke<void>("install_claude_hook"),
  uninstallHook: () => invoke<boolean>("uninstall_claude_hook"),
  deferHookOnboarding: () => invoke<void>("defer_hook_onboarding"),
  clearHistory: () => invoke<CleanupResult>("clear_completed_history"),
  reindex: () => invoke<ReindexStarted>("reindex_transcripts"),
  openNotificationSettings: () => invoke<void>("open_notification_settings"),
  testDesktopNotification: () => invoke<void>("test_desktop_notification"),
};

export function decimal(value: Decimal): bigint {
  if (typeof value === "number") return Number.isSafeInteger(value) && value >= 0 ? BigInt(value) : 0n;
  return /^\d+$/.test(value) ? BigInt(value) : 0n;
}

export function tokens(value: UsageSummary): bigint {
  return decimal(value.inputTokens) + decimal(value.outputTokens) + decimal(value.cacheWriteTokens) + decimal(value.cacheReadTokens);
}

export function compact(value: Decimal | bigint): string {
  const amount = typeof value === "bigint" ? value : decimal(value);
  return new Intl.NumberFormat("zh-CN", {
    notation: amount >= 1_000n ? "compact" : "standard",
    maximumFractionDigits: 1,
  }).format(amount);
}

export function cost(value: Decimal, known = true): string {
  if (!known) return "费用待定";
  const scaled = (decimal(value) * 10_000n + 500_000_000_000n) / 1_000_000_000_000n;
  return `$${scaled / 10_000n}.${String(scaled % 10_000n).padStart(4, "0")}`;
}
