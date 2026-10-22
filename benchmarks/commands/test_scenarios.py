"""Graders must reject doing less: a no-op fails every scenario, the Git reference passes.

Run: python3 benchmarks/commands/test_scenarios.py  (needs only git on PATH)
"""

import shutil
import tempfile
import unittest
from pathlib import Path

from run import one_run
from scenarios import TOOLS, build_template, environment, scenarios


def count(text):
    return len(text.encode())


class ScenarioTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = Path(tempfile.mkdtemp())
        cls.binaries = {"git": shutil.which("git"), "cat": shutil.which("cat")}
        for scenario in scenarios():
            build_template(scenario, cls.root / "templates" / scenario.name,
                           environment(cls.root / "templates" / scenario.name))

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.root, ignore_errors=True)

    def run_git(self, scenario):
        return one_run(scenario, "git", self.root / "templates" / scenario.name, self.root, self.binaries, count, 0)

    def test_git_reference_passes_every_scenario(self):
        for scenario in scenarios():
            with self.subTest(scenario=scenario.name):
                result = self.run_git(scenario)
                self.assertIsNone(result["error"])
                self.assertTrue(result["passed"], result["checks"])

    def test_noop_fails_every_scenario(self):
        for scenario in scenarios():
            with self.subTest(scenario=scenario.name):
                scenario.solutions["git"] = []
                result = self.run_git(scenario)
                self.assertFalse(result["passed"], scenario.name + " passes without doing the task")

    def test_every_tool_has_a_sequence_or_is_marked_unsupported(self):
        for scenario in scenarios():
            self.assertEqual(set(scenario.solutions), set(TOOLS), scenario.name)


if __name__ == "__main__":
    unittest.main()
