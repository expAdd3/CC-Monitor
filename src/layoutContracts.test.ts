import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const styles = readFileSync("src/styles.css", "utf8");
const lightRoot = styles.match(/^:root\s*\{([^}]*)\}/m)?.[1] ?? "";
const darkStart = styles.indexOf("@media (prefers-color-scheme: dark)");
const reducedMotionStart = styles.indexOf("@media (prefers-reduced-motion: reduce)");
const darkRoot = styles
  .slice(darkStart, reducedMotionStart)
  .match(/:root\s*\{([^}]*)\}/)?.[1] ?? "";
const reducedMotionStyles = styles.slice(reducedMotionStart);

function property(block: string, name: string) {
  return block.match(new RegExp(`--${name}:\\s*([^;]+)`))?.[1].trim();
}

function hexProperty(block: string, name: string) {
  const value = property(block, name);
  if (!value || !/^#[0-9a-f]{6}$/i.test(value)) {
    throw new Error(`--${name} must be a six-digit hex color`);
  }
  return value;
}

function relativeLuminance(hex: string) {
  const channels = [hex.slice(1, 3), hex.slice(3, 5), hex.slice(5, 7)]
    .map((channel) => Number.parseInt(channel, 16) / 255)
    .map((channel) => channel <= 0.04045
      ? channel / 12.92
      : ((channel + 0.055) / 1.055) ** 2.4);
  return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
}

function contrastRatio(foreground: string, background: string) {
  const foregroundLuminance = relativeLuminance(foreground);
  const backgroundLuminance = relativeLuminance(background);
  return (Math.max(foregroundLuminance, backgroundLuminance) + 0.05)
    / (Math.min(foregroundLuminance, backgroundLuminance) + 0.05);
}

describe("layout contracts", () => {
  it("keeps the shared typography floor and semantic theme tokens", () => {
    for (const [token, value] of [
      ["font-xs", "12px"],
      ["font-sm", "13px"],
      ["font-body", "14px"],
      ["font-section", "16px"],
      ["font-page", "28px"],
    ]) {
      expect(property(lightRoot, token)).toBe(value);
    }
    expect(styles).not.toMatch(/font-size:\s*(?:8|9|10|11)px/);

    for (const token of [
      "primary-background", "primary-text", "status-danger-text",
      "danger-soft", "danger-hover", "metric-token-accent",
      "state-failed", "toggle-off", "toggle-thumb", "heatmap-3",
    ]) {
      expect(property(lightRoot, token), `light --${token}`).toBeTruthy();
      expect(property(darkRoot, token), `dark --${token}`).toBeTruthy();
    }
  });

  it("keeps destructive controls at WCAG AA contrast in both themes", () => {
    for (const root of [lightRoot, darkRoot]) {
      const foreground = hexProperty(root, "status-danger-text");
      for (const background of ["danger-soft", "danger-hover"]) {
        expect(contrastRatio(foreground, hexProperty(root, background)))
          .toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  it("uses one compact transition and contains naturally wide data views", () => {
    const widthMediaQueries = Array.from(
      styles.matchAll(/@media\s+([^{]+)\{/g),
      (match) => match[1].replace(/\s+/g, " ").trim(),
    ).filter((query) => /(?:min|max)-width\s*:/.test(query));
    expect(widthMediaQueries).toEqual(["(max-width: 1120px)"]);
    expect(styles).toMatch(/body\s*\{[^}]*min-width:\s*320px/);
    expect(styles).not.toMatch(/body\s*\{[^}]*min-width:\s*820px/);
    for (const selector of ["heatmap-scroll", "price-table-wrap", "session-model-table-wrap"]) {
      expect(styles).toMatch(new RegExp(`\\.${selector}\\s*\\{[^}]*overflow-x:\\s*auto`));
    }
  });

  it("keeps keyboard focus visible without framing page titles", () => {
    expect(styles).toMatch(/\.skip-link:focus\s*\{[^}]*outline:/);
    expect(styles).toMatch(/\.page-title h1:focus\s*\{[^}]*outline:\s*none/);
    expect(styles).toMatch(/button:focus-visible[^}]*outline:\s*3px solid/);
  });

  it("turns off live motion when reduced motion is requested", () => {
    expect(reducedMotionStyles).toMatch(/\*, \*::before, \*::after\s*\{/);
    expect(reducedMotionStyles).toMatch(/scroll-behavior:\s*auto !important/);
    expect(reducedMotionStyles).toMatch(/transition-duration:\s*\.01ms !important/);
    expect(reducedMotionStyles).toMatch(/animation-duration:\s*\.01ms !important/);
  });
});
