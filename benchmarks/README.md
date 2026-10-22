# Benchmarks

## Command benchmark (commands-v2)

`commands/` contains a scripted benchmark with no model in the loop. For ten
version-control tasks it measures how many tool calls an expert agent needs,
how many bytes the tools print, a token estimate for that text, and wall time.
It compares Git, AISC, GitButler (`but`) and Jujutsu (`jj`). Results are in
[`results/commands-v2.json`](results/commands-v2.json). The single-task v1
comparison (`commands.py`, `results/commands-v1.json`) is kept unchanged.

### Tasks

The tasks follow the task families of GitButler's version-control-bench
(vcbench). Every tool starts from the same synthetic history, built with
fixed dates so the commit hashes are identical. Each sequence is graded on
Git-visible state: subject and full tree of every commit on `feature` (and on
`docs` where the task creates it), the base commit, an unchanged `main`, the
exact working-copy files that differ from `feature`, no rebase or merge left
in progress, and no conflict markers. The test suite checks that a no-op fails
every task and that the Git sequence passes every task.

| Task | vcbench family | What must happen |
|---|---|---|
| select_hunks | selective commit (pilot-1) | Commit one hunk of `app.py` plus a test file. Another hunk, a README edit and an untracked file stay uncommitted. |
| amend_older | multi-amend (pilot-2) | Fold an uncommitted fix into the older of two commits. |
| split_commit | split commit (pilot-3) | Split a lower commit into two commits, in order, below the top commit. |
| reorder_commits | reorder (pilot-4) | Move the oldest of three commits to the top. |
| squash_commits | squash (pilot-5) | Squash two commits into one with a new message. |
| uncommit_file | recover amend (pilot-10) | Remove a file from an older commit and keep it on disk, uncommitted. |
| undo_rewrite | operation undo | Undo the tool's own squash (done untimed beforehand), keeping a dirty README. |
| sync_conflict | update dirty branch (pilot-6/11) | Fetch the moved `origin/main`, rebase onto it, resolve a one-line conflict, keep a dirty README. |
| parallel_branch | parallel branch / stack | Commit one dirty file on a new branch `docs` based on `main`, leaving `feature` and its WIP alone. |
| inspect_state | status/log/diff | Report the branch, its commits and the changed paths with counts. Nothing changes. |

The sequences in [`commands/scenarios.py`](commands/scenarios.py) are the
shortest correct non-interactive ones we found for each tool:

- Git uses `GIT_SEQUENCE_EDITOR` one-liners for `rebase -i`, `--fixup` with
  `--autosquash`, `add -p` answered on stdin, and `reset --keep feature@{1}` for
  undo. Git has no operation log, so undo uses the branch reflog.
- GitButler needs the short IDs from `but status`, `but diff` or `--status-after`
  output before most writes. The runner parses them from the human-readable
  output, as an agent would. GitButler has no split verb, so splitting takes
  uncommit, reword and commit.
- Jujutsu has no non-interactive hunk selection. For select_hunks the agent
  reads the file, writes the partial version, commits, restores the file and
  moves the bookmark. Bookmarks do not follow a commit rebased past them, so
  reorder and select_hunks each need a `bookmark move`.
- AISC uses its history verbs with message and `path~text` selectors.
  parallel_branch takes `switch main`, then `commit -b docs`, then
  `switch feature`.

There are deliberate model differences. Both GitButler branches stay applied,
so in parallel_branch the guide edit stays on disk. In Jujutsu, new files
become part of the working-copy commit. Adoption is untimed, as vcbench does it
before an agent starts: `but setup`/`apply`, with metrics disabled and the
skill installed so no install notice prints, or `jj git init --colocate`.

### Metrics

- **Tool calls**: shell commands plus file edits. An edit is an agent writing
  a file, such as a conflict resolution.
- **Output bytes**: stdout plus stderr of every command, which is the text an
  agent reads back.
- **Token estimate**: `tiktoken` `o200k_base` (0.14.0, offline). It counts the
  output text plus the authored text (`$ command line`, stdin, edit contents).
  Temporary run paths are replaced with `<run>`. This is an offline proxy, not
  provider-billed usage.
- **Wall time**: the sum of per-command `perf_counter` time around each
  subprocess. Fixture copy, adoption and grading are excluded. The figure is
  the median of 10 measured repetitions after 1 warmup. Tools rotate through
  every position across repetitions and tasks. `but` starts a background fetch
  after commands, so the runner waits 1 s, untimed, after each GitButler run.
  The fetch then cannot race the next command or tool.

### Results

The run used macOS arm64 with 11 CPUs, Apple Git 2.50.1, AISC 0.1.0 (dev
`0843c6a`), `but` 0.22.3 and `jj` 0.42.0. The machine was heavily shared, with
a load average of 20 to 37. Absolute times are noisy. The ratios between tools
matched the earlier smoke runs. Each tool passed all 10 tasks in all 11 runs.

| Tool | Tool calls (cmds + edits) | Output KB | Est. tokens (read + typed) | Median total ms |
|---|---|---|---|---|
| git | 27 (26 + 1) | 1.94 | 1061 (619 + 442) | 1595 |
| aisc | 14 (13 + 1) | 2.39 | 964 (784 + 180) | 12538 |
| but | 23 (22 + 1) | 5.63 | 1962 (1752 + 210) | 2097 |
| jj | 21 (18 + 3) | 3.64 | 1594 (1192 + 403) | 1821 |

Each cell gives calls (`e` = edits) / output bytes / estimated tokens /
median ms:

| Task | git | aisc | but | jj |
|---|---|---|---|---|
| select_hunks | 3 / 837 B / 321 t / 127 ms | 1 / 216 B / 85 t / 884 ms | 2 / 1801 B / 481 t / 142 ms | 3+2e / 494 B / 310 t / 224 ms |
| amend_older | 2 / 46 B / 56 t / 153 ms | 1 / 156 B / 68 t / 991 ms | 2 / 268 B / 118 t / 121 ms | 1 / 156 B / 67 t / 107 ms |
| split_commit | 5 / 166 B / 126 t / 197 ms | 1 / 153 B / 81 t / 1064 ms | 4 / 898 B / 375 t / 411 ms | 2 / 428 B / 182 t / 172 ms |
| reorder_commits | 1 / 0 B / 47 t / 139 ms | 1 / 202 B / 80 t / 1008 ms | 2 / 280 B / 116 t / 164 ms | 2 / 251 B / 116 t / 133 ms |
| squash_commits | 1 / 0 B / 59 t / 182 ms | 1 / 174 B / 81 t / 1011 ms | 2 / 278 B / 123 t / 173 ms | 1 / 164 B / 82 t / 85 ms |
| uncommit_file | 4 / 163 B / 111 t / 201 ms | 1 / 172 B / 69 t / 1351 ms | 2 / 341 B / 145 t / 227 ms | 1 / 128 B / 62 t / 89 ms |
| undo_rewrite | 1 / 0 B / 13 t / 23 ms | 1 / 177 B / 65 t / 499 ms | 1 / 53 B / 30 t / 82 ms | 1 / 320 B / 130 t / 52 ms |
| sync_conflict | 3+1e / 666 B / 224 t / 288 ms | 2+1e / 521 B / 177 t / 2498 ms | 3+1e / 1294 B / 327 t / 410 ms | 3+1e / 1077 B / 338 t / 507 ms |
| parallel_branch | 3 / 0 B / 34 t / 115 ms | 3 / 428 B / 172 t / 2550 ms | 2 / 276 B / 121 t / 162 ms | 2 / 357 B / 147 t / 187 ms |
| inspect_state | 3 / 113 B / 70 t / 109 ms | 1 / 246 B / 86 t / 161 ms | 2 / 274 B / 126 t / 57 ms | 2 / 357 B / 160 t / 119 ms |

### Where AISC loses

- **Wall time, in every task.** AISC was the slowest tool in all ten tasks.
  Its mutating verbs were 6 to 22 times slower than Git, and `aisc log` was
  1.5 times slower than Git's three inspection commands. The profile in the results file is the
  evidence. A mutating verb spawns 36 to 74 `git` processes, counted with a
  PATH shim. In the `GIT_TRACE2_EVENT` timings, those processes run for only
  30 to 110 ms of a 0.4 to 1.4 s step. The other roughly 90% goes to starting
  processes. That is about 13 ms per spawn, and about 5.5 ms of it is the macOS
  `/usr/bin/git` xcrun shim: `git --version` took 12.7 ms through the shim and
  7.2 ms on the resolved binary. AISC's own start is cheap: `aisc --version`
  took 4 ms and `aisc context` 42 ms.
- **Output bytes.** Plain Git prints nothing for quiet rewrites, so Git reads
  less (1.94 KB against 2.39 KB). AISC still has the lowest total token
  estimate, because its commands are much shorter to type. Every AISC mutation
  prints a branch summary after the result, which accounts for most of its
  output.
- **parallel_branch.** This task needs 3 calls, because `commit -b` branches
  from HEAD and the agent must switch to `main` and back. GitButler and
  Jujutsu need 2.

### Optimization opportunities

1. Resolve the real `git` binary once per process. On macOS, use
   `git --exec-path` or `xcrun --find git` instead of `/usr/bin/git`. That
   saves about 5 ms per spawn, which is about 300 ms on a 60-call verb.
2. Batch repeated reads. The profile shows about 11 `rev-parse`, 6 `var`,
   5 `config` and 5 `symbolic-ref` calls per mutation. One `rev-parse` with
   several arguments, a single ident lookup and one config read would remove
   about 20 spawns per verb. Streaming `hash-object`/`ls-tree`/`cat-file`
   through one `cat-file --batch` or `update-index --stdin` process would
   remove more.
3. Build the post-operation branch summary from data already in memory.
   Running `aisc log` alone takes 9 Git calls and about 70 ms. Making the
   summary optional would also cut output bytes. A `commit -b NEW --onto BASE`
   form would make parallel_branch a single call.

### Reproduce

```sh
CARGO_TARGET_DIR=$PWD/target cargo build --release
python3 benchmarks/commands/run.py --aisc target/release/aisc \
  --but /path/to/but --jj /path/to/jj \
  --output benchmarks/results/commands-v2.json   # 10 repetitions + 1 warmup
python3 benchmarks/commands/report.py benchmarks/results/commands-v2.json
python3 benchmarks/commands/test_scenarios.py    # graders reject no-ops; Git passes
```

Any missing tool is skipped and listed under `skipped`. Without `tiktoken`,
tokens fall back to bytes/4, and the results label them as an estimate. Temp
repositories live in the ignored `benchmarks/.scratch/` and are deleted after
each run.
