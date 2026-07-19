import sqlite3
import unittest

import cc_hook


class HookNotificationTests(unittest.TestCase):
    def setUp(self):
        self.conn = sqlite3.connect(":memory:")
        cc_hook.ensure_schema(self.conn)
        self.base = {
            "session_id": "session-1",
            "cwd": "/tmp/project",
            "transcript_path": "/tmp/session.jsonl",
        }

    def tearDown(self):
        self.conn.close()

    def emit(self, event, **extra):
        cc_hook.upsert(
            self.conn,
            {**self.base, "hook_event_name": event, **extra},
        )
        return self.conn.execute(
            "SELECT status,last_event,notify_pending,notify_kind "
            "FROM sessions WHERE session_id=?",
            (self.base["session_id"],),
        ).fetchone()

    def test_idle_prompt_after_consumed_stop_does_not_notify_again(self):
        self.emit("UserPromptSubmit")
        self.emit("Stop")
        self.conn.execute(
            "UPDATE sessions SET notify_pending=0 WHERE session_id='session-1'"
        )
        row = self.emit("Notification", notification_type="idle_prompt")
        self.assertEqual(row[0], "WAITING")
        self.assertEqual(row[1], "Notification:idle_prompt")
        self.assertEqual(row[2], 0)

    def test_permission_prompt_is_actionable(self):
        self.emit("UserPromptSubmit")
        row = self.emit(
            "Notification", notification_type="permission_prompt")
        self.assertEqual(row[0], "NEEDS_INPUT")
        self.assertEqual(row[1], "Notification:permission_prompt")
        self.assertEqual(row[2], 1)
        self.assertEqual(row[3], "NEEDS_INPUT")

    def test_auth_success_is_not_actionable(self):
        row = self.emit("Notification", notification_type="auth_success")
        self.assertEqual(row[0], "RUNNING")
        self.assertEqual(row[2], 0)

    def test_all_known_non_actionable_notification_types_are_suppressed(self):
        for index, notification_type in enumerate(
            sorted(cc_hook.NON_ACTIONABLE_NOTIFICATION_TYPES)
        ):
            with self.subTest(notification_type=notification_type):
                self.base["session_id"] = f"non-actionable-{index}"
                row = self.emit(
                    "Notification", notification_type=notification_type
                )
                self.assertEqual(row[2], 0)

    def test_elicitation_dialog_is_actionable(self):
        row = self.emit(
            "Notification", notification_type="elicitation_dialog"
        )
        self.assertEqual(row[0], "NEEDS_INPUT")
        self.assertEqual(row[2], 1)
        self.assertEqual(row[3], "NEEDS_INPUT")

    def test_unknown_notification_type_remains_actionable(self):
        row = self.emit(
            "Notification", notification_type="future_notification_type"
        )
        self.assertEqual(row[0], "NEEDS_INPUT")
        self.assertEqual(row[2], 1)


if __name__ == "__main__":
    unittest.main()
