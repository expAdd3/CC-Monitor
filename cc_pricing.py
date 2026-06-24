#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
cc_pricing.py —— 模型计费 + transcript 用量解析(零三方依赖)

为什么单独成一个模块:
  - hook 端(采集)和菜单栏端(展示/兜底)都要用,抽出来避免重复。
  - 价格会变、模型会多,集中一处维护,还支持用户自定义覆盖。

token 数 vs. 算钱,是两件事:
  - 数 token:几乎对任何模型都能数(只要日志里有 usage),还兼容 OpenAI 风格字段。
  - 算钱:得知道该模型的单价。内置了 Claude / GPT / GLM 几个常见的;
    在 CC 里用第三方模型(走代理)时,可在 ~/.cc-monitor/prices.json 自定义价格。
    匹配不到价格的模型 → token 照样数,但成本标记为"未知"。
"""
import os
import json

MTOK = 1_000_000.0

# 元组 = (input, cache_write_5m, cache_read, output)  单位 $ / MTok
_BUILTIN_PRICES = {
    "opus":     (5.0,  6.25, 0.50, 25.0),
    "sonnet":   (3.0,  3.75, 0.30, 15.0),
    "haiku":    (1.0,  1.25, 0.10,  5.0),

    "gpt-4o-mini": (0.15, 0.15, 0.075, 0.60),
    "gpt-4o":      (2.5,  2.5,  1.25,  10.0),
    "gpt-4.1":     (2.0,  2.0,  0.50,   8.0),
    "o3":          (2.0,  2.0,  0.50,   8.0),

    "glm-4-plus":  (0.70, 0.70, 0.70, 0.70),
    "glm-4":       (0.14, 0.14, 0.14, 0.14),
    "glm":         (0.30, 0.30, 0.30, 0.30),
}

_USER_PRICES_PATH = os.path.expanduser("~/.cc-monitor/prices.json")


def _load_prices():
    prices = dict(_BUILTIN_PRICES)
    try:
        if os.path.exists(_USER_PRICES_PATH):
            user = json.load(open(_USER_PRICES_PATH))
            for k, v in user.items():
                if isinstance(v, (list, tuple)) and len(v) == 4:
                    prices[k.lower()] = tuple(float(x) for x in v)
                elif isinstance(v, dict):
                    prices[k.lower()] = (
                        float(v.get("input", 0)), float(v.get("cache_write", 0)),
                        float(v.get("cache_read", 0)), float(v.get("output", 0)))
    except Exception:
        pass
    return prices


def prices_for(model: str):
    m = (model or "").lower()
    prices = _load_prices()
    best = None
    for fam, p in prices.items():
        if fam in m and (best is None or len(fam) > len(best[0])):
            best = (fam, p)
    return best[1] if best else None


def extract_usage(usage: dict) -> dict:
    if not isinstance(usage, dict):
        return {"input": 0, "output": 0, "cache_write": 0, "cache_read": 0}
    inp = usage.get("input_tokens")
    out = usage.get("output_tokens")
    cw = usage.get("cache_creation_input_tokens", 0) or 0
    cr = usage.get("cache_read_input_tokens", 0) or 0

    if inp is None:
        inp = usage.get("prompt_tokens", 0) or 0
    if out is None:
        out = usage.get("completion_tokens", 0) or 0
    if not cr:
        det = usage.get("prompt_tokens_details") or {}
        if isinstance(det, dict):
            cr = det.get("cached_tokens", 0) or 0
            if cr and inp >= cr:
                inp = inp - cr

    return {
        "input": inp or 0,
        "output": out or 0,
        "cache_write": cw,
        "cache_read": cr,
    }


def cost_of(usage: dict, model: str):
    p = prices_for(model)
    u = extract_usage(usage)
    if p is None:
        return 0.0, False
    inp_p, cw_p, cr_p, out_p = p
    cost = (u["input"] * inp_p + u["cache_write"] * cw_p +
            u["cache_read"] * cr_p + u["output"] * out_p) / MTOK
    return cost, True


def _iter_usages(path):
    try:
        with open(path, "rb") as fp:
            for line in fp:
                s = line.strip()
                if not s:
                    continue
                try:
                    obj = json.loads(s)
                except Exception:
                    continue
                if obj.get("type") != "assistant":
                    continue
                msg = obj.get("message") or {}
                usage = msg.get("usage")
                if isinstance(usage, dict):
                    yield usage, msg.get("model", "")
    except OSError:
        return


def summarize_transcript(path):
    agg = {"input": 0, "output": 0, "cache_write": 0, "cache_read": 0,
           "cost_usd": 0.0, "cost_known": True}
    saw_any = False
    for usage, model in _iter_usages(path):
        saw_any = True
        u = extract_usage(usage)
        agg["input"] += u["input"]
        agg["output"] += u["output"]
        agg["cache_write"] += u["cache_write"]
        agg["cache_read"] += u["cache_read"]
        c, known = cost_of(usage, model)
        agg["cost_usd"] += c
        if not known:
            agg["cost_known"] = False

    if not saw_any:
        agg["cost_known"] = True

    agg["total_tokens"] = (
        agg["input"] + agg["output"] + agg["cache_write"] + agg["cache_read"]
    )
    return agg


def fmt_tokens(n: int) -> str:
    n = n or 0
    if n >= 1_000_000:
        return f"{n/1_000_000:.1f}M"
    if n >= 1_000:
        return f"{n/1_000:.1f}K"
    return str(n)


def fmt_usd(x: float) -> str:
    x = x or 0.0
    if x < 0.01:
        return f"${x:.4f}"
    if x < 1:
        return f"${x:.3f}"
    return f"${x:.2f}"
