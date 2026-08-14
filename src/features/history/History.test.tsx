import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import App from "../../App";
import { compact } from "../../api";
import { appTestState, invalidateSnapshot } from "../../appTestHarness";
import { buildRecentHistory } from "./model";

describe("History", () => {
  it("renders one concise annual heatmap summary without exposing 365 screen-reader items", async () => {
    appTestState.snapshot = {
      ...appTestState.snapshot,
      trends: [
        { day: "2026-07-28", tokens: 1234, costPicoUsd: 9_000_000_000, costKnown: false, unpricedTokens: 1234 },
        { day: "2026-06-01", tokens: 10, costPicoUsd: 1_000_000_000, costKnown: true },
      ],
    };
    location.hash = "history";
    render(<App />);
    const grid = await screen.findByRole("img", { name: /过去一年 Token 活跃度/ });
    const cells = grid.querySelectorAll(".heatmap-cell");
    expect(cells).toHaveLength(365);
    expect(grid.getAttribute("aria-label")).toContain("$0.0100 · 1234 未计");
    expect(Array.from(cells).every((cell) => !cell.hasAttribute("tabindex"))).toBe(true);
    expect(Array.from(cells).every((cell) => cell.getAttribute("aria-hidden") === "true")).toBe(true);
    expect(grid.querySelectorAll('[role="listitem"]')).toHaveLength(0);
    expect(Array.from(cells).find((cell) => cell.getAttribute("title")?.includes("2026-07-28，1234 Token，费用待定"))).toBeTruthy();
    expect(screen.getByLabelText("活跃度图例：从少到多")).toBeTruthy();
    const annualSection = grid.closest("section") as HTMLElement;
    expect(within(annualSection).getByText("过去一年 Token")).toBeTruthy();
    expect(within(annualSection).getByText("未计费 Token")).toBeTruthy();
    expect(screen.queryByLabelText("活跃度时间范围")).toBeNull();
    expect(screen.getByText("最近 30 天明细")).toBeTruthy();
  });

  it("renders 30 compact bins with one tab stop, semantic rows, and shared day selection", async () => {
    const now = new Date();
    const used = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 2);
    const half = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 3);
    const usedKey = `${used.getFullYear()}-${String(used.getMonth() + 1).padStart(2, "0")}-${String(used.getDate()).padStart(2, "0")}`;
    const halfKey = `${half.getFullYear()}-${String(half.getMonth() + 1).padStart(2, "0")}-${String(half.getDate()).padStart(2, "0")}`;
    appTestState.snapshot = {
      ...appTestState.snapshot,
      trends: [
        { day: halfKey, tokens: 500, costPicoUsd: 0, costKnown: true, unpricedTokens: 0 },
        { day: usedKey, tokens: 1000, costPicoUsd: 1_000_000_000, costKnown: true, unpricedTokens: 400 },
      ],
    };
    location.hash = "history";
    render(<App />);
    const section = (await screen.findByRole("heading", { name: "最近 30 天明细" })).closest("section") as HTMLElement;
    const chart = within(section).getByRole("group", { name: /最近 30 天每日 Token 用量/ });
    const bars = section.querySelectorAll(".recent-bar");
    const table = within(section).getByRole("table", { name: "最近 30 天每日用量数据" });

    expect(bars).toHaveLength(30);
    expect(section.querySelectorAll(".recent-chart-ticks span")).toHaveLength(5);
    expect(section.querySelectorAll('[tabindex="0"]')).toHaveLength(1);
    expect(table.querySelectorAll("tbody tr")).toHaveLength(30);
    const renderedRecent = buildRecentHistory(appTestState.snapshot.trends, new Date());
    const halfIndex = renderedRecent.days.findIndex((day) => day.day === halfKey);
    const usedIndex = renderedRecent.days.findIndex((day) => day.day === usedKey);
    expect((bars[halfIndex].firstElementChild as HTMLElement).style.getPropertyValue("--bar-height")).toBe("50%");
    expect((bars[usedIndex].firstElementChild as HTMLElement).style.getPropertyValue("--bar-height")).toBe("100%");
    expect(chart.getAttribute("aria-describedby")).toBe("recent-chart-instructions");
    expect(chart.getAttribute("aria-label")).toContain(`${usedKey}，${compact(1000)} Token，$0.0010 · 400 未计，未计费 Token 400`);

    fireEvent.keyDown(chart, { key: "ArrowLeft" });
    expect(chart.getAttribute("aria-label")).toContain("0 Token，$0.0000，未计费 Token 0");
    fireEvent.keyDown(chart, { key: "End" });
    expect(chart.getAttribute("aria-label")).toContain("0 Token，$0.0000，未计费 Token 0");
    fireEvent.pointerEnter(bars[0]);
    expect(chart.getAttribute("aria-label")).toContain((table.querySelector("tbody th") as HTMLElement).textContent || "missing");
    fireEvent.click(bars[usedIndex]);
    expect(chart.getAttribute("aria-label")).toContain(`${usedKey}，${compact(1000)} Token`);
    expect(within(section).getByRole("status").textContent).toContain("费用$0.0010 · 400 未计");
    expect(within(section).getByRole("status").textContent).toContain("未计费 Token400");
  });

  it("keeps all 30 bins when every recent day is zero and initially selects today", async () => {
    appTestState.snapshot = { ...appTestState.snapshot, trends: [] };
    location.hash = "history";
    render(<App />);
    const section = (await screen.findByRole("heading", { name: "最近 30 天明细" })).closest("section") as HTMLElement;
    const chart = within(section).getByRole("group", { name: /最近 30 天每日 Token 用量/ });
    expect(section.querySelectorAll(".recent-bar")).toHaveLength(30);
    expect(within(section).getByText("最近 30 天暂无用量，仍显示完整日期范围。")).toBeTruthy();
    expect(chart.getAttribute("aria-label")).toContain("0 Token，$0.0000，未计费 Token 0");
    fireEvent.keyDown(chart, { key: "Home" });
    expect(chart.getAttribute("aria-label")).toContain(buildRecentHistory([], new Date()).days[0].day);
  });

  it("keeps the selected day stable as the local window moves and falls back after it leaves", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    try {
      vi.setSystemTime(new Date(2026, 7, 2, 12, 0, 0));
      appTestState.snapshot = {
        ...appTestState.snapshot,
        trends: [
          { day: "2026-07-05", tokens: 100, costPicoUsd: 0, costKnown: true },
          { day: "2026-08-20", tokens: 200, costPicoUsd: 0, costKnown: true },
        ],
      };
      location.hash = "history";
      render(<App />);
      const chart = await screen.findByRole("group", { name: /最近 30 天每日 Token 用量/ });
      await waitFor(() => expect(appTestState.listeners.has("monitor://invalidated")).toBe(true));
      expect(chart.getAttribute("aria-label")).toContain("2026-07-05，100 Token");

      vi.setSystemTime(new Date(2026, 7, 3, 12, 0, 0));
      await invalidateSnapshot(8);
      expect(chart.getAttribute("aria-label")).toContain("2026-07-05，100 Token");

      vi.setSystemTime(new Date(2026, 8, 4, 12, 0, 0));
      await invalidateSnapshot(9);
      expect(chart.getAttribute("aria-label")).toContain("2026-08-20，200 Token");
    } finally {
      vi.useRealTimers();
    }
  });
});
