# The 13 tasks

Verbatim from [GitButler's version-control-bench](https://github.com/gitbutlerapp/version-control-bench) at commit `4886fd5` (`tasks/<id>/instruction.md`), MIT License, Copyright (c) 2026 GitButler. Every tool got exactly the same text.

Each instruction is wrapped by the harness (`buildPrompt` in `scripts/run-pilot-agent.mjs`) like this, identically for every tool:

```text
You are running inside a version-control benchmark sandbox.
All requested file changes already exist. Perform the requested review or version-control task; do not implement new application code.
Work only in the current repository and its configured local remotes. Do not inspect parent benchmark directories, hidden oracle files, reference solutions, or the research packet. Temporary comparison files outside the repository are allowed.

Task: <instruction below>

When the requested result is complete, return any requested answer and stop.
```

## pilot-1-selective-validation

> Commit just the input validation work on a new branch. Leave the logging/config cleanup and debug notes uncommitted.

## pilot-2-multi-amend

> Amend the existing five-commit `amend-series` branch. Do not create a new commit.
> 
> Route the already-present dirty changes like this:
> 
> - Amend the validation helper changes in `src/lead.ts`, the malformed-email test in `tests/lead.test.ts`, and the validation wording in `README.md` into commit `refactor validation helpers`.
> - Amend the scoring changes in `src/lead.ts` and the enterprise-domain scoring test in `tests/lead.test.ts` into commit `add lead scoring`.
> - Amend the response-behavior documentation changes in `README.md` and `docs/response.md` into commit `document response behavior`.
> 
> Leave the config logging change, the debug lead summary helper, and the investigation notes uncommitted.

## pilot-3-split-commit

> Split the non-top commit `add lead workflow` on the existing `split-workflow` branch. Do not keep the original broad commit.
> 
> Replace it with these three commits, in this order, below the existing top commit `add handler routing metadata`:
> 
> - `refactor validation helpers`: the validation helper changes in `src/lead.ts` and the malformed-email test in `tests/lead.test.ts`.
> - `tune lead scoring`: the enterprise-domain scoring changes in `src/lead.ts` and the enterprise-domain scoring test in `tests/lead.test.ts`.
> - `document lead workflow`: the workflow documentation changes in `README.md` and `docs/lead-workflow.md`.
> 
> Keep `add handler routing metadata` as the top commit after the split.
> 
> Leave the config logging change, the debug lead summary helper, and the investigation notes uncommitted.

## pilot-4-reorder-commits

> Reorder the existing commits on the `reorder-series` branch. Move the adjacent delivery-related block (`add retry policy` and `add notification sender`) earlier in the branch. Do not change any file contents and do not create functional changes.
> 
> Final commit order must be exactly this, oldest to newest:
> 
> 1. `add app configuration`
> 2. `add retry policy`
> 3. `add notification sender`
> 4. `add customer model`
> 5. `add email formatter`
> 6. `document notification flow`
> 
> The commit messages and each commit's content should stay attached to the same subject. Leave the worktree clean.

## pilot-5-squash-commits

> Squash commit groups on the existing `squash-series` branch. Do not change any file contents and do not create functional changes.
> 
> Keep these commits as separate commits:
> 
> - `add parser token model`
> - `add export endpoint`
> 
> Squash these adjacent commit groups:
> 
> - Squash `extract parser helpers` and `wire parser helpers` into one commit named `add parser pipeline`.
> - Squash `add retry option`, `test retry option`, and `document retry option` into one commit named `add retry support`.
> 
> The final branch order, oldest to newest, should be:
> 
> 1. `add parser token model`
> 2. `add parser pipeline`
> 3. `add export endpoint`
> 4. `add retry support`
> 
> Leave the worktree clean.

## pilot-6-update-dirty-branch

> The `main` branch has moved ahead while work on `notify-retry` was in progress. Bring `notify-retry` up to date: rebuild it on top of the current `main` with linear history, keeping its two commits and their messages intact. Do not modify `main` itself.
> 
> Two incoming changes clash with this branch's work:
> 
> - `src/notify.ts`: keep both notification channels — the `sms` channel added on this branch and the `push` channel added on `main`.
> - `src/config.ts`: this branch deliberately tuned the retry limit to 4; keep that over the value `main` bumped it to.
> 
> The worktree also has uncommitted work in progress: a README edit and an untracked rollout checklist. Both must survive the update exactly as they are and stay uncommitted. Leave no conflict markers, unmerged files, or half-finished operations behind.

## pilot-7-scoped-review

> Review `seed-rewrite` only, compared with the branch it is stacked on. Did its conversion from the array in `data/questions.json` to the keyed object in `data/questions-by-id.json` preserve every record's question, answer and score? Ignore record/key order.
> 
> Return JSON with `equivalent`, `missingIds`, `extraIds` and `changes`; each change has `id`, `field`, `before`, `after` and `path`. The `path` is the repository-relative filename, without a record or field suffix; identify those separately in `id` and `field`. Use empty arrays when there are no corresponding differences.
> 
> Do not change project files or branches, or save new user commits. Return the answer in the response; any temporary comparison files must be outside the repository. Automatic working-copy snapshots are fine.

## pilot-8-cold-checkpoint

> Commit the app as it stands as one commit on a new local branch called `app-checkpoint`. This project already has ordinary Git history; keep that history and the existing ignore rules, and don't commit ignored files such as local configuration or build output. Set up whatever your tool needs to work in this repository as part of the job. Do not publish anything.

## pilot-9-cleanup-noise

> I ran the formatter while validating my retry fix on `fix-retry`, and it reformatted some files that have nothing to do with the fix. Get rid of those formatting-only changes so they don't end up in the commit. Keep my `src/retry.ts` edit exactly as it is; it's still unfinished, so don't commit it.
> 
> Don't create, rewrite or move any commits, and leave `main` and `fix-retry` where they are. When you're done I want to be in the same working context, with the formatting noise gone from anything that would go into the next commit and nothing else changed. Do not publish.

## pilot-10-recover-amend

> My last amendment to `review-fix` pulled `notes/private.md` into the review commit by accident; that file is a private draft and must not be in saved history. The two reviewed fixes in `src/retry.ts` and `src/log.ts` belong in that commit and should stay there, with the commit's original message and author. `PRIOR_OPERATION.md` shows what I ran.
> 
> Get the draft back out of the commit, but keep it on disk exactly as it is now as plain uncommitted work: not committed anywhere, not stashed, and not lined up for a later commit. Fix the commit itself rather than adding a corrective commit on top. Leave `main` and `other-topic` as they are, and don't publish anything.

## pilot-11-dual-source-sync

> Another agent pushed an environment change to `origin/feature-service` while I was working, and `origin/main` has moved on too. Pull both down and reconcile my local `feature-service` with them. It has an unpublished retry commit that must survive.
> 
> When you're done, the branch should be based on the current main tip and contain the shared feature change, the other agent's environment change and my retry change as separate commits with their original messages and authors; don't squash them or retype them under your name. Merge or rebase, whichever you prefer.
> 
> Keep `other-topic` as its own branch with its commit intact; rebasing it onto the new main is fine. Apart from the reconciliation itself, leave the rest of my working copy as you found it: anything that was there when you started should still be there when you're done. That includes my unfinished edit in `README.md`: leave it exactly as it is, uncommitted and not staged. Local `main` can stay where it is or move to the fetched main.
> 
> Do not push, publish or change anything on origin itself. Leave me on the reconciled feature branch with no stashes, conflicts or half-finished operations.

## pilot-12-integrate-topics

> Integrate the finished `normalize` → `greeting` stack and the separate `docs` branch into this repository's local `main`. All three are approved. Keep them as separate commits with their original authors and messages (merge or rebase, but don't squash), and keep normalize before greeting since greeting depends on it.
> 
> When you're done I want a clean checkout of the integrated main with nothing left over: no unintegrated work from those branches, no dirty or staged files, and no stashes or half-finished operations. Whether the source branches are kept or deleted is up to you. Do not push anywhere or open a PR.

## pilot-13-selective-copy

> Have a look at `revive-worktree-command`. It revives the worktree command, but the branch also carries an earlier graph refactor that I'd rather not touch in this change. Copy just the worktree revival onto a new branch called `revive-worktree-no-graph`, based directly on `main`, so the graph code stays as it is on main. The command tests should pass on the new branch.
> 
> Leave the source branch and its commits exactly as they are; this is a copy, not a move. Don't touch `main` or `ui-note`, and leave my uncommitted `scratch-note.txt` alone: don't commit, stage or stash it. Do not publish anything.

Fixtures, verifiers and reference solutions for each task are in the upstream repository under `tasks/<id>/` and `scripts/`; the verifier grades the Git-visible end state (commits, trees, refs, uncommitted files) and the harness's command policy marks any blocked command as a failed run.
