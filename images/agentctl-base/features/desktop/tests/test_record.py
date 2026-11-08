import contextlib
import fcntl
import io
import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

import _path  # noqa: F401
from agent_desktop import record
from agent_desktop.session import DesktopError, Session
from agent_media.video import MediaError


class StateTest(unittest.TestCase):
    def setUp(self):
        self.captures = tempfile.TemporaryDirectory()
        self.session = Session({'DESKTOP_CAPTURES': self.captures.name})
        self.out = os.path.join(self.captures.name, 'take.mp4')

    def tearDown(self):
        self.captures.cleanup()

    def write_state(self, out='/tmp/x y.mp4'):
        with open(self.session.recording_state, 'w') as f:
            f.write(f'123 {out}\n')

    def capture(self, out=None):
        """Leaves the raw capture of OUT, or of take.mp4, as a recorder does."""
        parts = record.parts_dir(out or self.out)
        os.makedirs(parts, exist_ok=True)
        for name in ('raw.mkv', 'ffmpeg.log'):
            with open(os.path.join(parts, name), 'w'):
                pass

    def stop(self, recording, encode=None):
        """Runs `record stop` with the recorder RECORDING or not, and returns what it printed to stderr. ENCODE is what
        encoding does. self.finish is what ended the recorder."""
        err = io.StringIO()
        with mock.patch.object(record, 'recorder', return_value=recording), mock.patch('time.sleep'), \
                mock.patch.object(record, 'finish') as self.finish, \
                mock.patch('agent_media.video.encode', side_effect=encode), mock.patch('agent_media.check.check'), \
                contextlib.redirect_stderr(err):
            record.stop(self.session, 1280)
        return err.getvalue()

    def files(self):
        return sorted(name for name in os.listdir(self.captures.name) if not name.endswith('.lock'))

    def test_a_process_another_user_owns_is_not_a_recorder(self):
        with mock.patch('os.kill', side_effect=PermissionError):
            self.assertFalse(record.alive(123))

    def test_a_process_that_ended_but_is_not_reaped_has_ended(self):
        # The shell starts `true` and becomes `sleep`, which never reaps it, as a batch that started a recorder does
        # not until it ends.
        parent = subprocess.Popen(['sh', '-c', 'true & echo $!; exec sleep 30'], stdout=subprocess.PIPE, text=True)
        try:
            zombie = int(parent.stdout.readline())
            for _ in range(100):
                with open(f'/proc/{zombie}/stat') as f:
                    if f.read().rsplit(')', 1)[1].split()[0] == 'Z':
                        break
                time.sleep(0.01)
            os.kill(zombie, 0)
            self.assertFalse(record.alive(zombie))
        finally:
            parent.kill()
            parent.wait()
            parent.stdout.close()

    def test_a_recorder_that_ends_before_it_is_signalled_has_ended(self):
        ended = subprocess.Popen(['true'])
        ended.wait()
        record.finish(ended.pid)
        with mock.patch.object(record, 'alive', return_value=True), mock.patch('time.sleep'), \
                mock.patch('os.kill', side_effect=[None, ProcessLookupError]):
            record.finish(123)

    def test_reads_the_running_recording(self):
        self.assertFalse(record.running(self.session))
        self.write_state()
        self.assertEqual(record.read_state(self.session), (123, '/tmp/x y.mp4'))
        with mock.patch.object(record, 'recorder', return_value=True):
            self.assertTrue(record.running(self.session))
        with mock.patch.object(record, 'recorder', return_value=False):
            self.assertFalse(record.running(self.session))

    def test_only_one_of_two_stops_goes_on(self):
        self.write_state(self.out)
        self.capture()
        with open(self.session.recording_state + '.lock', 'w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            with self.assertRaisesRegex(DesktopError, 'another `desktop record start` or `stop` is running'):
                self.stop(True)
        self.assertEqual(record.read_state(self.session), (123, self.out))
        self.stop(True, encode=self.encode)
        with self.assertRaisesRegex(DesktopError, 'no recording is running'):
            self.stop(False)

    def test_a_stop_interrupted_before_the_recorder_ends_keeps_the_recording(self):
        self.write_state()
        with mock.patch.object(record, 'recorder', return_value=True), mock.patch('time.sleep'), \
                mock.patch.object(record, 'finish', side_effect=SystemExit(143)):
            with self.assertRaises(SystemExit):
                record.stop(self.session, 1280)
        self.assertEqual(record.read_state(self.session), (123, '/tmp/x y.mp4'))

    def test_a_stop_renders_a_recording_whose_recorder_had_ended(self):
        self.write_state(self.out)
        self.capture()
        self.assertIn('already ended; rendering what it captured', self.stop(False, encode=self.encode))
        self.finish.assert_not_called()
        self.assertEqual(self.files(), ['take.mp4'])

    def test_a_recording_that_ended_without_a_capture_is_let_go(self):
        self.write_state(self.out)
        with self.assertRaisesRegex(DesktopError, 'the recording had already ended and left nothing to render$'):
            self.stop(False)
        self.assertEqual(self.files(), [])

    def test_a_reused_process_number_is_not_the_recorder(self):
        raw = os.path.join(record.parts_dir(self.out), 'raw.mkv')
        other = subprocess.Popen(['sleep', '30'])
        # It says so once it runs the command line it was given, rather than a copy of this process's.
        ours = subprocess.Popen([sys.executable, '-c', 'print(flush=True); input()', 'ffmpeg', raw],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        ours.stdout.readline()
        try:
            self.assertFalse(record.recorder(other.pid, self.out))
            self.assertTrue(record.recorder(ours.pid, self.out))
        finally:
            for process in (other, ours):
                process.kill()
                process.wait()
            ours.stdin.close()
            ours.stdout.close()
        self.assertFalse(record.recorder(ours.pid, self.out))

    def test_a_stop_encodes_the_recording_and_checks_it(self):
        self.write_state(self.out)
        self.capture()
        with mock.patch.object(record, 'recorder', return_value=True), mock.patch('time.sleep'), \
                mock.patch.object(record, 'finish') as finish, \
                mock.patch('agent_media.video.encode', side_effect=self.encode) as encode, \
                mock.patch('agent_media.check.check') as check:
            record.stop(self.session, 960)
        finish.assert_called_once_with(123)
        raw, encoded, width = encode.call_args[0]
        self.assertEqual((raw, width), (os.path.join(record.parts_dir(self.out), 'raw.mkv'), 960))
        self.assertEqual(os.path.dirname(encoded), record.parts_dir(self.out))
        check.assert_called_once_with(self.out)
        self.assertEqual(self.files(), ['take.mp4'])

    @staticmethod
    def encode(raw, out, width, then=None):
        """Writes OUT as encoding does, and then raises THEN."""
        with open(out, 'w') as f:
            f.write('video')
        if then:
            raise then

    def test_a_stop_that_cannot_encode_leaves_the_recording_to_the_next(self):
        self.write_state(self.out)
        self.capture()
        with self.assertRaisesRegex(DesktopError, 'record: cannot encode .*take.mp4: ffmpeg: broken\n'
                                                  'Its capture is kept for `desktop record stop` to try again, and '
                                                  '`desktop record start` discards it.$'):
            self.stop(True, encode=lambda *args: self.encode(*args, then=MediaError('ffmpeg: broken')))
        self.assertEqual(record.read_state(self.session), (123, self.out))
        self.assertTrue(os.path.exists(os.path.join(record.parts_dir(self.out), 'raw.mkv')))
        self.assertFalse(os.path.exists(self.out))
        self.stop(False, encode=self.encode)
        self.assertEqual(self.files(), ['take.mp4'])

    def test_a_stop_that_cannot_write_the_video_leaves_the_recording_to_the_next(self):
        self.write_state(self.out)
        self.capture()
        with mock.patch('os.replace', side_effect=PermissionError(13, 'Permission denied')), \
                self.assertRaisesRegex(DesktopError, 'record: cannot write .*take.mp4: Permission denied\n'
                                                     'Its capture is kept for `desktop record stop` to try again'):
            self.stop(True, encode=self.encode)
        self.assertEqual(record.read_state(self.session), (123, self.out))
        self.stop(False, encode=self.encode)
        self.assertEqual(self.files(), ['take.mp4'])

    def test_a_stop_interrupted_while_encoding_leaves_the_recording_to_the_next(self):
        self.write_state(self.out)
        self.capture()
        with self.assertRaises(SystemExit):
            self.stop(True, encode=lambda *args: self.encode(*args, then=SystemExit(143)))
        self.assertEqual(record.read_state(self.session), (123, self.out))
        self.assertFalse(os.path.exists(self.out))
        self.stop(False, encode=self.encode)
        self.assertEqual(self.files(), ['take.mp4'])

    def test_only_one_of_two_starts_goes_on(self):
        with open(self.session.recording_state + '.lock', 'w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            with self.assertRaisesRegex(DesktopError, 'another `desktop record start` or `stop` is running'):
                record.start(self.session, self.out)

    def test_keeps_a_recording_per_display(self):
        other = Session({'DESKTOP_CAPTURES': self.captures.name, 'DESKTOP_DISPLAY': ':2'})
        self.write_state()
        with mock.patch.object(record, 'recorder', return_value=True):
            self.assertTrue(record.running(self.session))
            self.assertFalse(record.running(other))

    def start(self, out, session=None, recorder=None):
        """Runs `record start OUT` on SESSION, this test's by default, with RECORDER, or one that keeps running, and
        returns the Popen mock and stderr."""
        session = session or self.session
        if recorder is None:
            recorder = mock.Mock(pid=4321)
            recorder.poll.return_value = None
        session.geometry = lambda: mock.Mock(width=1600, height=900)
        err = io.StringIO()
        with mock.patch('subprocess.Popen', return_value=recorder) as popen, mock.patch('time.sleep'), \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
            record.start(session, out)
        return popen, err.getvalue()

    def test_start_records_what_stop_needs(self):
        popen, _ = self.start(self.out)
        self.assertIn('x11grab', popen.call_args[0][0])
        self.assertEqual(record.read_state(self.session), (4321, self.out))
        self.assertTrue(os.path.isdir(record.parts_dir(self.out)))

    def test_start_says_which_unrendered_recording_it_replaces(self):
        earlier = os.path.join(self.captures.name, 'earlier.mp4')
        self.write_state(earlier)
        self.capture(earlier)
        with mock.patch.object(record, 'recorder', return_value=False):
            _, err = self.start(self.out)
        self.assertEqual(err, f'desktop: record: discarding the recording of {earlier}, which `desktop record stop` '
                              'would have rendered\n')
        self.assertFalse(os.path.exists(record.parts_dir(earlier)))
        self.assertEqual(record.read_state(self.session), (4321, self.out))

    def test_start_replaces_its_own_unrendered_recording_of_the_same_file(self):
        self.write_state(self.out)
        self.capture()
        with mock.patch.object(record, 'recorder', return_value=False):
            _, err = self.start(self.out)
        self.assertIn(f'discarding the recording of {self.out}', err)
        self.assertEqual(record.read_state(self.session), (4321, self.out))

    def test_start_keeps_an_unrendered_capture_beside_other_files_and_says_so(self):
        earlier = os.path.join(self.captures.name, 'earlier.mp4')
        self.write_state(earlier)
        self.capture(earlier)
        with open(os.path.join(record.parts_dir(earlier), 'notes.txt'), 'w'):
            pass
        with mock.patch.object(record, 'recorder', return_value=False):
            _, err = self.start(self.out)
        self.assertEqual(err, f'desktop: record: letting go of the recording of {earlier} without rendering it; its '
                              f'capture is left in {record.parts_dir(earlier)}, which holds other files too\n')
        self.assertTrue(os.path.exists(os.path.join(record.parts_dir(earlier), 'raw.mkv')))
        self.assertEqual(record.read_state(self.session), (4321, self.out))

    def test_start_leaves_alone_a_capture_another_display_is_recording(self):
        other = Session({'DESKTOP_CAPTURES': self.captures.name, 'DESKTOP_DISPLAY': ':92'})
        raw = os.path.join(record.parts_dir(self.out), 'raw.mkv')
        # A stand-in for the other display's ffmpeg, which says so once it runs with the capture on its command line.
        recording = subprocess.Popen([sys.executable, '-c', 'print(flush=True); input()', 'ffmpeg', raw],
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        recording.stdout.readline()
        try:
            with open(other.recording_state, 'w') as f:
                f.write(f'{recording.pid} {self.out}\n')
            with self.assertRaisesRegex(DesktopError, f'record: another recording is writing to {raw}; '
                                                      'record to another file'):
                self.start(self.out)
            self.assertTrue(record.recorder(recording.pid, self.out))
            self.assertEqual(record.read_state(other), (recording.pid, self.out))
            self.assertIsNone(record.read_state(self.session))
        finally:
            recording.kill()
            recording.wait()
            recording.stdin.close()
            recording.stdout.close()

    def test_start_leaves_alone_a_capture_another_display_has_not_rendered(self):
        other = Session({'DESKTOP_CAPTURES': self.captures.name, 'DESKTOP_DISPLAY': ':92'})
        self.start(self.out, session=other)
        self.capture()
        with self.assertRaisesRegex(DesktopError, f'record: {record.parts_dir(self.out)} holds a capture that '
                                                  '`desktop record stop` has not rendered; render it on the display '
                                                  'that recorded it, or remove it, or record to another file'):
            self.start(self.out)
        self.assertEqual(record.read_state(other), (4321, self.out))
        self.assertTrue(os.path.exists(os.path.join(record.parts_dir(self.out), 'raw.mkv')))

    def test_start_refuses_a_file_name_it_cannot_write_down(self):
        out = os.fsdecode(os.path.join(os.fsencode(self.captures.name), b'take-\xff.mp4'))
        with self.assertRaisesRegex(DesktopError, 'record: the recording\'s path has to be UTF-8'):
            popen, _ = self.start(out)
        self.assertIsNone(record.read_state(self.session))

    def test_start_ends_its_recorder_when_it_cannot_say_so(self):
        recorder = mock.Mock(pid=4321)
        with mock.patch('os.replace', side_effect=KeyboardInterrupt), self.assertRaises(KeyboardInterrupt):
            self.start(self.out, recorder=recorder)
        recorder.kill.assert_called_once_with()
        recorder.wait.assert_called_once_with()
        self.assertIsNone(record.read_state(self.session))

    def test_a_start_ended_by_a_signal_leaves_no_recorder(self):
        # As the command line does, a signal to end raises SystemExit.
        def terminate(signum, frame):
            raise SystemExit(128 + signum)

        def pid(_):
            os.kill(os.getpid(), signal.SIGTERM)
            return 4321

        recorder = mock.Mock()
        type(recorder).pid = property(pid)
        previous = signal.signal(signal.SIGTERM, terminate)
        try:
            with self.assertRaises(SystemExit):
                self.start(self.out, recorder=recorder)
        finally:
            signal.signal(signal.SIGTERM, previous)
        recorder.kill.assert_called_once_with()
        recorder.wait.assert_called_once_with()
        self.assertIsNone(record.read_state(self.session))

    def test_the_recorder_takes_signals_to_end(self):
        # A stand-in for ffmpeg that writes the signals it blocks to its log, and then runs on.
        stand_in = os.path.join(self.captures.name, 'bin', 'ffmpeg')
        os.makedirs(os.path.dirname(stand_in))
        with open(stand_in, 'w') as f:
            f.write(f'#!{sys.executable} -I\nimport sys, time\n'
                    "sys.stderr.write([line for line in open('/proc/self/status') if line.startswith('SigBlk')][0])\n"
                    'sys.stderr.flush()\ntime.sleep(30)\n')
        os.chmod(stand_in, 0o755)
        session = Session({'DESKTOP_CAPTURES': self.captures.name, 'PATH': os.path.dirname(stand_in)})
        session.geometry = lambda: mock.Mock(width=1600, height=900)
        with mock.patch('time.sleep'), contextlib.redirect_stdout(io.StringIO()):
            record.start(session, self.out)
        pid = record.read_state(session)[0]
        try:
            log = os.path.join(record.parts_dir(self.out), 'ffmpeg.log')
            for _ in range(250):
                with open(log) as f:
                    written = f.read()
                if written.endswith('\n'):
                    break
                time.sleep(0.02)
            self.assertRegex(written, r'^SigBlk:\s+0+\n$')
        finally:
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)

    def test_start_refuses_while_a_recording_runs(self):
        self.write_state()
        with mock.patch.object(record, 'recorder', return_value=True):
            with self.assertRaisesRegex(DesktopError, 'a recording is already running'):
                record.start(self.session, self.out)

    def test_refuses_what_it_cannot_record(self):
        with self.assertRaisesRegex(DesktopError, 'must be an .mp4 file'):
            record.record(self.session, ['start', 'out.webm'])
        with self.assertRaisesRegex(DesktopError, 'no recording is running'):
            record.record(self.session, ['stop'])
        with self.assertRaisesRegex(DesktopError, 'record: takes start'):
            record.record(self.session, ['stop', '--width', 'wide'])


if __name__ == '__main__':
    unittest.main()
