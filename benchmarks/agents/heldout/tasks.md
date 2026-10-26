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

# Held-out set 2

Six more tasks, written on 2026-10-27 after v0.1.2 (reword, drop, move --to) and **frozen (committed and pushed) before any agent ran them**. Fixtures, verifiers and reference solutions for Git, GitButler and AISC are in `benchmarks/vcbench/aisc-arm.patch` (`scripts/lib/scenario-heldout7.mjs` to `scenario-heldout12.mjs`); all 18 references pass their verifiers, and doing nothing fails every task.

Results: [`../../results/agent-runs-heldout2-v0.1.2.json`](../../results/agent-runs-heldout2-v0.1.2.json), summarized in the README. The AISC gap below was closed after the runs (`aisc amend --into` now finds the commit's branch).

Known before running: AISC 0.1.2 has no verb that amends into a commit on a branch that is not checked out (heldout-10). Its reference needs two calls (`aisc commit -b BRANCH --onto BRANCH --existing`, then `aisc squash --branch BRANCH`); GitButler's is one `but amend` after a status read; Git's uses a temporary index.

## heldout-7-amend-and-reword

> On `feature-parser`, my newest commit "Add parser" should also include the new file tests/parser.test.ts, and its message should be "Add parser with tests". Make it so: one commit, that message, with the parser and the test in it. Leave my uncommitted src/app.ts edit alone and stay on `feature-parser`. Do not publish anything.

## heldout-8-uncommit-newest

> On `feature-cache`, the newest commit "Add cache layer" was premature. Take it out of the history but keep its changes (src/cache.ts and src/app.ts) on disk as uncommitted work, next to my uncommitted README.md edit. The earlier commit "Add cache config" must stay exactly as it is. Stay on `feature-cache` and do not publish anything.

## heldout-9-stack-branch

> I'm on `feature-api`. Start a new branch `feature-api-tests` stacked on top of `feature-api` (it must contain the `feature-api` commit) and commit my new file tests/api.test.ts there as "Add API tests". Leave my docs/api.md edit uncommitted, leave `feature-api` itself unchanged, and end up on `feature-api-tests`. Do not publish anything.

## heldout-10-amend-elsewhere

> I'm on `feature-ui` with unfinished work in src/ui.ts. My uncommitted change to src/config.ts (the port) is not UI work: it belongs inside the existing commit "Add config loader" on branch `feature-config`, which is not checked out. Fold it into that commit, keeping that commit's message, without leaving `feature-ui` and keeping my src/ui.ts edit uncommitted. Do not publish anything.

## heldout-11-commit-files

> Which files did the commit "Refactor upload client" on `main` change? Reply with just those paths, one per line. Do not change anything in the repository.

## heldout-12-two-branches

> I'm on `feature-ui` with three uncommitted changes. Keep my src/ui.ts edit uncommitted. Commit my src/parser.ts change here on `feature-ui` as "Fix parser crash". Commit my new file docs/ui.md on a new branch `docs-ui` based on `main` as "Document UI". Stay on `feature-ui` and do not publish anything.
