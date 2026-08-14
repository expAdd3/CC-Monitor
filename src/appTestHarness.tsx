import { act, cleanup } from "@testing-library/react";
import { listen } from "@tauri-apps/api/event";
import { afterEach, beforeEach, vi } from "vitest";
import { api, demoSnapshot, type ModelPrice, type SettingsDto } from "./api";

const defaultSettings = (): SettingsDto => ({
  ntfyEnabled: false,
  ntfyServer: "https://ntfy.sh",
  ntfyTopic: "",
  ntfyUsername: "",
  ntfyPasswordSet: false,
  autostart: false,
});

export const savedSettings = (settings: SettingsDto): SettingsDto => ({
  ...settings,
  ntfyPassword: undefined,
  ntfyPasswordSet: Boolean(settings.ntfyPassword || settings.ntfyPasswordSet),
});

const initialSnapshot = () => ({
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
});

export const appTestState = {
  snapshot: initialSnapshot(),
  rejectedListener: null as string | null,
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
  unlistenInvalidated: vi.fn(),
  unlistenNavigate: vi.fn(),
};

export async function invalidateSnapshot(revision: number) {
  appTestState.snapshot = { ...appTestState.snapshot, revision };
  await act(async () => {
    appTestState.listeners.get("monitor://invalidated")?.({ payload: { revision } });
  });
}

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    if (name === appTestState.rejectedListener) throw new Error("listener unavailable");
    appTestState.listeners.set(name, handler);
    return name === "monitor://invalidated"
      ? appTestState.unlistenInvalidated
      : appTestState.unlistenNavigate;
  }),
}));

vi.mock("./api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./api")>();
  return {
    ...actual,
    api: {
      ...actual.api,
      snapshot: vi.fn(async () => appTestState.snapshot),
      session: vi.fn(async () => { throw new Error("missing session"); }),
      reindex: vi.fn(async () => ({ runId: "run-a" })),
      clearHistory: vi.fn(async () => ({ rawEventsDeleted: 0, notificationsDeleted: 0 })),
      openNotificationSettings: vi.fn(async () => undefined),
      testDesktopNotification: vi.fn(async () => undefined),
      settings: vi.fn(async () => defaultSettings()),
      saveSettings: vi.fn(async (settings: SettingsDto) => savedSettings(settings)),
      modelPrices: vi.fn(async () => []),
      saveModelPrice: vi.fn(async (price: ModelPrice) => price),
      deleteModelPrice: vi.fn(async (modelId: string) => modelId),
      testNtfy: vi.fn(async () => undefined),
      installHook: vi.fn(async () => undefined),
      uninstallHook: vi.fn(async () => true),
      deferHookOnboarding: vi.fn(async () => undefined),
    },
  };
});

beforeEach(() => {
  vi.mocked(api.snapshot).mockClear();
  vi.mocked(api.session).mockReset().mockRejectedValue(new Error("missing session"));
  vi.mocked(api.reindex).mockReset().mockResolvedValue({ runId: "run-a" });
  vi.mocked(api.clearHistory).mockReset().mockResolvedValue({ rawEventsDeleted: 0, notificationsDeleted: 0 });
  vi.mocked(api.openNotificationSettings).mockReset().mockResolvedValue(undefined);
  vi.mocked(api.testDesktopNotification).mockReset().mockResolvedValue(undefined);
  vi.mocked(api.settings).mockReset().mockResolvedValue(defaultSettings());
  vi.mocked(api.saveSettings).mockReset().mockImplementation(async (settings) => savedSettings(settings));
  vi.mocked(api.modelPrices).mockReset().mockResolvedValue([]);
  vi.mocked(api.saveModelPrice).mockReset().mockImplementation(async (price) => price);
  vi.mocked(api.deleteModelPrice).mockReset().mockImplementation(async (modelId) => modelId);
  vi.mocked(api.testNtfy).mockReset().mockResolvedValue(undefined);
  vi.mocked(api.installHook).mockReset().mockResolvedValue(undefined);
  vi.mocked(api.uninstallHook).mockReset().mockResolvedValue(true);
  vi.mocked(api.deferHookOnboarding).mockReset().mockResolvedValue(undefined);
  Object.assign(navigator, {
    clipboard: { writeText: vi.fn(async () => undefined) },
  });
  appTestState.snapshot = {
    ...initialSnapshot(),
    sessionsHasMore: false,
    hook: { status: "absent", issueCode: null, version: null },
    hookOnboardingDisposition: null,
    trends: [],
    diagnostics: demoSnapshot().diagnostics,
    index: {
      runId: null,
      state: "idle",
      completed: 0,
      total: 0,
      failedFiles: 0,
      quarantinedSessions: 0,
      interrupted: false,
    },
  };
  location.hash = "";
  appTestState.listeners.clear();
  appTestState.rejectedListener = null;
  vi.mocked(listen).mockClear();
  appTestState.unlistenInvalidated.mockClear();
  appTestState.unlistenNavigate.mockClear();
});

afterEach(cleanup);
