import json
import io
import os
import tempfile
import unittest
import urllib.error
from unittest import mock

import cc_notify
import cc_monitor


class NotifyConfigTests(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.config_dir = self.tempdir.name
        self.config_path = os.path.join(self.config_dir, "notify.json")
        self.path_patch = mock.patch.multiple(
            cc_notify,
            CONFIG_DIR=self.config_dir,
            CONFIG_PATH=self.config_path,
        )
        self.path_patch.start()

    def tearDown(self):
        self.path_patch.stop()
        self.tempdir.cleanup()

    def test_missing_config_uses_independent_defaults(self):
        first = cc_notify.load_config()
        first["priority_mapping"]["DONE"] = "urgent"
        self.assertEqual(
            cc_notify.load_config()["priority_mapping"]["DONE"], "default"
        )

    def test_save_and_load_round_trip(self):
        config = cc_notify.load_config()
        config["enabled"] = True
        config["backends"] = [{
            "type": "ntfy",
            "server": "https://ntfy.example.com",
            "topic": "test-topic",
            "username": "alice",
            "password": "secret",
        }]
        cc_notify.save_config(config)
        self.assertEqual(cc_notify.load_config(), config)
        self.assertEqual(os.stat(self.config_dir).st_mode & 0o777, 0o700)
        self.assertEqual(os.stat(self.config_path).st_mode & 0o777, 0o600)
        self.assertFalse(any(name.endswith(".tmp") for name in os.listdir(
            self.config_dir
        )))

    def test_failed_save_removes_private_temporary_file(self):
        with mock.patch.object(
            cc_notify.json, "dump", side_effect=ValueError("invalid")
        ):
            with self.assertRaisesRegex(ValueError, "invalid"):
                cc_notify.save_config({"password": "secret"})
        self.assertFalse(any(name.endswith(".tmp") for name in os.listdir(
            self.config_dir
        )))

    def test_replace_failure_removes_temporary_file(self):
        with mock.patch.object(
            cc_notify.os, "replace", side_effect=OSError("disk failure")
        ):
            with self.assertRaisesRegex(OSError, "disk failure"):
                cc_notify.save_config({"password": "secret"})
        self.assertFalse(any(name.endswith(".tmp") for name in os.listdir(
            self.config_dir
        )))
        self.assertFalse(os.path.exists(self.config_path))

    def test_legacy_message_format_is_ignored(self):
        with open(self.config_path, "w", encoding="utf-8") as fh:
            json.dump({"message_format": "simple"}, fh)
        self.assertNotIn("message_format", cc_notify.load_config())

    def test_malformed_and_non_object_config_use_defaults(self):
        for raw in ("{broken", "[]", "null"):
            with self.subTest(raw=raw):
                with open(self.config_path, "w", encoding="utf-8") as fh:
                    fh.write(raw)
                self.assertEqual(cc_notify.load_config(), cc_notify.DEFAULT_CONFIG)


class NotifySendTests(unittest.TestCase):
    def test_message_always_contains_session_and_host(self):
        message = cc_notify._message("开发机", "session-123")
        self.assertEqual(message, "会话：session-123\n主机：开发机")

    def test_message_uses_fallbacks_for_missing_details(self):
        self.assertEqual(
            cc_notify._message("", ""),
            "会话：未知\n主机：未知",
        )

    def test_sqlite_style_row_is_formatted_and_sent(self):
        config = {
            "enabled": True,
            "hostname": "dev-mac",
            "priority_mapping": {"DONE": "default"},
            "backends": [{"type": "ntfy", "server": "https://n", "topic": "t"}],
        }
        with mock.patch.object(cc_notify, "_send_one") as sender:
            cc_notify.send_notifications([{
                "session_id": "1234567890abcdef",
                "project": "CC-Monitor",
                "notify_kind": "DONE",
            }], config=config)
        args = sender.call_args.args
        self.assertIn("CC-Monitor", args[1])
        self.assertNotIn("✅", args[1])
        self.assertIn("dev-mac", args[2])
        self.assertEqual(args[3], "default")

    def test_all_failures_raise(self):
        config = {
            "enabled": True,
            "hostname": "",
            "priority_mapping": {},
            "backends": [{"type": "ntfy", "server": "https://n", "topic": "t"}],
        }
        with mock.patch.object(
            cc_notify, "_send_one", side_effect=OSError("offline")
        ):
            with self.assertRaisesRegex(RuntimeError, "全部发送失败"):
                cc_notify.send_notifications(
                    [{"project": "p", "notify_kind": "DONE"}],
                    config=config,
                )

    def test_disabled_or_empty_backend_config_sends_nothing(self):
        rows = [{"project": "p", "notify_kind": "DONE"}]
        with mock.patch.object(cc_notify, "_send_one") as sender:
            cc_notify.send_notifications(
                rows, config={"enabled": False, "backends": [{"type": "ntfy"}]}
            )
            cc_notify.send_notifications(
                rows, config={"enabled": True, "backends": []}
            )
        sender.assert_not_called()

    def test_partial_backend_failure_does_not_hide_success(self):
        config = {
            "enabled": True,
            "backends": [{"type": "ntfy"}, {"type": "ntfy"}],
        }
        with mock.patch.object(
            cc_notify,
            "_send_one",
            side_effect=[OSError("first failed"), None],
        ) as sender:
            cc_notify.send_notifications(
                [{"project": "p", "notify_kind": "DONE"}],
                config=config,
            )
        self.assertEqual(sender.call_count, 2)

    def test_unknown_backend_is_reported(self):
        with self.assertRaisesRegex(ValueError, "不支持的通知后端"):
            cc_notify._send_one(
                {"type": "unknown"}, "标题", "正文", "default", "test_tube"
            )

    def test_ntfy_uses_json_api_and_basic_auth(self):
        backend = {
            "server": "https://ntfy.example.com/",
            "topic": "my topic",
            "username": "alice",
            "password": "secret",
        }
        response = mock.MagicMock()
        response.status = 200
        response.__enter__.return_value = response
        with mock.patch.object(
            cc_notify.urllib.request, "urlopen", return_value=response
        ) as urlopen:
            cc_notify._send_ntfy(
                backend, "中文标题 ✅", "正文", "urgent", "warning"
            )
        request = urlopen.call_args.args[0]
        payload = json.loads(request.data.decode("utf-8"))
        self.assertEqual(request.full_url, "https://ntfy.example.com")
        self.assertEqual(payload["topic"], "my topic")
        self.assertEqual(payload["title"], "中文标题 ✅")
        self.assertEqual(payload["priority"], 5)
        self.assertTrue(request.get_header("Authorization").startswith("Basic "))

    def test_ntfy_allows_remote_plaintext_http(self):
        backend = {
            "server": "http://192.0.2.10:8088",
            "topic": "alice-cc-monitor",
            "username": "alice",
            "password": "secret",
        }
        response = mock.MagicMock()
        response.status = 200
        response.__enter__.return_value = response
        with mock.patch.object(
            cc_notify.urllib.request, "urlopen", return_value=response
        ) as urlopen:
            cc_notify._send_ntfy(
                backend, "标题", "正文", "default", "warning"
            )
        urlopen.assert_called_once()

    def test_ntfy_http_error_includes_response_detail(self):
        backend = {
            "server": "https://ntfy.example.com",
            "topic": "alice-cc-monitor",
        }
        error = urllib.error.HTTPError(
            backend["server"],
            403,
            "Forbidden",
            {},
            io.BytesIO(b'{"error":"forbidden"}'),
        )
        with mock.patch.object(
            cc_notify.urllib.request, "urlopen", side_effect=error
        ):
            with self.assertRaisesRegex(RuntimeError, r"HTTP 403.*forbidden"):
                cc_notify._send_ntfy(
                    backend, "标题", "正文", "default", "warning"
                )

    def test_ntfy_rejects_missing_server_or_topic(self):
        for backend in (
            {"server": "", "topic": "topic"},
            {"server": "https://ntfy.example.com", "topic": ""},
        ):
            with self.subTest(backend=backend):
                with self.assertRaisesRegex(ValueError, "不能为空"):
                    cc_notify._send_ntfy(
                        backend, "标题", "正文", "default", "warning"
                    )

    def test_ntfy_non_2xx_response_is_reported(self):
        backend = {
            "server": "https://ntfy.example.com",
            "topic": "alice-cc-monitor",
        }
        response = mock.MagicMock()
        response.status = 500
        response.__enter__.return_value = response
        with mock.patch.object(
            cc_notify.urllib.request, "urlopen", return_value=response
        ):
            with self.assertRaisesRegex(RuntimeError, "HTTP 500"):
                cc_notify._send_ntfy(
                    backend, "标题", "正文", "default", "warning"
                )

    def test_send_test_uses_detailed_chinese_message(self):
        config = {"hostname": "dev-mac"}
        backend = {"type": "ntfy"}
        with mock.patch.object(cc_notify, "_send_one") as sender:
            cc_notify.send_test(backend, config=config)
        args = sender.call_args.args
        self.assertEqual(args[0], backend)
        self.assertEqual(args[1], "CC Monitor 测试通知")
        self.assertEqual(args[2], "会话：测试通知\n主机：dev-mac")


class NotifyServerValidationTests(unittest.TestCase):
    def test_https_is_allowed(self):
        self.assertEqual(
            cc_monitor.validate_notify_server("https://ntfy.example.com"),
            "",
        )

    def test_loopback_http_is_allowed(self):
        self.assertEqual(
            cc_monitor.validate_notify_server("http://127.0.0.1:8088"),
            "",
        )

    def test_remote_http_is_allowed(self):
        server = "http://192.0.2.10:8088"
        self.assertEqual(
            cc_monitor.validate_notify_server(server),
            "",
        )

    def test_embedded_credentials_are_rejected(self):
        self.assertIn(
            "用户名或密码",
            cc_monitor.validate_notify_server(
                "https://alice:secret@ntfy.example.com"
            ),
        )

    def test_invalid_server_addresses_are_rejected(self):
        cases = (
            "ntfy.example.com",
            "ftp://ntfy.example.com",
            "https://ntfy.example.com?token=secret",
            "https://ntfy.example.com#fragment",
            "https://[invalid",
        )
        for server in cases:
            with self.subTest(server=server):
                self.assertNotEqual(
                    cc_monitor.validate_notify_server(server),
                    "",
                )


if __name__ == "__main__":
    unittest.main()
