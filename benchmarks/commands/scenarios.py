"""Scripted vcbench-style tasks: one synthetic fixture, one expert sequence per tool.

Every scenario builds the same Git history, lets each tool adopt it untimed, then
runs the timed sequence an expert agent would type. ``Run`` steps are shell
commands, ``Edit`` steps are file writes an agent makes with its editor tool.
A step's arguments may depend on earlier output (GitButler's short IDs), which
is parsed here the way an agent would read it. ``verify`` grades only the
resulting Git refs, trees and working files, so no tool passes by doing less.
"""

import os
import re
import shutil
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path


TOOLS = ("git", "aisc", "but", "jj")
DATE = "2026-01-01T00:00:00Z"


def environment(root):
    values = {key: value for key, value in os.environ.items()
              if key in ("PATH", "TMPDIR", "USER", "LOGNAME", "SHELL", "TERM")}
    values.update(
        HOME=str(root / "home"), GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
        GIT_AUTHOR_NAME="Benchmark", GIT_AUTHOR_EMAIL="bench@example.invalid",
        GIT_COMMITTER_NAME="Benchmark", GIT_COMMITTER_EMAIL="bench@example.invalid",
        GIT_AUTHOR_DATE=DATE, GIT_COMMITTER_DATE=DATE, LC_ALL="C", TZ="UTC",
        GIT_EDITOR="true", GIT_PAGER="cat", PAGER="cat", NO_COLOR="1",
        E2E_TEST_APP_DATA_DIR=str(root / "appdata"), BUT_PAGER="cat",
        JJ_CONFIG=os.devnull, JJ_USER="Benchmark", JJ_EMAIL="bench@example.invalid",
        JJ_EDITOR="true", AISC_NO_UPDATE_CHECK="1")
    return values


def git(repository, *arguments, env=None):
    return subprocess.run(["git", *arguments], cwd=repository, env=env, check=True,
                          stdin=subprocess.DEVNULL, capture_output=True, timeout=60).stdout.decode()


# ---------------------------------------------------------------- content

APP = """import sys


def load(path):
    with open(path) as handle:
        return handle.read()


def parse(text):
    return text.split()


def run(argv):
    data = load(argv[1])
    return parse(data)


def main():
    print(run(sys.argv))
"""
APP_VALIDATED = APP.replace("def load(path):\n", "def load(path):\n    if not path:\n        raise ValueError(\"path required\")\n")
APP_LOGGED = APP.replace("    print(run(sys.argv))\n", "    print(\"result\", run(sys.argv))\n")
APP_BOTH = APP_VALIDATED.replace("    print(run(sys.argv))\n", "    print(\"result\", run(sys.argv))\n")
TEST = "from app import parse\n\n\ndef test_parse():\n    assert parse(\"a b\") == [\"a\", \"b\"]\n"
TEST_MORE = TEST + "\n\ndef test_load_requires_path():\n    assert load_rejects_empty()\n"
README = "# Demo\n\nA small demo.\n"
README_EDIT = "# Demo\n\nA small demo with notes.\n"
GUIDE = "# Guide\n\nRun the demo.\n"
GUIDE_EDIT = "# Guide\n\nRun the demo with a path.\n"
NOTIFY = "CHANNELS = [\"email\"]\n"
NOTIFY_PUSH = "CHANNELS = [\"email\", \"push\"]\n"
NOTIFY_SMS = "CHANNELS = [\"email\", \"sms\"]\n"
NOTIFY_BOTH = "CHANNELS = [\"email\", \"push\", \"sms\"]\n"
PARSER_TYPO = "def parse(text):\n    return text.splt()\n"
PARSER = "def parse(text):\n    return text.split()\n"
PARSER_FIXED = "def parse(text):\n    return [word for word in text.split() if word]\n"
MAIN_FILES = {"app.py": APP, "README.md": README, "docs/guide.md": GUIDE,
              "notify.py": NOTIFY, "tests/test_app.py": TEST}


# ---------------------------------------------------------------- steps

MODES = ("default", "quietest")


class Q(str):
    """An argument that exists only to reduce output; dropped in the "default" mode."""


def for_mode(argv, mode):
    return [str(part) for part in argv if mode == "quietest" or not isinstance(part, Q)]


@dataclass
class Run:
    argv: object  # list[str] or callable(context) -> list[str]
    stdin: str = None
    env: dict = field(default_factory=dict)
    may_fail: bool = False  # e.g. a rebase that stops on the expected conflict


@dataclass
class Edit:
    path: str
    content: str


@dataclass
class Scenario:
    name: str
    family: str
    task: str
    commits: list          # [(subject, {path: content or None})] on feature, oldest first
    dirty: dict            # working-copy edits applied after adoption
    expected: list         # [(subject, {path: content})] overlays on the base tree
    expected_dirty: dict   # working files that must differ from the feature tree
    solutions: dict        # tool -> [steps] or None when unsupported
    upstream: list = field(default_factory=list)  # commits pushed to origin/main only
    rebased: bool = False  # expected base is the updated origin/main
    side: dict = None      # extra branch expectations {branch: [(subject, overlay)]}
    before: dict = None    # tool -> untimed steps that create the state to undo
    read_only: bool = False
    facts: tuple = ()
    notes: dict = field(default_factory=dict)
    tool_dirty: dict = field(default_factory=dict)  # tool-specific working-copy model


# ---------------------------------------------------------------- GitButler ID lookup
# These parse the human `but` output exactly as an agent reads it.

def but_commit(context, subject, index=-1):
    lines = context["outputs"][index].splitlines()
    for number, line in enumerate(lines):
        conflict = re.match(r"^\s+(\S+) \[conflict\] (.*?)\s*$", line)  # `but pull` summary
        if conflict and conflict.group(2) == subject:
            return conflict.group(1)
        match = re.match(r"^[┊│ ]*●\s+(\S+)\s*(.*?)\s*$", line)
        if not match:
            continue
        text = match.group(2)
        if re.search(r"\(sha [0-9a-f]+\)$", text) and number + 1 < len(lines):
            text = lines[number + 1].strip("┊│ ")  # --status-after prints the subject on the next line
        if re.sub(r"\s*(\(no changes\)|\{conflicted\})", "", text).strip() == subject:
            return match.group(1)
    raise LookupError("no GitButler commit id for " + subject)


def but_file(context, path, index=-1):
    for line in context["outputs"][index].splitlines():
        match = re.match(r"^[┊│ ]*(\S+)\s+[MADR?]\s+(.*?)\s*$", line)
        if match and match.group(2) == path:
            return match.group(1)
    raise LookupError("no GitButler file id for " + path)


def but_hunk(context, path, snippet, index=-1):
    blocks, current = [], None
    for line in context["outputs"][index].splitlines():
        header = re.match(r"^\s*(\S+:\S+)\s+(.*?)\s*│\s*$", line)
        if header:
            current = [header.group(1), header.group(2), []]
            blocks.append(current)
        elif current:
            current[2].append(line)
    for hunk, name, body in blocks:
        if name == path and any(snippet in line for line in body):
            return hunk
    raise LookupError("no GitButler hunk for " + snippet)


def sequence_editor(script):
    return {"GIT_SEQUENCE_EDITOR": script}


# ---------------------------------------------------------------- scenarios

def git_reflog_before_rewrite(context, index=-1):
    """First reflog entry that is not part of a rebase: the tip before the last rewrite."""
    for line in context["outputs"][index].splitlines():
        sha, _, message = line.partition(" ")
        if not message.split(": ", 1)[-1].startswith("rebase"):
            return sha
    raise LookupError("no pre-rewrite reflog entry")


# Inspection rule: the agent knows the task text (branch names, commit subjects,
# paths, and a phrase from the hunk to select). Each sequence runs exactly the
# reads its tool needs to turn that into arguments the tool accepts:
#   git  - hunk order for `add -p` (diff) and the pre-rewrite reflog entry for undo;
#          commits are addressed by subject in rebase todo edits or `:/subject`.
#   aisc - none: commits by subject, hunks by `path~text`.
#   but  - short IDs for commits, hunks and committed files (status/diff); paths
#          and branch names are accepted directly, so those need no lookup.
#   jj   - commits by `subject()` revsets; to commit part of a file it must read
#          the file (no non-interactive hunk selection).
# Quiet flags are wrapped in Q() and appear only in the "quietest" mode.
q, jq, nh = Q("-q"), Q("--quiet"), Q("--no-hint")


def scenarios():
    helper = ("add helper", {"lib.py": "def helper():\n    return 1\n"})
    parser_cli = [("add parser", {"parser.py": PARSER}), ("add cli", {"cli.py": "import parser\n"})]
    squash_series = [("add parser", {"parser.py": PARSER_TYPO}), ("fix parser typo", {"parser.py": PARSER}),
                     ("add cli", {"cli.py": "import parser\n"})]
    squashed = [("add parser module", {"parser.py": PARSER}), ("add cli", {"cli.py": "import parser\n"})]
    squash_editor = "perl -pi -e 's/^pick(.* fix parser typo)$/fixup$1\\nexec git commit {}--amend -m \"add parser module\"/'"
    git_squash = Run(["git", "rebase", q, "-i", "--autostash", "main"], env={"GIT_SEQUENCE_EDITOR": (
        squash_editor.format(""), squash_editor.format("-q "))})
    aisc_squash = Run(["aisc", "squash", q, "--into", "add parser module", "add parser", "fix parser typo"])
    but_squash = [Run(["but", "status", nh]),
                  Run(lambda c: ["but", "squash", but_commit(c, "fix parser typo"), "--target",
                                 but_commit(c, "add parser"), "-m", "add parser module"])]
    jj_squash = Run(["jj", "squash", jq, "--from", 'subject("fix parser typo")', "--into", 'subject("add parser")',
                     "-m", "add parser module"])
    return [
        Scenario(
            "select_hunks", "selective commit (pilot-1)",
            "Commit only the validation hunk of app.py (the one raising ValueError) plus the new test as 'add validation'; keep the logging hunk, README edit and untracked notes uncommitted.",
            [helper], {"app.py": APP_BOTH, "tests/test_app.py": TEST_MORE, "README.md": README_EDIT, "notes.txt": "scratch\n"},
            [helper, ("add validation", {"app.py": APP_VALIDATED, "tests/test_app.py": TEST_MORE})],
            {"app.py": APP_BOTH, "README.md": README_EDIT, "notes.txt": "scratch\n"},
            {"git": [Run(["git", "diff", "--", "app.py"]),
                     Run(["git", "add", "-p", "--", "app.py"], stdin="y\nn\n"),
                     Run(["git", "commit", q, "-i", "-m", "add validation", "--", "tests/test_app.py"])],
             "aisc": [Run(["aisc", "commit", q, "-m", "add validation", "app.py~ValueError", "tests/test_app.py"])],
             "but": [Run(["but", "diff"]),
                     Run(lambda c: ["but", "commit", "-m", "add validation", but_hunk(c, "app.py", "ValueError"),
                                    "tests/test_app.py"])],
             "jj": [Run(["cat", "app.py"]), Edit("app.py", APP_VALIDATED),
                    Run(["jj", "commit", jq, "-m", "add validation", "app.py", "tests/test_app.py"]),
                    Edit("app.py", APP_BOTH),
                    Run(["jj", "bookmark", "move", jq, "feature", "--to", "@-"])]},
            notes={"aisc": "hunk chosen by its text with the `path~text` selector, no lookup",
                   "but": "hunk chosen by the short hunk ID read from `but diff`; the test file by path",
                   "jj": "closest non-interactive equivalent: read the file, write the partial version, `jj commit` with a fileset, restore the file",
                   "git": "`git add -p` answered on stdin after reading the diff to learn hunk order"}),
        Scenario(
            "amend_older", "multi-amend (pilot-2)",
            "Fold the uncommitted parser.py fix into the older commit 'add parser'; keep the README edit uncommitted.",
            parser_cli, {"parser.py": PARSER_FIXED, "README.md": README_EDIT},
            [("add parser", {"parser.py": PARSER_FIXED}), parser_cli[1]], {"README.md": README_EDIT},
            {"git": [Run(["git", "commit", q, "--fixup", ":/add parser", "--", "parser.py"]),
                     Run(["git", "rebase", q, "-i", "--autosquash", "--autostash", "main"], env=sequence_editor("true"))],
             "aisc": [Run(["aisc", "amend", q, "--into", "add parser", "parser.py"])],
             "but": [Run(["but", "status", nh]),
                     Run(lambda c: ["but", "amend", "-t", but_commit(c, "add parser"), "parser.py"])],
             "jj": [Run(["jj", "squash", jq, "--into", 'subject("add parser")', "parser.py"])]}),
        Scenario(
            "split_commit", "split commit (pilot-3)",
            "Split 'add models' into 'add user model' (user.py) then 'add order model' (order.py), below 'add api'.",
            [("add models", {"user.py": "class User:\n    pass\n", "order.py": "class Order:\n    pass\n"}),
             ("add api", {"api.py": "from user import User\n"})], {},
            [("add user model", {"user.py": "class User:\n    pass\n"}),
             ("add order model", {"order.py": "class Order:\n    pass\n"}), ("add api", {"api.py": "from user import User\n"})], {},
            {"git": [Run(["git", "rebase", q, "-i", "main"], env=sequence_editor("sed -i.bak '/add models/s/^pick/edit/'")),
                     Run(["git", "reset", q, "--soft", "HEAD~"]),
                     Run(["git", "commit", q, "-m", "add user model", "--", "user.py"]),
                     Run(["git", "commit", q, "-m", "add order model"]),
                     Run(["git", "rebase", "--continue"])],
             "aisc": [Run(["aisc", "split", q, "add models", "--into", "add user model", "user.py", "--into", "add order model", "order.py"])],
             "but": [Run(["but", "status", "-f", nh]),
                     Run(lambda c: ["but", "uncommit", "--status-after", but_file(c, "order.py")]),
                     Run(lambda c: ["but", "reword", but_commit(c, "add models"), "-m", "add user model"]),
                     Run(lambda c: ["but", "commit", "-A", but_commit(c, "add models", -2), "-m", "add order model",
                                    "order.py"])],
             "jj": [Run(["jj", "split", jq, "-r", 'subject("add models")', "-m", "add user model", "user.py"]),
                    Run(["jj", "describe", jq, "-r", 'subject("add models")', "-m", "add order model"])]},
            notes={"but": "no split verb: uncommit the file (status-after prints the new IDs), reword the commit, commit the file above it"}),
        Scenario(
            "reorder_commits", "reorder commits (pilot-4)",
            "Reorder so 'add config' comes after 'add sender': add retry, add sender, add config.",
            [("add config", {"config.py": "LIMIT = 3\n"}), ("add retry", {"retry.py": "TRIES = 2\n"}),
             ("add sender", {"sender.py": "def send():\n    pass\n"})], {},
            [("add retry", {"retry.py": "TRIES = 2\n"}), ("add sender", {"sender.py": "def send():\n    pass\n"}),
             ("add config", {"config.py": "LIMIT = 3\n"})], {},
            {"git": [Run(["git", "rebase", q, "-i", "main"],
                         env=sequence_editor("sed -i.bak -e '/add config/{h;d;}' -e '/add sender/G'"))],
             "aisc": [Run(["aisc", "move", q, "add config", "--after", "add sender"])],
             "but": [Run(["but", "status", nh]),
                     Run(lambda c: ["but", "move", but_commit(c, "add config"), "--above", but_commit(c, "add sender")])],
             "jj": [Run(["jj", "rebase", jq, "-r", 'subject("add config")', "-A", 'subject("add sender")']),
                    Run(["jj", "bookmark", "move", jq, "feature", "--to", 'subject("add config")'])]}),
        Scenario(
            "squash_commits", "squash commits (pilot-5)",
            "Squash 'add parser' and 'fix parser typo' into one commit 'add parser module'.",
            squash_series, {}, squashed, {},
            {"git": [git_squash], "aisc": [aisc_squash], "but": but_squash, "jj": [jj_squash]}),
        Scenario(
            "uncommit_file", "recover accidental amend (pilot-10)",
            "Remove notes/private.md from the older commit 'add api' and keep it on disk, uncommitted.",
            [("add api", {"api.py": "ROUTES = []\n", "notes/private.md": "private\n"}),
             ("add docs", {"docs/guide.md": GUIDE_EDIT})], {},
            [("add api", {"api.py": "ROUTES = []\n"}), ("add docs", {"docs/guide.md": GUIDE_EDIT})],
            {"notes/private.md": "private\n"},
            {"git": [Run(["git", "rebase", q, "-i", "main"], env=sequence_editor("sed -i.bak '/add api/s/^pick/edit/'")),
                     Run(["git", "rm", q, "--cached", "notes/private.md"]),
                     Run(["git", "commit", q, "--amend", "--no-edit"]),
                     Run(["git", "rebase", "--continue"])],
             "aisc": [Run(["aisc", "uncommit", q, "add api", "notes/private.md"])],
             "but": [Run(["but", "status", "-f", nh]),
                     Run(lambda c: ["but", "uncommit", but_file(c, "notes/private.md")])],
             "jj": [Run(["jj", "squash", jq, "--from", 'subject("add api")', "--into", "@", "notes/private.md"])]}),
        Scenario(
            "undo_rewrite", "undo last operation (oplog)",
            "The squash of 'add parser' + 'fix parser typo' was a mistake: undo it, keeping the README edit.",
            squash_series, {"README.md": README_EDIT}, squash_series, {"README.md": README_EDIT},
            {"git": [Run(["git", "reflog", "-n", "5", "feature"]),
                     Run(lambda c: ["git", "reset", q, "--keep", git_reflog_before_rewrite(c)])],
             "aisc": [Run(["aisc", "undo", q])],
             "but": [Run(["but", "undo"])],
             "jj": [Run(["jj", "undo", jq])]},
            before={"git": [git_squash], "aisc": [aisc_squash], "but": but_squash, "jj": [jj_squash]},
            notes={"git": "no operation log: read the branch reflog, reset to the entry before the rebase"}),
        Scenario(
            "sync_conflict", "update dirty branch onto moved upstream (pilot-6/11)",
            "Fetch origin, rebase feature onto origin/main, resolve notify.py keeping push and sms, keep the README edit.",
            [("add sms channel", {"notify.py": NOTIFY_SMS})], {"README.md": README_EDIT},
            [("add sms channel", {"notify.py": NOTIFY_BOTH})], {"README.md": README_EDIT},
            {"git": [Run(["git", "pull", q, "--rebase", "--autostash", "origin", "main"], may_fail=True),
                     Edit("notify.py", NOTIFY_BOTH), Run(["git", "add", "notify.py"]),
                     Run(["git", "rebase", "--continue"])],
             "aisc": [Run(["aisc", "sync", q, "origin/main", "--fetch"], may_fail=True),
                      Edit("notify.py", NOTIFY_BOTH), Run(["aisc", "sync", q, "--continue"])],
             "but": [Run(["but", "pull"]),
                     Run(lambda c: ["but", "resolve", but_commit(c, "add sms channel")]),
                     Edit("notify.py", NOTIFY_BOTH), Run(["but", "resolve", "finish"])],
             "jj": [Run(["jj", "git", "fetch", jq]), Run(["jj", "rebase", jq, "-b", "feature", "-o", "main@origin"]),
                    Edit("notify.py", NOTIFY_BOTH), Run(["jj", "squash", jq, "notify.py"])]},
            notes={"but": "the conflicted commit ID comes from the `but pull` summary, so no status call"},
            upstream=[("add push channel", {"notify.py": NOTIFY_PUSH})], rebased=True),
        Scenario(
            "parallel_branch", "parallel branch / stack",
            "Commit the docs/guide.md edit as 'update guide' on a new branch docs based on main; feature and its app.py WIP stay as they are.",
            [helper], {"app.py": APP_LOGGED, "docs/guide.md": GUIDE_EDIT},
            [helper], {"app.py": APP_LOGGED},
            {"git": [Run(["git", "switch", q, "-c", "docs", "main"]),
                     Run(["git", "commit", q, "-m", "update guide", "--", "docs/guide.md"]),
                     Run(["git", "switch", q, "feature"])],
             "aisc": [Run(["aisc", "commit", q, "-b", "docs", "--onto", "main", "-m", "update guide",
                           "docs/guide.md"])],
             "but": [Run(["but", "commit", "-b", "docs", "-m", "update guide", "docs/guide.md"])],
             "jj": [Run(["jj", "split", jq, "-o", "main", "-m", "update guide", "docs/guide.md"]),
                    Run(["jj", "bookmark", "create", jq, "docs", "-r", 'subject("update guide")'])]},
            side={"docs": [("update guide", {"docs/guide.md": GUIDE_EDIT})]},
            tool_dirty={"but": {"app.py": APP_LOGGED, "docs/guide.md": GUIDE_EDIT}},
            notes={"but": "graded against a different expected working copy: both branches stay applied in the GitButler workspace, so the guide edit stays on disk"}),
        Scenario(
            "inspect_state", "inspect status/log/diff",
            "Report the branch, its commits since main, and every changed path with line counts. Change nothing.",
            parser_cli, {"parser.py": PARSER_FIXED, "README.md": README_EDIT, "notes.txt": "scratch\n"},
            parser_cli, {"parser.py": PARSER_FIXED, "README.md": README_EDIT, "notes.txt": "scratch\n"},
            {"git": [Run(["git", "status", "--short", "--branch"]), Run(["git", "log", "--oneline", "main..HEAD"]),
                     Run(["git", "diff", "--numstat"])],
             "aisc": [Run(["aisc", "log"])],
             "but": [Run(["but", "status", nh]), Run(["git", "diff", "--numstat"])],
             "jj": [Run(["jj", "log", "-r", "main..@"]), Run(["jj", "diff", "--stat"])]},
            read_only=True, facts=("feature", "add parser", "add cli", "parser.py", "README.md", "notes.txt")),
    ]


# ---------------------------------------------------------------- fixtures

def write_files(repository, files):
    for name, content in files.items():
        path = repository / name
        if content is None:
            path.unlink()
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)


def build_template(scenario, root, env):
    """Origin plus a clone on `feature`; identical bytes and hashes for every tool."""
    root.mkdir(parents=True)
    origin, repository = root / "origin.git", root / "repo"
    git(root, "init", "-q", "--bare", "-b", "main", str(origin), env=env)
    git(root, "init", "-q", "-b", "main", str(repository), env=env)
    write_files(repository, MAIN_FILES)
    git(repository, "add", ".", env=env)
    git(repository, "commit", "-qm", "initial", env=env)
    git(repository, "remote", "add", "origin", "../origin.git", env=env)
    git(repository, "push", "-q", "origin", "main", env=env)
    git(repository, "fetch", "-q", "origin", env=env)
    if scenario.upstream:
        upstream = root / "upstream"
        git(root, "clone", "-q", str(origin), str(upstream), env=env)
        commit_series(upstream, scenario.upstream, env)
        git(upstream, "push", "-q", "origin", "main", env=env)
        shutil.rmtree(upstream)
    git(repository, "switch", "-q", "-c", "feature", env=env)
    commit_series(repository, scenario.commits, env)
    return repository


def commit_series(repository, commits, env):
    for subject, files in commits:
        write_files(repository, files)
        git(repository, "add", "-A", env=env)
        git(repository, "commit", "-qm", subject, env=env)


def settle(tool):
    """`but` forks a background fetch; let it finish so it cannot race the timed commands."""
    if tool == "but":
        time.sleep(1.0)


def adopt(tool, repository, root, env, binaries):
    """Untimed per-tool adoption, as the vcbench harness does before an agent starts."""
    def run(*arguments):
        subprocess.run([binaries[arguments[0]], *arguments[1:]], cwd=repository, env=env, check=True,
                       stdin=subprocess.DEVNULL, capture_output=True, timeout=60)
    if tool == "but":
        git(repository, "switch", "-q", "main", env=env)
        run("but", "config", "metrics", "disable")  # no telemetry child process per command
        run("but", "setup")
        run("but", "skill", "install", "--path", str(root / "appdata/home/.claude/skills/gitbutler"))
        run("but", "apply", "feature")
        settle(tool)
    elif tool == "jj":
        run("jj", "git", "init", "--colocate")


# ---------------------------------------------------------------- grading

def tree_files(repository, revision):
    listing = git(repository, "ls-tree", "-r", "-z", revision)
    files = {}
    for entry in filter(None, listing.split("\0")):
        meta, path = entry.split("\t", 1)
        files[path] = subprocess.run(["git", "cat-file", "blob", meta.split()[2]], cwd=repository,
                                     check=True, capture_output=True).stdout.decode()
    return files


def disk_files(repository):
    files = {}
    for path in sorted(repository.rglob("*")):
        relative = path.relative_to(repository)
        if relative.parts[0] in (".git", ".jj") or not path.is_file():
            continue
        files[str(relative)] = path.read_text()
    return files


def expected_trees(base, series):
    trees, current = [], dict(base)
    for subject, overlay in series:
        current = dict(current)
        current.update(overlay)
        trees.append((subject, current))
    return trees


def branch_state(repository, branch, base):
    try:
        revisions = git(repository, "rev-list", "--reverse", "--parents", base + "..refs/heads/" + branch).split("\n")
    except subprocess.CalledProcessError:
        return None  # branch missing
    state = []
    for line in filter(None, revisions):
        commit, *parents = line.split()
        state.append((git(repository, "log", "-1", "--format=%s", commit).strip(), tree_files(repository, commit),
                      len(parents)))
    return state


def verify(scenario, tool, repository, main_before, transcript_text=""):
    base_ref = "refs/remotes/origin/main" if scenario.rebased else "refs/heads/main"
    base_sha = git(repository, "rev-parse", base_ref).strip()
    base = tree_files(repository, base_ref)
    checks = {}
    want = [(subject, files, 1) for subject, files in expected_trees(base, scenario.expected)]
    have = branch_state(repository, "feature", base_ref)
    checks["feature_commits"] = have == want
    first = git(repository, "rev-list", "--reverse", base_ref + "..refs/heads/feature").split()
    checks["feature_base"] = bool(first) and git(repository, "rev-parse", first[0] + "^").strip() == base_sha
    for branch, series in (scenario.side or {}).items():
        side = [(subject, files, 1) for subject, files in expected_trees(base, series)]
        checks[branch + "_commits"] = branch_state(repository, branch, base_ref) == side
    checks["main_unchanged"] = git(repository, "rev-parse", "refs/heads/main").strip() == main_before
    feature_tree = tree_files(repository, "refs/heads/feature")
    disk = disk_files(repository)
    ignored = (".claude", ".codex", "AGENTS.md", "CLAUDE.md")
    disk = {path: content for path, content in disk.items() if not path.startswith(ignored)}
    dirty = {path: content for path, content in disk.items() if feature_tree.get(path) != content}
    missing = sorted(set(feature_tree) - set(disk))
    checks["working_copy"] = dirty == scenario.tool_dirty.get(tool, scenario.expected_dirty) and not missing
    git_dir = repository / ".git"
    checks["no_operation_in_progress"] = not any((git_dir / name).exists() for name in (
        "rebase-merge", "rebase-apply", "MERGE_HEAD", "CHERRY_PICK_HEAD")) and not any(
        "<<<<<<<" in content for content in disk.values())
    if scenario.read_only:
        checks["facts_reported"] = all(fact in transcript_text for fact in scenario.facts)
    return checks
