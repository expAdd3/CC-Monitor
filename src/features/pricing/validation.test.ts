import { describe, expect, it } from "vitest";
import priceValidationFixtures from "../../../tests/fixtures/model_price_validation.json";
import { canonicalPriceModelId, priceRateError } from "./validation";

describe("model price validation", () => {
  it("uses the shared model-price validation contract", () => {
    for (const { value, picoUsd: _picoUsd } of priceValidationFixtures.validRates) {
      expect(priceRateError(value), `valid rate: ${JSON.stringify(value)}`).toBeUndefined();
    }
    for (const value of priceValidationFixtures.invalidRates) {
      expect(priceRateError(value), `invalid rate: ${JSON.stringify(value)}`).toContain("请输入");
    }
    for (const value of priceValidationFixtures.outOfRangeRates) {
      expect(priceRateError(value), `out-of-range rate: ${JSON.stringify(value)}`).toContain("超出");
    }
    for (const { value, canonical } of priceValidationFixtures.canonicalModels) {
      expect(canonicalPriceModelId(value)).toBe(canonical);
    }
  });
});
