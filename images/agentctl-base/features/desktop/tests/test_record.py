import fcntl
import os
import tempfile
import unittest
from unittest import mock

import _path  # noqa: F401
from agent_desktop import record
from agent_desktop.session import DesktopError, Session


class StateTest(unittest.TestCase):
    def setUp(self):
        self.captures = tempfile.TemporaryDirectory()
        self.session = Session({'DESKTOP_CAPTURES': self.captures.name})

    def tearDown(self):
        self.captures.cleanup()

    def write_state(self):
        with open(self.session.recording_state, 'w') as f:
            f.write('123 1600 900 /tmp/x y.mp4\n')

    def test_a_process_another_user_owns_is_not_a_recorder(self):
        with mock.patch('os.kill', side_effect=PermissionError):
            self.assertFalse(record.alive(123))

    def test_reads_the_running_recording(self):
        self.assertFalse(record.running(self.session))
        self.write_state()
        self.assertEqual(record.read_state(self.session), (123, 1600, 900, '/tmp/x y.mp4'))
        with mock.patch.object(record, 'alive', return_value=True):
            self.assertTrue(record.running(self.session))
        with mock.patch.object(record, 'alive', return_value=False):
            self.assertFalse(record.running(self.session))

    def test_only_one_stop_claims_a_recording(self):
        self.write_state()
        with mock.patch.object(record, 'alive', return_value=False), mock.patch('time.sleep'):
            with self.assertRaisesRegex(DesktopError, 'the recording had already ended'):
                record.stop(self.session, 1280)
            with self.assertRaisesRegex(DesktopError, 'no recording is running'):
                record.stop(self.session, 1280)
        self.assertEqual(os.listdir(self.captures.name), [])

    def test_only_one_of_two_starts_goes_on(self):
        with open(self.session.recording_state + '.lock', 'w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            with self.assertRaisesRegex(DesktopError, 'another recording is starting'):
                record.start(self.session, os.path.join(self.captures.name, 'out.mp4'))

    def test_keeps_a_recording_per_display(self):
        other = Session({'DESKTOP_CAPTURES': self.captures.name, 'DESKTOP_DISPLAY': ':2'})
        self.write_state()
        with mock.patch.object(record, 'alive', return_value=True):
            self.assertTrue(record.running(self.session))
            self.assertFalse(record.running(other))

    def test_refuses_what_it_cannot_record(self):
        with self.assertRaisesRegex(DesktopError, 'must be an .mp4 file'):
            record.record(self.session, ['start', 'out.webm'])
        with self.assertRaisesRegex(DesktopError, 'no recording is running'):
            record.record(self.session, ['stop'])
        with self.assertRaisesRegex(DesktopError, 'record: takes start'):
            record.record(self.session, ['stop', '--width', 'wide'])


if __name__ == '__main__':
    unittest.main()
