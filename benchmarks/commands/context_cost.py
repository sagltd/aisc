"""Token size of agent guidance files (skills, AGENTS.md blocks): a context-cost proxy.

Usage: python3 context_cost.py LABEL=PATH [LABEL=PATH ...]
Prints JSON rows {label, bytes, tokens}. Tokens use tiktoken o200k_base when it is
installed (offline), else bytes/4, and the output names which one was used.
"""

import json
import sys
from pathlib import Path


def counter():
    try:
        import tiktoken
        encoding = tiktoken.get_encoding("o200k_base")
        return "tiktoken/o200k_base " + tiktoken.__version__, lambda text: len(encoding.encode(text))
    except Exception:
        return "estimate/bytes_div_4", lambda text: (len(text.encode()) + 3) // 4


def main(pairs):
    label, count = counter()
    rows = []
    for pair in pairs:
        name, _, path = pair.partition("=")
        text = Path(path).read_text()
        rows.append({"label": name, "bytes": len(text.encode()), "tokens": count(text)})
    print(json.dumps({"tokenizer": label, "rows": rows}, indent=1))


if __name__ == "__main__":
    main(sys.argv[1:])
