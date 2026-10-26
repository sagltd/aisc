"""Export metrics-only run records from a run-agents.sh output directory.

Usage: python3 benchmarks/agents/export.py RUNS_DIR OUTPUT.json --aisc-version "0.1.2 (dev 3bfe06e)" \
           --note "..." [--note "..."]

Reads OUT/rep*/TASK--ARM/{result.json,command-trace.tsv} as the patched vcbench harness writes
them and writes the `aisc-agent-runs/1` schema used by benchmarks/results/agent-runs-*.json:
pass/fail with failed checks and policy violations, tool calls, turns, the Claude CLI's token and
cost report, wall time, and every version-control command the agent itself ran (tool, arguments,
exit code). Temporary paths become <tmp>. Harness probes, the CLI's own git probes and git calls
made by a tool are left out; no model text, prompt or path is included.
"""

import argparse
import json
import os
import re
import sys


def agent_commands(run_dir):
    """Commands the agent's shell ran: not tool-internal, not the harness's or the CLI's probes."""
    commands = []
    trace = os.path.join(run_dir, "command-trace.tsv")
    if not os.path.exists(trace):
        return commands
    for line in open(trace):
        if not line.startswith("v3\t"):
            continue
        record = json.loads(line.split("\t", 1)[1])
        if record.get("internal") or record.get("grandparent") != "claude":
            continue
        argv = record.get("argv_array") or []
        args = " ".join(argv)
        args = re.sub(re.escape(run_dir) + r"\S*", "<tmp>", args)
        args = re.sub(r"/(?:private/)?(?:tmp|var/folders)/\S+", "<tmp>", args)
        commands.append({"tool": record["tool"], "args": args, "exit": record.get("status")})
    return commands


def run_record(root, rep, name):
    run_dir = os.path.join(root, rep, name)
    result = json.load(open(os.path.join(run_dir, "result.json")))
    task, arm = name.split("--")
    perf = result["performance"]
    cost = perf["time_cost"]
    usage = cost.get("token_usage") or {}
    compliance = perf.get("compliance", {})
    return {
        "rep": int(rep.removeprefix("rep")), "task": task, "arm": arm,
        "passed": bool(perf["correctness"]["passed"]) and bool(compliance.get("passed", True)),
        "correctness_passed": bool(perf["correctness"]["passed"]),
        "failed_checks": perf["correctness"].get("failed_checks", []),
        "compliance_passed": bool(compliance.get("passed", True)),
        "policy_violations": [" ".join([v.get("tool", ""), *(v.get("argv", []) if isinstance(v.get("argv"), list) else [str(v.get("argv", ""))])]).strip()
                              for v in compliance.get("violations", [])],
        "model_tool_calls": perf["effort"]["model_tool_calls"],
        "turns": cost.get("turns"),
        "tokens": {key: usage.get(key) for key in (
            "input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens", "output_tokens")},
        "cost_usd_reported_by_cli": cost.get("cost_usd"),
        "wall_ms": cost.get("wall_ms"),
        "vc_commands": agent_commands(run_dir),
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("runs")
    parser.add_argument("output")
    parser.add_argument("--aisc-version", required=True)
    parser.add_argument("--agent", default="claude CLI 2.1.310, claude-sonnet-5-5, effort medium, sandboxed (acceptEdits + allowlist), identical for every arm; arm order rotated per repetition")
    parser.add_argument("--harness", default="GitButler version-control-bench 4886fd5 + benchmarks/vcbench/aisc-arm.patch (held-out tasks in scripts/lib/scenario-heldout*.mjs)")
    parser.add_argument("--note", action="append", default=[])
    arguments = parser.parse_args()
    runs = []
    reps = sorted(d for d in os.listdir(arguments.runs) if d.startswith("rep"))
    for rep in reps:
        for name in sorted(os.listdir(os.path.join(arguments.runs, rep))):
            if os.path.exists(os.path.join(arguments.runs, rep, name, "result.json")):
                runs.append(run_record(arguments.runs, rep, name))
    text = json.dumps({
        "schema": "aisc-agent-runs/1",
        "harness": arguments.harness,
        "agent": arguments.agent,
        "versions": {"aisc": arguments.aisc_version, "but": "0.22.3", "git": "2.50.1 (Apple Git-155)"},
        "repetitions": len(reps),
        "notes": arguments.note + [
            "Token counts and cost are what the Claude CLI reported, cache reads included; no transcripts or prompts beyond the task files are included.",
            "vc_commands lists the version-control commands the agent ran (tool, arguments, exit code; temporary paths replaced by <tmp>); harness setup probes are excluded. No model text is included.",
        ],
        "runs": runs,
    }, indent=1) + "\n"
    if re.search(r"/Users/|/home/", text):
        sys.exit("refusing to write: a machine path remains in the export")
    with open(arguments.output, "w") as handle:
        handle.write(text)
    print(f"{len(runs)} runs -> {arguments.output}")


if __name__ == "__main__":
    main()
