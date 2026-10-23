# Held-out tasks

Six tasks written after the tuned 13-task runs and **frozen (committed and pushed) before any agent ran them**. AISC was not changed after seeing them. Fixtures, verifiers and reference solutions for Git, GitButler and AISC are in `benchmarks/vcbench/aisc-arm.patch` (`scripts/lib/scenario-heldout*.mjs`); every tool's reference was checked, and doing nothing fails every task.

Known before running: AISC 0.1.1 has no command that rewrites a commit message, so it has no reference solution for heldout-1 and its agent is expected to fail that task. GitButler and Git have references for all six.

## heldout-1-reword

> On `fix-auth`, the commit "Fix tokne refresh" has a typo in its message: it should read "Fix token refresh". Fix that message only. Keep every commit's content, the other commit messages and the commit order as they are, and leave my uncommitted README.md edit alone. Do not publish anything.

## heldout-2-drop

> Remove the commit "Add debug logging" from `feature-export` completely, as if it had never been made. Keep the other two commits with their changes and messages, stay on `feature-export`, and leave my untracked notes.md alone. Do not publish anything.

## heldout-3-two-commits

> On `docs-and-tests`, commit my documentation changes (everything under docs/) as "Update API docs", then my test changes (everything under tests/) as "Add API tests": two commits, in that order. Leave my change to src/app.ts uncommitted. Do not publish anything.

## heldout-4-move-commit

> The commit "Add billing export" on `feature-search` was made on the wrong branch: it belongs on `feature-billing`. Move it there, on top of the existing billing commit, and remove it from `feature-search`. Keep every other commit as it is, stay on `feature-search`, and do not publish anything.

## heldout-5-find-commit

> Which commit on `main` first set `MAX_RETRIES = 7`? Reply with just that commit's subject line. Do not change anything in the repository.

## heldout-6-commit-elsewhere

> I'm on `feature-ui` with unfinished work. My change to src/config.ts is an unrelated fix: commit just that change onto the existing `hotfix` branch as "Fix default port", without leaving `feature-ui`. My src/ui.ts edit must stay uncommitted here. Do not publish anything.

