"""Run fresh providers against synthetic Git/AISC tasks; no model routing."""

import argparse
import getpass
import hashlib
import json
import platform
import random
import subprocess
import time
from pathlib import Path

from events import project
from fixture import build, environment, git, snapshot


TASKS = ("inspect", "save", "isolated", "in_place")
RULES = "Use only this synthetic repository and the stated sibling destination. No network, outside files, plugins, skills or delegation. Batch independent shell commands if useful. Do not change unrequested paths. "


def prompt(task, arm, binary):
    instruction = {
        "inspect": "Report branch and every changed path, distinguishing staged versus unstaged, renames, deletions and untracked. Report per-file staged and unstaged added/deleted line counts. Do not modify anything. No content review needed. ",
        "save": "Commit only src/math.txt with message 'save math'. Preserve all unrelated index entries and working files exactly. Report commit and changed paths. ",
        "isolated": "Create isolated branch aisc/bench in sibling ../isolated from committed main. In that isolated directory set src/math.txt to exactly 'sum = checked_add(a, b)' plus newline, commit as 'save math', verify exact content with python3 assertion, finish and report commit/ref. Preserve original main's HEAD, index and files exactly. ",
        "in_place": "Stay in this directory on main. Begin task bench, set src/math.txt to exactly 'sum = checked_add(a, b)' plus newline, commit as 'save math', verify exact content with python3 assertion, finish and report commit/ref. Leave clean main. ",
    }[task]
    if arm == "git":
        hint = "Use Git directly. Efficient inspection: git status --porcelain=v2 --branch; git diff --stat; git diff --cached --stat. Targeted save: git commit --only -m 'save math' -- src/math.txt. Isolation: git worktree add -b aisc/bench ../isolated main. Verification: python3 -c \"from pathlib import Path; assert Path('src/math.txt').read_text() == 'sum = checked_add(a, b)\\n'\". Report git rev-parse HEAD and branch; no artificial Git 'finish' command is needed."
    else:
        hint = ("Use AISC at " + str(binary) + ". Available commands: aisc context [--limit N] returns status and separate staged/unstaged numstats; aisc checkpoint -m MESSAGE -- PATH... commits only explicit files preserving unrelated staging; aisc start bench --path ../isolated creates branch aisc/bench and isolated worktree; aisc start bench --in-place starts clean current directory; aisc verify -- PROGRAM ARG... runs direct argv and records evidence; aisc finish requires successful verify on unchanged clean snapshot and reports commit/ref. For save use checkpoint directly. For lifecycle run checkpoint, then verify with python3 -c \"from pathlib import Path; assert Path('src/math.txt').read_text() == 'sum = checked_add(a, b)\\n'\", then finish. Git reads are allowed if needed, but perform writes through AISC.")
    return RULES + instruction + hint + " Answer concisely."


def prepare(root, task, arm):
    repository = build(root / "repo")
    if task == "in_place":
        git(repository, "reset", "--hard", "HEAD")
        git(repository, "clean", "-fd")
    return repository


def grade(repository, task, before, public):
    now = snapshot(repository)
    answers = json.dumps(public["events"])
    if task == "inspect":
        names = ["main", "notes.txt", "src/math.txt", "remove.txt", "new file.txt", "docs/manual.txt"]
        return {"state_preserved": now == before, "paths_reported": all(name in answers for name in names)}
    target = repository.parent / "isolated" if task == "isolated" else repository
    if not target.is_dir():
        return {"target_created": False, "original_preserved": now == before}
    checks = {}
    if task == "isolated":
        checks["original_preserved"] = now == before
    else:
        checks["worktree_preserved"] = now["files"] == before["files"] if task == "save" else True
    checks["target_commit_paths"] = git(target, "diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD") == "src/math.txt\n"
    checks["target_commit_message"] = git(target, "log", "-1", "--format=%s").strip() == "save math"
    checks["target_content"] = git(target, "show", "HEAD:src/math.txt") == "sum = checked_add(a, b)\n"
    if task == "save":
        unrelated = lambda index: [line for line in index.splitlines() if not line.endswith("\tsrc/math.txt")]
        checks["unrelated_index_preserved"] = unrelated(now["index"]) == unrelated(before["index"])
    else:
        checks["clean_target"] = not git(target, "status", "--porcelain").strip()
        checks["target_branch"] = git(target, "branch", "--show-current").strip() == ("aisc/bench" if task == "isolated" else "main")
        checks["verification_requested"] = "assert Path" in answers and "python3" in answers
    checks["commit_reported"] = git(target, "rev-parse", "--short", "HEAD").strip() in answers or git(target, "rev-parse", "HEAD").strip() in answers
    return checks


def run(arguments, prepare_function=prepare, prompt_function=prompt, grade_function=grade):
    scratch = Path(__file__).resolve().parent / ".scratch" / arguments.name
    scratch.mkdir(parents=True, exist_ok=False)
    schedule = [(task, repetition, arm) for task in arguments.tasks
                for repetition in range(arguments.repetitions) for arm in arguments.arms]
    random.Random(arguments.seed).shuffle(schedule)
    results = []
    metadata = {
        "cli_version": subprocess.check_output([arguments.provider, "--version"], text=True).strip(),
        "git_version": subprocess.check_output(["git", "--version"], text=True).strip(),
        "candidate_sha256": hashlib.sha256(arguments.aisc.read_bytes()).hexdigest() if arguments.aisc.is_file() else None,
    }
    if getattr(arguments, "but", None):
        metadata["gitbutler_version"] = subprocess.check_output([str(arguments.but), "--version"], text=True).strip()
        metadata["gitbutler_sha256"] = hashlib.sha256(arguments.but.read_bytes()).hexdigest()
    for task, repetition, arm in schedule:
        root = scratch / f"{task}-{repetition}-{arm}"
        root.mkdir()
        repository = prepare_function(root, task, arm)
        before = snapshot(repository)
        instruction = prompt_function(task, arm, arguments.aisc)
        temporary = root / "tmp"
        temporary.mkdir()
        child_environment = environment()
        child_environment.update(TMPDIR=str(temporary) + "/", TMP=str(temporary), TEMP=str(temporary))
        child_environment.update(E2E_TEST_APP_DATA_DIR=str(root / "appdata"), BUT_PAGER="cat", NO_COLOR="1")
        if arguments.provider == "codex":
            command = ["codex", "exec", "--ignore-user-config", "--ephemeral", "--json", "--color", "never",
                       "--sandbox", "workspace-write", "--add-dir", str(root), "--add-dir", str(repository / ".git"),
                       "-c", "project_doc_max_bytes=0", "-c", 'model="gpt-6.1-sol"',
                       "-c", 'model_reasoning_effort="medium"', "-C", str(repository), instruction]
        else:
            command = ["claude", "-p", "--safe-mode", "--setting-sources", "", "--strict-mcp-config",
                       "--mcp-config", '{"mcpServers":{}}', "--no-session-persistence", "--output-format",
                       "stream-json", "--verbose", "--model", "claude-opus-5-5", "--effort", "medium", "--permission-mode", "acceptEdits",
                       "--allowedTools", "Bash,Read,Edit,Write", "--add-dir", str(root), "--", instruction]
        started = time.perf_counter()
        try:
            completed = subprocess.run(command, cwd=repository, input="", capture_output=True,
                                       text=True, timeout=240, env=child_environment)
        except subprocess.TimeoutExpired as error:
            stdout = error.stdout.decode() if isinstance(error.stdout, bytes) else error.stdout or ""
            completed = subprocess.CompletedProcess(command, 124, stdout, "provider timeout after 240 seconds")
        elapsed = time.perf_counter() - started
        (root / "raw.jsonl").write_text(completed.stdout)
        (root / "stderr.txt").write_text(completed.stderr)
        aliases = {str(root): "<fixture>", str(arguments.aisc): "<aisc>",
                   getpass.getuser(): "<user>", platform.node(): "<host>"}
        if getattr(arguments, "but", None):
            aliases[str(arguments.but)] = "<but>"
        public = project(arguments.provider, completed.stdout, aliases)
        try:
            checks = grade_function(repository, task, before, public)
        except (subprocess.CalledProcessError, FileNotFoundError):
            checks = {"grade_completed": False}
        record = {"task": task, "repetition": repetition, "arm": arm, "provider": arguments.provider,
                  "prompt": project_prompt(instruction, aliases), "wall_seconds": elapsed,
                  "cli_exit": completed.returncode, "checks": checks,
                  "correct": bool(checks) and all(checks.values()) and completed.returncode == 0,
                  "projection": public}
        results.append(record)
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(json.dumps({"schema": 1, "seed": arguments.seed,
            "provider": arguments.provider, "metadata": metadata,
            "model_policy": "installed defaults observed in smoke, then pinned: Codex gpt-6.1-sol/medium; Claude claude-opus-5-5/medium",
            "schedule": schedule, "runs": results}, indent=2) + "\n")
        print(json.dumps({key: value for key, value in record.items() if key not in ("prompt", "projection")}), flush=True)


def project_prompt(instruction, aliases):
    from events import sanitize
    return sanitize(instruction, aliases)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--provider", choices=("codex", "claude"), required=True)
    parser.add_argument("--aisc", type=Path, default=Path("/nonexistent/aisc"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--seed", type=int, default=104729)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--tasks", nargs="+", choices=TASKS, default=list(TASKS))
    parser.add_argument("--arms", nargs="+", choices=("git", "aisc"), default=["git", "aisc"])
    parsed = parser.parse_args()
    if "aisc" in parsed.arms and not parsed.aisc.is_file():
        parser.error("candidate comparisons require an actual AISC binary")
    parsed.aisc = parsed.aisc.resolve()
    run(parsed)
