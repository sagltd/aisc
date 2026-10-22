"""Deterministic command benchmark: tool calls, transcript bytes, token estimates and
wall time for vcbench-style tasks across Git, AISC, GitButler and Jujutsu.

No model is involved. Token figures are an offline tokenizer estimate of the
transcript text an agent would read or type, never provider-billed usage.
"""

import argparse
import os
import hashlib
import json
import platform
import shlex
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

from scenarios import (MODES, TOOLS, Edit, adopt, build_template, environment, for_mode, git, scenarios, settle,
                       verify, write_files)

TIMEOUT = 120


def tokenizer():
    try:
        import tiktoken
        encoding = tiktoken.get_encoding("o200k_base")
        return "tiktoken/o200k_base " + tiktoken.__version__, lambda text: len(encoding.encode(text))
    except Exception:  # offline without the package or its cached BPE file
        return "estimate/bytes_div_4", lambda text: (len(text.encode()) + 3) // 4


def resolve_binaries(arguments):
    binaries, skipped = {"cat": shutil.which("cat")}, {}
    candidates = {"git": shutil.which("git"), "aisc": arguments.aisc, "but": arguments.but, "jj": arguments.jj}
    for tool, path in candidates.items():
        if path and Path(path).is_file():
            # Keep the `but` symlink name: GitButler selects CLI mode from argv[0].
            binaries[tool] = str(Path(path).absolute())
        else:
            skipped[tool] = "binary not found"
    return binaries, skipped


def version(binaries, tool):
    argument = "--version"
    result = subprocess.run([binaries[tool], argument], capture_output=True, text=True,
                            stdin=subprocess.DEVNULL, timeout=30)
    return (result.stdout or result.stderr).strip().splitlines()[0]


def step_env(step, mode):
    """A tuple value is (default-mode value, quietest-mode value)."""
    return {key: (value[MODES.index(mode)] if isinstance(value, tuple) else value) for key, value in step.env.items()}


def command_line(extra, argv):
    prefix = "".join(key + "=" + shlex.quote(value) + " " for key, value in extra.items())
    return prefix + " ".join(shlex.quote(part) for part in argv)


def execute(steps, repository, env, binaries, scrub, log=None, records=None, context=None, mode="default"):
    """Run steps; return per-step records. With `log`, record Git calls seen by the PATH wrapper."""
    context = {"outputs": []} if context is None else context
    records = [] if records is None else records
    for step in steps:
        if isinstance(step, Edit):
            write_files(repository, {step.path: step.content})
            context["outputs"].append("")
            records.append({"kind": "edit", "path": step.path, "authored": step.path + "\n" + step.content,
                            "read": "", "wall_ms": 0.0, "exit_code": 0})
            continue
        argv = for_mode(step.argv(context) if callable(step.argv) else step.argv, mode)
        extra = step_env(step, mode)
        values = dict(env)
        values.update(extra)
        seen = len(log.read_text().splitlines()) if log else 0
        started = time.perf_counter()
        result = subprocess.run([binaries[argv[0]], *argv[1:]], cwd=repository, env=values,
                                input=(step.stdin or "").encode(), capture_output=True, timeout=TIMEOUT)
        elapsed = (time.perf_counter() - started) * 1000
        output = scrub(result.stdout.decode(errors="replace") + result.stderr.decode(errors="replace"))
        context["outputs"].append(output)
        authored = "$ " + command_line(extra, argv) + "\n" + (step.stdin or "")
        records.append({"kind": "run", "command": command_line(extra, argv), "stdin": step.stdin,
                        "authored": authored, "read": output, "wall_ms": elapsed,
                        "exit_code": result.returncode})
        if log:
            # A top-level `git` step is logged last, after its own nested calls; keep only nested ones.
            calls = [line.split("\t") for line in log.read_text().splitlines()[seen:]]
            records[-1]["git_calls"] = calls[:-1] if argv[0] == "git" else calls
        if result.returncode and not step.may_fail:
            raise RuntimeError("step failed: " + command_line(extra, argv) + "\n" + output)
    return records


def one_run(scenario, tool, template, scratch, binaries, count, index, mode="default"):
    root = scratch / ("run-" + scenario.name + "-" + tool + "-" + mode + "-" + str(index))
    if root.exists():
        shutil.rmtree(root)
    shutil.copytree(template, root, symlinks=True)
    repository = root / "repo"
    env = environment(root)
    (root / "home").mkdir(exist_ok=True)

    def scrub(text):
        return text.replace(str(root.resolve()), "<run>").replace(str(root), "<run>")
    try:
        adopt(tool, repository, root, env, binaries)
        write_files(repository, scenario.dirty)
        if scenario.before:
            execute(scenario.before[tool], repository, env, binaries, scrub, mode=mode)
            settle(tool)
        main_before = git(repository, "rev-parse", "refs/heads/main").strip()
        records, error = [], None
        try:
            execute(scenario.solutions[tool], repository, env, binaries, scrub, records=records, mode=mode)
        except (RuntimeError, LookupError, subprocess.TimeoutExpired) as failure:
            error = scrub(str(failure))[:2000]
        settle(tool)  # untimed: let a background `but` fetch finish before grading and the next run
        read ="".join(record["read"] for record in records)
        checks = verify(scenario, tool, repository, main_before, read) if error is None else {"completed": False}
        if error or not all(checks.values()):
            debug = [(record.get("command") or record.get("path"), record["read"]) for record in records]
            state = git(repository, "log", "--format=%h %s", "--stat", "main~0..refs/heads/feature")
            print(json.dumps({"scenario": scenario.name, "tool": tool, "steps": debug, "feature": state,
                              "checks": checks}, indent=1), file=sys.stderr)
    finally:
        shutil.rmtree(root, ignore_errors=True)
    authored = "".join(record["authored"] for record in records)
    return {
        "scenario": scenario.name, "tool": tool, "mode": mode, "index": index, "error": error,
        "working_copy_expectation": "tool-specific" if tool in scenario.tool_dirty else "shared",
        "commands": sum(record["kind"] == "run" for record in records),
        "edits": sum(record["kind"] == "edit" for record in records),
        "output_bytes": len(read.encode()), "authored_bytes": len(authored.encode()),
        "output_tokens_est": count(read), "authored_tokens_est": count(authored),
        "wall_ms": sum(record["wall_ms"] for record in records),
        "step_wall_ms": [round(record["wall_ms"], 3) for record in records],
        "checks": checks, "passed": bool(checks) and all(checks.values()),
        "transcript": [{key: value for key, value in record.items() if key != "authored"} for record in records],
    }


def summarize(runs, tools, names):
    rows, totals = [], {}
    for name in names:
        for tool in tools:
            sample = [run for run in runs if run["scenario"] == name and run["tool"] == tool]
            if not sample:
                continue
            wall = [run["wall_ms"] for run in sample]
            first = sample[0]
            rows.append({
                "scenario": name, "tool": tool, "runs": len(sample),
                "working_copy_expectation": first["working_copy_expectation"],
                "passes": sum(run["passed"] for run in sample),
                "commands": first["commands"], "edits": first["edits"], "tool_calls": first["commands"] + first["edits"],
                "output_bytes": statistics.median(run["output_bytes"] for run in sample),
                "authored_bytes": statistics.median(run["authored_bytes"] for run in sample),
                "output_tokens_est": statistics.median(run["output_tokens_est"] for run in sample),
                "authored_tokens_est": statistics.median(run["authored_tokens_est"] for run in sample),
                "wall_ms": {"median": round(statistics.median(wall), 3), "min": round(min(wall), 3),
                            "max": round(max(wall), 3)},
            })
    for tool in tools:
        mine = [row for row in rows if row["tool"] == tool]
        if not mine:
            continue
        per_repetition = {}
        for run in runs:
            if run["tool"] == tool:
                per_repetition.setdefault(run["index"], 0.0)
                per_repetition[run["index"]] += run["wall_ms"]
        totals[tool] = {
            "scenarios": len(mine), "all_passed": all(row["passes"] == row["runs"] for row in mine),
            **{key: sum(row[key] for row in mine) for key in (
                "commands", "edits", "tool_calls", "output_bytes", "authored_bytes",
                "output_tokens_est", "authored_tokens_est")},
            "wall_ms_sum_of_medians": round(sum(row["wall_ms"]["median"] for row in mine), 3),
            "wall_ms_median_of_totals": round(statistics.median(per_repetition.values()), 3),
        }
    return rows, totals


def git_wrapper(directory, real_git):
    """PATH shim that logs "<subcommand>\\t<elapsed ms>" per call, skipping global options."""
    directory.mkdir(exist_ok=True)
    shim = directory / "git"
    shim.write_text(
        "#!/usr/bin/perl\nuse Time::HiRes qw(time);\nmy @a = @ARGV;\n"
        "my $i = 0;\nwhile ($i < @a && $a[$i] =~ /^-/) {\n"
        "  $i += ($a[$i] =~ /^(-C|-c|--git-dir|--work-tree|--namespace|--config-env)$/) ? 2 : 1;\n}\n"
        "my $s = time;\nmy $rc = system(" + json.dumps(real_git) + ", @a);\n"
        "open(my $f, '>>', $ENV{AISC_BENCH_GIT_LOG});\n"
        "printf $f \"%s\\t%.3f\\n\", ($a[$i] // '-'), (time - $s) * 1000;\nclose($f);\n"
        "exit($rc == -1 ? 127 : ($rc & 127 ? 128 + ($rc & 127) : $rc >> 8));\n")
    shim.chmod(0o755)
    return shim


def trace2_busy(path):
    """Git processes seen by trace2 and the wall time they were running (union of spans)."""
    from datetime import datetime
    if not path.exists():
        return 0, 0, 0.0
    starts, spans, nested = {}, [], 0
    for line in path.read_text().splitlines():
        event = json.loads(line)
        if event.get("event") == "start":
            starts[event["sid"]] = datetime.fromisoformat(event["time"].replace("Z", "+00:00")).timestamp()
        elif event.get("event") == "exit" and event["sid"] in starts:
            if "/" in event["sid"]:
                nested += 1  # spawned by another git process
                continue
            spans.append((starts[event["sid"]],
                          datetime.fromisoformat(event["time"].replace("Z", "+00:00")).timestamp()))
    busy, reach = 0.0, None
    for start, end in sorted(spans):
        if reach is None or start > reach:
            busy += end - start
            reach = end
        elif end > reach:
            busy += end - reach
            reach = end
    return len(spans), nested, busy * 1000


def prepared(scenario, tool, template, root, binaries):
    if root.exists():
        shutil.rmtree(root)
    shutil.copytree(template, root, symlinks=True)
    env = environment(root)
    (root / "home").mkdir(exist_ok=True)
    adopt(tool, root / "repo", root, env, binaries)
    write_files(root / "repo", scenario.dirty)
    if scenario.before:
        execute(scenario.before[tool], root / "repo", env, binaries, str)
        settle(tool)
    return env


def real_git(binaries):
    """On macOS /usr/bin/git is an xcrun shim; find the binary it forwards to."""
    if platform.system() != "Darwin" or binaries["git"] != "/usr/bin/git":
        return None
    found = subprocess.run(["xcrun", "--find", "git"], capture_output=True, text=True, stdin=subprocess.DEVNULL)
    return found.stdout.strip() or None


def profile(binaries, scratch, templates, chosen, repetitions):
    """Where the time goes: process start vs cheap verbs, and Git subprocesses per step.

    Pass 1 counts Git calls through a PATH shim (the shim's own cost is not timed).
    Pass 2 uses GIT_TRACE2_EVENT, which adds no process, to split each step's wall time
    into time spent inside Git processes and time outside them (spawn/exec, the
    /usr/bin/git shim, the tool's own work).
    """
    report = {"startup": {}, "steps": {}}
    root = scratch / "profile"
    inspect = next(scenario for scenario in scenarios() if scenario.name == "inspect_state")
    resolved = real_git(binaries)
    probes = [("aisc --version", "aisc", ["aisc", "--version"]), ("aisc context", "aisc", ["aisc", "context"]),
              ("aisc log", "aisc", ["aisc", "log"]), ("git --version", "git", ["git", "--version"]),
              ("git status", "git", ["git", "status", "--short"]),
              ("but --version", "but", ["but", "--version"]), ("but status", "but", ["but", "status", "--no-hint"]),
              ("jj --version", "jj", ["jj", "--version"]), ("jj log", "jj", ["jj", "log", "-r", "main..@"])]
    if resolved:
        probes.insert(4, ("git --version (xcrun-resolved binary, no shim)", "git", [resolved, "--version"]))
    for label, tool, argv in probes:
        if tool not in binaries:
            continue
        env = prepared(inspect, tool if tool in ("but", "jj") else "git", templates / "inspect_state", root, binaries)
        command = [binaries.get(argv[0], argv[0]), *argv[1:]]
        samples = []
        for _ in range(repetitions + 2):
            started = time.perf_counter()
            subprocess.run(command, cwd=root / "repo", env=env, capture_output=True, stdin=subprocess.DEVNULL, timeout=60)
            samples.append((time.perf_counter() - started) * 1000)
        samples = samples[2:]
        report["startup"][label] = {"median_ms": round(statistics.median(samples), 3), "min_ms": round(min(samples), 3)}
    shim = git_wrapper(scratch / "git-wrapper", binaries["git"])
    log = scratch / "git-calls.log"
    for scenario in chosen:
        for tool in TOOLS:
            if tool not in binaries or scenario.solutions.get(tool) is None:
                continue
            env = prepared(scenario, tool, templates / scenario.name, root, binaries)
            log.write_text("")
            counted = dict(env, PATH=str(shim.parent) + ":" + env["PATH"], AISC_BENCH_GIT_LOG=str(log))
            records = execute(scenario.solutions[tool], root / "repo", counted, dict(binaries, git=str(shim)), str, log=log)
            env = prepared(scenario, tool, templates / scenario.name, root, binaries)
            context, steps, traced = {"outputs": []}, [], []
            for number, step in enumerate(scenario.solutions[tool]):
                trace = scratch / ("trace-" + str(number) + ".json")
                if trace.exists():
                    trace.unlink()
                traced.extend(execute([step], root / "repo", dict(env, GIT_TRACE2_EVENT=str(trace)), binaries, str,
                                      context=context))
                traced[-1]["trace"] = trace2_busy(trace)
            for counts, timing in zip(records, traced):
                if counts["kind"] != "run":
                    continue
                by_name = {}
                for name, _ in counts["git_calls"]:
                    name = "<path>" if "/" in name else name  # never record machine paths
                    by_name[name] = by_name.get(name, 0) + 1
                processes, nested, busy = timing["trace"]
                steps.append({"command": counts["command"], "wall_ms": round(timing["wall_ms"], 3),
                              "git_subprocesses": len(counts["git_calls"]),
                              "git_processes_traced": processes + nested,
                              "inside_git_ms": round(busy, 3),
                              "outside_git_ms": round(timing["wall_ms"] - busy, 3),
                              "by_subcommand": dict(sorted(by_name.items(), key=lambda item: -item[1]))})
            report["steps"].setdefault(scenario.name, {})[tool] = steps
    shutil.rmtree(root, ignore_errors=True)
    return report




def build_aisc(source, target_dir):
    """Build the candidate with the release profile so a debug binary can never be measured."""
    values = dict(os.environ, CARGO_TARGET_DIR=str(target_dir))
    subprocess.run(["cargo", "build", "--release", "--locked", "--quiet"], cwd=source, env=values, check=True,
                   stdin=subprocess.DEVNULL, timeout=1800)
    return target_dir / "release" / "aisc"


def build_record(source, binary, built):
    def read(*argv):
        return subprocess.run(argv, cwd=source, capture_output=True, text=True, stdin=subprocess.DEVNULL).stdout.strip()
    binary = Path(binary).resolve()
    if binary.parent.name != "release":
        raise SystemExit("--aisc must be a cargo release build (<target>/release/aisc); omit --aisc to build one")
    manifest = (source / "Cargo.toml").read_text()
    release = manifest.split("[profile.release]", 1)[1].split("\n[", 1)[0] if "[profile.release]" in manifest else ""
    return {
        "cargo_profile": "release", "built_by_runner": built,
        "profile_release_overrides": [line.strip() for line in release.strip().splitlines() if line.strip()],
        "debug_assertions": "debug-assertions = true" in release.replace("  ", " "),  # cargo release default: off
        "binary": "target/release/aisc", "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "git_commit": read("git", "rev-parse", "HEAD"),
        "source_clean": read("git", "status", "--porcelain", "--", "src", "Cargo.toml", "Cargo.lock") == "",
        "rustc": read("rustc", "-V"),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--aisc", default=None, help="a release binary; omitted: build with cargo --release")
    parser.add_argument("--but", default=None)
    parser.add_argument("--jj", default=None)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repetitions", type=int, default=10)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--scenarios", nargs="*", default=None)
    parser.add_argument("--tools", nargs="*", default=list(TOOLS))
    parser.add_argument("--modes", nargs="*", default=list(MODES), choices=MODES)
    parser.add_argument("--no-profile", action="store_true")
    arguments = parser.parse_args()
    if arguments.repetitions < 10:
        print("note: fewer than 10 repetitions; medians are less stable", file=sys.stderr)
    source = Path(__file__).resolve().parent.parent.parent
    built = arguments.aisc is None and "aisc" in arguments.tools
    if built:
        arguments.aisc = str(build_aisc(source, source / "target"))
    build = build_record(source, arguments.aisc, built) if "aisc" in arguments.tools else None
    binaries, skipped = resolve_binaries(arguments)
    tools = [tool for tool in arguments.tools if tool in binaries]
    modes = arguments.modes
    label, count = tokenizer()
    scratch = Path(__file__).resolve().parent.parent / ".scratch" / "commands"
    if scratch.exists():
        shutil.rmtree(scratch)
    scratch.mkdir(parents=True)
    chosen = [scenario for scenario in scenarios()
              if not arguments.scenarios or scenario.name in arguments.scenarios]
    templates = scratch / "templates"
    needed = chosen + [scenario for scenario in scenarios() if scenario.name == "inspect_state"
                       and scenario.name not in {chosen_one.name for chosen_one in chosen}]
    for scenario in needed:  # the startup profile always reads the inspection fixture
        build_template(scenario, templates / scenario.name, environment(templates / scenario.name))
    runs = []
    load_before = os.getloadavg()
    total = arguments.warmup + arguments.repetitions
    pairs = [(mode, tool) for mode in modes for tool in tools]
    try:
        for repetition in range(total):
            for position, scenario in enumerate(chosen):
                shift = (repetition * len(tools) + position) % len(pairs)
                order = pairs[shift:] + pairs[:shift]  # each (mode, tool) takes every slot over the run
                for mode, tool in order:
                    if scenario.solutions.get(tool) is None:
                        continue
                    run = one_run(scenario, tool, templates / scenario.name, scratch, binaries, count, repetition, mode)
                    run["warmup"] = repetition < arguments.warmup
                    runs.append(run)
                    if not run["passed"]:
                        print("FAIL", scenario.name, tool, mode, run["error"] or run["checks"], file=sys.stderr)
            print("repetition", repetition + 1, "of", total, file=sys.stderr)
        measured = [run for run in runs if not run["warmup"]]
        summary = {mode: summarize([run for run in measured if run["mode"] == mode], tools,
                                   [scenario.name for scenario in chosen]) for mode in modes}
        profiled = None if arguments.no_profile else profile(
            {tool: path for tool, path in binaries.items() if tool in tools or tool == "cat"},
            scratch, templates, chosen, arguments.repetitions)
        load_after = os.getloadavg()
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    samples = {}
    for run in measured:
        samples.setdefault((run["scenario"], run["tool"], run["mode"]), run)
    result = {
        "schema": 3, "benchmark": "commands-v2",
        "metric_kind": "deterministic_command_transcripts_not_provider_usage",
        "tokenizer": label,
        "token_text": "output = stdout+stderr of each command; authored = '$ ' + command line (+ stdin), or path + content for an edit step; run paths scrubbed to <run>",
        "timing": "sum of per-command wall time (perf_counter around each subprocess); fixture copy, tool adoption (but setup/apply, jj git init) and grading excluded; warm filesystem",
        "modes": {"default": "no quiet flags anywhere",
                  "quietest": "each tool's documented quiet flags: git -q, aisc -q (not on switch/undo/log, which have none), jj --quiet, but status --no-hint (but has no quiet flag on writes)"},
        "repetitions": arguments.repetitions, "warmup": arguments.warmup,
        "order": "(mode, tool) pairs rotate through every slot across repetitions and scenarios",
        "platform": platform.system() + " " + platform.machine(), "cpu_count": os.cpu_count(),
        "load_average_1_5_15": {"before": [round(value, 2) for value in load_before],
                                "after": [round(value, 2) for value in load_after]},
        "python": platform.python_version(),
        "versions": {tool: version(binaries, tool) for tool in tools},
        "aisc_build": build,
        "skipped": skipped,
        "scenarios": [{"name": scenario.name, "family": scenario.family, "task": scenario.task,
                       "notes": scenario.notes,
                       "graded_against_tool_specific_working_copy": sorted(scenario.tool_dirty),
                       "unsupported": [tool for tool in tools if scenario.solutions.get(tool) is None]}
                      for scenario in chosen],
        "totals": {mode: summary[mode][1] for mode in modes},
        "rows": {mode: summary[mode][0] for mode in modes},
        "profile_mode": "default", "profile": profiled,
        "sample_transcripts": [{"scenario": key[0], "tool": key[1], "mode": key[2], "steps": value["transcript"],
                                "checks": value["checks"]} for key, value in samples.items()],
        "runs": [{key: value for key, value in run.items() if key != "transcript"} for run in runs],
    }
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(json.dumps(result, indent=1) + "\n")
    print(json.dumps(result["totals"], indent=1))


if __name__ == "__main__":
    main()
