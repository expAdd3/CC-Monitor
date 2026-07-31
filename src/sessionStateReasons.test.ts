import { describe, expect, it } from "vitest";
import {
  sessionStateReasonContract,
  sessionStateReasonLabel,
} from "./sessionStateReasons";

describe("session state reason contract", () => {
  it("provides a unique, concise Chinese label for every catalogued reason", () => {
    const codes = sessionStateReasonContract.map(({ code }) => code);
    expect(new Set(codes).size).toBe(codes.length);

    for (const { code, label } of sessionStateReasonContract) {
      expect(code).not.toBe("");
      expect(label).not.toBe("");
      expect(sessionStateReasonLabel(code)).toBe(label);
      expect(label).not.toBe("状态已更新");
    }
  });

  it("does not silently generalize an unknown backend reason", () => {
    expect(sessionStateReasonLabel("future_reason"))
      .toBe("未知状态原因（future_reason）");
  });
});
