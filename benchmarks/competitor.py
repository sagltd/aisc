"""Supported equivalent tasks for a fair optimized Git/GitButler/AISC cohort."""

import argparse
import json
import subprocess
import time
from pathlib import Path

from agents import RULES, run
from events import final_text
from fixture import build, environment, git, snapshot


def prepare(root, task, arm, binary):
    started = time.perf_counter()
    repository = build(root / "repo")
    git(repository, "reset", "--hard", "HEAD")
    git(repository, "clean", "-fd")
    git(repository, "config", "user.name", "Benchmark")
    git(repository, "config", "user.email", "bench@example.invalid")
    if arm == "but":
        values = environment()
        values.update(E2E_TEST_APP_DATA_DIR=str(root / "appdata"), BUT_PAGER="cat", NO_COLOR="1")
        for arguments in (["setup", "--json"], ["branch", "new", "bench", "--json"]):
            subprocess.run([str(binary), *arguments], cwd=repository, env=values,
                           capture_output=True, text=True, check=True, timeout=45)
        for directory in (".codex", ".claude"):
            subprocess.run([str(binary), "skill", "install", "--path",
                            str(root / "appdata/home" / directory / "skills/gitbutler"), "--json"],
                           cwd=repository, env=values, capture_output=True, text=True, check=True, timeout=45)
    else:
        git(repository, "switch", "-c", "bench")
    (repository / "src/math.txt").write_text("sum = checked_add(a, b)\n")
    (repository / "docs/guide.txt").write_text("Unrelated documentation edit\n")
    (root / "setup-metrics.json").write_text(json.dumps({"wall_seconds": time.perf_counter() - started}))
    return repository


def prompt(task, arm, aisc, but):
    task_text = ("The existing task branch is bench. Report both changed paths and their added/deleted line counts. No content review is required. Do not modify source, index or task-branch commit. " if task == "inspect_compact" else
                 "Commit only src/math.txt to the existing task branch bench with message 'save math'. Preserve the unrelated docs/guide.txt working edit exactly and leave it uncommitted. Report the resulting task-branch commit hash. ")
    if arm == "git":
        card = ("Use optimized Git. Batch git status --porcelain=v2 --branch and git diff --numstat in one shell tool call; numstat gives exact line counts." if task == "inspect_compact" else
                "Use optimized Git: git commit --only -m 'save math' -- src/math.txt. The mutation output reports commit. No preliminary or repeated status is needed for this fully specified operation.")
    elif arm == "aisc":
        card = "Use AISC at " + str(aisc) + (". Read aisc context once; it returns bounded JSON with per-file status and separate staged/unstaged numstats." if task == "inspect_compact" else
                ". Run aisc checkpoint -m 'save math' -- src/math.txt; JSON reports full commit hash. No preliminary or repeated status is needed for this fully specified operation. Perform writes through AISC.")
    else:
        card = "Use GitButler at " + str(but) + ". Its workspace and existing task branch bench are already configured. " + ("Batch but status --no-hint and git diff --numstat in one shell tool call; optimized Git reads supply exact counts without a full diff." if task == "inspect_compact" else
                "Run but commit -b bench -m 'save math' src/math.txt --json; JSON reports full commitId. File paths work directly. No preliminary or repeated status is needed for this fully specified operation. Perform writes through GitButler.")
    answer = (' Return only JSON {"branch": "bench", "changes": [{"path": "a changed path", "added": 0, "deleted": 0}]} with actual counts for every changed path.' if task == "inspect_compact" else
              ' Return only JSON {"commit": "the resulting full or at least 7-character commit hash"}.')
    return RULES + task_text + card + answer


def grade(repository, task, before, public):
    now = snapshot(repository)
    try:
        answer = json.loads(final_text(public).strip().removeprefix("```json").removesuffix("```").strip())
    except (ValueError, AttributeError):
        answer = {}
    if not isinstance(answer, dict):
        answer = {}
    if task == "inspect_compact":
        expected = [{"path": "docs/guide.txt", "added": 1, "deleted": 1}, {"path": "src/math.txt", "added": 1, "deleted": 1}]
        changes = answer.get("changes", [])
        valid = isinstance(changes, list) and all(isinstance(change, dict) and isinstance(change.get("path"), str) and type(change.get("added")) is int and type(change.get("deleted")) is int for change in changes)
        counts_match = valid and sorted(changes, key=lambda change: change["path"]) == expected
        return {"state_preserved": now == before,
                "final_branch": answer.get("branch") == "bench",
                "final_counts": counts_match}
    commit = git(repository, "rev-parse", "bench").strip()
    reported = answer.get("commit", "")
    return {"worktree_preserved": now["files"] == before["files"],
            "commit_paths": git(repository, "diff-tree", "--no-commit-id", "--name-only", "-r", "bench") == "src/math.txt\n",
            "commit_message": git(repository, "log", "-1", "--format=%s", "bench").strip() == "save math",
            "committed_math": git(repository, "show", "bench:src/math.txt") == "sum = checked_add(a, b)\n",
            "uncommitted_docs": git(repository, "show", "bench:docs/guide.txt") == "Guide version one\n" and (repository / "docs/guide.txt").read_text() == "Unrelated documentation edit\n",
            "commit_reported": isinstance(reported, str) and len(reported) >= 7 and commit.startswith(reported)}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--provider", choices=("codex", "claude"), required=True)
    parser.add_argument("--aisc", type=Path, required=True)
    parser.add_argument("--but", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--seed", type=int, default=130363)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--tasks", nargs="+", choices=("inspect_compact", "selected_save"), default=["inspect_compact", "selected_save"])
    parser.add_argument("--arms", nargs="+", choices=("git", "aisc", "but"), default=["git", "aisc", "but"])
    arguments = parser.parse_args()
    arguments.grading_version = "final_json_v2"
    arguments.aisc = arguments.aisc.resolve(strict=True)
    # The official app selects CLI mode by the `but` symlink's argv[0].
    arguments.but = arguments.but.absolute()
    if not arguments.but.is_file():
        parser.error("GitButler CLI binary must exist")
    run(arguments,
        prepare_function=lambda root, task, arm: prepare(root, task, arm, arguments.but),
        prompt_function=lambda task, arm, aisc: prompt(task, arm, aisc, arguments.but),
        grade_function=grade)
