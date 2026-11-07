import contextlib
import io
import unittest

import _path  # noqa: F401
from agent_desktop import cli
from agent_desktop.session import DesktopError


class BatchLineTest(unittest.TestCase):
    def test_words_are_split_without_quoting_or_expansion(self):
        self.assertEqual(cli.batch_line('  click 10 20  '), ['click', '10', '20'])
        self.assertEqual(cli.batch_line('key "$HOME"'), ['key', '"$HOME"'])

    def test_type_and_focus_take_the_rest_of_the_line(self):
        self.assertEqual(cli.batch_line('type Zoë  Ångström, 1 234 €'), ['type', 'Zoë  Ångström, 1 234 €'])
        self.assertEqual(cli.batch_line('  focus Google Chrome'), ['focus', 'Google Chrome'])
        self.assertEqual(cli.batch_line('type'), ['type'])

    def test_blank_lines_and_comments_are_not_actions(self):
        self.assertIsNone(cli.batch_line('   '))
        self.assertIsNone(cli.batch_line('  # click 1 2'))


class BatchTest(unittest.TestCase):
    def run_batch(self, text, results):
        ran = []

        def run(words):
            ran.append(words)
            result = results.get(words[0])
            if isinstance(result, Exception):
                raise result
            print(f'did {words[0]}')
            return result

        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            status = cli.batch(None, ['--no-screenshot'], io.StringIO(text), run)
        return status, ran, output.getvalue()

    def test_runs_each_command_and_indents_its_output(self):
        status, ran, output = self.run_batch('move 1 2\n\n# note\ntype a b\n', {})
        self.assertEqual(status, 0)
        self.assertEqual(ran, [['move', '1', '2'], ['type', 'a b']])
        self.assertEqual(output, ' 1 move 1 2\n   did move\n 4 type a b\n   did type\n')

    def test_stops_at_the_first_failure(self):
        status, ran, output = self.run_batch('move 1 2\nclick\nkey Tab\n', {'click': DesktopError('click failed')})
        self.assertEqual(status, 1)
        self.assertEqual(ran, [['move', '1', '2'], ['click']])
        self.assertIn('   desktop: click failed\n', output)
        self.assertTrue(output.endswith(
            ' 3 key Tab\n   Not executed: an earlier computer action in this turn failed.\n'))

    def test_only_a_newline_ends_a_line(self):
        status, ran, _ = self.run_batch('type abc\u2028click 10 10\rkey\x85x\ncursor', {})
        self.assertEqual(status, 0)
        self.assertEqual(ran, [['type', 'abc\u2028click 10 10\rkey\x85x'], ['cursor']])

    def test_any_failure_stops_the_batch_at_its_line(self):
        status, ran, output = self.run_batch('wait 1\ncursor\n', {'wait': OverflowError('timestamp out of range')})
        self.assertEqual(status, 1)
        self.assertEqual(ran, [['wait', '1']])
        self.assertEqual(output, ' 1 wait 1\n   desktop: OverflowError: timestamp out of range\n'
                                 ' 2 cursor\n   Not executed: an earlier computer action in this turn failed.\n')

    def test_refuses_unknown_options(self):
        with self.assertRaisesRegex(DesktopError, "batch: unexpected argument '--fast'"):
            cli.batch(None, ['--fast'], io.StringIO(''))


class MainTest(unittest.TestCase):
    def test_without_a_command_prints_usage(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(cli.main([]), 64)
        self.assertTrue(output.getvalue().startswith('desktop <command>'))

    def test_an_unknown_command_fails(self):
        output = io.StringIO()
        with contextlib.redirect_stderr(output):
            self.assertEqual(cli.main(['--json', 'fly']), 1)
        self.assertEqual(output.getvalue(), "desktop: unknown command 'fly'; run 'desktop help'\n")

    def test_every_command_is_in_the_usage(self):
        for command in cli.COMMANDS:
            self.assertIn(f'\n  {command} ', cli.USAGE, command)


if __name__ == '__main__':
    unittest.main()
