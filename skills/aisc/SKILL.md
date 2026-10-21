---
name: aisc
description: Use AISC when an agent needs to inspect or checkpoint code work through Git while preserving the user's existing changes and repository state.
license: MIT
compatibility: Requires Git and the AISC command-line tool. Install AISC from the sagltd/aisc repository using the installation instructions in its README.
---

# AISC

Use AISC for Git-backed checkpoints and repository operations during agent work.

## Before acting

1. Read the repository's `AGENTS.md`, `CONTRIBUTING.md`, and any applicable local instructions.
2. Inspect the current branch, worktree status, and relevant history before changing state.
3. Read `aisc --help` and the relevant subcommand help. Follow the syntax and behavior documented by the installed version.
4. Preserve staged and unstaged user changes. Never reset, clean, overwrite, or publish work unless the user asked for that action.
5. Confirm the host permits the Git metadata writes the requested command needs. In a Codex non-interactive run, use `--add-dir` for only the resolved repository Git metadata directories while retaining the workspace sandbox; never recommend disabling the sandbox.

## Save and verify work

- Choose `aisc start NAME --in-place` only when staying on the current branch and
  working directory is intentional and the starting tree is clean. In-place
  mode does not isolate filesystem changes.
- For independent work, choose `aisc start NAME --path PATH` with a complete,
  ignored destination path that does not already exist. Create its parent first
  and inspect the destination before starting; AISC does not create directories
  or change `.gitignore`.
- Save only the files and state that belong to the requested task.
- Do not describe or attempt restore or rollback commands unless the installed AISC version documents them.
- After an operation, inspect the resulting status and diff. Report what AISC saved or verified, plus any conflicts or skipped paths.
- Treat local repository data as private. Do not upload session transcripts, credentials, or machine-specific files.

## Optional GitHub context

- Use `aisc github [--limit N]` only when pull-request status is relevant and
  network access is available. It is read-only and requires the authenticated
  GitHub CLI (`gh`). Do not infer that its summary contains every result or
  reduces API requests compared with an optimized `gh` query.
- Keep local context, diff, checkpoint, verification, and finish operations
  independent of GitHub availability.

## Keep claims measurable

When comparing AISC with a Git workflow, use the same task, repository state, agent, model, and tool settings for each run. Record provider-reported token usage separately from token counts estimated from saved transcript text. Describe time savings and reliability only when the corresponding measurements or checks are available.
