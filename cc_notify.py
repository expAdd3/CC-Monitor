#!/usr/bin/env python3
"""CC-Monitor 的零依赖远程通知模块。"""

import base64
import copy
import json
import os
import socket
import tempfile
import urllib.error
import urllib.parse
import urllib.request

CONFIG_DIR = os.path.expanduser("~/.cc-monitor")
CONFIG_PATH = os.path.join(CONFIG_DIR, "notify.json")

DEFAULT_CONFIG = {
    "enabled": False,
    "hostname": "",
    "priority_mapping": {
        "DONE": "default",
        "NEEDS_INPUT": "urgent",
    },
    "backends": [],
}

_STATUS_TEXT = {"DONE": "已完成", "NEEDS_INPUT": "需要输入"}
_NTFY_TAG = {"DONE": "white_check_mark", "NEEDS_INPUT": "warning"}
_NTFY_PRIORITY = {
    "min": 1,
    "low": 2,
    "default": 3,
    "high": 4,
    "urgent": 5,
}


def validate_server_url(server):
    """校验 ntfy 地址格式。"""
    try:
        parsed = urllib.parse.urlsplit(server)
    except ValueError as exc:
        raise ValueError("ntfy server 地址格式无效") from exc
    if parsed.scheme not in ("http", "https") or not parsed.hostname:
        raise ValueError("ntfy server 必须是完整的 HTTP(S) 地址")
    if parsed.username or parsed.password:
        raise ValueError("ntfy server 地址不能包含用户名或密码")
    if parsed.query or parsed.fragment:
        raise ValueError("ntfy server 地址不能包含查询参数或片段")
    return parsed


def load_config():
    """读取 notify.json，并用默认值补齐缺失字段。"""
    merged = copy.deepcopy(DEFAULT_CONFIG)
    try:
        with open(CONFIG_PATH, encoding="utf-8") as fh:
            user = json.load(fh)
    except (FileNotFoundError, OSError, json.JSONDecodeError, TypeError):
        return merged
    if not isinstance(user, dict):
        return merged
    for key, value in user.items():
        if key == "message_format":
            continue  # 旧配置字段；通知现已统一为带会话的详细模板
        if key == "priority_mapping" and isinstance(value, dict):
            merged[key].update(value)
        else:
            merged[key] = value
    return merged


def save_config(config):
    """原子写入配置，避免应用退出时留下半个 JSON 文件。"""
    os.makedirs(CONFIG_DIR, mode=0o700, exist_ok=True)
    os.chmod(CONFIG_DIR, 0o700)
    fd, tmp_path = tempfile.mkstemp(
        prefix=".notify.",
        suffix=".tmp",
        dir=CONFIG_DIR,
    )
    try:
        os.chmod(tmp_path, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            fd = -1
            json.dump(config, fh, indent=2, ensure_ascii=False)
            fh.write("\n")
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp_path, CONFIG_PATH)
        os.chmod(CONFIG_PATH, 0o600)
    except Exception:
        if fd >= 0:
            os.close(fd)
        try:
            os.unlink(tmp_path)
        except FileNotFoundError:
            pass
        raise


def _row_value(row, key, default=""):
    try:
        value = row[key]
    except (KeyError, IndexError, TypeError):
        try:
            value = row.get(key, default)
        except AttributeError:
            value = default
    return default if value is None else value


def _hostname(config):
    return config.get("hostname") or socket.gethostname()


def _message(host, session_id=""):
    # 项目和状态已经完整出现在 ntfy title 中，正文只放补充信息，
    # 避免系统通知连续两三行重复同一内容。
    session = str(session_id or "未知")[:12]
    return f"会话：{session}\n主机：{host or '未知'}"


def _send_ntfy(backend, title, message, priority, tags):
    server = str(backend.get("server", "")).strip().rstrip("/")
    topic = str(backend.get("topic", "")).strip()
    if not server or not topic:
        raise ValueError("ntfy server 和 topic 不能为空")
    validate_server_url(server)
    # 使用 ntfy JSON API，标题和正文可原样保留中文与 emoji。
    payload = json.dumps({
        "topic": topic,
        "title": title,
        "message": message,
        # ntfy 的 HTTP Priority header 接受字符串，但 JSON API 要求 1–5。
        "priority": _NTFY_PRIORITY.get(str(priority).lower(), 3),
        "tags": [tags],
    }, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(server, data=payload, method="POST")
    request.add_header("Content-Type", "application/json; charset=utf-8")
    username = backend.get("username", "")
    password = backend.get("password", "")
    if username or password:
        token = base64.b64encode(
            f"{username}:{password}".encode("utf-8")
        ).decode("ascii")
        request.add_header("Authorization", f"Basic {token}")
    try:
        with urllib.request.urlopen(request, timeout=8) as response:
            if not 200 <= response.status < 300:
                raise RuntimeError(f"ntfy 返回 HTTP {response.status}")
    except urllib.error.HTTPError as exc:
        try:
            detail = exc.read().decode("utf-8", errors="replace").strip()
        except Exception:
            detail = ""
        suffix = f"：{detail}" if detail else ""
        raise RuntimeError(f"ntfy 返回 HTTP {exc.code}{suffix}") from exc


BACKENDS = {"ntfy": _send_ntfy}


def _send_one(backend, title, message, priority, tags):
    backend_type = backend.get("type", "ntfy")
    sender = BACKENDS.get(backend_type)
    if sender is None:
        raise ValueError(f"不支持的通知后端：{backend_type}")
    sender(backend, title, message, priority, tags)


def send_notifications(rows, config=None):
    """向所有后端发送；全部尝试失败时抛错，供主程序执行退避提示。"""
    config = config or load_config()
    backends = config.get("backends") or []
    if not config.get("enabled") or not backends:
        return
    host = _hostname(config)
    priorities = config.get("priority_mapping") or {}
    attempts = successes = 0
    last_error = None
    for row in rows:
        project = _row_value(row, "project") or str(
            _row_value(row, "session_id")
        )[:8]
        kind = _row_value(row, "notify_kind", "DONE")
        session_id = _row_value(row, "session_id")
        # ntfy 会把 tags 渲染为标题前的图标，标题自身不再重复放 emoji。
        title = f"{project} · {_STATUS_TEXT.get(kind, kind)}"
        body = _message(host, session_id)
        priority = priorities.get(kind, "default")
        tags = _NTFY_TAG.get(kind, "loudspeaker")
        for backend in backends:
            attempts += 1
            try:
                _send_one(backend, title, body, priority, tags)
                successes += 1
            except Exception as exc:
                last_error = exc
    if attempts and not successes:
        raise RuntimeError(f"远程通知全部发送失败：{last_error}")


def send_test(backend, config=None):
    config = config or load_config()
    host = _hostname(config)
    _send_one(
        backend,
        "CC Monitor 测试通知",
        _message(host, "测试通知"),
        "default",
        "test_tube",
    )
