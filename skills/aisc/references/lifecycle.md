# AISC task lifecycle: parallel agents, worktrees, PRs

Use this flow when several agents share a repository, or when the work must be
handed over for review.

- `aisc start NAME --path PATH` creates an isolated worktree on a new branch,
  `aisc/NAME`. PATH must not exist, but its parent must. Put it under an ignored
  directory; AISC creates no directories and does not edit `.gitignore`.
  - `--sparse DIR...` checks out only those directories, in cone mode. This
    enables `extensions.worktreeConfig` in the shared Git config, and that
    setting stays on.
  - `aisc start NAME --in-place` keeps the current directory and branch. It
    requires a clean, committed checkout and does not isolate files.
- `aisc checkpoint -m MSG -- PATH...` commits whole files and keeps unrelated
  staging. Hooks and signing run normally. For hunks or history edits, use the
  verbs in `SKILL.md`.
- `aisc verify -- PROGRAM ARG...` runs the program directly, without a shell.
  Success is bound to the clean committed candidate. The default timeout is
  `--timeout 300` seconds; on timeout the process group is stopped.
- `aisc finish` reports the verified commit and branch for review. It never
  pushes or merges, and it refuses (exit 1) unless the current candidate passed
  `verify`.
  - Disk: `finish` removes a `--path` worktree and keeps the branch.
  - `--keep` leaves the worktree in place.
  - It refuses to delete ignored files that are secret-like or not known to be
    regenerable. `--delete-ignored` overrides that.
  - If your shell was inside the removed worktree, `cd` to the path it prints.
- `aisc gc` lists reclaimable AISC worktrees; this is a dry run. `aisc gc --apply`
  removes them.
  - `--min-age HOURS` (default 24) keeps worktrees with Git activity in that
    window.
  - `--delete-ignored` behaves as it does for `finish`.
- `aisc github [--limit N]` and `aisc gitlab [--limit N]` are read-only PR/MR
  overviews. They use the existing `gh` or `glab` authentication and need the
  network. If the CLI is missing or fails, they exit 1. Never infer passing
  checks from missing data.
- Codex non-interactive runs: grant only the repository's Git directories with
  `--add-dir`, and keep the workspace sandbox. Never disable the sandbox.
- Keep transcripts, credentials and machine-specific paths private.
