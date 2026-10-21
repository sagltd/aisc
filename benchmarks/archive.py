"""Publish allowlisted exploratory runs, always excluded from paired claims."""

import getpass
import json
import platform
from pathlib import Path

from events import project


SOURCES = [
    ("codex-initial-inspect", "codex", "smoke/codex.jsonl", "unfrozen initial provider smoke; no reliable latency retained"),
    ("claude-invalid-prompt", "claude", "claude-smoke/inspect-0-git/raw.jsonl", "CLI prompt consumed by variadic option; no model call"),
    ("claude-inspect-smoke", "claude", "claude-smoke2/inspect-0-git/raw.jsonl", "unpaired provider smoke"),
    ("codex-git-readonly-metadata", "codex", "codex-v1/isolated-0-aisc/raw.jsonl", "Git metadata sandbox denied; missing target grader originally crashed; no reliable latency retained"),
    ("codex-minimal-profile", "codex", "codex-scoped-smoke/isolated-0-aisc/raw.jsonl", "240-second timeout; repeated xcrun cache denial and per-tool environment changed"),
    ("codex-exact-gitdir-smoke", "codex", "codex-add-dir-smoke/in_place-0-aisc/raw.jsonl", "unpaired positive sandbox smoke; initial candidate"),
    ("codex-environment-diagnostic", "codex", "env-probe/raw.jsonl", "unpaired diagnostic: CODEX_TOOL_CALL_ID alone changed between verify and finish"),
    ("codex-environment-fixed", "codex", "env-fixed/raw.jsonl", "unpaired confirmation: separate calls now verify and finish successfully"),
]


if __name__ == "__main__":
    base = Path(__file__).resolve().parent
    scratch = base / ".scratch"
    runs = []
    for name, provider, relative, reason in SOURCES:
        path = scratch / relative
        if not path.exists():
            continue
        aliases = {str(scratch): "<scratch>", str(Path.home() / ".config/git/ignore"): "<global-git-ignore>",
                   getpass.getuser(): "<user>", platform.node(): "<host>"}
        runs.append({"name": name, "provider": provider, "comparable": False,
                     "exclusion": reason, "projection": project(provider, path.read_text(), aliases)})
    (base / "results/exploratory.json").write_text(json.dumps({"schema": 1, "runs": runs}, indent=2) + "\n")
