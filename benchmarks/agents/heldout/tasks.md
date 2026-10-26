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

# Held-out set 3

Six more tasks, written on 2026-10-27 after the fixes measured on sets 1 and 2 (directory selectors, `commit --onto BRANCH`, the cross-branch `amend`, `show COMMIT:PATH`) and **frozen (committed and pushed) before any agent ran them**. Fixtures, verifiers and reference solutions for Git, GitButler and AISC are in `benchmarks/vcbench/aisc-arm.patch` (`scripts/lib/scenario-heldout13.mjs` to `scenario-heldout18.mjs`); all 18 references pass their verifiers, and doing nothing fails every task.

Known before running: AISC has no verb that lists the branches containing a commit (heldout-17; read-only `git branch --contains` is allowed in every arm), and no one-call way to move commits to a *new* branch (heldout-15: its reference is `pick -b NEW --onto main` then `drop`, two calls).

## heldout-13-validate-form

> On `feature-forms`, I changed src/form.ts in two places: an input validation (it throws "empty form") and a debug console.log in send(). Commit only the validation change together with my new tests/form.test.ts as "Validate form input". Leave the console.log change and my README.md edit uncommitted. Stay on `feature-forms` and do not publish anything.

## heldout-14-split-by-directory

> On `feature-search`, the commit "Add search" mixes code and docs. Split it into two commits: first "Add search" with everything under src/, then "Document search" with everything under docs/, in that order, with the same final content. Keep my README.md edit uncommitted, stay on `feature-search`, and do not publish anything.

## heldout-15-move-to-new-branch

> The commits "Add export" and "Add export tests" on `feature-search` are a separate piece of work. Move them, in that order, to a new branch `feature-export` based directly on `main`, and remove them from `feature-search`, which keeps "Add search". Stay on `feature-search`, keep my README.md edit uncommitted, and do not publish anything.

## heldout-16-reorder-and-reword

> On `feature-cache`, the commit "Add cahce layer" has a typo in its message (it should read "Add cache layer") and it should come first, before "Add cache config". Fix the message and the order; keep every commit's content and "Add cache tests" on top. Leave my README.md edit uncommitted, stay on `feature-cache`, and do not publish anything.

## heldout-17-branches-with-commit

> Which local branches contain the commit "Add retry"? Reply with just the branch names, one per line. Do not change anything in the repository.

## heldout-18-discard-one-hunk

> In src/app.ts I made two uncommitted changes: the service name is now "service-v2" (keep that) and I added a console.log line (throw that away). Discard only the console.log change. Keep my uncommitted edit to src/other.ts as it is, do not commit anything, and do not publish anything.

