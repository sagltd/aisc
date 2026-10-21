# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/sagltd/aisc/commits/dev
