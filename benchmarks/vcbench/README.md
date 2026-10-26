# vcbench `aisc+skill` arm

Compares AISC with plain Git, GitButler (`but`) and Jujutsu (`jj`) on identical tasks, using GitButler's MIT-licensed [version-control-bench](https://github.com/gitbutlerapp/version-control-bench) harness (`vcbench`). This directory holds only our patch to it; the harness itself is not vendored.

Status: the harness side (arm, policy, disk metric, reference solutions) is complete and all 13 aisc reference solutions pass against the history verbs on `dev` (`npm run aisc:check`). `aisc gc`, `finish` and `start --sparse` were still in flight when this was written. The six held-out tasks (`scripts/lib/scenario-heldout*.mjs`) have reference solutions for the git, but+skill and aisc+skill arms; the AISC ones use `reword`, `drop` and `move --to` (v0.1.2) and all 18 pass their verifiers.

## Reproduce

1. Get the harness at the pinned commit and apply the patch:

   ```sh
   git clone https://github.com/gitbutlerapp/version-control-bench.git vcbench
   cd vcbench
   git checkout 4886fd5bf809dc908a871940313928b413a744e4
   git apply /path/to/aisc/benchmarks/vcbench/aisc-arm.patch
   ```

   The patch is `git diff --binary 4886fd5 aisc-arm` of a local branch; it has no dependencies (Node >= 20).

2. Install the tools and record their versions and hashes. Use the same binaries for every run and pass them explicitly:

   | Tool | Version used | SHA-256 |
   |---|---|---|
   | git | 2.50.1 (Apple Git-155) | not pinned (system binary) |
   | but | 0.22.3 | `b79af0f95d3bce2274d03f9c91d3868d17a91c9fcfe6f4224e3aea117155c550` |
   | jj | 0.42.0-b8f7c455170e3273897aaf94431f8ccfb1afa7ad (aarch64-apple-darwin) | `9677341bc90a1f4b6e1df52d2c1ddcb666ed588b7834d3f131a77fa521b2591d` (binary); release tarball `98764966f22b599dc0b19bb9bd00d21df86156aeca5827f8274900356768db08` |
   | aisc | `aisc --human --version` of the release build under test (`--version` alone prints JSON when redirected) | record `shasum -a 256 $(command -v aisc)` |
   | codex / claude | 0.163.0 / 2.1.306 | record `--version` |

   The harness defaults pin git 2.44.0 and but 0.5.2189. Because we use other versions, supply a pin file so mismatches fail loudly instead of silently:

   ```json
   {"git": "git version 2.50.1 (Apple Git-155)", "but": "but 0.22.3", "jj": "jj 0.42.0-b8f7c455170e3273897aaf94431f8ccfb1afa7ad", "aisc": "aisc <version>"}
   ```

3. Freeze the skill so every run installs identical bytes, and keep its hash with the results:

   ```sh
   cp -R /path/to/aisc/skills/aisc ./frozen-skills/aisc
   shasum -a 256 frozen-skills/aisc/SKILL.md
   ```

   Each run records the skill sha256 (`skill.skill_file_sha256`) and `aisc --version` (`skill.aisc_version`, `aisc_binary`) in `result.json`.

4. Check the harness before spending any model tokens:

   ```sh
   npm run runner:check                                   # policy, guard and disk unit tests
   npm run pilot:check -- --but-bin /path/to/but          # also pilot2..pilot6:check; git/but/jj references
   npm run aisc:check -- --aisc-bin /path/to/aisc --aisc-skill-dir frozen-skills/aisc --but-bin /path/to/but --version-pins pins.json
   ```

   `aisc:check` runs the aisc reference solution for pilots 1-6 and the deterministic references for expansion tasks 7-13. `check-runner-infra.mjs` (part of `runner:check`) has a 250 ms timing assertion that can fail on a loaded machine.

5. Permission smoke test: one real run per arm and agent on the simplest task, before the matrix. Check that no run hit a sandbox or permission denial (for example `grep -il "permission\|denied\|not allowed\|sandbox" runs/*/agent-stdout.txt runs/*/command-output/*.stderr`), that every arm's tool state was writable (GitButler app data, `.jj`, `.git`) and that no run ended in `AGENT_RUNTIME_ERROR`. If a denial appears, widen the allowlist or `--add-dir` set identically for all arms, and discard the smoke runs.

6. Run trials (all arms in one batch, same sandbox, models and effort):

   ```sh
   npm run matrix:run -- --arms git,but+skill,jj+skill,aisc+skill --k 5 \
     --codex-model <id> --claude-model <id> --codex-effort-level <level> --claude-effort-level <level> \
     --but-bin /path/to/but --jj-bin /path/to/jj --aisc-bin /path/to/aisc \
     --aisc-skill-dir frozen-skills/aisc --jj-skill-dir <frozen jj skill> \
     --version-pins pins.json --build-but false --batch-name aisc-k5
   ```

   Use `--dry-run true` first to see the plan. `--k` should be at least 5 per cell. Do a one-run pilot per agent before the full matrix.

## Reference solutions (no LLM)

Every reference passes its verifier. Calls and bytes are the `aisc` invocations of the reference script only (stdout plus stderr); an agent usually adds one `aisc log` first.

| Task | Calls | Output bytes |
|---|---|---|
| 1 selective commit | 1 | 274 |
| 2 multi-amend | 1 | 464 |
| 3 split | 1 | 597 |
| 4 reorder | 1 | 609 |
| 5 squash | 1 | 440 |
| 6 dirty sync | 2 | 648 |
| 7 scoped review | 1 | 524 |
| 8 cold checkpoint | 1 | 203 |
| 9 cleanup noise | 1 | 144 |
| 10 recover amend | 1 | 177 |
| 11 dual-source sync | 2 | 702 |
| 12 integrate topics | 1 | 360 |
| 13 selective copy | 1 | 216 |

## What the patch adds

- **Arm `aisc+skill`.** The fixture is prepared exactly like the `git` arm (dirty fixture created directly, no tool setup), so state equivalence with Git holds trivially. The skill is copied to `.codex/skills/aisc` and `.claude/skills/aisc`, with an `AGENTS.md`/`CLAUDE.md` block taken from `aisc agent setup --print`, as the upstream `but+skill` arm embeds `but agent setup --print` (the cold task gets a generic block in both arms). Scenario verifiers use `gitLike(arm)` (`scripts/lib/arms.mjs`).
- **Policy** (`tool-policy.mjs`, `command-guards.mjs`): `aisc` is blocked in the other arms; raw git writes, `but` and `jj` are blocked in the aisc arm; read-only git stays allowed; git spawned by `aisc` counts as tool-internal, not as an agent command. `aisc log/show/oplog/context/diff/github/gitlab` and `gc` without `--apply` count as inspection, everything else as mutation. Shims are PATH wrappers (observability), not enforcement: an absolute path bypasses them.
- **Disk metric** (`scripts/lib/disk.mjs`): `result.disk.{before,after,delta}_{apparent,allocated}_bytes` around the agent phase, covering the workspace including `.git`/`.jj`, linked worktrees outside it (from `git worktree list`) and GitButler's app-data directory. Apparent = `st_size`; allocated = `st_blocks * 512`; symlinks are not followed and hard links count once.
- **Sandboxed permissions** (see below).
- **Reference solutions** `tasks/*/solution/solve-aisc.sh` for all 13 tasks, run by `aisc:check`.

## Fairness rules

- The `AGENTS.md`/`CLAUDE.md` block has the same frame in every skill arm (benchmark boundary, local skill location, version-control block). In the `but+skill` and `aisc+skill` arms the version-control block is the tool's own `agent setup --print` output; `jj+skill` and `git` get the harness's short policy text, as upstream.
- Same task text and wrapper prompt, fixtures, sandbox, agent CLI, model, effort and timeout for every arm; arms differ only in the tool and its skill/instructions. Use `--rotate-arms true` to spread order effects.
- Fixture creation, tool setup and skill installation happen before the timed agent phase and are excluded from wall time, commands and tokens (except the deliberately cold task 8).
- Same verifier for every arm: Git-visible state, with the minefield checks (stash, operation residue, protected refs, uncommitted work).
- Policy violations (for example raw git writes in the aisc arm) fail the run (`PROTOCOL_VIOLATION`); keep failed runs in the denominator.
- Report per arm and task: pass rate, VC commands (task, platform and tool-internal separately), wall time, transcript KB, provider-reported tokens (kept separate from transcript size; never present transcript tokens as billed usage), cost where the provider reports it, and disk delta. Keep negative results. Skill text length is a cost of the arm and is part of what is measured.
- k >= 5 per cell, the same models in every arm. Do not cherry-pick runs or retune the skill between arms mid-batch; change the skill only between batches and record its hash.
- Never publish private agent sessions, credentials or machine-specific paths with results.

## Difference from the upstream setup

Upstream runs Codex with `--dangerously-bypass-approvals-and-sandbox` and Claude with `--permission-mode bypassPermissions`. Our configuration does not bypass the sandbox, and this fork defaults to it for all arms (`--agent-sandbox false` restores the upstream behavior; `matrix:run` forwards the flag):

- **Codex:** `--sandbox workspace-write` plus `--add-dir` for the fixture's Git directory (`--absolute-git-dir` and `--git-common-dir`; Git metadata is read-only under the workspace sandbox), the run directory (policy-shim trace and GitButler's isolated app data) and, when the arm created it, `.jj`.
- **Claude:** `--permission-mode acceptEdits` with an allowlist in the per-run settings file: file tools (`Read`, `Edit`, `MultiEdit`, `Write`, `Glob`, `Grep`), `Bash(git|but|jj|aisc:*)` and common shell and file utilities (`cat`, `ls`, `head`, `tail`, `wc`, `echo`, `printf`, `pwd`, `diff`, `sort`, `grep`, `rg`, `sed`, `awk`, `cut`, `tr`, `uniq`, `find`, `mkdir`, `cp`, `mv`, `rm`, `touch`, `chmod`, `test`, `cd`, `node`, `python3`). Override with `--claude-allowed-tools`. Claude has no OS sandbox in this mode, so the allowlist only limits which commands run without a prompt.

Results are therefore not directly comparable with the published upstream numbers; compare arms within one batch. Both configurations are recorded in `result.json` under `agent_config`.

`runs:export` replaces host paths (run, batch, harness and home directories) with placeholders and reduces tool and pin paths in `result.json` to file names before the secret scan.
