<!-- CLAIMS: fill only from measured results -->
<!-- Placeholders: each ⏳ cell has a BENCH comment; replace both together. -->
<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
  <img src="assets/logo.svg" alt="AISC" width="420">
</picture>

**Git-backed source control for coding agents.**
One call per history edit. Compact output. Your work stays safe.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust 1.91.1+](https://img.shields.io/badge/rust-1.91.1%2B-orange.svg)](Cargo.toml)
[![Platforms](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey.svg)](#safety-model)
[![Git 2.43+](https://img.shields.io/badge/git-2.43%2B-f05032.svg)](#safety-model)

[Why](#why-aisc) · [Before/after](#before--after) · [Benchmarks](#benchmarks) · [Quickstart](#quickstart) · [Commands](#commands) · [Safety](#safety-model) · [Disk](#disk-efficiency)

</div>

## Why AISC

- **One call per history edit.** Amend, split, squash, reorder or sync is a single command, not a rebase script.
- **Compact output.** Short ids, one line per commit, one line per hunk. Built to cost few transcript tokens.
- **Never loses your work.** Every call is atomic, recorded in an oplog, and reversible with `aisc undo`.
- **Disk-lean.** No worktree by default, automatic cleanup on `finish`, and `aisc gc` for the rest.

It is a thin layer over Git: Git stays the storage and transport backend, and your repository stays a normal repository.

## Before / after

Task: move three uncommitted hunks into three older commits.

<table>
<tr><th>Plain Git</th><th>AISC</th></tr>
<tr><td>

```sh
git stash
git log --oneline -n 6
git rebase -i HEAD~5      # mark 3 edits
git stash pop             # conflicts?
git add -p                # pick hunk 1
git commit --fixup 3a5d
git add -p                # pick hunk 2
git commit --fixup 9c07
git add -p                # pick hunk 3
git commit --fixup e41b
git rebase -i --autosquash HEAD~8
```

</td><td>

```sh
aisc log
aisc amend \
  --into 3a5d gnh \
  --into 9c07 kqv \
  --into e41b tmr
```

</td></tr>
</table>

Illustrative command counts only; nothing is claimed about speed or cost. See [Benchmarks](#benchmarks) for what is and is not measured.

## Benchmarks

Per-scenario means. Competitor figures are the numbers [published by vcbench.dev](https://vcbench.dev). GitButler ran them with their own setup; **AISC will be re-run under identical conditions** with the same harness, and every AISC cell stays a placeholder until then.

| Tool | Commands / task | Wall time | Transcript | Provider tokens | Disk added |
| --- | --- | --- | --- | --- | --- |
| **AISC** | ⏳ <!-- BENCH:aisc.commands --> | ⏳ <!-- BENCH:aisc.time --> | ⏳ <!-- BENCH:aisc.kb --> | ⏳ <!-- BENCH:aisc.tokens --> | ⏳ <!-- BENCH:aisc.disk --> |
| GitButler | 4.5 | 34.0 s | 13.3 KB | not published | not published |
| Jujutsu | 21.0 | 110 s | 46.1 KB | not published | not published |
| Git | 20.4 | 92.2 s | 31.7 KB | not published | not published |

Design target, not a result: roughly 2 calls and 2-3 KB of output per scenario. Transcript size is a text proxy, not billed provider usage.

Method and existing measurements: [`docs/benchmarks.md`](docs/benchmarks.md) · harness: [`benchmarks/vcbench/README.md`](benchmarks/vcbench/README.md) (lands with the vcbench arm).

## Quickstart

```sh
# build from source (Git and Rust 1.91.1+); prebuilt release binaries: coming with v0.2
cargo install --git https://github.com/sagltd/aisc --locked

# teach your agent the workflow
npx skills add sagltd/aisc --skill aisc
```

A typical agent loop (output abbreviated from the v0.2 history design):

```text
$ aisc log
amend-series: 5 commits on main (newest first)
3a5d add lead scoring | README.md src/lead.ts tests/lead.test.ts
b2c9 refactor validation helpers | README.md src/lead.ts tests/lead.test.ts
uncommitted:
src/lead.ts
 gnh @21 +6-1 export function isLikelyEmail(email: string): boolean {
$ aisc amend --into b2c9 gnh
amended b2c9 +1 hunk; rewrote 2; op 1a7f
$ aisc verify -- cargo test && aisc finish
```

Today's in-place task flow: `aisc start NAME --in-place`, `aisc context`, `aisc checkpoint -m MSG -- PATHS`, `aisc verify -- CMD`, `aisc finish`.

## Commands

Available now: `start`, `context`, `diff`, `checkpoint`, `verify`, `finish`, `github`, `gitlab`. The history verbs below land in **v0.2**. Selectors are a hunk id (`gnh`), a path, `PATH:L-R`, or `PATH~text`.

| Verb | What it does | Example |
| --- | --- | --- |
| `log` | Branch view plus uncommitted hunks with ids | `aisc log --limit 10` |
| `show` | Hunks of a commit, range or uncommitted changes | `aisc show 3a5d` |
| `commit` | New commit(s) from selected hunks | `aisc commit -m "Add validation" src/lead.ts` |
| `amend --into` | Move uncommitted hunks into older commits | `aisc amend --into 3a5d gnh` |
| `split` | Break a commit into several | `aisc split 4c1d --into "A" a.ts --into "B" b.ts` |
| `uncommit` | Pull hunks out of a commit back to the worktree | `aisc uncommit 4c1d src/config.ts` |
| `squash` | Join adjacent commits | `aisc squash --into "Feature" 9c07 e41b` |
| `move` | Reorder commits | `aisc move e41b --before 3a5d` |
| `pick` | Replay commits onto another branch | `aisc pick 9c07 -b hotfix` |
| `sync` | Rebase onto base; pauses on conflict | `aisc sync --fetch` |
| `land` | Fast-forward or replay branches into a target | `aisc land feature` |
| `switch` | Change branch, carrying dirty edits | `aisc switch -c topic` |
| `discard` | Revert selected uncommitted changes (undoable) | `aisc discard --whitespace-only` |
| `undo` / `oplog` | Reverse or list recorded operations | `aisc undo` |
| `gc` | Reclaim stale AISC worktrees | `aisc gc --apply` |

`aisc <verb> --help` is the authority for the installed version.

## Safety model

```mermaid
flowchart LR
  A[plan] --> B[preflight] --> C["ref transaction (CAS)"] --> D[index / worktree] --> E[oplog]
```

- **Atomic per call.** Refs update by compare-and-swap. If anything fails or something changed concurrently (exit 4), refs, index and worktree stay byte-identical.
- **Undo.** Each mutation writes an oplog entry; `aisc undo [OP]` restores it, including bytes removed by `discard`.
- **Your index is preserved.** Staged state you did not touch is left alone.
- **Hooks.** `checkpoint` runs your Git hooks. History plumbing commits do not, as with `git rebase`.
- **Requirements.** Git 2.43+, macOS or Linux. CI covers Ubuntu 24.04 and macOS 15; Windows is untested.
- **One agent per checkout.** Use separate worktrees for parallel agents.

## Disk efficiency

- **Single-checkout model.** History verbs edit in place; `--branch B` rewrites another branch with no checkout.
- **`finish` cleans up.** Worktrees AISC created are removed (never forced; `--keep` to retain) and freed bytes are reported.
- **`aisc gc`.** Dry run by default; `--apply` removes only verified, clean, merged-or-finished AISC worktrees. Foreign worktrees are never touched.
- **Sparse worktrees.** `aisc start NAME --path P --sparse DIR...` checks out only the directories you need.

Measured disk numbers: pending, see [Benchmarks](#benchmarks).

## Advanced

<details><summary>Sandbox permissions (Codex, Claude)</summary>

AISC writes Git metadata, so the agent host must allow it. For non-interactive Codex, pass `--add-dir /abs/path/to/repo/.git` and keep the workspace sandbox on. For a linked worktree, allow the resolved directories:

```sh
git -C /path/to/worktree rev-parse --path-format=absolute --git-dir
git -C /path/to/worktree rev-parse --path-format=absolute --git-common-dir
```

Interactive agents can use their normal approval flow. Do not disable the sandbox.

</details>

<details><summary>Output formats and JSON</summary>

Terminals get readable text; redirected output is stable JSON for existing verbs. New history verbs print compact text by default. Force with `--json` or `AISC_OUTPUT=json`, or `--human --color always|never`. `NO_COLOR` disables color. Errors carry a `code` (`usage|selector|stale|dirty|conflict|unsupported|state|git`).

</details>

<details><summary>GitHub and GitLab overviews</summary>

`aisc github` (via `gh pr status`) and `aisc gitlab` (via `glab`, open MRs whose source branch matches) are explicit, read-only, need network access and may return partial fields. Local commands never need either CLI.

</details>

<details><summary>Verify and finish evidence binding</summary>

`verify` runs a program directly (use `sh -c` only when you need a shell) on a clean committed checkout, macOS or Linux. The record binds tracked files, index, Git config, environment and executable identity. Ignored files, interpreters and external services are not hermetic inputs. `finish` succeeds only when the same clean commit and environment have a passing record; it never pushes or merges. Git hooks and signing programs run normally and are trusted.

</details>

<details><summary>Current measured evidence</summary>

Measured to date, with limits, in [`docs/benchmarks.md`](docs/benchmarks.md) and `benchmarks/results/`. On one compact inspection AISC context was slower and used more text-proxy tokens than GitButler status plus Git numstat. `cargo-audit` 0.22.2 reported zero advisories across 49 dependencies for the audited lockfile snapshot only. These do not establish whole-workflow or provider-usage savings, or production readiness.

</details>

## Project

[Contributing](CONTRIBUTING.md) · [Security](SECURITY.md) · [MIT License](LICENSE) · [Research notes](docs/research.md)

Credits: the benchmark harness builds on [vcbench](https://vcbench.dev), an MIT-licensed project by GitButler. Competitor numbers belong to its published runs.
