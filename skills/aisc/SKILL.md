---
name: aisc
description: One-call Git history edits: commit hunks, amend older commits, split, squash, reorder, sync, undo.
---
Use `aisc` for all version-control writes. Run `aisc log` first for ids and state. One command per task:

- read: `aisc show REV` or `A..B` (on this branch); another branch: `aisc log BRANCH` (local or `origin/NAME`)
- commit: `aisc commit -m MSG SEL...` (`--all`; `-b NEW` switches to it; `-b NEW --onto BASE` stays here, runs no hooks)
- amend older: `aisc amend --into COMMIT SEL...` (repeatable)
- split: `aisc split COMMIT --into MSG SEL...` (repeatable)
- uncommit, keep on disk: `aisc uncommit COMMIT SEL...`
- squash: `aisc squash --into MSG COMMIT COMMIT...`
- reorder: `aisc move COMMIT... --before|--after COMMIT`; to another branch: `aisc move COMMIT... --to BRANCH`
- reword: `aisc reword COMMIT -m MSG`; remove commits: `aisc drop COMMIT...`
- rebase, keep dirty work: `aisc sync [ONTO] [--fetch] [--prefer-branch|--prefer-onto PATH]`
- copy: `aisc pick [--from BRANCH] COMMIT... [-b NEW --onto BASE]` (COMMIT may be a branch; BRANCH may be `origin/NAME`)
- integrate: `aisc land BRANCH... --into main --switch [--delete]` (`--delete` removes the landed branches)
- discard: `aisc discard SEL...`; switch (also to run tests on another branch; edits come along): `aisc switch [-c] BRANCH`
- undo (again = redo): `aisc undo`

COMMIT: hex id or exact subject. SEL: `PATH`, `"PATH~text"` (hunk containing text), `PATH:A-B` (new-side lines) or a hunk id from `aisc log`.

Rules:
- Writes print the result (`-q`: one line); don't re-check. Unselected changes stay uncommitted.
- Exit 1, 2 or 4: nothing changed, except a reported partial failure, which prints `aisc undo OP`. Read `error:`/`hint:`, fix, rerun (4: `aisc log` first).
- Sync conflicts exit 0 with CONFLICTS: edit the marked files, then `aisc sync --continue` (or `--abort`). Other verbs refuse while it is paused. Commits already on the base branch are refused.
- Raw git rebase, reset or commit --amend makes undo and ids stale; use aisc verbs.
- Keep unrelated changes; never reset, clean or push unless asked.

Parallel agents, worktrees, verify/finish, PRs or Codex sandbox: read `references/lifecycle.md`.
