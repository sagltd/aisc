---
name: aisc
description: One-call Git history edits for agents: commit hunks, amend into older commits, split, squash, reorder, sync dirty branches, undo.
license: MIT
compatibility: Requires Git and the AISC command-line tool (sagltd/aisc).
---

# AISC

Use `aisc` for every version-control write. Run `aisc log` once: it shows the branch's commits (hex ids) and uncommitted hunks (letter ids). Don't also run git status/log/diff.

Refs: COMMIT = hex id or exact subject in quotes. SEL = PATH | hunk id | PATH:START-END | "PATH~text in the change".

- Commit: `aisc commit -m MSG [-b NEW_BRANCH] SEL...` (`--all` = everything)
- Fold changes into existing commits: `aisc amend --into COMMIT SEL... [--into COMMIT SEL...]`
- Split: `aisc split COMMIT --into MSG SEL... [--into MSG SEL...]` (unselected parts become uncommitted)
- Remove part of a commit: `aisc uncommit COMMIT SEL...`
- Squash: `aisc squash --into MSG COMMIT COMMIT... [--into MSG ...]`
- Reorder: `aisc move COMMIT... --before|--after COMMIT`
- Rebase onto a moved base, keeping uncommitted work: `aisc sync [ONTO]`. On CONFLICTS, edit the marked files, then `aisc sync --continue`
- Copy commits: `aisc pick COMMIT... [-b NEW --onto BASE]`; integrate: `aisc land BRANCH... --into main --switch`
- Discard uncommitted changes: `aisc discard SEL...`; undo the last operation: `aisc undo`

Every write prints the resulting log; don't re-check. Anything not selected stays uncommitted. Use `aisc show COMMIT` to see a commit's hunks.

## Safety

- Follow the repository's `AGENTS.md`. Touch only what the task names; keep other staged, unstaged and untracked changes.
- Never reset, clean, force-push or publish unless asked. Read `aisc --help` if a flag here is rejected.
- Codex non-interactive runs need `--add-dir` for the repository's Git directories; never disable the sandbox.
- Parallel work: `aisc start NAME --path PATH` (new ignored path) or `--in-place` (clean tree only).
- Read-only context: `aisc github` / `aisc gitlab` (need `gh` / `glab`).
- Keep transcripts, credentials and machine paths private.
