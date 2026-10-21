---
name: aisc
description: Use AISC when an agent needs to save, inspect, or restore code work through Git while preserving the user's existing changes and repository state.
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

## Save and restore work

- Save only the files and state that belong to the requested task.
- Before restoring or applying a saved change, inspect its base and affected paths; stop if it would overwrite unrelated work.
- After an operation, inspect the resulting status and diff. Report what AISC saved or restored and any conflicts or skipped paths.
- Treat local repository data as private. Do not upload session transcripts, credentials, or machine-specific files.

## Keep claims measurable

When comparing AISC with a Git workflow, use the same task, repository state, agent, model, and tool settings for each run. Record provider-reported token usage separately from token counts estimated from saved transcript text. Describe time savings and reliability only when the corresponding measurements or checks are available.
