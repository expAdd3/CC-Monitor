import { decimal, type DashboardSnapshot } from "../../api";

type TrendDay = DashboardSnapshot["trends"][number];
type HeatmapDay = TrendDay & { date: Date; level: number; column: number; row: number };

function localDayKey(date: Date) {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

function localDateFromKey(value: string) {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!match) return null;
  const date = new Date(Number(match[1]), Number(match[2]) - 1, Number(match[3]));
  return localDayKey(date) === value ? date : null;
}

function addLocalDays(date: Date, days: number) {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate() + days);
}

function mondayOf(date: Date) {
  return addLocalDays(date, -((date.getDay() + 6) % 7));
}

function safeTokens(value: TrendDay["tokens"]) {
  return decimal(value);
}

function daySerial(date: Date) {
  return Math.floor(Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / 86_400_000);
}

export function calendarWeekColumn(date: Date, gridStart: Date) {
  return Math.floor((daySerial(date) - daySerial(gridStart)) / 7) + 1;
}

export function buildAnnualHeatmap(trends: TrendDay[], now = new Date()) {
  return buildHeatmap(trends, 365, now);
}

export function buildRecentHistory(trends: TrendDay[], now = new Date()) {
  const days = buildHeatmap(trends, 30, now).days;
  const maxTokens = days.reduce(
    (maximum, day) => maximum > safeTokens(day.tokens) ? maximum : safeTokens(day.tokens),
    0n,
  );
  let initialIndex = days.length - 1;
  for (let index = days.length - 1; index >= 0; index -= 1) {
    if (safeTokens(days[index].tokens) > 0n) {
      initialIndex = index;
      break;
    }
  }
  return { days, maxTokens, initialIndex, tickIndexes: [0, 7, 14, 21, 29] };
}

function buildHeatmap(trends: TrendDay[], daysInRange: number, now = new Date()) {
  const end = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const range = Math.max(1, Math.trunc(daysInRange));
  const start = addLocalDays(end, -(range - 1));
  const byDay = new Map<string, TrendDay>();
  for (const trend of trends) {
    const date = localDateFromKey(trend.day);
    if (date && date >= start && date <= end) {
      byDay.set(trend.day, { ...trend, tokens: safeTokens(trend.tokens).toString() });
    }
  }
  const days = Array.from({ length: range }, (_, index) => {
    const date = addLocalDays(start, index);
    const day = byDay.get(localDayKey(date));
    return {
      date,
      day: localDayKey(date),
      tokens: day?.tokens ?? "0",
      costPicoUsd: day?.costPicoUsd ?? "0",
      costKnown: day?.costKnown ?? true,
      unpricedTokens: day?.unpricedTokens ?? "0",
    };
  });
  const max = days.reduce((value, day) => value > safeTokens(day.tokens) ? value : safeTokens(day.tokens), 0n);
  const gridStart = mondayOf(start);
  const heatmapDays: HeatmapDay[] = days.map((day) => ({
    ...day,
    level: safeTokens(day.tokens) === 0n || max === 0n ? 0 : Number((safeTokens(day.tokens) * 4n + max - 1n) / max),
    column: calendarWeekColumn(day.date, gridStart),
    row: (day.date.getDay() + 6) % 7 + 1,
  }));
  const monthStarts = heatmapDays.filter((day, index) => index === 0 || day.date.getMonth() !== heatmapDays[index - 1].date.getMonth());
  const repeatedMonths = new Set(
    monthStarts
      .filter((day, index) => monthStarts.some((other, otherIndex) => otherIndex !== index && other.date.getMonth() === day.date.getMonth()))
      .map((day) => day.date.getMonth()),
  );
  const columns = Math.max(...heatmapDays.map((day) => day.column));
  const monthLabels = monthStarts.map((day) => {
    const repeated = repeatedMonths.has(day.date.getMonth());
    const span = repeated ? 3 : 2;
    return {
      ...day,
      label: repeated ? `${String(day.date.getFullYear()).slice(-2)}/${day.date.getMonth() + 1}` : `${day.date.getMonth() + 1}月`,
      labelColumn: Math.min(day.column, Math.max(1, columns - span + 1)),
      labelSpan: span,
    };
  });
  return {
    days: heatmapDays,
    monthLabels,
    columns,
  };
}
