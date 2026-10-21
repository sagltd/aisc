"""Paired actual command timings and canonical text proxies, never model usage."""

import argparse
import hashlib
import importlib.metadata
import json
import random
import statistics
import tempfile
from pathlib import Path

from baseline import measure, text_for
from competitor import prepare
from fixture import environment, snapshot


EXPECTED = {"branch": "bench", "changes": {"docs/guide.txt": (1, 1), "src/math.txt": (1, 1)}}


def inspection_facts(arm, transcript):
    if arm == "aisc":
        value = json.loads(transcript[0]["stdout"])
        return {"branch": value["branch"], "changes": {
            item["path"]: (int(item["added"]), int(item["deleted"]))
            for item in value["diffstat"]["unstaged"]["files"]}}
    status = transcript[0]["stdout"]
    branch = "bench" if ("# branch.head bench\n" in status if arm == "git" else "[bench]" in status) else None
    changes = {}
    for line in transcript[-1]["stdout"].splitlines():
        added, deleted, path = line.split("\t", 2)
        changes[path] = (int(added), int(deleted))
    return {"branch": branch, "changes": changes}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--aisc", type=Path, required=True)
    parser.add_argument("--but", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repetitions", type=int, default=5)
    arguments = parser.parse_args()
    arguments.aisc = arguments.aisc.resolve(strict=True)
    arguments.but = arguments.but.absolute()
    import tiktoken
    encoding = tiktoken.get_encoding("o200k_base")
    scratch = Path(__file__).resolve().parent / ".scratch"
    schedule = [(repetition, arm) for repetition in range(arguments.repetitions) for arm in ("git", "aisc", "but")]
    random.Random(130363).shuffle(schedule)
    records = []
    for repetition, arm in schedule:
        temporary = tempfile.mkdtemp(dir=scratch)
        root = Path(temporary)
        repository = prepare(root, "inspect_compact", arm, arguments.but)
        before = snapshot(repository)
        commands = {
            "git": [["git", "status", "--porcelain=v2", "--branch"], ["git", "diff", "--numstat"]],
            "aisc": [[str(arguments.aisc), "context"]],
            "but": [[str(arguments.but), "status", "--no-hint"], ["git", "diff", "--numstat"]],
        }[arm]
        values = environment()
        values.update(E2E_TEST_APP_DATA_DIR=str(root / "appdata"), BUT_PAGER="cat", NO_COLOR="1")
        transcript, elapsed = measure(repository, commands, values)
        for entry in transcript:
            if entry["command"][0] == str(arguments.aisc):
                entry["command"][0] = "aisc"
            if entry["command"][0] == str(arguments.but):
                entry["command"][0] = "but"
        text = text_for(transcript)
        facts = inspection_facts(arm, transcript)
        records.append({"repetition": repetition, "arm": arm, "commands": transcript,
                        "wall_ms": elapsed, "text_tokens": len(encoding.encode(text)),
                        "text_bytes": len(text.encode()), "state_preserved": before == snapshot(repository),
                        "inspection_facts": facts, "semantic_grade": facts == EXPECTED})
    rows = []
    for arm in ("git", "aisc", "but"):
        runs = [record for record in records if record["arm"] == arm]
        rows.append({"arm": arm, "state_preserved_runs": sum(record["state_preserved"] for record in runs),
                     "semantic_passes": sum(record["semantic_grade"] for record in runs),
                     "metrics": {key: {"median": statistics.median(record[key] for record in runs),
                                      "min": min(record[key] for record in runs), "max": max(record[key] for record in runs)}
                                 for key in ("wall_ms", "text_tokens", "text_bytes")}})
    result = {"schema": 1, "task": "inspect_compact", "metric_kind": "command_runtime_and_text_proxy_not_provider_usage",
              "tokenizer": "tiktoken/o200k_base", "tokenizer_version": importlib.metadata.version("tiktoken"),
              "seed": 130363, "candidate_sha256": hashlib.sha256(arguments.aisc.read_bytes()).hexdigest(),
              "timing": "fresh prepared fixture per arm; setup/grading excluded; warm filesystem; no inference",
              "token_text": "canonical executable names git/aisc/but, command + stdout + stderr; no provider envelopes",
              "rows": rows, "runs": records}
    arguments.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(rows))
