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
| Isolated task | From dirty main, create `aisc/bench` at `../isolated`, change math to checked addition there, checkpoint as `save math`, run the supplied content assertion and finish by reporting commit/ref. Keep original main, its index and worktree unchanged. | Original snapshot exact; isolated branch/HEAD and committed math content exact, clean isolated state, verification command requested and final commit reported. AISC finish additionally enforces its evidence contract. |
| Same-checkout task | From clean main, stay on main in the original directory, begin task, edit math, checkpoint as `save math`, assert exact content and finish by reporting commit/ref. | Correct committed content/message, clean main, assertion requested and commit reported. No worktree required. |

For inspect, Git may batch `git status --porcelain=v2 --branch`, `git diff --stat`, `git diff --cached --stat` in one shell call. Full diff is optional and targeted when content is needed. For save, `git commit --only -m 'save math' -- src/math.txt` is a valid concise baseline preserving unrelated staging. For isolation, `git worktree add -b aisc/bench ../isolated main`, edit, targeted commit, supplied assertion and commit/ref report are sufficient. Count agent tool calls and external commands separately; shell batching is allowed for both arms. Internal Git subprocesses are not agent tool calls.

AISC contract: `start NAME --path PATH` creates `aisc/NAME`; `start NAME --in-place` starts in the clean current checkout; `context [--limit N]`; `checkpoint -m MESSAGE -- PATH...`; `verify -- PROGRAM ARG...`; `finish`. Candidate timing waits for an actual built implementation. Do not compare simulated AISC output to real Git output.

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

Fresh provider runs (billable/subscription quota may apply) use installed default models observed once in smoke and then pinned: Codex `gpt-6.1-sol`, Claude `claude-opus-5-5`, effort `medium` for each. Codex disables user configuration and project documents with `--ignore-user-config -c project_doc_max_bytes=0`; Claude uses `--safe-mode --setting-sources '' --strict-mcp-config`. Both run synthetic cwd, ephemeral/no-persistence sessions, and prompts prohibit network, outside files, skills and delegation. Native provider system/tool overhead is included and may dwarf command output.

Codex uses `--sandbox workspace-write` with write access added only to the synthetic fixture root and its exact `.git` directory. This supports Git writes while retaining restricted command network access. An earlier broad-sandbox proposal was rejected by automatic approval review and was never executed. A narrower minimal-runtime profile was also tested, but repeated denied xcrun cache writes caused a timeout; it is not part of comparative timing. The separate-tool-call smoke exposed `CODEX_TOOL_CALL_ID` changing across otherwise equivalent environments; AISC now excludes only that orchestration key, and a real separate-call confirmation passes.

```sh
python3 benchmarks/agents.py --provider codex --aisc /path/to/aisc --name codex-v1 --output benchmarks/results/codex-v1.json
python3 benchmarks/agents.py --provider claude --aisc /path/to/aisc --name claude-v1 --output benchmarks/results/claude-v1.json
python3 benchmarks/summarize.py benchmarks/results/codex-v1.json benchmarks/results/codex-summary.json
```

Each CLI gets 24 fresh sessions: four tasks × two arms × three repetitions, shuffled with seed 104729. The script retains raw envelopes locally and exports sanitized allowlisted fields. It never publishes private session envelopes. The summary counts Codex `cached_input_tokens` as a subset of `input_tokens`; Claude input/cache-read/cache-creation counters are separate and additive. Nested Claude `iterations` are retained but never summed into the aggregate again. `provider_accounted_tokens` combines the provider aggregate categories and output for comparison, with their individual fields always available; this is not a universal billing formula. `shell_tool_calls` counts command-execution/Bash invocations; multiple external commands may be batched inside one call and full command strings are retained. External command counts are therefore separate from shell-tool-call counts.

## Exploratory limitations and competitor cohort

[Exploratory projections](../benchmarks/results/exploratory.json) retain failed setup, timeout, initial provider checks and environment-diagnostic runs. They are explicitly incomparable and excluded from paired claims. The first lifecycle cohort's command cards list recipes for all tasks. An observed Git inspection run executed an unnecessary verification command from that card and duplicated stat/numstat reads; its result remains public, but such behavior cannot establish an optimized-baseline advantage.

`competitor.py` therefore uses separate minimal cards per task and requires efficient batching. Its three arms are optimized Git, AISC and official GitButler 0.22.3. Each starts on an existing task branch `bench` with the same two unstaged source edits and no unrelated staging requirement. One task inspects exact path/line counts; the other commits only math while preserving the docs edit uncommitted. Deterministic checks grade branch commit, exact content/message/paths and unrelated worktree bytes. GitButler workspace setup and public skill installation for notice detection occur before timing, using its documented source override `E2E_TEST_APP_DATA_DIR` to keep app data and apparent home in ignored scratch. Equivalent concise command cards, rather than full skills, are the model instructions. Both tools' setup costs are excluded, and different workspace topology is disclosed; this cohort does not test preservation of an existing Git index.

The official GitButler app asset is pinned by [Homebrew metadata](https://formulae.brew.sh/api/cask/gitbutler.json), version 0.22.3 build 3234, SHA256 `edf0f680c102ff86faee1d919ea2a6166fcbe6e3a98986706c91a38eed662f74`. The extracted `but` binary is SHA256 `b79af0f95d3bce2274d03f9c91d3868d17a91c9fcfe6f4224e3aea117155c550`; the `but` symlink must retain its name because the app uses argv[0] to select CLI mode. [Tag-matched app-data override source](https://github.com/gitbutlerapp/gitbutler/blob/release/0.22.3/crates/but-path/src/lib.rs).

```sh
python3 benchmarks/competitor.py --provider codex --aisc /path/to/aisc --but /path/to/but --name three-way-v1 --output benchmarks/results/three-way-v1.json
```

That cohort has 18 sessions/provider (two tasks × three tools × three repetitions), shuffled with seed 130363. `commands.py` separately runs actual prepared commands using canonical executable names before text tokenization; foreground command time excludes setup and grading. It is a text proxy and local latency measurement, not provider usage. GitButler can complete asynchronous local maintenance after CLI exit; scratch is retained to avoid racing that maintenance during cleanup. No savings relative to GitButler is established merely by defining this protocol.
