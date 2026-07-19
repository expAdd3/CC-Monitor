import os
import tempfile
import unittest
from unittest import mock

import cc_monitor


class SingleInstanceTests(unittest.TestCase):
    def test_second_lock_is_rejected(self):
        with tempfile.TemporaryDirectory() as tempdir:
            path = os.path.join(tempdir, "cc-monitor.lock")
            with mock.patch.object(cc_monitor, "INSTANCE_LOCK_PATH", path):
                with mock.patch.object(cc_monitor, "_instance_lock_file", None):
                    self.assertTrue(cc_monitor.acquire_instance_lock())
                    first = cc_monitor._instance_lock_file
                    try:
                        cc_monitor._instance_lock_file = None
                        self.assertFalse(cc_monitor.acquire_instance_lock())
                    finally:
                        first.close()


if __name__ == "__main__":
    unittest.main()
