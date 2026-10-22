"""Recompute the README's real-agent table from the published per-run results.

    python3 benchmarks/agents/summarize.py benchmarks/results/agent-runs-v0.1.0.json [...]
    python3 benchmarks/agents/summarize.py --runs ./agent-runs   # fresh run-agents.sh output

Prints, per tool: runs passed, and per 13-task suite the mean (min-max over repetitions) of
commands, tokens (input + cache creation + cache read + output, as the Claude CLI reported),
cost (the CLI's own estimate) and wall time. Then every failed run and why.
"""
import json
import os
import statistics
import sys
from collections import defaultdict


def argv_text(violation):
    argv = violation.get("argv", [])
    return f"{violation.get('tool', '')} {argv if isinstance(argv, str) else ' '.join(argv)}"


def from_result_dirs(root):
    runs = []
    for rep in sorted(os.listdir(root)):
        rep_dir = os.path.join(root, rep)
        if not os.path.isdir(rep_dir):
            continue
        for name in sorted(os.listdir(rep_dir)):
            path = os.path.join(rep_dir, name, "result.json")
            if not os.path.exists(path):
                continue
            task, arm = name.split("--")
            perf = json.load(open(path))["performance"]
            cost = perf["time_cost"]
            runs.append({
                "rep": int(rep.removeprefix("rep")), "task": task, "arm": arm,
                "passed": bool(perf["correctness"]["passed"])
                and bool(perf.get("compliance", {}).get("passed", True)),
                "failed_checks": perf["correctness"].get("failed_checks"),
                "policy_violations": [argv_text(v)
                                      for v in perf.get("compliance", {}).get("violations", [])],
                "model_tool_calls": perf["effort"]["model_tool_calls"],
                "tokens": cost.get("token_usage") or {},
                "cost_usd_reported_by_cli": cost.get("cost_usd"),
                "wall_ms": cost.get("wall_ms"),
            })
    return runs


def main(argv):
    runs = []
    if argv[:1] == ["--runs"]:
        runs = from_result_dirs(argv[1])
    else:
        for path in argv:
            runs += json.load(open(path))["runs"]
    keys = ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens")
    suites = defaultdict(lambda: defaultdict(lambda: defaultdict(float)))
    passed = defaultdict(lambda: [0, 0])
    for run in runs:
        suite = suites[run["arm"]][run["rep"]]
        suite["calls"] += run["model_tool_calls"]
        suite["tokens"] += sum(run["tokens"].get(k) or 0 for k in keys)
        suite["cost"] += run["cost_usd_reported_by_cli"] or 0
        suite["wall"] += (run["wall_ms"] or 0) / 1000
        passed[run["arm"]][0] += run["passed"]
        passed[run["arm"]][1] += 1

    def cell(values, fmt):
        return f"{fmt(statistics.mean(values))} ({fmt(min(values))}-{fmt(max(values))})"

    print("| Tool | Passed | Commands | Tokens | Cost | Time |")
    print("| --- | ---: | ---: | ---: | ---: | ---: |")
    for arm in sorted(suites):
        reps = list(suites[arm].values())
        col = lambda key: [r[key] for r in reps]
        print(f"| {arm} | {passed[arm][0]}/{passed[arm][1]} "
              f"| {cell(col('calls'), lambda v: f'{v:.0f}')} "
              f"| {cell(col('tokens'), lambda v: f'{v / 1e6:.2f}M')} "
              f"| {cell(col('cost'), lambda v: f'${v:.2f}')} "
              f"| {cell(col('wall'), lambda v: f'{v:.0f} s')} |")
    for run in runs:
        if not run["passed"]:
            why = run.get("failed_checks") or run.get("policy_violations")
            print(f"failed: {run['arm']} rep{run['rep']} {run['task']}: {why}")


if __name__ == "__main__":
    main(sys.argv[1:])
