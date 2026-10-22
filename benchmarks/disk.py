"""Disk cost of parallel agent workspaces: git worktrees, AISC, GitButler and jj.

Every scenario starts from a byte-identical copy of one synthetic repository and reports the bytes
it added (apparent size and allocated blocks, hard links counted once) across the checkout, its
`.git`/`.jj`, every extra workspace and the tool's own data directory. The JSON report holds no
machine-specific paths.
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from fixture import environment, git

ROOT = Path(__file__).resolve().parent.parent
GITBUTLER = Path("/tmp/aisc-gitbutler-bin/but")
DIRECTORIES = ("src", "docs", "assets", "tests", "vendor")


def measure(*roots):
    """Bytes under the given paths without following symlinks; each inode counts once."""
    seen = set()
    apparent = allocated = 0
    for root in roots:
        if not os.path.lexists(root):
            continue
        pending = [Path(root)]
        while pending:
            path = pending.pop()
            info = os.lstat(path)
            if (info.st_dev, info.st_ino) in seen:
                continue
            seen.add((info.st_dev, info.st_ino))
            apparent += info.st_size
            allocated += info.st_blocks * 512
            if path.is_dir() and not path.is_symlink():
                pending.extend(path.iterdir())
    return apparent, allocated


def content(index, size):
    """Deterministic text that compresses about as well as source code."""
    lines = []
    total = 0
    line = 0
    while total < size:
        digest = hashlib.sha256(f"{index}:{line}".encode()).hexdigest()
        text = f"value_{line % 97} = compute('{digest[:24]}', {line}, {index % 13})\n"
        lines.append(text)
        total += len(text)
        line += 1
    return "".join(lines)[:size]


def build_repository(path, files, file_bytes):
    path.mkdir(parents=True)
    git(path, "init", "-q", "-b", "main")
    for index in range(files):
        directory = DIRECTORIES[index % len(DIRECTORIES)]
        target = path / directory / f"group{index % 40:02d}" / f"file{index:05d}.txt"
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content(index, file_bytes))
    git(path, "add", ".")
    git(path, "commit", "-qm", "synthetic base")
    git(path, "gc", "-q")
    return path


def tool_environment(scenario):
    values = environment()
    home = scenario / "home"
    home.mkdir(exist_ok=True)
    # Tool data (GitButler app data, jj config) lands inside the measured scenario directory.
    values.update(HOME=str(home), E2E_TEST_APP_DATA_DIR=str(scenario / "appdata"),
                  XDG_CONFIG_HOME=str(home / ".config"), XDG_DATA_HOME=str(home / ".local/share"),
                  BUT_PAGER="cat", NO_COLOR="1", GIT_EDITOR="true",
                  JJ_USER="Benchmark", JJ_EMAIL="bench@example.invalid",
                  JJ_CONFIG=str(home / "jj.toml"))
    (home / "jj.toml").write_text("")
    return values


def run(arguments, cwd, values):
    result = subprocess.run([str(part) for part in arguments], cwd=cwd, env=values,
                            stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=300)
    if result.returncode != 0:
        raise RuntimeError(f"{Path(str(arguments[0])).name} {arguments[1:3]} failed: "
                           f"{result.stderr.strip()[-400:]}")
    return result.stdout


def version(arguments):
    try:
        result = subprocess.run([str(part) for part in arguments], stdin=subprocess.DEVNULL,
                                capture_output=True, text=True, timeout=30)
    except OSError:
        return None
    text = (result.stdout or result.stderr).strip().splitlines()
    return text[0] if result.returncode == 0 and text else None


def aisc_tasks(aisc, scenario, values, agents, extra=()):
    for agent in range(agents):
        run([aisc, "start", f"agent-{agent}", "--path", scenario / f"wt-{agent}", *extra],
            scenario / "repo", values)


def finish_all(aisc, scenario, values, agents, keep):
    for agent in range(agents):
        worktree = scenario / f"wt-{agent}"
        target = f"src/group00/agent-{agent}.txt"
        (worktree / target).write_text(f"agent {agent} result\n")
        run([aisc, "checkpoint", "-m", f"agent {agent}", "--", target], worktree, values)
        run([aisc, "verify", "--", "true"], worktree, values)
        run([aisc, "finish", *(["--keep"] if keep else [])], worktree, values)


def scenarios(aisc, jj):
    def git_worktrees(scenario, values, agents):
        for agent in range(agents):
            run(["git", "worktree", "add", "-q", "-b", f"agent-{agent}", scenario / f"wt-{agent}"],
                scenario / "repo", values)

    def git_no_checkout(scenario, values, agents):
        for agent in range(agents):
            run(["git", "worktree", "add", "-q", "--no-checkout", "-b", f"agent-{agent}",
                 scenario / f"wt-{agent}"], scenario / "repo", values)

    def git_sparse(scenario, values, agents):
        git_no_checkout(scenario, values, agents)
        for agent in range(agents):
            worktree = scenario / f"wt-{agent}"
            run(["git", "sparse-checkout", "set", "--cone", "src"], worktree, values)
            run(["git", "checkout", "-q"], worktree, values)

    def start(scenario, values, agents):
        aisc_tasks(aisc, scenario, values, agents)

    def sparse(scenario, values, agents):
        aisc_tasks(aisc, scenario, values, agents, ("--sparse", "src"))

    def finished(scenario, values, agents):
        aisc_tasks(aisc, scenario, values, agents)
        finish_all(aisc, scenario, values, agents, keep=False)

    def collected(scenario, values, agents):
        aisc_tasks(aisc, scenario, values, agents)
        finish_all(aisc, scenario, values, agents, keep=True)
        run([aisc, "gc", "--apply", "--min-age", "0"], scenario / "repo", values)

    def in_place(scenario, values, agents):
        # `aisc start NAME` defaults to in-place (D6): every lane shares the one checkout, so
        # lanes take turns: start, edit, checkpoint, verify, finish.
        repository = scenario / "repo"
        for agent in range(agents):
            target = f"src/group00/agent-{agent}.txt"
            run([aisc, "start", f"lane-{agent}"], repository, values)
            (repository / target).write_text(f"agent {agent} result\n")
            run([aisc, "checkpoint", "-m", f"agent {agent}", "--", target], repository, values)
            run([aisc, "verify", "--", "true"], repository, values)
            run([aisc, "finish"], repository, values)

    def in_place_commits(scenario, values, agents):
        repository = scenario / "repo"
        for agent in range(agents):
            target = f"src/group00/agent-{agent}.txt"
            (repository / target).write_text(f"agent {agent} result\n")
            run([aisc, "commit", "-m", f"agent {agent}", target], repository, values)

    def gitbutler(scenario, values, agents):
        repository = scenario / "repo"
        run([GITBUTLER, "setup"], repository, values)
        for agent in range(agents):
            run([GITBUTLER, "branch", "new", f"agent-{agent}"], repository, values)

    def jujutsu(scenario, values, agents):
        repository = scenario / "repo"
        run([jj, "git", "init", "--colocate"], repository, values)
        for agent in range(agents):
            run([jj, "workspace", "add", "--name", f"agent-{agent}", scenario / f"wt-{agent}"],
                repository, values)

    found = [
        ("git worktree add", git_worktrees, "N full checkouts"),
        ("git worktree add --no-checkout", git_no_checkout,
         "N worktrees with Git metadata only (no index, no files); not usable until checked out"),
        ("git worktree add + sparse-checkout src", git_sparse,
         "plain-Git equivalent of aisc start --sparse src"),
        ("aisc start --path", start, "N full checkouts plus registry"),
        ("aisc start --path --sparse src", sparse, "N checkouts of the src cone only"),
        ("aisc finish (worktrees removed)", finished, "start, checkpoint, verify, finish; branches kept"),
        ("aisc gc --apply (after finish --keep)", collected, "finished worktrees reclaimed by gc"),
        ("aisc start (in-place default) x N lanes", in_place,
         "one shared checkout, N sequential start/checkpoint/verify/finish lanes; no parallel "
         "file isolation, same trade-off as GitButler"),
        ("aisc commit in place", in_place_commits,
         "one checkout, N sequential history commits; object growth only"),
    ]
    if GITBUTLER.exists():
        found.append(("gitbutler but branch new", gitbutler,
                      "but setup plus N virtual branches in one shared workspace; no parallel "
                      "file isolation; app data included"))
    if jj:
        found.append(("jj workspace add", jujutsu, "colocated jj plus N workspaces"))
    return found


def locate_aisc(explicit):
    if explicit:
        return Path(explicit).resolve()
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    for profile in ("release", "debug"):
        if (target / profile / "aisc").exists():
            return target / profile / "aisc"
    subprocess.run(["cargo", "build", "--quiet"], cwd=ROOT, check=True, stdin=subprocess.DEVNULL)
    return target / "debug" / "aisc"


def locate_jj(explicit):
    candidates = [explicit, os.environ.get("AISC_BENCH_JJ"), shutil.which("jj")]
    return next((Path(candidate) for candidate in candidates
                 if candidate and Path(candidate).is_file() and os.access(candidate, os.X_OK)), None)


def mebibytes(count):
    return f"{count / 1_048_576:8.1f} MiB"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--files", type=int, default=5_000)
    parser.add_argument("--file-bytes", type=int, default=10_240)
    parser.add_argument("--agents", type=int, default=4)
    parser.add_argument("--aisc", help="aisc binary (default: target/{release,debug}/aisc)")
    parser.add_argument("--jj", help="jj binary (default: PATH or AISC_BENCH_JJ)")
    parser.add_argument("--output", default=str(ROOT / "benchmarks/results/disk.json"))
    arguments = parser.parse_args()
    aisc = locate_aisc(arguments.aisc)
    jj = locate_jj(arguments.jj)
    report = {
        "version": 1,
        "agents": arguments.agents,
        "tools": {"git": version(["git", "--version"]), "aisc": version([aisc, "--version", "--human"]),
                  "gitbutler": version([GITBUTLER, "--version"]) if GITBUTLER.exists() else None,
                  "jj": version([jj, "--version"]) if jj else None},
        "platform": sys.platform,
        "scenarios": [],
    }
    with tempfile.TemporaryDirectory(prefix="aisc-disk-") as temporary:
        work = Path(temporary).resolve()
        template = build_repository(work / "template", arguments.files, arguments.file_bytes)
        apparent, allocated = measure(template)
        checkout = measure(*[path for path in template.iterdir() if path.name != ".git"])
        report["repository"] = {"files": arguments.files, "apparent_bytes": apparent,
                                "allocated_bytes": allocated, "checkout_allocated_bytes": checkout[1],
                                "src_share": round(1 / len(DIRECTORIES), 2)}
        for name, action, note in scenarios(aisc, jj):
            scenario = work / f"scenario-{len(report['scenarios'])}"
            scenario.mkdir()
            shutil.copytree(template, scenario / "repo", symlinks=True)
            values = tool_environment(scenario)
            before = measure(scenario)
            started = time.perf_counter()
            entry = {"name": name, "note": note}
            try:
                action(scenario, values, arguments.agents)
                after = measure(scenario)
                entry.update(added_apparent_bytes=after[0] - before[0],
                             added_allocated_bytes=after[1] - before[1],
                             per_agent_allocated_bytes=(after[1] - before[1]) // arguments.agents,
                             seconds=round(time.perf_counter() - started, 2))
            except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
                entry["error"] = str(error).replace(str(work), "<work>")
            report["scenarios"].append(entry)
            shutil.rmtree(scenario, ignore_errors=True)
    text = json.dumps(report, indent=2) + "\n"
    for private in (str(Path.home()), str(ROOT), tempfile.gettempdir()):
        if private in text:
            raise SystemExit(f"refusing to write a machine-specific path into the report: {private}")
    Path(arguments.output).write_text(text)
    baseline = next((entry for entry in report["scenarios"] if entry["name"] == "git worktree add"), None)
    print(f"{arguments.agents} agents on {arguments.files} files "
          f"({mebibytes(report['repository']['allocated_bytes']).strip()} repository); "
          f"added allocated bytes per scenario:")
    for entry in report["scenarios"]:
        if "error" in entry:
            print(f"  {entry['name']:<42} error: {entry['error'][:120]}")
            continue
        ratio = ""
        if baseline and "added_allocated_bytes" in baseline and baseline["added_allocated_bytes"]:
            ratio = f"  {entry['added_allocated_bytes'] / baseline['added_allocated_bytes']:6.1%} of git worktree add"
        print(f"  {entry['name']:<42} {mebibytes(entry['added_allocated_bytes'])}{ratio}")
    print(f"wrote {Path(arguments.output).name}")


if __name__ == "__main__":
    main()
