#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
cc_hook.py —— Claude Code Hook 上报端(确定性事件源)

由 Claude Code 的 hooks 调用,从 stdin 读取事件 JSON,把"哪个会话、在干嘛"
确定性地写入共享 SQLite。它是一个**短命进程**:做完就退出,绝不阻塞 CC。

设计原则:
  - 零三方依赖(只用标准库),保证在任何 CC 环境都能跑起来。
  - 单一职责:只写库,不弹通知(通知由常驻的 cc_monitor.py 统一负责、统一去重)。
  - 并发安全:多个 CC 会话会同时调它 → WAL + busy_timeout + 重试。
  - 永不让 CC 卡住:任何异常都吞掉并 exit 0。

注册方式(写入 ~/.claude/settings.json,见文末 SETTINGS_SNIPPET):
  对 Stop / Notification / SessionStart / SessionEnd / UserPromptSubmit /
  PostToolUse / PreToolUse(AskUserQuestion) 这几个事件各挂一条:
      python3 /绝对路径/cc_hook.py
"""

import sys
import os
import json
import time
import sqlite3

DB_DIR = os.path.expanduser("~/.cc-monitor")
DB_PATH = os.path.join(DB_DIR, "state.db")

# CC 事件 → 会话状态 的确定性映射
#   RUNNING     正在干活
#   WAITING     一轮答完,等你下一句(Stop)→ 触发"完成"通知
#   NEEDS_INPUT 需要你授权/补充输入(Notification)→ 触发"需介入"通知
#   ENDED       会话结束
EVENT_TO_STATUS = {
    "SessionStart":     "WAITING",     # 起会话,等首条 prompt
    "UserPromptSubmit": "RUNNING",     # 你发了一句,它开始干
    "PreToolUse":       "RUNNING",     # 心跳; AskUserQuestion 会在 upsert 中特殊处理
    "PostToolUse":      "RUNNING",     # 心跳
    "Notification":     "NEEDS_INPUT", # 要授权 / 长时间等待
    "Stop":             "WAITING",     # ★ 一轮结束 → 关键通知信号
    "StopFailure":      "WAITING",     # 出错也算停了,需要你看一眼
    "SessionEnd":       "ENDED",
}

def connect():
    os.makedirs(DB_DIR, exist_ok=True)
    conn = sqlite3.connect(DB_PATH, timeout=10)
    conn.execute("PRAGMA journal_mode=WAL;")
    conn.execute("PRAGMA busy_timeout=5000;")
    conn.execute("PRAGMA synchronous=NORMAL;")
    return conn


def ensure_schema(conn):
    conn.executescript("""
    CREATE TABLE IF NOT EXISTS sessions (
        session_id      TEXT PRIMARY KEY,
        cwd             TEXT,
        project         TEXT,
        status          TEXT,
        last_event      TEXT,
        last_event_ts   REAL,
        turn_started_ts REAL,
        notify_pending  INTEGER DEFAULT 0,  -- 1=有待弹通知,App 弹完置 0
        notify_kind     TEXT,               -- DONE / NEEDS_INPUT
        transcript_path TEXT,
        source          TEXT DEFAULT 'hook'
    );
    CREATE TABLE IF NOT EXISTS events (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id  TEXT,
        event       TEXT,
        ts          REAL
    );
    """)


def is_ask_user_question(payload):
    return (
        payload.get("hook_event_name") == "PreToolUse"
        and payload.get("tool_name") == "AskUserQuestion"
    )


def get_previous_session(conn, sid):
    row = conn.execute(
        "SELECT status,last_event,turn_started_ts FROM sessions WHERE session_id=?",
        (sid,),
    ).fetchone()
    if not row:
        return None, None, None
    return row[0], row[1], row[2]


def upsert(conn, payload):
    sid   = payload.get("session_id") or "unknown"
    event = payload.get("hook_event_name") or "unknown"
    cwd   = payload.get("cwd") or ""
    tpath = payload.get("transcript_path") or ""
    project = os.path.basename(cwd.rstrip("/")) if cwd else "(unknown)"
    now = time.time()

    ask_user_question = is_ask_user_question(payload)
    status = "NEEDS_INPUT" if ask_user_question else EVENT_TO_STATUS.get(event, "RUNNING")
    previous_status, previous_event, previous_turn_started = get_previous_session(conn, sid)

    # 通知种类: Stop/StopFailure → DONE; Notification/AskUserQuestion → NEEDS_INPUT
    # 基于事件而非派生状态，避免 SessionStart 也触发 DONE 通知
    notify_kind = None
    notify_pending = 0
    if event in ("Stop", "StopFailure"):
        if previous_status == "NEEDS_INPUT" and previous_event == "PreToolUse":
            status = "NEEDS_INPUT"
        else:
            notify_kind, notify_pending = "DONE", 1
    elif ask_user_question:
        notify_kind, notify_pending = "NEEDS_INPUT", 1
    elif event == "Notification":
        # AskUserQuestion 已设为 NEEDS_INPUT 时不重复通知
        if previous_status != "NEEDS_INPUT":
            notify_kind, notify_pending = "NEEDS_INPUT", 1

    # 用户回应后清除待通知，避免 drain 在用户已回应后弹窗
    if event == "UserPromptSubmit":
        conn.execute("UPDATE sessions SET notify_pending=0 WHERE session_id=?", (sid,))

    # turn_started_ts:开始新一轮时记一次,用于算"这轮跑了多久"
    turn_started = now if event == "UserPromptSubmit" else None
    if turn_started is None:
        turn_started = previous_turn_started

    conn.execute("""
        INSERT INTO sessions
            (session_id, cwd, project, status, last_event, last_event_ts,
             turn_started_ts, notify_pending, notify_kind, transcript_path, source)
        VALUES (?,?,?,?,?,?,?,?,?,?, 'hook')
        ON CONFLICT(session_id) DO UPDATE SET
            cwd=excluded.cwd,
            project=excluded.project,
            status=excluded.status,
            last_event=excluded.last_event,
            last_event_ts=excluded.last_event_ts,
            turn_started_ts=excluded.turn_started_ts,
            -- 通知是"取或":新事件要求弹,就置 1;不主动清(清由 App 负责)
            notify_pending=MAX(sessions.notify_pending, excluded.notify_pending),
            notify_kind=CASE WHEN excluded.notify_pending=1
                             THEN excluded.notify_kind ELSE sessions.notify_kind END,
            transcript_path=excluded.transcript_path,
            source='hook'
    """, (sid, cwd, project, status, event, now,
          turn_started, notify_pending, notify_kind, tpath))

    conn.execute("INSERT INTO events(session_id,event,ts) VALUES(?,?,?)",
                 (sid, event, now))
    conn.commit()


def main():
    raw = ""
    try:
        raw = sys.stdin.read()
        payload = json.loads(raw) if raw.strip() else {}
    except Exception:
        payload = {}

    for attempt in range(3):
        conn = None
        try:
            conn = connect()
            ensure_schema(conn)
            upsert(conn, payload)
            break
        except sqlite3.OperationalError:
            time.sleep(0.2 * (attempt + 1))  # 锁竞争,退避重试
        except Exception:
            break  # 任何其它异常都不能影响 CC
        finally:
            if conn:
                conn.close()

    sys.exit(0)  # 永远成功退出,绝不阻断 Claude Code


if __name__ == "__main__":
    main()
