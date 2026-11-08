import unittest

import _path  # noqa: F401
from agent_desktop import geometry


class BudgetTest(unittest.TestCase):
    def test_each_harness_gets_its_budget_and_others_the_default(self):
        for harness, source, expected in (
            ('claudeCode', 'harness', (1175000, 1568)),
            ('codex', 'harness', (2457600, 2048)),
            ('pi', 'harness', (1175000, 1568)),
            ('', 'default', (1175000, 1568)),
            ('other', 'unknown', (1175000, 1568)),
        ):
            budget = geometry.budget({'AGENT_HARNESS': harness})
            self.assertEqual((budget.pixels, budget.edge), expected, harness)
            self.assertEqual(budget.source, source, harness)

    def test_a_box_replaces_the_table(self):
        budget = geometry.budget({'AGENT_HARNESS': 'codex', 'DESKTOP_MAX_WIDTH': '1000'})
        self.assertEqual((budget.pixels, budget.edge, budget.width, budget.height), (None, None, 1000, 819))
        self.assertEqual(budget.overrides, ('DESKTOP_MAX_WIDTH',))

    def test_overrides_must_be_positive_integers(self):
        for value in ('0', '012', '-5', '1.5', 'many'):
            with self.assertRaisesRegex(geometry.BudgetError, f"DESKTOP_SCREENSHOT_PIXELS .* got '{value}'"):
                geometry.budget({'DESKTOP_SCREENSHOT_PIXELS': value})

    def test_an_empty_override_is_ignored(self):
        self.assertEqual(geometry.budget({'DESKTOP_MAX_WIDTH': ''}).overrides, ())


class FitTest(unittest.TestCase):
    def test_screenshots_fit_the_budget_of_the_harness(self):
        claude = geometry.budget({'AGENT_HARNESS': 'claudeCode'})
        codex = geometry.budget({'AGENT_HARNESS': 'codex'})
        self.assertEqual(geometry.fit(1600, 900, claude), geometry.Geometry(1600, 900, 1445, 812))
        self.assertEqual(geometry.fit(1600, 900, codex), geometry.Geometry(1600, 900, 1600, 900))
        fitted = geometry.fit(2560, 1600, claude)
        self.assertLessEqual(fitted.sent_width * fitted.sent_height, 1175000)
        self.assertLessEqual(max(fitted.sent_width, fitted.sent_height), 1568)

    def test_a_box_bounds_both_edges(self):
        box = geometry.budget({'DESKTOP_MAX_WIDTH': '1456', 'DESKTOP_MAX_HEIGHT': '600'})
        self.assertEqual(geometry.fit(1600, 900, box), geometry.Geometry(1600, 900, 1066, 600))

    def test_scale_maps_between_pixel_spaces(self):
        self.assertEqual(geometry.scale(100, 1445, 1600), 111)
        self.assertEqual(geometry.scale(111, 1600, 1445), 100)
        self.assertEqual(geometry.scale(7, 1600, 1600), 7)


if __name__ == '__main__':
    unittest.main()
