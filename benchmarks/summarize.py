"""Summarize provider counters without treating tokenizer estimates as usage."""

import argparse
import json
import statistics
from pathlib import Path


def counters(run):
    usage = run["projection"]["usage"]
    if len(usage) != 1:
        raise ValueError("Expected one completed fresh-session usage aggregate")
    usage = usage[0]
    if run["provider"] == "codex":
        input_tokens = usage["input_tokens"]
        cache_read = usage.get("cached_input_tokens", 0)
        cache_write = usage.get("cache_write_input_tokens", 0)
        # Codex's cached input is a subset of input, not an additional charge.
        total = input_tokens + usage["output_tokens"]
    else:
        input_tokens = usage["input_tokens"]
        cache_read = usage.get("cache_read_input_tokens", 0)
        cache_write = usage.get("cache_creation_input_tokens", 0)
        total = input_tokens + cache_read + cache_write + usage["output_tokens"]
    events = run["projection"]["events"]
    shell_calls = sum(event.get("type") == "command_execution" or
                      (event.get("type") == "tool_use" and event.get("name") == "Bash")
                      for event in events)
    return {"provider_accounted_tokens": total, "input_tokens": input_tokens,
            "cache_read_tokens": cache_read, "cache_write_tokens": cache_write,
            "output_tokens": usage["output_tokens"], "shell_tool_calls": shell_calls,
            "wall_seconds": run["wall_seconds"]}


def summarize(document):
    groups = {}
    for run in document["runs"]:
        key = (run["task"], run["arm"])
        groups.setdefault(key, []).append(run)
    rows = []
    for (task, arm), runs in sorted(groups.items()):
        values = [counters(run) for run in runs if run["projection"]["usage"]]
        rows.append({"task": task, "arm": arm, "runs": len(runs),
                     "correct": sum(run["correct"] for run in runs),
                     "metrics": {key: {"median": statistics.median(value[key] for value in values),
                                      "min": min(value[key] for value in values),
                                      "max": max(value[key] for value in values)}
                                 for key in values[0]} if values else {}})
    paired = []
    for task in sorted({run["task"] for run in document["runs"]}):
        runs = {(run["repetition"], run["arm"]): run for run in document["runs"] if run["task"] == task}
        deltas = []
        for repetition in sorted({key[0] for key in runs}):
            if (repetition, "git") not in runs or (repetition, "aisc") not in runs:
                continue
            baseline, candidate = runs[repetition, "git"], runs[repetition, "aisc"]
            if not baseline["projection"]["usage"] or not candidate["projection"]["usage"]:
                continue
            left, right = counters(baseline), counters(candidate)
            deltas.append({"repetition": repetition, "both_correct": baseline["correct"] and candidate["correct"],
                           "savings_percent": {key: (left[key] - right[key]) / left[key] * 100
                                               for key in ("provider_accounted_tokens", "wall_seconds", "shell_tool_calls") if left[key]}})
        paired.append({"task": task, "pairs": deltas})
    return {"schema": 1, "provider": document["provider"],
            "negative_savings_mean_regression": True, "rows": rows, "paired": paired}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("output", type=Path)
    arguments = parser.parse_args()
    arguments.output.write_text(json.dumps(summarize(json.loads(arguments.input.read_text())), indent=2) + "\n")
