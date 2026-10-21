import tempfile
import unittest
from pathlib import Path

from baseline import measure, text_for
from fixture import build, git, snapshot
from events import project, sanitize
from summarize import counters
import competitor
import json


class BenchmarkTests(unittest.TestCase):
    def test_final_grade_cannot_pass_from_tool_output_alone(self):
        with tempfile.TemporaryDirectory() as temporary:
            repository = competitor.prepare(Path(temporary), "selected_save", "git", None)
            before = snapshot(repository)
            public = {"events": [{"type": "command_execution", "aggregated_output": "docs/guide.txt src/math.txt"}, {"type": "answer", "text": ""}]}
            self.assertFalse(competitor.grade(repository, "inspect_compact", before, public)["final_counts"])
            git(repository, "commit", "--only", "-qm", "save math", "--", "src/math.txt")
            public["events"][0]["aggregated_output"] = git(repository, "rev-parse", "HEAD")
            public["events"][1]["text"] = '{"commit":"wrong-hash"}'
            checks = competitor.grade(repository, "selected_save", before, public)
            self.assertFalse(checks["commit_reported"])
            self.assertTrue(checks["commit_paths"])

    def test_competitor_grade_rejects_unrelated_committed_content(self):
        with tempfile.TemporaryDirectory() as temporary:
            repository = competitor.prepare(Path(temporary), "selected_save", "git", None)
            before = snapshot(repository)
            git(repository, "commit", "--only", "-qm", "save math", "--", "src/math.txt", "docs/guide.txt")
            public = {"events": [git(repository, "rev-parse", "HEAD")]}
            checks = competitor.grade(repository, "selected_save", before, public)
            self.assertFalse(checks["commit_paths"])
            self.assertFalse(checks["uncommitted_docs"])

    def test_usage_counters_do_not_double_count_cache(self):
        base = {"projection": {"usage": [{"input_tokens": 20, "cached_input_tokens": 10, "output_tokens": 3}], "events": []}, "provider": "codex", "wall_seconds": 1}
        self.assertEqual(counters(base)["provider_accounted_tokens"], 23)
        base["provider"] = "claude"
        base["projection"]["usage"] = [{"input_tokens": 2, "cache_read_input_tokens": 10, "cache_creation_input_tokens": 5, "output_tokens": 3, "iterations": [{"input_tokens": 999}]}]
        self.assertEqual(counters(base)["provider_accounted_tokens"], 20)

    def test_provider_projection_drops_identity_and_preserves_usage(self):
        raw = '\n'.join(json.dumps(event) for event in [
            {"type": "thread.started", "thread_id": "private-thread"},
            {"type": "item.completed", "item": {"id": "private-item", "type": "command_execution", "command": "git status", "aggregated_output": "/tmp/fixture/repo", "exit_code": 0}},
            {"type": "turn.completed", "usage": {"input_tokens": 20, "cached_input_tokens": 10, "output_tokens": 3}},
        ])
        result = project("codex", raw, {"/tmp/fixture": "<fixture>"})
        self.assertNotIn("private", json.dumps(result))
        self.assertEqual(result["usage"][0]["input_tokens"], 20)
        self.assertEqual(result["events"][0]["aggregated_output"], "<fixture>/repo")
        with self.assertRaises(ValueError):
            sanitize("/Users/private/data", {})
        self.assertEqual(sanitize("cache /var/folders/example/T/xcrun-cache", {}), "cache <runtime-temp>")

    def test_mixed_fixture_and_read_only_measurement(self):
        with tempfile.TemporaryDirectory() as temporary:
            repository = build(Path(temporary) / "repo")
            before = snapshot(repository)
            self.assertIn("MM", before["status"])
            self.assertIn("R100", before["status"])
            self.assertIn("? new file.txt", before["status"])
            records, elapsed = measure(repository, [["git", "status", "--porcelain=v2", "--branch"]])
            self.assertEqual(before, snapshot(repository))
            self.assertIn("$ git status", text_for(records))
            self.assertGreater(elapsed, 0)

    def test_targeted_git_commit_preserves_unrelated_index(self):
        with tempfile.TemporaryDirectory() as temporary:
            repository = build(Path(temporary) / "repo")
            before = snapshot(repository)
            git(repository, "commit", "--only", "-qm", "save math", "--", "src/math.txt")
            self.assertEqual(git(repository, "diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"), "src/math.txt\n")
            self.assertEqual(before["files"], snapshot(repository)["files"])
            self.assertEqual(before["index"].splitlines()[0], snapshot(repository)["index"].splitlines()[0])
            self.assertIn("Unrelated staged change", git(repository, "show", ":notes.txt"))
            self.assertNotIn("Unstaged tail", git(repository, "show", ":notes.txt"))


if __name__ == "__main__":
    unittest.main()
