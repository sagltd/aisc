"""Print Markdown tables (one pair per output mode) from a commands-v2 results file."""

import json
import sys
from pathlib import Path


def main(path):
    result = json.loads(Path(path).read_text())
    for mode, totals in result["totals"].items():
        tools = list(totals)
        print(f"#### Mode: {mode} ({result['modes'][mode]})\n")
        print("| Tool | Tool calls (cmds + edits) | Output KB | Est. tokens (read + typed) | Median total ms | All verified |")
        print("|---|---|---|---|---|---|")
        for tool in tools:
            total = totals[tool]
            print(f"| {tool} | {total['tool_calls']} ({total['commands']} + {total['edits']}) | "
                  f"{total['output_bytes'] / 1024:.2f} | {total['output_tokens_est'] + total['authored_tokens_est']:.0f} "
                  f"({total['output_tokens_est']:.0f} + {total['authored_tokens_est']:.0f}) | "
                  f"{total['wall_ms_median_of_totals']:.0f} | {'yes' if total['all_passed'] else 'NO'} |")
        print()
        print("| Task | " + " | ".join(tools) + " |")
        print("|---|" + "---|" * len(tools))
        rows = {(row["scenario"], row["tool"]): row for row in result["rows"][mode]}
        for scenario in result["scenarios"]:
            cells = []
            for tool in tools:
                row = rows.get((scenario["name"], tool))
                if row is None:
                    cells.append("unsupported")
                    continue
                calls = str(row["commands"]) + ("+" + str(row["edits"]) + "e" if row["edits"] else "")
                mark = " †" if row["working_copy_expectation"] == "tool-specific" else ""
                cells.append(f"{calls} / {row['output_bytes']:.0f} B / {row['output_tokens_est'] + row['authored_tokens_est']:.0f} t / "
                             f"{row['wall_ms']['median']:.0f} ms{mark}")
            print(f"| {scenario['name']} | " + " | ".join(cells) + " |")
        print()


if __name__ == "__main__":
    main(sys.argv[1])
