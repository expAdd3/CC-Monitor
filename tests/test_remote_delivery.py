import sqlite3
import unittest
from unittest import mock

import cc_monitor


class RemoteDeliveryTests(unittest.TestCase):
    def setUp(self):
        self.conn = sqlite3.connect(":memory:")
        self.conn.row_factory = sqlite3.Row
        cc_monitor.ensure_schema(self.conn)
        cc_monitor._remote_fail_count = 0
        cc_monitor._remote_had_failure = False

    def tearDown(self):
        self.conn.close()
        cc_monitor._remote_fail_count = 0
        cc_monitor._remote_had_failure = False

    def insert_pending(self, kind="DONE"):
        self.conn.execute(
            "INSERT INTO sessions("
            "session_id,project,status,last_event_ts,notify_pending,notify_kind,"
            "transcript_path,client_bundle_id"
            ") VALUES(?,?,?,?,?,?,?,?)",
            (
                f"session-{kind}",
                "project",
                "WAITING",
                1.0,
                1,
                kind,
                "/tmp/transcript.jsonl",
                "com.example.client",
            ),
        )
        self.conn.commit()

    def test_drain_sends_local_clears_queue_and_hands_off_remote(self):
        self.insert_pending("DONE")
        self.insert_pending("NEEDS_INPUT")
        with mock.patch.object(cc_monitor, "macos_notify") as local:
            with mock.patch.object(cc_monitor, "_send_remote_async") as remote:
                cc_monitor.drain_notifications(self.conn)
        self.assertEqual(local.call_count, 2)
        self.assertEqual(remote.call_count, 1)
        rows = remote.call_args.args[0]
        self.assertEqual(
            {row["notify_kind"] for row in rows},
            {"DONE", "NEEDS_INPUT"},
        )
        pending = self.conn.execute(
            "SELECT COUNT(*) FROM sessions WHERE notify_pending=1"
        ).fetchone()[0]
        self.assertEqual(pending, 0)

    def test_drain_empty_queue_does_nothing(self):
        with mock.patch.object(cc_monitor, "macos_notify") as local:
            with mock.patch.object(cc_monitor, "_send_remote_async") as remote:
                cc_monitor.drain_notifications(self.conn)
        local.assert_not_called()
        remote.assert_not_called()

    def test_async_sender_skips_disabled_config(self):
        config = {"enabled": False, "backends": [{"type": "ntfy"}]}
        with mock.patch.object(
            cc_monitor.cc_notify, "load_config", return_value=config
        ):
            with mock.patch.object(cc_monitor.threading, "Thread") as thread:
                cc_monitor._send_remote_async([{"session_id": "s"}])
        thread.assert_not_called()

    def test_async_sender_starts_named_daemon_thread(self):
        config = {"enabled": True, "backends": [{"type": "ntfy"}]}
        with mock.patch.object(
            cc_monitor.cc_notify, "load_config", return_value=config
        ):
            with mock.patch.object(cc_monitor.threading, "Thread") as thread:
                cc_monitor._send_remote_async([{"session_id": "s"}])
        kwargs = thread.call_args.kwargs
        self.assertEqual(kwargs["target"], cc_monitor._do_send_remote)
        self.assertEqual(kwargs["name"], "cc-monitor-notify")
        self.assertTrue(kwargs["daemon"])
        thread.return_value.start.assert_called_once()

    def test_failure_notices_stop_after_three_and_success_reports_recovery(self):
        config = {"enabled": True, "backends": [{"type": "ntfy"}]}
        rows = [{"session_id": "s", "notify_kind": "DONE"}]
        with mock.patch.object(cc_monitor, "macos_notify") as local:
            with mock.patch.object(
                cc_monitor.cc_notify,
                "send_notifications",
                side_effect=OSError("offline"),
            ):
                for _ in range(4):
                    cc_monitor._do_send_remote(rows, config)
            self.assertEqual(local.call_count, 3)
            self.assertEqual(
                [call.args[2] for call in local.call_args_list],
                [
                    "发送失败（连续 1 次）⚠️",
                    "发送失败（连续 2 次）⚠️",
                    "发送失败（连续 3 次）⚠️",
                ],
            )
            self.assertEqual(cc_monitor._remote_fail_count, 4)
            self.assertTrue(cc_monitor._remote_had_failure)

            with mock.patch.object(
                cc_monitor.cc_notify, "send_notifications"
            ):
                cc_monitor._do_send_remote(rows, config)

        self.assertEqual(local.call_count, 4)
        self.assertIn("已恢复", local.call_args.args[2])
        self.assertEqual(cc_monitor._remote_fail_count, 0)
        self.assertFalse(cc_monitor._remote_had_failure)


if __name__ == "__main__":
    unittest.main()
