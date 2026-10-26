# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `aisc branches [COMMIT]` lists local branches, or the ones whose history contains COMMIT (an
  id, an exact subject on any local branch, or a revision). Agents used raw `git branch` for this.
- `aisc move COMMIT... --to NEW --onto BASE` moves commits to a new branch started at BASE in
  one call; `aisc undo` deletes the branch and brings the commits back. (Held-out set 3: agents
  tried `move --to NEW` first and needed `pick` plus `drop`.)
- A directory is a selector: `aisc commit -m MSG docs/` takes every changed file under it,
  tracked or untracked (agents wrote this and got "unknown selector").
- `aisc commit --onto BRANCH -m MSG SEL...` commits onto an existing local branch without
  leaving this one; `-b NAME --onto NAME` means the same, so `--existing` is no longer needed
  (agents wrote both and were told to add it).
- `aisc amend --into COMMIT SEL...` works when COMMIT is on another local branch (named with
  `--branch`, or found when exactly one branch has it): the selected changes are folded into
  that commit from this worktree, the branch is rewritten above it, nothing is checked out, and
  `aisc undo` restores both. One `--into` per call there.
- `aisc show COMMIT:PATH` prints a whole file as it is at that commit (several `COMMIT:PATH`
  arguments get a header each; binary files are summarized). Review tasks needed `git show`
  for this before.

### Changed

- `aisc move COMMIT... --to BRANCH` refuses, with nothing changed, when BRANCH already contains
  a selected commit's change; before, it added an empty duplicate commit there.
- The vcbench held-out reference solutions for AISC use `reword`, `drop` and `move --to`.

## [0.1.2] - 2026-10-23

### Added

- `aisc reword COMMIT -m MSG` changes one commit's message, keeping every tree.
- `aisc drop COMMIT...` removes commits and their changes; uncommitted edits are kept or the
  command refuses with nothing changed.
- `aisc move COMMIT... --to BRANCH` moves commits to the top of another branch in one
  transaction; `aisc undo` restores both branches.

These close the gaps the held-out benchmark found (reword, drop, move to another branch).

## [0.1.1] - 2026-10-22

### Added

- `aisc land --delete` removes the landed branches in the same operation; `aisc undo` restores
  them. Branches checked out or being rebased in any worktree are refused.

### Changed

- The agent setup block and skill send agents to `aisc switch` to test another branch and to
  `land --delete` instead of raw `git worktree` and `git branch -D`.
- README reports the 3-run real-agent comparison for v0.1.0, failures and caveats included.

## [0.1.0] - 2026-10-22

### Added

- `aisc start` begins a task in the current checkout or a new isolated worktree.
- `aisc context` reports branch, changes, conflicts and staged/unstaged line
  counts with explicit limits and omitted counts.
- `aisc diff` shows tracked changes against HEAD with a byte bound, optionally
  narrowed to literal files.
- `aisc checkpoint`, `aisc verify` and `aisc finish` cover the task lifecycle on
  top of Git.
- `aisc github` and `aisc gitlab` provide bounded repository overviews.
- Human-readable colored output (`--color auto|always|never`) and JSON output.
- Reproducible source-control benchmark tooling and documentation.
- Release workflow, dependency audit (`cargo-deny`) and Dependabot configuration.
- History verbs, each one call: `log`, `show`, `commit` (path, hunk id, `PATH:L-R`
  and `PATH~text` selectors), `commit -b NEW --onto BASE`, `amend --into`, `split`,
  `uncommit`, `squash`, `move`, `pick`, `discard`, `sync` (pause, `--continue`,
  `--abort`, `--prefer-*`), `land`, `switch`, `undo`/redo and `oplog`.
- Crash-safe engine: one compare-and-swap ref transaction per operation, an
  operation journal that rolls an interrupted command forward or back, and
  compare-before-write that keeps edits made while aisc runs as `PATH.aisc-conflict`.
- Line-level verification of every rewritten commit, refusing ambiguous placements.
- Disk: `finish` removes the worktree it created (`--keep`), `gc` (dry run, `--apply`)
  with rescue refs and an allowlist of regenerable ignored content, `start --sparse`.
- Lean agent skill (`skills/aisc`) with an on-demand lifecycle reference.
- Command, disk and vcbench benchmarks against Git, GitButler and Jujutsu.

### Changed

- 6-10x fewer Git processes per command: batched reads, in-process object, ref and
  config access through gitoxide (falling back to Git whenever unsure), and Git's
  one-line answers remembered per executable.
- `--help` and `--version` always print plain text.

### Known limitations

- `aisc sync` takes commits someone else pushed to the branch's upstream. It recognises your
  own pushed commits from Git's records of `git push` to a named remote; after pushing to a bare
  URL instead, a later local squash of those commits can be undone by the next sync (use
  `aisc undo`).

- Do not mix an `aisc` binary from before this release with a new one on the
  same checkout. Old binaries write operation locks with no flock and no owner
  tag, so the two versions cannot see each other's locks and can both run at
  once. Upgrade all running agents together.

[Unreleased]: https://github.com/sagltd/aisc/compare/v0.1.2...dev
[0.1.2]: https://github.com/sagltd/aisc/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/sagltd/aisc/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/sagltd/aisc/releases/tag/v0.1.0
