# Reproducible benchmark protocol

Protocol fixed before candidate comparisons, 2026-10-22. AISC is compared with straightforward optimized Git, not verbose transcripts. Generated fixtures contain public synthetic text only; no developer repository, agent history or credentials are inputs.

## Three distinct measurements

1. **Offline command transcript proxy:** tokenize the exact `$ command\nstdoutstderr` concatenation with `tiktoken==0.12.0`, encoding `o200k_base`. Include command text; do not claim it is the proprietary provider tokenizer, model reasoning or billed usage. Tool-call envelopes and prompt overhead are excluded from this proxy and therefore cannot support whole-agent cost claims.
2. **Command runtime:** monotonic wall time from first subprocess launch to last exit, excluding fixture construction, tokenization and grading. Warm filesystem; report Git version, repetitions, median and full range. This is distinct from agent completion latency.
3. **Fresh-agent usage:** Codex JSONL `turn.completed.usage` and Claude JSON `usage`/`modelUsage`, when available. Include the complete task, tool instructions and any skill overhead in each session. Preserve input, cached input, output and cache creation separately. Never add cached tokens twice or infer dollars from token counts. CLI-reported dollars, when present, remain a separate reported field.

## Outcome-equivalent tasks

| Task | Required result, both arms | Grading |
| --- | --- | --- |
| Inspect mixed repository | Describe branch and staged/unstaged/untracked/rename/deletion paths without changing any content or refs. | Compare HEAD, index stage entries, worktree bytes and reported path names; acknowledge bounded truncation if applicable. |
| Targeted save | Commit only `src/math.txt` as `save math`; leave unrelated staged rename and staged+unstaged `notes.txt`, deletion and untracked file intact. | Exact commit paths/content/message; compare unrelated index entries and all worktree bytes. |
| Isolated task | From dirty main, create `task/bench` at `../isolated`, change math to checked addition there, checkpoint as `save math`, run the supplied content assertion and finish by reporting commit/ref. Keep original main, its index and worktree unchanged. | Original snapshot exact; isolated branch/HEAD and committed math content exact, clean isolated state, verification command succeeded and final commit reported. AISC finish additionally enforces its evidence contract. |

For inspect, Git may batch `git status --porcelain=v2 --branch`, `git diff --stat`, `git diff --cached --stat` in one shell call. Full diff is optional and targeted when content is needed. For save, `git commit --only -m 'save math' -- src/math.txt` is a valid concise baseline preserving unrelated staging. For isolation, `git worktree add -b task/bench ../isolated main`, edit, targeted commit, supplied assertion and commit/ref report are sufficient. Count agent tool calls and external commands separately; shell batching is allowed for both arms. Internal Git subprocesses are not agent tool calls.

AISC contract: `start NAME --path PATH`; `context [--limit N]`; `checkpoint -m MESSAGE -- PATH...`; `verify -- PROGRAM ARG...`; `finish`. Candidate timing waits for an actual built implementation. Do not compare simulated AISC output to real Git output.

## Execution and reporting

Use fresh repository and fresh provider session per arm, fixed CLI model and effort within a provider. Start from the installed CLI default model/effort, record it, and hold it constant; no model routing. Randomize arm order with a recorded seed. Aim for at least three paired repetitions per task/provider; report all failures, refusals, invalid runs and negative deltas. Report per-task median, range and paired deltas; do not advertise the most favorable task as an overall result. Three pairs are exploratory, not a precise population estimate.

The fixture is small by design. It measures source-control overhead only; it does not estimate full software-development savings, large-repository performance, remote transport, conflict resolution or billing. Model latency, caching, shared quota and machine load can dominate. The existing installed CLI configuration is not itself a public artifact; publish only recorded non-secret model/effort/version settings.

Provider events must be generated in synthetic fresh cwd with private project/user instructions disabled. Store raw envelopes only in ignored `benchmarks/.scratch/`. Export an allowlist of public prompts, commands, outputs, final answers, usage and timing, replacing fixture root/binary paths with `<fixture>`/`<aisc>` and omitting account/session/thread identifiers. Refuse export if machine-specific paths or credential-shaped values remain. Record sanitizer rules and artifact hashes; do not claim sanitized event files are untouched provider envelopes.

## Reproduce offline baseline

```sh
python3 -m venv /tmp/aisc-bench-venv
/tmp/aisc-bench-venv/bin/pip install -r benchmarks/requirements.txt
python3 -m unittest discover -s benchmarks -v
/tmp/aisc-bench-venv/bin/python benchmarks/baseline.py --output benchmarks/results/git-baseline.json
```

The tokenizer may download its public vocabulary on first run. Five baseline runs passed state grading, with 409 text-proxy tokens each; see [the raw JSON](../benchmarks/results/git-baseline.json). No provider usage or AISC savings is established by that baseline.
