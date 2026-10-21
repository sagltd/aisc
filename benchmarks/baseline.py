"""Measure optimized Git commands; token counts are text proxies, not usage."""

import argparse
import importlib.metadata
import json
import platform
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

from fixture import build, environment, git, snapshot


def measure(repository, commands, values=None):
    transcript = []
    started = time.perf_counter()
    for command in commands:
        before = time.perf_counter()
        result = subprocess.run(command, cwd=repository, env=values or environment(),
                                capture_output=True, text=True, check=False)
        transcript.append({"command": command, "stdout": result.stdout,
                           "stderr": result.stderr, "exit_code": result.returncode,
                           "wall_ms": (time.perf_counter() - before) * 1000})
        if result.returncode:
            raise RuntimeError("Benchmark command failed: " + repr(command))
    return transcript, (time.perf_counter() - started) * 1000


def text_for(records):
    return "".join("$ " + " ".join(record["command"]) + "\n" + record["stdout"]
                   + record["stderr"] for record in records)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repetitions", type=int, default=5)
    arguments = parser.parse_args()
    if arguments.repetitions < 3:
        parser.error("at least three repetitions required")
    import tiktoken
    encoding = tiktoken.get_encoding("o200k_base")
    records = []
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    scratch = Path(__file__).parent / ".scratch"
    scratch.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(dir=scratch) as temporary:
        repository = build(Path(temporary) / "repo")
        before = snapshot(repository)
        for repetition in range(arguments.repetitions):
            commands = [["git", "status", "--porcelain=v2", "--branch"],
                        ["git", "diff", "--stat"],
                        ["git", "diff", "--cached", "--stat"]]
            transcript, elapsed = measure(repository, commands)
            text = text_for(transcript)
            records.append({"repetition": repetition, "commands": transcript,
                            "wall_ms": elapsed, "text_tokens": len(encoding.encode(text)),
                            "text_bytes": len(text.encode()), "correct": snapshot(repository) == before})
        assert all(record["correct"] for record in records)
        result = {"schema": 1, "task": "inspect_mixed", "tool": "optimized_git",
                  "metric_kind": "offline_transcript_proxy_not_provider_usage",
                  "tokenizer": "tiktoken/o200k_base",
                  "tokenizer_version": importlib.metadata.version("tiktoken"),
                  "git_version": git(repository, "--version").strip(),
                  "platform": platform.system() + " " + platform.machine(),
                  "timing": "commands only; fixture setup and grading excluded; warm filesystem",
                  "median_wall_ms": statistics.median(record["wall_ms"] for record in records),
                  "runs": records}
        arguments.output.write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps({key: value for key, value in result.items() if key != "runs"}))


if __name__ == "__main__":
    main()
