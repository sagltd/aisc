---
name: aisc
description: One-call Git history edits: commit hunks, amend older commits, split, squash, reorder, sync, undo.
license: MIT
---
Use `aisc` for all version-control writes. One command per task:

- state: `aisc log`; read: `aisc show REV` or `A..B` (on this branch)
- commit: `aisc commit -m MSG SEL...` (`--all`; `-b NEW` switches to it; `-b NEW --onto BASE` stays here)
- amend older: `aisc amend --into COMMIT SEL...` (repeatable)
- split: `aisc split COMMIT --into MSG SEL...` (repeatable)
- uncommit, keep on disk: `aisc uncommit COMMIT SEL...`
- squash: `aisc squash --into MSG COMMIT COMMIT...`
- reorder: `aisc move COMMIT... --before|--after COMMIT`
- rebase, keep dirty work: `aisc sync [ONTO] [--fetch] [--prefer-branch PATH]`
- copy: `aisc pick [--from BRANCH] COMMIT... [-b NEW --onto BASE]` (COMMIT may be a branch)
- integrate: `aisc land BRANCH... --into main --switch`
- discard: `aisc discard SEL...`; switch: `aisc switch [-c] BRANCH`
- undo (again = redo): `aisc undo`

COMMIT: hex id or exact subject. SEL: `PATH`, `"PATH~text"` (hunk containing text), `PATH:A-B` (new-side lines) or a hunk id from `aisc log`.

Rules: writes print the result, don't re-check; `-q` prints one line; unselected changes stay uncommitted. Exit 1, 2 or 4 means nothing changed: read `error:`/`hint:`, fix, rerun (4: run `aisc log` first). A sync conflict exits 0 with CONFLICTS: edit the marked files, then `aisc sync --continue` (or `--abort`). Keep unrelated changes; never reset, clean or push unless asked.
