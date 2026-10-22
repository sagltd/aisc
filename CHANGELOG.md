# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/sagltd/aisc/compare/v0.1.0...dev
[0.1.0]: https://github.com/sagltd/aisc/releases/tag/v0.1.0
