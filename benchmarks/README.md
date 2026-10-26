# Benchmarks

## Command benchmark (commands-v2 schema)

`commands/` contains a scripted benchmark with no model in the loop. For thirteen
version-control tasks it measures how many tool calls an expert agent needs,
how many bytes the tools print, a token estimate for that text, and wall time.
It compares Git, AISC, GitButler (`but`) and Jujutsu (`jj`). Results are in
[`results/commands-v4.json`](results/commands-v4.json) (dev build 7011e25, thirteen tasks; the ten-task runs
are [`results/commands-v3.json`](results/commands-v3.json), v0.1.0 release binary, and [`results/commands-v2.json`](results/commands-v2.json)). The single-task v1
comparison (`commands.py`, `results/commands-v1.json`) is kept unchanged.

### Tasks and grading

The tasks follow the task families of GitButler's version-control-bench
(vcbench). Every tool starts from the same synthetic history, built with fixed
dates so the commit hashes are identical.

The grader checks Git-visible state only:

- the subject and full tree of every commit on `feature`, and on `docs` where
  the task creates it;
- the base commit, and that `main` is unchanged;
- the exact working-copy files that differ from `feature`;
- that no rebase or merge is left in progress and no conflict markers remain.

The test suite checks that a no-op fails every task and that the Git sequence
passes every task in both output modes. One exception, marked † below: in
parallel_branch, `but` is graded against a different working copy. Both
GitButler branches stay applied in its workspace, so the guide edit
legitimately stays on disk. Every other task/tool pair is graded against the
same expected end state. The results JSON lists the exception under
`scenarios[].graded_against_tool_specific_working_copy` and as
`working_copy_expectation: "tool-specific"` in each row.

| Task | vcbench family | What must happen |
|---|---|---|
| select_hunks | selective commit (pilot-1) | Commit the `ValueError` hunk of `app.py` plus a test file. Another hunk, a README edit and an untracked file stay uncommitted. |
| amend_older | multi-amend (pilot-2) | Fold an uncommitted fix into the older of two commits. |
| split_commit | split commit (pilot-3) | Split a lower commit into two commits, in order, below the top commit. |
| reorder_commits | reorder (pilot-4) | Move the oldest of three commits to the top. |
| squash_commits | squash (pilot-5) | Squash two commits into one with a new message. |
| uncommit_file | recover amend (pilot-10) | Remove a file from an older commit and keep it on disk, uncommitted. |
| undo_rewrite | operation undo | Undo the tool's own squash (done untimed beforehand), keeping a dirty README. |
| sync_conflict | update dirty branch (pilot-6/11) | Fetch the moved `origin/main`, rebase onto it, resolve a one-line conflict, keep a dirty README. |
| parallel_branch | parallel branch / stack | Commit one dirty file on a new branch `docs` based on `main`, leaving `feature` and its WIP alone. |
| inspect_state | status/log/diff | Report the branch, its commits and the changed paths with counts. Nothing changes. |
| reword_commit | reword (held-out 1) | Change the message of the middle of three commits; keep a dirty README. |
| drop_commit | drop (held-out 2) | Remove the middle commit and its change; keep a dirty README and an untracked file. |
| move_to_branch | move to another branch (held-out 4) | Move the middle commit on top of the existing `billing` branch and take it off `feature`; stay on `feature`, keep a dirty README. |

### Inspection rule

The agent knows only the task text: branch names, commit subjects, paths, and
a phrase from the hunk it must select. Each sequence in
[`commands/scenarios.py`](commands/scenarios.py) runs exactly the reads its
tool needs to turn that knowledge into arguments the tool accepts. It runs no
extra checks.

- **git**: reads the diff to learn hunk order for `add -p`. For undo it reads
  `git reflog -n 5 feature` and resets to the entry before the rebase, because
  Git has no operation log. It addresses commits by subject, through
  `GIT_SEQUENCE_EDITOR` one-liners or `--fixup ':/subject'`, so it needs no
  hash lookup.
- **aisc**: needs no reads. Commits are addressed by subject and hunks by
  `path~text`.
- **but**: reads `but status`, `but diff` or `--status-after` output for the
  short IDs of commits, hunks and committed files. The runner parses them from
  the human-readable output, as an agent would. Paths and branch names are
  accepted directly and need no lookup. For example, parallel_branch is a single
  `but commit -b docs ... docs/guide.md`. In sync_conflict, the conflicted
  commit's ID comes from the `but pull` summary. `but undo` needs nothing.
- **jj**: addresses commits by `subject()` revsets. To commit part of a file
  it must read the file, because it has no non-interactive hunk selection.
  `jj undo` needs nothing.

For the three held-out tasks: Git edits the rebase todo (`exec git commit --amend -m` after
the pick to reword, a deleted line to drop) and cherry-picks by `:/subject` before dropping to
move; AISC runs `reword`, `drop` or `move --to` by subject; GitButler reads `but status` for
the commit id, then `reword`, `discard` or `move --branch`; Jujutsu runs `describe`, `abandon`,
or `rebase -d` plus a bookmark move.

### Where AISC's call savings come from

AISC's `path~text` hunk selector is a real feature. In select_hunks it picks
the hunk by its text in one call. The closest real equivalents for the other
tools are:

- Git reads the diff, then answers `git add -p` on stdin.
- GitButler reads `but diff` for the hunk ID, then commits that ID together
  with the test file's path.
- Jujutsu reads the file, writes the partial version, runs `jj commit` with a
  fileset, restores the file and moves the bookmark.

Across the thirteen tasks, AISC saves 19 tool calls against Git, 13 against
GitButler and 10 against Jujutsu. The selector accounts for 2 of the 19
against Git, 1 of the 13 against GitButler, and 4 of the 10 against Jujutsu
(3 commands + 2 edits versus 1). Against Jujutsu, then, the hunk selector is
where most of the savings come from. Against Git and GitButler, most savings
come from addressing commits by subject in one verb:

- Git needs multi-step `rebase -i` sequences for split and uncommit, and a
  reflog lookup for undo.
- GitButler needs an ID lookup before each history edit, and has no split verb.

### Output modes

- **default** uses no quiet flags anywhere.
- **quietest** adds each tool's documented quiet flags:
  - git: `-q` where a command has it;
  - aisc: `-q` (Print only the result line) on commit, amend, split, uncommit,
    squash, move, sync and undo. Earlier runs (the tables below) left `undo`
    without it because it had no quiet flag then; `undo -q` now exists, so the
    quietest arm uses it like every other documented quiet flag. `log` is an
    inspection command and stays the same in both modes.
  - jj: `--quiet`;
  - but: `--no-hint` on status. Its write commands have no quiet flag.

The inspection commands are the same in both modes.

### Metrics

- **Tool calls**: shell commands plus file edits. An edit is an agent writing
  a file, such as a conflict resolution.
- **Output bytes**: stdout plus stderr, which is what the agent reads back.
- **Token estimate**: `tiktoken` `o200k_base` (0.14.0, offline). It counts the
  output plus the authored text (`$ command line`, stdin, edit contents).
  Temporary paths are replaced with `<run>`. This is an offline proxy, never
  provider-billed usage.
- **Wall time**: the sum of per-command `perf_counter` time. Fixture copy,
  adoption and grading are excluded. Adoption is untimed, as in vcbench:
  `but setup`/`apply` with metrics off and the skill installed, or
  `jj git init --colocate`. The figure is the median of **6** measured
  repetitions after 1 warmup, per mode. The (mode, tool) pairs rotate through
  every slot. `but` forks a background fetch, so the runner waits 1 s, untimed,
  after each GitButler run.
- **AISC build**: `run.py` builds AISC itself with
  `cargo build --release --locked`. It records the profile in `aisc_build`:
  release, `lto`, `codegen-units = 1`, `strip`, and no debug-assertions
  override. It also records the Git commit, whether `src/` is clean, the rustc
  version and the binary's SHA-256. A binary passed with `--aisc` must sit in a
  `release` directory.

### Results

The v4 run used macOS arm64 with 11 CPUs, Apple Git 2.50.1, a release build of
AISC `dev` at 7011e25 (v0.1.2 plus `show COMMIT:PATH` and the cross-branch
amend), `but` 0.22.3 and `jj` 0.42.0: 6 measured repetitions after 1 warm-up,
both modes. Each tool passed all 13 tasks in every run. The machine was shared;
its one-minute load average fell from 35 to 9 during the run, so absolute
times are indicative. Tokens are `tiktoken` `o200k_base` counts of the
transcript text, not billed usage.

Each per-task cell gives calls (`e` = edits) / output bytes / estimated tokens
/ median ms. † marks the tool-specific working-copy expectation.

#### Default mode (no quiet flags)

| Tool | Tool calls (cmds + edits) | Output KB | Est. tokens (read + typed) | Median total ms | All verified |
|---|---|---|---|---|---|
| git | 34 (33 + 1) | 4.41 | 2035 (1482 + 553) | 1053 | yes |
| aisc | 15 (14 + 1) | 2.27 | 976 (766 + 210) | 838 | yes |
| but | 28 (27 + 1) | 7.07 | 2450 (2226 + 224) | 1580 | yes |
| jj | 25 (22 + 3) | 4.39 | 1900 (1430 + 469) | 1467 | yes |

| Task | git | aisc | but | jj |
|---|---|---|---|---|
| select_hunks | 3 / 904 B / 340 t / 67 ms | 1 / 170 B / 73 t / 78 ms | 2 / 1801 B / 484 t / 93 ms | 3+2e / 494 B / 310 t / 158 ms |
| amend_older | 2 / 212 B / 109 t / 91 ms | 1 / 141 B / 68 t / 87 ms | 2 / 379 B / 149 t / 83 ms | 1 / 156 B / 66 t / 93 ms |
| split_commit | 5 / 451 B / 213 t / 110 ms | 1 / 172 B / 89 t / 42 ms | 4 / 937 B / 383 t / 201 ms | 2 / 428 B / 182 t / 137 ms |
| reorder_commits | 1 / 98 B / 83 t / 44 ms | 1 / 216 B / 86 t / 37 ms | 2 / 319 B / 123 t / 76 ms | 2 / 251 B / 116 t / 89 ms |
| squash_commits | 1 / 293 B / 159 t / 68 ms | 1 / 179 B / 82 t / 34 ms | 2 / 317 B / 130 t / 78 ms | 1 / 164 B / 84 t / 60 ms |
| uncommit_file | 4 / 396 B / 189 t / 85 ms | 1 / 153 B / 64 t / 81 ms | 2 / 380 B / 152 t / 96 ms | 1 / 128 B / 62 t / 73 ms |
| undo_rewrite | 2 / 272 B / 125 t / 26 ms | 1 / 42 B / 20 t / 24 ms | 1 / 53 B / 28 t / 43 ms | 1 / 320 B / 132 t / 32 ms |
| sync_conflict | 3+1e / 842 B / 272 t / 146 ms | 2+1e / 481 B / 170 t / 207 ms | 3+1e / 1294 B / 327 t / 335 ms | 3+1e / 1077 B / 338 t / 256 ms |
| parallel_branch | 3 / 170 B / 86 t / 54 ms | 1 / 37 B / 33 t / 42 ms | 1 / 71 B / 33 t / 72 ms † | 2 / 357 B / 147 t / 124 ms |
| inspect_state | 3 / 113 B / 70 t / 37 ms | 1 / 246 B / 86 t / 13 ms | 2 / 385 B / 156 t / 45 ms | 2 / 357 B / 158 t / 75 ms |
| reword_commit | 1 / 306 B / 159 t / 97 ms | 1 / 164 B / 72 t / 33 ms | 2 / 406 B / 153 t / 82 ms | 1 / 156 B / 72 t / 75 ms |
| drop_commit | 1 / 114 B / 67 t / 57 ms | 1 / 130 B / 52 t / 67 ms | 2 / 418 B / 155 t / 91 ms | 1 / 297 B / 104 t / 90 ms |
| move_to_branch | 4 / 340 B / 163 t / 117 ms | 1 / 189 B / 80 t / 89 ms | 2 / 481 B / 177 t / 78 ms † | 2 / 310 B / 128 t / 134 ms |

#### Quietest mode (documented quiet flags)

| Tool | Tool calls (cmds + edits) | Output KB | Est. tokens (read + typed) | Median total ms | All verified |
|---|---|---|---|---|---|
| git | 34 (33 + 1) | 2.49 | 1421 (820 + 601) | 1021 | yes |
| aisc | 15 (14 + 1) | 1.14 | 624 (388 + 236) | 838 | yes |
| but | 28 (27 + 1) | 6.38 | 2272 (2012 + 260) | 1475 | yes |
| jj | 25 (22 + 3) | 0.65 | 722 (215 + 507) | 1396 | yes |

| Task | git | aisc | but | jj |
|---|---|---|---|---|
| select_hunks | 3 / 837 B / 321 t / 64 ms | 1 / 62 B / 44 t / 71 ms | 2 / 1801 B / 484 t / 108 ms | 3+2e / 305 B / 250 t / 151 ms |
| amend_older | 2 / 46 B / 56 t / 92 ms | 1 / 41 B / 36 t / 96 ms | 2 / 268 B / 119 t / 90 ms | 1 / 0 B / 17 t / 100 ms |
| split_commit | 5 / 166 B / 126 t / 139 ms | 1 / 27 B / 43 t / 38 ms | 4 / 898 B / 376 t / 260 ms | 2 / 0 B / 45 t / 119 ms |
| reorder_commits | 1 / 0 B / 47 t / 41 ms | 1 / 78 B / 43 t / 25 ms | 2 / 280 B / 116 t / 73 ms | 2 / 0 B / 40 t / 104 ms |
| squash_commits | 1 / 0 B / 59 t / 63 ms | 1 / 75 B / 51 t / 23 ms | 2 / 278 B / 123 t / 86 ms | 1 / 0 B / 31 t / 52 ms |
| uncommit_file | 4 / 163 B / 111 t / 97 ms | 1 / 48 B / 32 t / 77 ms | 2 / 341 B / 145 t / 80 ms | 1 / 0 B / 21 t / 59 ms |
| undo_rewrite | 2 / 272 B / 127 t / 31 ms | 1 / 18 B / 14 t / 23 ms | 1 / 53 B / 28 t / 44 ms | 1 / 0 B / 6 t / 31 ms |
| sync_conflict | 3+1e / 666 B / 224 t / 151 ms | 2+1e / 389 B / 144 t / 199 ms | 3+1e / 1294 B / 327 t / 256 ms | 3+1e / 0 B / 45 t / 247 ms |
| parallel_branch | 3 / 0 B / 34 t / 53 ms | 1 / 37 B / 35 t / 44 ms | 1 / 71 B / 33 t / 78 ms † | 2 / 0 B / 35 t / 112 ms |
| inspect_state | 3 / 113 B / 70 t / 45 ms | 1 / 246 B / 86 t / 13 ms | 2 / 274 B / 126 t / 39 ms | 2 / 357 B / 158 t / 70 ms |
| reword_commit | 1 / 46 B / 72 t / 100 ms | 1 / 58 B / 38 t / 27 ms | 2 / 295 B / 123 t / 89 ms | 1 / 0 B / 22 t / 74 ms |
| drop_commit | 1 / 46 B / 49 t / 60 ms | 1 / 33 B / 23 t / 53 ms | 2 / 307 B / 125 t / 89 ms | 1 / 0 B / 15 t / 86 ms |
| move_to_branch | 4 / 190 B / 125 t / 108 ms | 1 / 57 B / 36 t / 73 ms | 2 / 370 B / 147 t / 108 ms † | 2 / 0 B / 37 t / 111 ms |

### Context cost (agent guidance)

The guidance an agent loads before it runs any command also costs tokens, in
every session. We counted it with
[`commands/context_cost.py`](commands/context_cost.py). The exact numbers below
need `tiktoken` `o200k_base` (0.14.0, run offline). Without it the script falls
back to bytes/4 and labels the output as an estimate. These are file sizes,
not billed usage.

Sources, pinned:

- **AGENTS block**: the tool-specific `## Version control` block that the
  vcbench harness writes into AGENTS.md/CLAUDE.md.
  - GitButler: `but agent setup --print` from `but` 0.22.3 (sha256
    `2945e8f55c4d…`).
  - git, jj and aisc: the blocks in the harness, from vcbench `c714c37`.
- **Skill**: the installed `SKILL.md`.
  - GitButler: `but skill install` from `but` 0.22.3 (`SKILL.md` sha256
    `30da26b0edee…`).
  - jj: the onevcat-jj skill at commit `4955f5422d99` (sha256 `e0364004…`),
    the one vcbench pins.
  - aisc: `skills/aisc/SKILL.md` in this repository.
  - Git has no skill.
- **References**: files the skill points to for detail, loaded on demand.

| Context cost (tokens) | git | aisc | but | jj |
|---|---|---|---|---|
| AGENTS block | 63 | 83 | 475 | 167 |
| Skill core (`SKILL.md`) | – | **664** (558 before the v0.1.2 verbs and `show REV:PATH`) | 5415 | 2591 |
| Block + core | 63 | **641** | 5890 | 2758 |
| References, on demand | – | 681 (`references/lifecycle.md`) | 14487 (`reference.md` 7383, `examples.md` 3662, `concepts.md` 3442) | – |
| Total, core + references (+ block)¹ | 63 | **1239** (1322) | 19902 (20377) | 2591 (2758) |

¹ The two cores do not cover the same scope. GitButler's core also covers
forge and branch workflows (push, PRs, apply/unapply). AISC moves its
equivalents to `references/lifecycle.md`: worktrees for parallel agents,
`verify`/`finish`, `gc` and `github`/`gitlab`. So the totals are the fairer
comparison: about 1.2k tokens for AISC against 19.9k for GitButler, or 1.3k
against 20.4k with the AGENTS block.

The AISC core is one page. It holds:

- the task-to-command list;
- the selector syntax;
- the rules on exit codes and partial failure, sync conflicts and paused-sync
  refusals, base-branch refusals, raw-git staleness, and undo.

The lifecycle reference covers `start`/`checkpoint`/`verify`/`finish`/`gc` with
their disk rules, `github`/`gitlab`, and the Codex sandbox note.

[`commands/skill_check.sh`](commands/skill_check.sh) runs every command form and
refusal in both files against a real binary (73 checks). The vcbench reference
check (`npm run aisc:check`) passes all 13 tasks with this skill.

### Where AISC loses

- **Wall time.** AISC is the slowest tool in every task. The one exception is
  inspect_state in quietest mode, where it ties Git and jj. Its writes are 5 to 15
  times slower than Git and roughly 6 times slower in total.
  - The profile counts 36 to 74 `git` subprocesses per write verb, through a
    PATH shim.
  - `GIT_TRACE2_EVENT` shows those processes running for only 56 to 165 ms of
    a 0.75 to 1.7 s step. The other 85 to 95% goes to starting processes.
  - Each spawn costs about 13 to 21 ms under this load. About 10 ms of that is
    the macOS `/usr/bin/git` xcrun shim: `git --version` took 18.5 ms through
    the shim and 8.2 ms on the resolved binary.
  - AISC's own start is cheap: `aisc --version` takes 4.5 ms.
- **Output in quietest mode.** Jujutsu with `--quiet` prints far less: 0.65 KB
  against AISC's 1.44 KB, which brings jj's token estimate (648) below AISC's
  (688). AISC has no quiet flag on `undo`, `switch` or `log`, and `undo` still
  prints a branch summary. In default mode AISC has the lowest token estimate.
- **parallel_branch.** It took 3 calls, because `commit -b` branched from
  HEAD: `switch main`, `commit -b docs`, `switch feature`. `commit -b NEW
  --onto BASE` now does it in one call (not yet re-measured here).

### Optimization opportunities

1. Resolve the real `git` binary once per process instead of exec'ing
   `/usr/bin/git`. This saves about 10 ms per spawn, roughly 0.5 s on a 60-call
   verb.
2. Batch repeated reads. Each mutation makes about 11 `rev-parse`, 6 `var`,
   5 `config` and 5 `symbolic-ref` calls; batching them would remove about 20
   spawns. Object traffic could go through one `cat-file --batch` or
   `update-index --stdin` process.
3. Build the post-operation summary from data already in memory. `aisc log`
   alone costs 9 Git calls and about 100 ms. Add `-q` to `undo` and `switch`,
   and a `commit -b NEW --onto BASE` form so parallel_branch is one call.

### Reproduce

```sh
python3 benchmarks/commands/run.py --but /path/to/but --jj /path/to/jj \
  --output benchmarks/results/commands-v2.json --repetitions 6   # builds aisc --release itself
python3 benchmarks/commands/report.py benchmarks/results/commands-v2.json
python3 benchmarks/commands/test_scenarios.py    # graders reject no-ops; Git passes in both modes
benchmarks/commands/skill_check.sh target/release/aisc   # every command in skills/aisc/SKILL.md
python3 benchmarks/commands/context_cost.py aisc=skills/aisc/SKILL.md but=/path/to/but-skill/SKILL.md
```

Any missing tool is skipped and listed under `skipped`. Without `tiktoken`,
tokens fall back to bytes/4, and the results label them as an estimate. Temp
repositories live in the ignored `benchmarks/.scratch/` and are deleted after
each run.
