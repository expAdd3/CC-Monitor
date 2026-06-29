#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
cc_monitor.py —— Claude Code 多会话监控(菜单栏 App)

混合架构的"消费端":
  - 主信源:cc_hook.py 写入的 ~/.cc-monitor/state.db(确定性事件,准)。
  - 兜底:对没装 hook 的会话,扫描 ~/.claude/projects 的 JSONL 日志启发式推断。
  - 唯一通知方:状态从 RUNNING → WAITING/NEEDS_INPUT 的**边沿**才弹一次,
    去重状态持久化在 DB,重启不丢、抖动不重复弹。

依赖:  pip3 install rumps   (仅菜单栏 UI;核心逻辑零依赖)
运行:  python3 cc_monitor.py
"""

import os
import json
import time
import glob
import sqlite3
import subprocess
import sys
import shutil
from datetime import datetime

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
try:
    import cc_pricing
except Exception:
    cc_pricing = None

DB_DIR  = os.path.expanduser("~/.cc-monitor")
DB_PATH = os.path.join(DB_DIR, "state.db")
PROJECTS_DIR = os.path.expanduser("~/.claude/projects")

REFRESH_SEC   = 2       # UI/兜底刷新间隔
IDLE_HIDE_SEC = 1800    # 超过该秒无活动的会话不显示
FALLBACK_RUNNING_GAP = 8    # 日志兜底:静默 < 此值视为运行中
FALLBACK_WAIT_GAP    = 30   # 日志兜底:静默 > 此值且最后是助手文本 → 等待

# ========================= 内部/插件会话过滤 =========================
# CC 用路径编码项目目录名: "/" → "-", "." → "-"
# 用户项目:  -Users-<name>-code-MyProject          (无 "--")
# 内部会话:  -Users-<name>--claude-mem-observer-sessions  ("--" 来自 /.claude-mem/)
# 规律: "--" 一定来源于路径中的隐藏/点目录, 是内部基础设施, 不需要监控
def _is_internal_project(project_name):
    """项目目录名含 '--' 表示路径中有隐藏/内部目录, 不监控不通知。"""
    return "--" in (project_name or "")

try:
    import rumps
    HAS_RUMPS = True
except ImportError:
    HAS_RUMPS = False


# ========================= 数据库 =========================

def connect():
    os.makedirs(DB_DIR, exist_ok=True)
    conn = sqlite3.connect(DB_PATH, timeout=10)
    conn.execute("PRAGMA journal_mode=WAL;")
    conn.execute("PRAGMA busy_timeout=5000;")
    conn.row_factory = sqlite3.Row
    return conn


def ensure_schema(conn):
    conn.executescript("""
    CREATE TABLE IF NOT EXISTS sessions (
        session_id TEXT PRIMARY KEY, cwd TEXT, project TEXT, status TEXT,
        last_event TEXT, last_event_ts REAL, turn_started_ts REAL,
        notify_pending INTEGER DEFAULT 0, notify_kind TEXT,
        transcript_path TEXT, client_bundle_id TEXT, source TEXT DEFAULT 'hook',
        tok_input INTEGER DEFAULT 0, tok_output INTEGER DEFAULT 0,
        tok_cache_write INTEGER DEFAULT 0, tok_cache_read INTEGER DEFAULT 0,
        tok_total INTEGER DEFAULT 0, cost_usd REAL DEFAULT 0,
        cost_known INTEGER DEFAULT 1
    );
    CREATE TABLE IF NOT EXISTS events (
        id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, event TEXT, ts REAL
    );
    CREATE TABLE IF NOT EXISTS daily_session_usage (
        day TEXT,
        session_id TEXT,
        tok_input INTEGER DEFAULT 0,
        tok_output INTEGER DEFAULT 0,
        tok_cache_write INTEGER DEFAULT 0,
        tok_cache_read INTEGER DEFAULT 0,
        tok_total INTEGER DEFAULT 0,
        cost_usd REAL DEFAULT 0,
        cost_known INTEGER DEFAULT 1,
        PRIMARY KEY(day, session_id)
    );
    """)
    for col, decl in (
        ("client_bundle_id", "TEXT"),
        ("tok_input", "INTEGER DEFAULT 0"), ("tok_output", "INTEGER DEFAULT 0"),
        ("tok_cache_write", "INTEGER DEFAULT 0"), ("tok_cache_read", "INTEGER DEFAULT 0"),
        ("tok_total", "INTEGER DEFAULT 0"), ("cost_usd", "REAL DEFAULT 0"),
        ("cost_known", "INTEGER DEFAULT 1"),
    ):
        try:
            conn.execute(f"ALTER TABLE sessions ADD COLUMN {col} {decl}")
        except sqlite3.OperationalError:
            pass
    conn.commit()


def today_key():
    return datetime.now().strftime("%Y-%m-%d")


def _aggregate_usage(by_day):
    return {
        "input": sum(d["input"] for d in by_day.values()),
        "output": sum(d["output"] for d in by_day.values()),
        "cache_write": sum(d["cache_write"] for d in by_day.values()),
        "cache_read": sum(d["cache_read"] for d in by_day.values()),
        "total_tokens": sum(d["total_tokens"] for d in by_day.values()),
        "cost_usd": sum(d["cost_usd"] for d in by_day.values()),
        "cost_known": all(d.get("cost_known", True) for d in by_day.values()),
    }


def _daily_usage_matches(conn, session_id, by_day):
    rows = conn.execute("""
        SELECT day, tok_input, tok_output, tok_cache_write, tok_cache_read,
               tok_total, cost_usd, cost_known
        FROM daily_session_usage
        WHERE session_id=?
    """, (session_id,)).fetchall()
    if len(rows) != len(by_day):
        return False
    current = {r["day"]: r for r in rows}
    for day, du in by_day.items():
        r = current.get(day)
        if not r:
            return False
        day_cost_known = 1 if du.get("cost_known", True) else 0
        if (
            r["tok_input"] != du["input"]
            or r["tok_output"] != du["output"]
            or r["tok_cache_write"] != du["cache_write"]
            or r["tok_cache_read"] != du["cache_read"]
            or r["tok_total"] != du["total_tokens"]
            or abs((r["cost_usd"] or 0.0) - (du["cost_usd"] or 0.0)) >= 1e-12
            or r["cost_known"] != day_cost_known
        ):
            return False
    return True


def _replace_daily_usage(conn, session_id, by_day):
    conn.execute(
        "DELETE FROM daily_session_usage WHERE session_id=?",
        (session_id,),
    )
    for day, du in by_day.items():
        day_cost_known = 1 if du.get("cost_known", True) else 0
        conn.execute("""
            INSERT INTO daily_session_usage(
                day, session_id, tok_input, tok_output,
                tok_cache_write, tok_cache_read, tok_total,
                cost_usd, cost_known
            ) VALUES(?,?,?,?,?,?,?,?,?)
        """, (
            day, session_id, du["input"], du["output"],
            du["cache_write"], du["cache_read"], du["total_tokens"],
            du["cost_usd"], day_cost_known,
        ))


# ========================= 日志兜底解析 =========================
# 仅用于"DB 里没有、但日志在动"的会话(没装 hook 的旧会话)。
# 修复点对照旧版:
#   1. 超大尾行:逐行 json.loads,坏行跳过,不会因一条巨型消息全盘失败。
#   2. 增量解析:记住每个文件 offset,只读新增部分,告别每秒重读 200KB。

_file_offsets = {}   # path -> (last_size, last_mtime, cached_tail_obj)

def parse_last_json_obj(path):
    """增量读取文件尾部,返回最后一条可解析的 JSON 对象。"""
    try:
        st = os.stat(path)
    except OSError:
        return None
    size, mtime = st.st_size, st.st_mtime
    cached = _file_offsets.get(path)
    if cached and cached[0] == size and cached[1] == mtime:
        return cached[2]   # 没变化,直接用缓存

    # 文件可能被 rotate/truncate(新 size < 旧 size)→ 从头读尾块
    read_from = 0
    if size > 200_000:
        read_from = size - 200_000
    last_obj = None
    try:
        with open(path, "rb") as fp:
            fp.seek(read_from)
            if read_from:
                fp.readline()  # 丢弃可能被切断的半行
            for line in fp:
                s = line.strip()
                if not s:
                    continue
                try:
                    last_obj = json.loads(s)
                except Exception:
                    continue   # 坏行/半行直接跳过,不影响整体
    except Exception:
        return None
    _file_offsets[path] = (size, mtime, last_obj)
    return last_obj


def infer_status_from_log(path):
    """从最后一条消息启发式推断状态(兜底用)。"""
    obj = parse_last_json_obj(path)
    if not obj:
        return None
    role = obj.get("role") or obj.get("type")
    msg = obj.get("message") or obj
    blocks = msg.get("content") if isinstance(msg, dict) else None
    last_kind = None
    if isinstance(blocks, list) and blocks:
        last_kind = blocks[-1].get("type")
    elif isinstance(blocks, str):
        last_kind = "text"

    try:
        idle = time.time() - os.path.getmtime(path)
    except OSError:
        idle = 9999

    # 助手在用工具/思考 → 运行中
    if role == "assistant" and last_kind in ("tool_use", "thinking"):
        return ("RUNNING", idle)
    # 用户的 tool_result → 助手马上要接着跑
    if last_kind == "tool_result":
        return ("RUNNING", idle)
    # 助手纯文本:近期=可能还在流式;久了=确实停下等你
    if role == "assistant" and last_kind in ("text", None):
        if idle < FALLBACK_RUNNING_GAP:
            return ("RUNNING", idle)
        if idle > FALLBACK_WAIT_GAP:
            return ("WAITING", idle)
        return ("RUNNING", idle)   # 灰色地带保守判为运行,避免误报
    return ("RUNNING", idle)


def merge_log_fallback(conn):
    """把 hook 没覆盖到的活跃会话,用日志补进 DB(source='log')。"""
    now = time.time()
    hooked = {r["session_id"] for r in
              conn.execute("SELECT session_id FROM sessions WHERE source='hook'")}
    # 先按目录过滤，避免遍历内部/插件会话的巨量文件
    for proj_dir in glob.glob(os.path.join(PROJECTS_DIR, "*")):
        if not os.path.isdir(proj_dir):
            continue
        project = os.path.basename(proj_dir)
        if _is_internal_project(project):
            continue
        for f in glob.glob(os.path.join(proj_dir, "*.jsonl")):
            try:
                if now - os.path.getmtime(f) > IDLE_HIDE_SEC:
                    continue
            except OSError:
                continue
            sid = os.path.splitext(os.path.basename(f))[0]
            if sid in hooked:
                continue   # 已有确定性 hook 数据,不用日志覆盖
            res = infer_status_from_log(f)
            if not res:
                continue
            status, idle = res
            notify_pending = 0  # 首次发现不通知;状态转换时由 UPDATE 子句置 1
            u = None
            by_day = None
            if cc_pricing:
                try:
                    by_day = cc_pricing.summarize_transcript_by_day(f)
                    u = _aggregate_usage(by_day) if by_day else None
                except Exception:
                    u = None
            ti = u["input"] if u else 0
            to = u["output"] if u else 0
            tcw = u["cache_write"] if u else 0
            tcr = u["cache_read"] if u else 0
            tt = u["total_tokens"] if u else 0
            cost = u["cost_usd"] if u else 0.0
            ck = 1 if (u.get("cost_known", True) if u else True) else 0
            conn.execute("""
                INSERT INTO sessions(session_id,cwd,project,status,last_event,
                    last_event_ts,notify_pending,notify_kind,transcript_path,source,
                    tok_input,tok_output,tok_cache_write,tok_cache_read,tok_total,cost_usd,cost_known)
                VALUES(?,?,?,?,?,?,?,?,?, 'log', ?,?,?,?,?,?,?)
                ON CONFLICT(session_id) DO UPDATE SET
                    project=excluded.project, status=excluded.status,
                    last_event_ts=excluded.last_event_ts,
                    notify_pending=CASE WHEN sessions.status != 'WAITING'
                                         AND excluded.status = 'WAITING'
                                         THEN 1 ELSE sessions.notify_pending END,
                    notify_kind=CASE WHEN sessions.status != 'WAITING'
                                      AND excluded.status = 'WAITING'
                                      THEN 'DONE' ELSE sessions.notify_kind END,
                    source='log',
                    tok_input=excluded.tok_input,
                    tok_output=excluded.tok_output,
                    tok_cache_write=excluded.tok_cache_write,
                    tok_cache_read=excluded.tok_cache_read,
                    tok_total=excluded.tok_total,
                    cost_usd=excluded.cost_usd,
                    cost_known=excluded.cost_known
                WHERE sessions.source!='hook'
            """, (sid, "", project, status, "log:"+status, now - idle,
                  notify_pending, None, f,
                  ti, to, tcw, tcr, tt, cost, ck))
            if by_day is not None and not _daily_usage_matches(conn, sid, by_day):
                _replace_daily_usage(conn, sid, by_day)
    conn.commit()


def refresh_hook_usage(conn):
    """后台刷新 hook 会话的 transcript 用量，避开 Stop hook 早于日志落盘的问题。"""
    if not cc_pricing:
        return
    rows = conn.execute("""
        SELECT session_id, transcript_path, tok_input, tok_output,
               tok_cache_write, tok_cache_read, tok_total, cost_usd, cost_known
        FROM sessions
        WHERE source='hook'
          AND transcript_path IS NOT NULL
          AND transcript_path != ''
          AND (
              status != 'ENDED'
              OR last_event_ts >= ?
              OR NOT EXISTS (
                  SELECT 1 FROM daily_session_usage
                  WHERE daily_session_usage.session_id = sessions.session_id
              )
          )
    """, (
        time.mktime(datetime.now().replace(
            hour=0, minute=0, second=0, microsecond=0).timetuple()),
    )).fetchall()
    changed = False
    for r in rows:
        try:
            by_day = cc_pricing.summarize_transcript_by_day(r["transcript_path"])
        except Exception:
            continue
        if not by_day:   # 空的 transcript，无数据可更新
            continue
        u = _aggregate_usage(by_day)
        cost_known = 1 if u.get("cost_known", True) else 0
        if (
            r["tok_input"] == u["input"]
            and r["tok_output"] == u["output"]
            and r["tok_cache_write"] == u["cache_write"]
            and r["tok_cache_read"] == u["cache_read"]
            and r["tok_total"] == u["total_tokens"]
            and abs((r["cost_usd"] or 0.0) - (u["cost_usd"] or 0.0)) < 1e-12
            and r["cost_known"] == cost_known
            and _daily_usage_matches(conn, r["session_id"], by_day)
        ):
            continue
        _replace_daily_usage(conn, r["session_id"], by_day)
        conn.execute("""
            UPDATE sessions SET
                tok_input=?,
                tok_output=?,
                tok_cache_write=?,
                tok_cache_read=?,
                tok_total=?,
                cost_usd=?,
                cost_known=?
            WHERE session_id=?
        """, (
            u["input"], u["output"], u["cache_write"], u["cache_read"],
            u["total_tokens"], u["cost_usd"], cost_known,
            r["session_id"],
        ))
        changed = True
    if changed:
        conn.commit()


# ========================= 通知(唯一出口) =========================

def _shell_escape_applescript_text(s):
    return (s or "").replace("\\", "\\\\").replace('"', '\\"')


def _terminal_notifier_path():
    tn = shutil.which("terminal-notifier")
    if tn:
        return tn
    for cand in (
        "/opt/homebrew/bin/terminal-notifier",
        "/usr/local/bin/terminal-notifier",
    ):
        if os.path.isfile(cand) and os.access(cand, os.X_OK):
            return cand
    return None


def macos_notify(title, subtitle, text, transcript_path=None, client_bundle_id=None):
    tn = _terminal_notifier_path()
    if tn:
        try:
            cmd = [
                tn,
                "-title", title or "",
                "-subtitle", subtitle or "",
                "-message", text or "",
                "-sound", "Glass",
            ]
            if client_bundle_id:
                cmd += ["-activate", client_bundle_id]
            # 不再默认打开 transcript；用户点击通知应回到对应终端/客户端界面。
            subprocess.run(cmd, check=False)
            return
        except Exception:
            pass

    try:
        t = _shell_escape_applescript_text(text)
        tt = _shell_escape_applescript_text(title)
        st = _shell_escape_applescript_text(subtitle)
        subprocess.run([
            "osascript", "-e",
            f'display notification "{t}" with title "{tt}" subtitle "{st}" sound name "Glass"'
        ], check=False)
    except Exception:
        pass


def drain_notifications(conn):
    """把所有 notify_pending=1 的会话弹一次,然后置 0(边沿触发,天然去重)。"""
    rows = conn.execute(
        "SELECT session_id,project,notify_kind,transcript_path,client_bundle_id FROM sessions WHERE notify_pending=1"
    ).fetchall()
    for r in rows:
        proj = r["project"] or r["session_id"][:8]
        tpath = r["transcript_path"] or ""
        cbid = (r["client_bundle_id"] if "client_bundle_id" in r.keys() else "") or ""
        if r["notify_kind"] == "NEEDS_INPUT":
            macos_notify("Claude Code 需要你", proj, "等待授权 / 输入", tpath, cbid)
        else:
            macos_notify("Claude Code 已完成", proj, "这一轮答完了 ✅", tpath, cbid)
    if rows:
        conn.execute("UPDATE sessions SET notify_pending=0 WHERE notify_pending=1")
        conn.commit()


# ========================= 汇总 =========================

def _fmt_tokens_cost_style(n):
    if not cc_pricing:
        return str(n or 0)
    s = cc_pricing.fmt_tokens(n or 0)
    return s.replace("K", "k").replace("M", "m")


def format_cost_style_line(totals):
    if not cc_pricing:
        return ""
    line = (
        f'{_fmt_tokens_cost_style(totals.get("tok_input", 0))} input, '
        f'{_fmt_tokens_cost_style(totals.get("tok_output", 0))} output, '
        f'{_fmt_tokens_cost_style(totals.get("tok_cache_read", 0))} cache read, '
        f'{_fmt_tokens_cost_style(totals.get("tok_cache_write", 0))} cache write '
        f'({cc_pricing.fmt_usd(totals.get("cost_usd", 0.0))})'
    )
    if not totals.get("cost_known", 1):
        line += " [partial]"
    return line


def _session_item_text(item):
    st = item["status"]
    icon = {"RUNNING": "🟢", "WAITING": "🟡", "NEEDS_INPUT": "🔴"}.get(st, "⚪")
    suffix = ""
    if cc_pricing and item.get("tok_total"):
        suffix = "  " + format_cost_style_line({
            "tok_input": item.get("tok_input", 0),
            "tok_output": item.get("tok_output", 0),
            "tok_cache_write": item.get("tok_cache_write", 0),
            "tok_cache_read": item.get("tok_cache_read", 0),
            "cost_usd": item.get("cost_usd", 0.0),
            "cost_known": item.get("cost_known", 1),
        })
    return (
        f'{icon} {item["project"]}  '
        f'[{st} · {item["idle"]}s · {item["tag"]}]'
        f'{suffix}'
    )


def _model_item_text(model, usage):
    line = (
        f'    · {model}  '
        f'{_fmt_tokens_cost_style(usage.get("input", 0))} in, '
        f'{_fmt_tokens_cost_style(usage.get("output", 0))} out, '
        f'{_fmt_tokens_cost_style(usage.get("cache_read", 0))} cr, '
        f'{_fmt_tokens_cost_style(usage.get("cache_write", 0))} cw '
        f'({cc_pricing.fmt_usd(usage.get("cost_usd", 0.0))})'
    )
    if not usage.get("cost_known", True):
        line += " [partial]"
    return line


def _query_daily_trend(conn, days):
    rows = conn.execute("""
        WITH RECURSIVE seq(n) AS (
            SELECT 0
            UNION ALL
            SELECT n + 1 FROM seq WHERE n + 1 < ?
        ),
        span AS (
            SELECT strftime('%Y-%m-%d', date('now', 'localtime', printf('-%d day', n))) AS day, n
            FROM seq
        )
        SELECT
            span.day AS day,
            COALESCE(SUM(d.tok_total), 0) AS tok_total,
            COALESCE(SUM(d.tok_input), 0) AS tok_input,
            COALESCE(SUM(d.tok_output), 0) AS tok_output,
            COALESCE(SUM(d.cost_usd), 0.0) AS cost_usd,
            COALESCE(MIN(d.cost_known), 1) AS cost_known
        FROM span
        LEFT JOIN daily_session_usage d ON d.day = span.day
        GROUP BY span.day, span.n
        ORDER BY span.day DESC
    """, (days,)).fetchall()
    return [dict(r) for r in rows]


def _history_header_line():
    return "    Day    Total      In      Out      Cost"


def _history_row_line(row):
    day = row.get("day", "")
    mmdd = day[5:] if len(day) >= 10 else day
    total = _fmt_tokens_cost_style(row.get("tok_total", 0))
    inp = _fmt_tokens_cost_style(row.get("tok_input", 0))
    out = _fmt_tokens_cost_style(row.get("tok_output", 0))
    cost = cc_pricing.fmt_usd(row.get("cost_usd", 0.0)) if cc_pricing else str(row.get("cost_usd", 0.0))
    line = f"    {mmdd:<5}  {total:>7}  {inp:>7}  {out:>7}  {cost:>8}"
    if not row.get("cost_known", 1):
        line += " *"
    return line


def summarize(conn):
    now = time.time()
    rows = conn.execute(
        "SELECT * FROM sessions WHERE last_event_ts > ? AND status != 'ENDED' "
        "ORDER BY last_event_ts DESC",
        (now - IDLE_HIDE_SEC,)
    ).fetchall()
    counts = {"RUNNING": 0, "WAITING": 0, "NEEDS_INPUT": 0}
    sessions = []
    for r in rows:
        # 跳过不需要监控的插件/内部会话(点目录 → 编码为 "--")
        if _is_internal_project(r["project"]):
            continue
        st = r["status"]
        counts[st] = counts.get(st, 0) + 1
        sessions.append({
            "session_id": r["session_id"],
            "project": r["project"],
            "status": st,
            "idle": int(now - r["last_event_ts"]),
            "tag": ("hook" if r["source"] == "hook" else "log"),
            "transcript_path": r["transcript_path"],
            "tok_input": (r["tok_input"] if "tok_input" in r.keys() else 0) or 0,
            "tok_output": (r["tok_output"] if "tok_output" in r.keys() else 0) or 0,
            "tok_cache_write": (r["tok_cache_write"] if "tok_cache_write" in r.keys() else 0) or 0,
            "tok_cache_read": (r["tok_cache_read"] if "tok_cache_read" in r.keys() else 0) or 0,
            "tok_total": (r["tok_total"] if "tok_total" in r.keys() else 0) or 0,
            "cost_usd": (r["cost_usd"] if "cost_usd" in r.keys() else 0.0) or 0.0,
            "cost_known": (r["cost_known"] if "cost_known" in r.keys() else 1),
        })

    trow = conn.execute(
        "SELECT COALESCE(SUM(tok_input),0) tok_input, "
        "COALESCE(SUM(tok_output),0) tok_output, "
        "COALESCE(SUM(tok_cache_write),0) tok_cache_write, "
        "COALESCE(SUM(tok_cache_read),0) tok_cache_read, "
        "COALESCE(SUM(tok_total),0) tok_total, "
        "COALESCE(SUM(cost_usd),0) cost_usd, "
        "COALESCE(MIN(cost_known),1) cost_known "
        "FROM daily_session_usage WHERE day=?",
        (today_key(),)
    ).fetchone()
    totals = {
        "tok_input": (trow["tok_input"] if trow else 0) or 0,
        "tok_output": (trow["tok_output"] if trow else 0) or 0,
        "tok_cache_write": (trow["tok_cache_write"] if trow else 0) or 0,
        "tok_cache_read": (trow["tok_cache_read"] if trow else 0) or 0,
        "tok_total": (trow["tok_total"] if trow else 0) or 0,
        "cost_usd": (trow["cost_usd"] if trow else 0.0) or 0.0,
        "cost_known": (trow["cost_known"] if trow else 1),
    }
    return counts, sessions, totals


# ========================= 菜单栏 App =========================
# 注意:类继承 rumps.App,必须放在 HAS_RUMPS 守卫内 —— 否则在没装 rumps 的
# 环境里,仅仅 import 本模块(如单元测试)就会因 class 定义求值 rumps.App 而崩。

def build_app():
    # 菜单栏图标: 手动加载 1x + 2x 避免 Retina 模糊
    def _load_icon(base_path):
        """加载 1x 和 @2x 合成 NSImage，确保 Retina 清晰。"""
        from Cocoa import NSImage, NSBitmapImageRep, NSData
        img = NSImage.alloc().initWithSize_((22, 22))
        for suffix in ("", "@2x"):
            path = base_path.replace(".png", f"{suffix}.png")
            if os.path.exists(path):
                data = NSData.dataWithContentsOfFile_(path)
                rep = NSBitmapImageRep.alloc().initWithData_(data)
                if rep:
                    rep.setSize_((22, 22))  # point size, pixels handled by scale
                    img.addRepresentation_(rep)
        return img

    def _resolve_menubar_icon_path():
        here = os.path.dirname(os.path.abspath(__file__))
        cands = [
            here,
            os.path.join(here, "..", "Resources"),
        ]
        if getattr(sys, "frozen", False):
            exe_dir = os.path.dirname(os.path.abspath(sys.executable))
            cands.extend([
                os.path.join(exe_dir, "..", "Resources"),
                os.path.join(exe_dir, "Resources"),
            ])

        checked = set()
        for base in cands:
            b = os.path.abspath(base)
            if b in checked:
                continue
            checked.add(b)
            for rel in ("assets/menubar_color.png", "menubar_color.png"):
                p = os.path.join(b, rel)
                if os.path.exists(p):
                    return p
        return None

    icon_path = _resolve_menubar_icon_path()
    icon_ns = _load_icon(icon_path) if icon_path else None

    class CCMonitor(rumps.App):
        def __init__(self):
            super().__init__("CC", icon=icon_path, template=False,
                             quit_button=None)   # 关掉自动退出键,改为手动维护
            if icon_ns is not None:
                self._icon_nsimage = icon_ns  # 替换为 Retina 合成图标
            self._last_status_render = None
            self.conn = connect()
            ensure_schema(self.conn)
            self.timer = rumps.Timer(self.tick, REFRESH_SEC)
            self.timer.start()

        def _status_button(self):
            try:
                nsapp = getattr(self, "_nsapp", None)
                item = getattr(nsapp, "nsstatusitem", None) if nsapp else None
                return item.button() if item else None
            except Exception:
                return None

        def _apply_vertical_colored_dots(self, r, w, n, tok_tag):
            """自绘整张状态图:左=图标，中=竖向三色点+计数，右=放大的 token。

            ── 为什么改成自绘 NSImage ──────────────────────────────────────
            旧实现把内容塞进 button 的 attributedTitle,但 button 同时还挂着
            icon 图片。macOS 会把 title 居中绘制到按钮上,于是和左侧图标重叠
            (截图现象);而且单个 attributedString 里无法让 token 行用比圆点
            行更大的字号。改为手动把「图标 + 竖向圆点 + 放大 token」合成到一
            张 NSImage,按像素精确摆放,彻底摆脱 image/title 自动布局的纠缠。
            """
            if not icon_path:
                return False

            payload = (r, w, n, tok_tag)
            if payload == self._last_status_render:
                return True

            btn = self._status_button()
            if btn is None:
                return False

            try:
                from AppKit import (
                    NSImage, NSColor, NSFont, NSBezierPath,
                    NSFontAttributeName, NSForegroundColorAttributeName,
                    NSImageOnly,
                )
                from Foundation import NSString, NSMakePoint, NSMakeRect

                H = 22.0                  # 菜单栏标题高度
                ICON_W = 18.0             # 图标绘制尺寸(留出上下边距)
                GAP = 5.0                 # 各区块水平间距
                DOT_R = 2.0               # 圆点半径
                DOT_NUM_GAP = 2.0         # 圆点与数字间距
                NUM_PT = 7.0              # 计数字号(单字符,小而清晰)
                TOK_PT = 12.0             # token 字号(放大)
                SRC_OVER = 2              # NSCompositingOperationSourceOver

                num_font = NSFont.monospacedDigitSystemFontOfSize_weight_(NUM_PT, 0.0)
                tok_font = NSFont.monospacedDigitSystemFontOfSize_weight_(TOK_PT, 0.0)
                txt_color = NSColor.whiteColor()

                # token 文本:去掉前导 " · " 分隔符
                tok_str = (tok_tag or "").lstrip(" ·").strip()

                def _measure(s, font):
                    attrs = {NSFontAttributeName: font,
                             NSForegroundColorAttributeName: txt_color}
                    sz = NSString.stringWithString_(s).sizeWithAttributes_(attrs)
                    return float(sz.width), float(sz.height), attrs

                labels = [str(r), str(w), str(n)]
                num_w = 0.0
                for s in labels:
                    wd, _, _ = _measure(s, num_font)
                    num_w = max(num_w, wd)

                dot_x = ICON_W + GAP
                num_x = dot_x + DOT_R * 2 + DOT_NUM_GAP
                dots_block_right = num_x + num_w

                tok_w = 0.0
                tok_x = dots_block_right
                if tok_str:
                    tok_w, _, _ = _measure(tok_str, tok_font)
                    tok_x = dots_block_right + GAP

                total_w = (tok_x + tok_w if tok_str else dots_block_right) + 3.0

                img = NSImage.alloc().initWithSize_((total_w, H))
                img.setTemplate_(False)
                img.lockFocus()

                # 1) 左侧图标,垂直居中
                if icon_ns is not None:
                    iy = (H - ICON_W) / 2.0
                    icon_ns.drawInRect_fromRect_operation_fraction_(
                        NSMakeRect(0.0, iy, ICON_W, ICON_W),
                        NSMakeRect(0.0, 0.0, 0.0, 0.0), SRC_OVER, 1.0)

                # 2) 竖向三色点 + 计数(非翻转坐标:y 越大越靠上 → 绿在顶)
                # 三行中心 y 等距分布,行距 7pt,确保 7pt 数字相互不粘连。
                rows = [
                    (17.0, NSColor.systemGreenColor(), labels[0]),
                    (10.0, NSColor.systemYellowColor(), labels[1]),
                    (3.0,  NSColor.systemRedColor(),    labels[2]),
                ]
                num_attrs = {NSFontAttributeName: num_font,
                             NSForegroundColorAttributeName: txt_color}
                for cy, color, s in rows:
                    color.set()
                    NSBezierPath.bezierPathWithOvalInRect_(
                        NSMakeRect(dot_x, cy - DOT_R, DOT_R * 2, DOT_R * 2)).fill()
                    _, nh, _ = _measure(s, num_font)
                    NSString.stringWithString_(s).drawAtPoint_withAttributes_(
                        NSMakePoint(num_x, cy - nh / 2.0), num_attrs)

                # 3) 右侧放大的 token,垂直居中
                if tok_str:
                    tok_attrs = {NSFontAttributeName: tok_font,
                                 NSForegroundColorAttributeName: txt_color}
                    _, th, _ = _measure(tok_str, tok_font)
                    NSString.stringWithString_(tok_str).drawAtPoint_withAttributes_(
                        NSMakePoint(tok_x, (H - th) / 2.0), tok_attrs)

                img.unlockFocus()

                # 用合成图整体替换按钮内容,清空 title 避免二次叠加
                btn.setImage_(img)
                btn.setImagePosition_(NSImageOnly)
                btn.setTitle_("")
                self._last_status_render = (r, w, n, tok_tag)
                return True
            except Exception:
                return False

        def cleanup_quit(self, _):
            """关闭数据库连接后再退出。"""
            try:
                self.conn.close()
            except Exception:
                pass
            rumps.quit_application(None)

        def tick(self, _):
            try:
                merge_log_fallback(self.conn)
                refresh_hook_usage(self.conn)
                drain_notifications(self.conn)
                counts, sessions, totals = summarize(self.conn)
            except Exception:
                self.title = " ⚠️"
                return
            r, w, n = counts["RUNNING"], counts["WAITING"], counts["NEEDS_INPUT"]
            # 有图标时只显示数字,无图标时回退到 "CC" 前缀
            prefix = "" if icon_path else "CC "
            tok_tag = ""
            if cc_pricing and totals["tok_total"]:
                tok_tag = f' · {cc_pricing.fmt_tokens(totals["tok_total"])}'
            # 竖向彩色点+数字;失败时回退到单行
            if not self._apply_vertical_colored_dots(r, w, n, tok_tag):
                self.title = f"{prefix}●{r} ●{w} ●{n}" + tok_tag
            head = f"运行中 {r} · 待处理 {w} · 需介入 {n}"
            if cc_pricing and totals["tok_total"]:
                head += f"   今日 {format_cost_style_line(totals)}"

            menu = [head, None]

            hist_7 = rumps.MenuItem("最近7天")
            hist_7.add(_history_header_line())
            for row in _query_daily_trend(self.conn, 7):
                hist_7.add(_history_row_line(row))

            hist_30 = rumps.MenuItem("最近30天")
            hist_30.add(_history_header_line())
            for row in _query_daily_trend(self.conn, 30):
                hist_30.add(_history_row_line(row))

            hist_parent = rumps.MenuItem("历史 Token 趋势")
            hist_parent.add(hist_7)
            hist_parent.add(hist_30)
            menu.append(hist_parent)

            menu.append(None)

            if not sessions:
                menu.append("(暂无活跃会话)")
            else:
                for s in sessions:
                    parent = rumps.MenuItem(_session_item_text(s))
                    if cc_pricing:
                        tpath = s.get("transcript_path") or ""
                        by_model = cc_pricing.summarize_transcript_by_model(tpath) if tpath else {}
                        if by_model:
                            items = sorted(
                                by_model.items(),
                                key=lambda kv: kv[1].get("total_tokens", 0),
                                reverse=True,
                            )
                            for model, usage in items:
                                parent.add(_model_item_text(model, usage))
                        else:
                            parent.add("(暂无模型用量明细)")
                    menu.append(parent)

            # 每次重建都手动补回「退出」,否则 clear() 会把它清掉
            menu += [None, rumps.MenuItem("退出 CC Monitor",
                                          callback=self.cleanup_quit)]
            self.menu.clear()
            self.menu = menu

    return CCMonitor()


# ========================= CLI 兜底(无 rumps 时) =========================

def run_cli():
    conn = connect(); ensure_schema(conn)
    print("无 rumps,降级为终端模式。Ctrl+C 退出。")
    try:
        while True:
            merge_log_fallback(conn)
            refresh_hook_usage(conn)
            drain_notifications(conn)
            counts, sessions, totals = summarize(conn)
            os.system("clear")
            extra = ""
            if cc_pricing and totals["tok_total"]:
                extra = f"  |  今日 {format_cost_style_line(totals)}"
            print(f"[{datetime.now():%H:%M:%S}] "
                  f"运行中 {counts['RUNNING']} · 待处理 {counts['WAITING']} "
                  f"· 需介入 {counts['NEEDS_INPUT']}{extra}\n")
            for s in sessions:
                print(" ", _session_item_text(s))
            time.sleep(REFRESH_SEC)
    except KeyboardInterrupt:
        pass
    finally:
        conn.close()


if __name__ == "__main__":
    if HAS_RUMPS:
        build_app().run()
    else:
        run_cli()