import type { SaveModelPrice } from "../../api";

export type PriceFieldName = keyof SaveModelPrice;
export type PriceFieldErrors = Partial<Record<PriceFieldName, string>>;

export const priceFieldLabels: Record<PriceFieldName, string> = {
  modelId: "模型 ID",
  inputCostPerMillion: "输入 Token 单价",
  outputCostPerMillion: "输出 Token 单价",
  cacheWriteCostPerMillion: "缓存创建单价",
  cacheReadCostPerMillion: "缓存读取单价",
};

export const blankPrice = (): SaveModelPrice => ({
  modelId: "",
  inputCostPerMillion: "",
  outputCostPerMillion: "",
  cacheWriteCostPerMillion: "0",
  cacheReadCostPerMillion: "0",
});

export function priceRateError(value: string) {
  const trimmed = value.trim();
  if (!/^\d+(?:\.\d{1,18})?$/.test(trimmed)) return "请输入不小于 0 的 USD 数字（最多 18 位小数）";
  const [whole, fraction = ""] = trimmed.split(".");
  const kept = fraction.slice(0, 12).padEnd(12, "0");
  let pico = BigInt(whole) * 1_000_000_000_000n + BigInt(kept || "0");
  if ((fraction[12] || "0") >= "5") pico += 1n;
  return pico > 9_223_372_036_854_775_807n ? "金额超出可保存范围" : undefined;
}

export function canonicalPriceModelId(modelId: string) {
  let canonical = modelId.trim().replace(/[A-Z]/g, (letter) => letter.toLowerCase()).split(":", 1)[0];
  if (canonical.endsWith("[1m]")) canonical = canonical.slice(0, -4);
  return canonical.replace(/[@_.]/g, "-").replace(/-+/g, "-").replace(/^-|-$/g, "");
}

export function validatePriceEditor(value: SaveModelPrice): PriceFieldErrors {
  const errors: PriceFieldErrors = {};
  const canonical = canonicalPriceModelId(value.modelId);
  if (!canonical) errors.modelId = "请输入模型 ID";
  else if (new TextEncoder().encode(canonical).length > 256) errors.modelId = "模型 ID 不能超过 256 字节";
  for (const field of ["inputCostPerMillion", "outputCostPerMillion", "cacheWriteCostPerMillion", "cacheReadCostPerMillion"] as const) {
    const error = priceRateError(value[field]);
    if (error) errors[field] = error;
  }
  return errors;
}

export function priceSaveCorrection(reason: unknown): { field?: PriceFieldName; message: string } {
  const code = typeof reason === "string" ? reason : reason instanceof Error ? reason.message : "";
  const definitions: Record<string, { field: PriceFieldName; message: string }> = {
    price_model_id_required: { field: "modelId", message: "请输入模型 ID" },
    price_model_id_too_long: { field: "modelId", message: "模型 ID 不能超过 256 字节" },
    price_input_invalid: { field: "inputCostPerMillion", message: "请输入有效的输入 Token 单价" },
    price_input_out_of_range: { field: "inputCostPerMillion", message: "输入 Token 单价超出可保存范围" },
    price_output_invalid: { field: "outputCostPerMillion", message: "请输入有效的输出 Token 单价" },
    price_output_out_of_range: { field: "outputCostPerMillion", message: "输出 Token 单价超出可保存范围" },
    price_cache_write_invalid: { field: "cacheWriteCostPerMillion", message: "请输入有效的缓存创建单价" },
    price_cache_write_out_of_range: { field: "cacheWriteCostPerMillion", message: "缓存创建单价超出可保存范围" },
    price_cache_read_invalid: { field: "cacheReadCostPerMillion", message: "请输入有效的缓存读取单价" },
    price_cache_read_out_of_range: { field: "cacheReadCostPerMillion", message: "缓存读取单价超出可保存范围" },
  };
  return definitions[code] || { message: "定价保存失败，请重试" };
}
