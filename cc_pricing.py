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
import sys
import json
import glob
from datetime import datetime, timezone

MTOK = 1_000_000.0

def _resource_base_dir():
    here = os.path.dirname(os.path.abspath(__file__))
    cands = [
        here,
        os.path.join(here, "..", "Resources"),
    ]

    # py2app / PyInstaller bundled app
    if getattr(sys, "frozen", False):
        exe_dir = os.path.dirname(os.path.abspath(sys.executable))
        cands.extend([
            os.path.join(exe_dir, "..", "Resources"),
            os.path.join(exe_dir, "Resources"),
        ])

    for c in cands:
        p = os.path.abspath(c)
        if os.path.exists(os.path.join(p, "prices.builtin.json")):
            return p
    return here


_BUILTIN_PRICES_PATH = os.path.join(_resource_base_dir(), "prices.builtin.json")
_USER_PRICES_PATH = os.path.expanduser("~/.cc-monitor/prices.json")
_PRICES_CACHE = None
_SUMMARY_CACHE = {}


def _normalize_price_value(v):
    if isinstance(v, (list, tuple)) and len(v) == 4:
        return tuple(float(x) for x in v)
    if isinstance(v, dict):
        return (
            float(v.get("input", 0)),
            float(v.get("cache_write", 0)),
            float(v.get("cache_read", 0)),
            float(v.get("output", 0)),
        )
    return None


def _load_prices_file(path):
    with open(path) as fp:
        raw = json.load(fp)
    out = {}
    if isinstance(raw, dict):
        for k, v in raw.items():
            nv = _normalize_price_value(v)
            if nv is not None:
                out[str(k).lower()] = nv
    return out


def _prices_cache_key():
    built = None
    user = None
    try:
        st = os.stat(_BUILTIN_PRICES_PATH)
        built = (st.st_size, st.st_mtime_ns)
    except OSError:
        built = None
    try:
        st = os.stat(_USER_PRICES_PATH)
        user = (st.st_size, st.st_mtime_ns)
    except OSError:
        user = None
    return built, user


def _load_prices():
    global _PRICES_CACHE
    cache_key = _prices_cache_key()
    if _PRICES_CACHE and _PRICES_CACHE[0] == cache_key:
        return dict(_PRICES_CACHE[1])

    prices = {}
    try:
        prices.update(_load_prices_file(_BUILTIN_PRICES_PATH))
    except Exception:
        pass

    try:
        prices.update(_load_prices_file(_USER_PRICES_PATH))
    except Exception:
        pass

    _PRICES_CACHE = (cache_key, prices)
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


def _request_id_from_obj(obj, msg):
    return (
        obj.get("requestId")
        or obj.get("request_id")
        or msg.get("requestId")
        or msg.get("request_id")
        or ""
    )


def _usage_record(obj, include_sidechain=False):
    if obj.get("type") != "assistant":
        return None
    is_sidechain = bool(obj.get("isSidechain"))
    if is_sidechain and not include_sidechain:
        return None

    msg = obj.get("message") or {}
    usage = msg.get("usage")
    if not isinstance(usage, dict):
        return None

    u = extract_usage(usage)
    return {
        "usage": usage,
        "model": msg.get("model", ""),
        "message_id": msg.get("id") or "",
        "request_id": _request_id_from_obj(obj, msg),
        "is_sidechain": is_sidechain,
        "final": bool(msg.get("stop_reason")),
        "day": _day_from_timestamp(obj.get("timestamp")),
        "tokens_total": (
            (u["input"] or 0)
            + (u["output"] or 0)
            + (u["cache_write"] or 0)
            + (u["cache_read"] or 0)
        ),
    }


def _day_from_timestamp(ts):
    if not ts:
        return datetime.now().strftime("%Y-%m-%d")
    try:
        if isinstance(ts, (int, float)):
            dt = datetime.fromtimestamp(ts, timezone.utc)
        else:
            dt = datetime.fromisoformat(str(ts).replace("Z", "+00:00"))
        return dt.astimezone().strftime("%Y-%m-%d")
    except Exception:
        return datetime.now().strftime("%Y-%m-%d")


def _iter_usage_records(path, include_sidechain=False):
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
                rec = _usage_record(obj, include_sidechain=include_sidechain)
                if rec:
                    yield rec
    except OSError:
        return


def _prefer_record(a, b):
    # 非 sidechain 记录优先
    if bool(a.get("is_sidechain")) != bool(b.get("is_sidechain")):
        return a if not a.get("is_sidechain") else b

    # token 更多的更可能是最终完整记录
    at = a.get("tokens_total", 0) or 0
    bt = b.get("tokens_total", 0) or 0
    if at != bt:
        return a if at > bt else b

    # final 优先（其余相同情况下）
    if bool(a.get("final")) != bool(b.get("final")):
        return a if a.get("final") else b

    return b


def _dedup_usage_records(path, include_sidechain=False):
    by_msg_req = {}   # (message_id, request_id) -> rec
    by_msg = {}       # message_id -> rec (用于 sidechain replay 归并)
    no_ids = []       # 无 message_id/request_id 兜底

    for rec in _iter_usage_records(path, include_sidechain=include_sidechain):
        mid = rec.get("message_id") or ""
        rid = rec.get("request_id") or ""

        if mid and rid:
            k = (mid, rid)
            prev = by_msg_req.get(k)
            by_msg_req[k] = rec if prev is None else _prefer_record(prev, rec)
            continue

        if mid:
            prev = by_msg.get(mid)
            by_msg[mid] = rec if prev is None else _prefer_record(prev, rec)
            continue

        no_ids.append(rec)

    out = []

    # 先收集精确键去重结果
    out.extend(by_msg_req.values())

    # sidechain replay 场景:同一个 message_id 可能来自不同 request_id
    # 这里做 message 级次级归并，避免重复计入
    merged_by_mid = {}
    for rec in out:
        mid = rec.get("message_id") or ""
        if not mid:
            continue
        prev = merged_by_mid.get(mid)
        merged_by_mid[mid] = rec if prev is None else _prefer_record(prev, rec)

    # 把只存在于 by_msg（无 request_id）的记录合并进来
    for mid, rec in by_msg.items():
        prev = merged_by_mid.get(mid)
        merged_by_mid[mid] = rec if prev is None else _prefer_record(prev, rec)

    # 用 message 级结果替换 message 有 ID 的记录，减少 replay 双计
    mid_set = set(merged_by_mid.keys())
    filtered = []
    for rec in out:
        mid = rec.get("message_id") or ""
        if mid and mid in mid_set:
            continue
        filtered.append(rec)
    filtered.extend(merged_by_mid.values())

    if no_ids:
        filtered.append(no_ids[-1])  # 与旧逻辑一致：无 ID 仅保留最新一条

    return filtered


def _related_transcripts(path):
    paths = [path]
    if not path.endswith(".jsonl"):
        return paths
    root = path[:-6]
    subdir = os.path.join(root, "subagents")
    if os.path.isdir(subdir):
        for p in sorted(glob.glob(os.path.join(subdir, "*.jsonl"))):
            paths.append(p)
    return paths


def summarize_transcript(path):
    paths = _related_transcripts(path)
    stats_sig = []
    for p in paths:
        try:
            st = os.stat(p)
            stats_sig.append((p, st.st_size, st.st_mtime_ns))
        except OSError:
            continue
    if not stats_sig:
        return {"input": 0, "output": 0, "cache_write": 0, "cache_read": 0,
                "cost_usd": 0.0, "cost_known": True, "total_tokens": 0}

    cache_key = (tuple(stats_sig), _prices_cache_key())
    cached = _SUMMARY_CACHE.get(path)
    if cached and cached[0] == cache_key:
        return dict(cached[1])

    agg = {"input": 0, "output": 0, "cache_write": 0, "cache_read": 0,
           "cost_usd": 0.0, "cost_known": True}
    saw_any = False
    for p in paths:
        include_sidechain = (p != path)
        for rec in _dedup_usage_records(p, include_sidechain=include_sidechain):
            saw_any = True
            usage = rec["usage"]
            u = extract_usage(usage)
            agg["input"] += u["input"]
            agg["output"] += u["output"]
            agg["cache_write"] += u["cache_write"]
            agg["cache_read"] += u["cache_read"]
            c, known = cost_of(usage, rec["model"])
            agg["cost_usd"] += c
            if not known:
                agg["cost_known"] = False

    if not saw_any:
        agg["cost_known"] = True

    agg["total_tokens"] = (
        agg["input"] + agg["output"] + agg["cache_write"] + agg["cache_read"]
    )
    _SUMMARY_CACHE[path] = (cache_key, dict(agg))
    return agg


def summarize_transcript_by_day(path):
    paths = _related_transcripts(path)
    stats_sig = []
    for p in paths:
        try:
            st = os.stat(p)
            stats_sig.append((p, st.st_size, st.st_mtime_ns))
        except OSError:
            continue
    if not stats_sig:
        return {}

    cache_key = ("by_day", tuple(stats_sig), _prices_cache_key())
    cached = _SUMMARY_CACHE.get((path, "by_day"))
    if cached and cached[0] == cache_key:
        return {day: dict(usage) for day, usage in cached[1].items()}

    by_day = {}
    for p in paths:
        include_sidechain = (p != path)
        for rec in _dedup_usage_records(p, include_sidechain=include_sidechain):
            day = rec["day"]
            usage = rec["usage"]
            u = extract_usage(usage)
            agg = by_day.setdefault(day, {
                "input": 0, "output": 0, "cache_write": 0, "cache_read": 0,
                "cost_usd": 0.0, "cost_known": True,
            })
            agg["input"] += u["input"]
            agg["output"] += u["output"]
            agg["cache_write"] += u["cache_write"]
            agg["cache_read"] += u["cache_read"]
            c, known = cost_of(usage, rec["model"])
            agg["cost_usd"] += c
            if not known:
                agg["cost_known"] = False

    for agg in by_day.values():
        agg["total_tokens"] = (
            agg["input"] + agg["output"] + agg["cache_write"] + agg["cache_read"]
        )
    _SUMMARY_CACHE[(path, "by_day")] = (
        cache_key, {day: dict(usage) for day, usage in by_day.items()}
    )
    return by_day


def summarize_transcript_by_model(path, day=None):
    paths = _related_transcripts(path)
    stats_sig = []
    for p in paths:
        try:
            st = os.stat(p)
            stats_sig.append((p, st.st_size, st.st_mtime_ns))
        except OSError:
            continue
    if not stats_sig:
        return {}

    cache_key = ("by_model", tuple(stats_sig), _prices_cache_key(), day or "")
    cached = _SUMMARY_CACHE.get((path, "by_model", day or ""))
    if cached and cached[0] == cache_key:
        return {model: dict(usage) for model, usage in cached[1].items()}

    by_model = {}
    for p in paths:
        include_sidechain = (p != path)
        for rec in _dedup_usage_records(p, include_sidechain=include_sidechain):
            if day and rec.get("day") != day:
                continue
            model = rec.get("model") or "(unknown model)"
            usage = rec["usage"]
            u = extract_usage(usage)
            agg = by_model.setdefault(model, {
                "input": 0, "output": 0, "cache_write": 0, "cache_read": 0,
                "cost_usd": 0.0, "cost_known": True,
            })
            agg["input"] += u["input"]
            agg["output"] += u["output"]
            agg["cache_write"] += u["cache_write"]
            agg["cache_read"] += u["cache_read"]
            c, known = cost_of(usage, model)
            agg["cost_usd"] += c
            if not known:
                agg["cost_known"] = False

    for agg in by_model.values():
        agg["total_tokens"] = (
            agg["input"] + agg["output"] + agg["cache_write"] + agg["cache_read"]
        )

    _SUMMARY_CACHE[(path, "by_model", day or "")] = (
        cache_key, {model: dict(usage) for model, usage in by_model.items()}
    )
    return by_model


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
