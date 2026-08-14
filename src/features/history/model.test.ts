import { describe, expect, it } from "vitest";
import { buildAnnualHeatmap, buildRecentHistory, calendarWeekColumn } from "./model";

describe("history view model", () => {
  it("gap-fills exactly 30 ordered local calendar days and exposes five ticks", () => {
    const recent = buildRecentHistory([
      { day: "2024-01-31", tokens: 50, costPicoUsd: 0, costKnown: true },
      { day: "2024-02-29", tokens: 100, costPicoUsd: 0, costKnown: true },
      { day: "2024-03-02", tokens: 999, costPicoUsd: 0, costKnown: true },
    ], new Date(2024, 2, 1, 18, 30));

    expect(recent.days).toHaveLength(30);
    expect(recent.days[0].day).toBe("2024-02-01");
    expect(recent.days.at(-1)?.day).toBe("2024-03-01");
    expect(recent.days[27].day).toBe("2024-02-28");
    expect(recent.days[27].tokens).toBe("0");
    expect(recent.days[28].tokens).toBe("100");
    expect(recent.days.some((day) => day.day === "2024-01-31")).toBe(false);
    expect(recent.days.some((day) => day.day === "2024-03-02")).toBe(false);
    expect(recent.tickIndexes).toEqual([0, 7, 14, 21, 29]);
    expect(recent.initialIndex).toBe(28);
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

  it("distinguishes the two partial edge months in a rolling 365-day range", () => {
    const annual = buildAnnualHeatmap([], new Date(2026, 7, 2));
    const first = annual.monthLabels[0] as typeof annual.monthLabels[number] & { label?: string };
    const last = annual.monthLabels.at(-1) as typeof annual.monthLabels[number] & { label?: string };

    expect(annual.days[0].day).toBe("2025-08-03");
    expect(annual.days.at(-1)?.day).toBe("2026-08-02");
    expect(first.label).toBe("25/8");
    expect(last.label).toBe("26/8");
    expect(first.labelSpan).toBe(3);
    expect(last.labelSpan).toBe(3);
    expect(last.labelColumn + last.labelSpan - 1).toBeLessThanOrEqual(annual.columns);
  });

  it("assigns distinct consecutive week columns across spring and autumn DST boundaries", () => {
    const springStart = new Date(2026, 2, 2);
    const autumnStart = new Date(2026, 9, 26);
    expect(calendarWeekColumn(new Date(2026, 2, 9), springStart)).toBe(
      calendarWeekColumn(springStart, springStart) + 1,
    );
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

  it("builds a complete annual heatmap from the snapshot data", () => {
    const trends = [{ day: "2026-07-30", tokens: 10, costPicoUsd: 0, costKnown: true }];
    const annual = buildAnnualHeatmap(trends, new Date(2026, 6, 30));
    expect(annual.days).toHaveLength(365);
    expect(annual.columns).toBe(53);
    expect(annual.days.at(-1)?.day).toBe("2026-07-30");
  });
});
