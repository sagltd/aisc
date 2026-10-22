# AISC history verbs — design spec (H0)

Status: design for slices 0-5. Base: `dev` with PR #5 merged first; it provides `context.rs`, `validate_environment` and `pub(crate) run_git`, plus the racy-index mtime fix that every alternate-index write here must keep (copy the index, then `set_modified(original mtime)`).

## 1. What vcbench actually measures (findings)

Source: github.com/gitbutlerapp/version-control-bench (MIT), read through `gh api`.

**Metric mechanics.**
- `scripts/lib/command-guards.mjs` puts wrapper binaries for `git`, `but` and `jj` on PATH. Every call is logged with argv, duration and stdout/stderr bytes.
- A git call counts as `internal` when its parent or grandparent process name is the `but` or `jj` binary. Internal calls are allowed and not counted as agent commands.
- `commands` is the number of non-internal git, but and jj invocations, reads included. File edits are not commands. Transcript KB counts all visible tool output.
- So every read we save (for example `git status` or `git diff`) counts.

**Tool policy** (`scripts/lib/tool-policy.mjs`):
- In the non-git arms, raw git is blocked unless `classifyCommand` says it is an inspection. `but` and `jj` are blocked outside their own arm.
- Read-only git is still counted as a command.

**Arm setup** (`run-pilot-agent.mjs`):
- Arms are `git`, `but+skill` and `jj+skill`. For `but`, the harness runs `but setup` (and `but apply BRANCH`), installs the skill into `.codex/skills` and `.claude/skills`, writes AGENTS.md/CLAUDE.md, then applies the dirty state. None of this is measured.
- For the git arm, the fixture is created dirty directly.

**Verifiers check only Git-visible final state.** Common checks: protected refs unchanged, exact subject order, per-commit snippets and paths, leftover snippets in the worktree and not in history, untracked files still untracked, `git diff HEAD -U0` free of target snippets, no `rebase-*`/`MERGE_HEAD`/`CHERRY_PICK_HEAD`, no `refs/stash`, no unmerged entries.

| # | Task | Facts that drive the design | `but` reference (count) | `git` reference |
|---|---|---|---|---|
| 1 | selective commit | A new branch must exist and be checked out (HEAD diff must not contain target snippets). `src/handler.ts` has a validation hunk and a separate logging hunk (`logLeadCreation`); the verifier requires validation committed and logging left out, so **hunk selection is required**. Targets: README, handler, test. Leftovers: config, logging hunk, untracked `notes/debug-log.md`. | `status -fv`, `commit -b input-validation -m .. ID ID ID` (2; the script fakes the hunk split by writing target-only files first) | switch -c, add, commit (3, same fake) |
| 2 | multi-amend | 5-commit `amend-series`. Dirty hunks go to commits 1, 3 and 5 (5 is the top). `src/lead.ts` has 3 hunks under 3-line context (validation, scoring, debug), confirmed by diffing the fixture in memory. The validation hunk's context lines come from commit 3 (`scoreLead`), so it has to be placed into commit 1 without that context. No new commits. | status/diff/amend ×3 groups (13 scripted; about 5 for a real agent) | scripted rewrite |
| 3 | split non-top | The broad commit `add lead workflow` contains validation, scoring, docs **and** leftovers (config logLevel, `debugLeadSummary`, new `notes/investigation.md`). It must become 3 ordered commits under the kept top commit, with leftovers uncommitted and the notes file untracked. | uncommit, then 3×(diff, commit, status, move) (20 scripted) | scripted |
| 4 | reorder | A B C D E F becomes A D E B C F. Disjoint files; final tree identical. | 3 status + `move --below` (4) | scripted |
| 5 | squash groups | A [B+C] D [E+F+G]. Final tree identical; old subjects unreachable. | 5 status + 2 squash (7) | scripted |
| 6 | dirty sync | main +2. Each branch commit conflicts in a different file (`notify.ts` needs both channels combined by hand; `config.ts` keeps the branch value 4). The README edit and untracked checklist must survive uncommitted. | pull, status, resolve, edit, finish ×2 (7) | stash, rebase, add, continue ×2, pop (7) |
| 7 | scoped review | Read-only answer. | `show seed-rewrite` (1) | — |
| 8 | cold checkpoint | Commit everything (respect ignores) on new branch `app-checkpoint`; tool adoption counts as task work. | setup, commit -b (2) | — |
| 9 | cleanup noise | Discard formatter-only changes in 3 files (worktree and index back to HEAD); keep the `src/retry.ts` edit uncommitted; refs untouched. | status, discard (2) | restore --staged --worktree (1) |
| 10 | recover amend | Remove `notes/private.md` from the top commit (byte-exact message and author), keep it on disk untracked. | status, uncommit (2) | rm --cached, commit --amend (2) |
| 11 | dual-source sync | Fetch, rebase onto origin/main, bring in the other agent's commit, keep the README edit unstaged. | pull, pick (2) | fetch, rebase --autostash, cherry-pick (3) |
| 12 | integrate topics | Land the normalize→greeting stack and docs into local main, checked out and clean. | land ×2 (2) | switch, merge ×2 (3) |
| 13 | selective copy | Cherry-pick one commit onto a new branch from main; HEAD and the scratch file untouched. | pick (1) | switch -c, cherry-pick (2) |

Published means: git 92.2 s / 20.4 commands / 31.7 KB, jj 110 s / 21.0 / 46.1 KB, but 34.0 s / 4.5 / 13.3 KB. The `but` advantage comes from one-call history verbs.

## 2. Principles

1. **One observation, one mutation.** `aisc log` is the only read a task needs. Every mutation prints the resulting log, so the agent does not need to verify with another call.
2. **Agents can name things they already know.** Commits can be referenced by exact subject; hunks by content (`PATH~TEXT`). Several tasks can then be done in one call with no observation.
3. **Plumbing only, single checkout.** Use `merge-tree --write-tree`, `merge-file --object-id`, `commit-tree`, temporary index plus `write-tree`, and `update-ref --stdin` with old-value checks (CAS). Never use rebase, stash or a sequencer, never add a worktree, never leave half-finished state. History verbs work on other branches without checking them out (`--branch`).
4. **All-or-nothing per call.** Compute everything (objects, worktree merges) first. Then do one ref transaction, then the index, then worktree files. Any precondition failure leaves refs, index and worktree byte-identical.
5. **Uncommitted delta is preserved.** Selection is always worktree versus HEAD. The index is preserved but never used for selection (the GitButler model).
6. **Undo everything** through an oplog that keeps old and new commits reachable.

## 3. Concepts

### 3.1 Branch view (`view.rs`)
- **Target branch B**: HEAD's branch, detached HEAD, or `--branch B` (must not be checked out in any worktree).
- **Base**: the default branch — `refs/remotes/origin/HEAD` target if present, else the first existing local `main`, `master` or `trunk`. If B is the base's local branch, there is no base.
- **Upstream** (`@{u}`) is shown separately and is never used as the base.
- **Range**: `git log --first-parent B --not BASE` (with no base: the last `--limit` commits).
- **Mutations** require the touched part of the range to be linear (one parent each). They refuse commits reachable from the base (published history): "error: … is on main".
- **Reads** (parallelize independent reads with `std::thread::scope`, as in context.rs):
  1. `for-each-ref` (locals, upstreams, origin/HEAD)
  2. `rev-list --left-right --count BASE...B`
  3. one `git log` for the range with `%H %h(abbrev=4) %T %P subject` and `--name-only`
  4. `status --porcelain=v2 --branch -z --untracked-files=all`
  5. `git diff HEAD` with the flags in 3.2
  That is 5 processes, about 15 ms.

### 3.2 Changes and hunks (`diff.rs`, `model.rs`)
- **Diff command** (one constant, used by both log and mutations):
  `git -c core.quotePath=true diff HEAD --no-ext-diff --no-textconv --no-color --no-renames --full-index -U3 --diff-algorithm=myers --indent-heuristic --src-prefix=a/ --dst-prefix=b/`
- **Commit diffs** use `git diff-tree -p` with the same flags (`--root` for root commits).
- **Parsing**: parse headers and quoted paths, `new file`/`deleted file`/mode lines, `Binary files` lines and `\ No newline` markers. Each `@@` hunk becomes **blocks** (runs of -/+ lines). Each block keeps `old_start`, `new_start`, `removed`, `added`, and up to 3 context lines before and after.
- **Whole-file items** have one pseudo-hunk: untracked (from status), added, deleted, binary, symlink, mode-only, type change. Submodules are refused.
- **Hunk id**:
  - `sha256("aisc-hunk-v1\n" + path + "\n" + Σ("-"+removed+"+"+added+"\n") + "#" + ordinal)`, where `ordinal` counts earlier hunks in the same file with identical content.
  - Each nibble n maps to the letter `'g'+n`, so ids use only the letters g–v and can never be confused with hex commit ids.
  - Display the shortest unique prefix, at least 3 letters.
  - Ids depend only on content. They stay stable across calls and across mutations that do not change that hunk. A stale or ambiguous id fails with "run aisc log".
- **Hint**: the first added line with at least 3 alphanumeric characters, else the first removed line prefixed `- `. Trimmed, cut at 72 characters with `…`, control characters escaped.
- **`ws` flag**: concat(removed) and concat(added) are equal once all ASCII whitespace is removed. A file is whitespace-only when that holds for every hunk.
- **Exactness gate** (for every mutation that partially selects a file): splicing all blocks onto the old blob must equal the new bytes (the worktree file or the commit blob). Otherwise fail with `unsupported`: "PATH: Git filters/eol conversion; select the whole file". Whole-file selections from the worktree use `git hash-object -w -- PATH…` (filters applied, like `git add`). Spliced content uses `--no-filters`.

### 3.3 Selectors (`selector.rs`)
Selectors are resolved against a changeset (uncommitted, or one commit's diff for split and uncommit):
1. `PATH:L-R` or `PATH:L`: new-side line numbers only. A block is selected when its added lines intersect L..R; a pure deletion is selected when the new-side line it falls at (the line after the deleted lines) lies within L..R+1. Old-side numbers never count, so a range never picks up a deletion recorded elsewhere.
2. `PATH~TEXT` (split at the first `~` whose prefix is a changed path): hunks whose changed lines contain TEXT (case-sensitive). Zero matches is an error.
3. An exact changed path: every hunk of the file. For text files this means all hunks; added, deleted, untracked and binary files are whole-file.
4. A token matching `^[g-v]{3,16}$`: a unique hunk-id prefix.
5. Anything else: `error: unknown selector 'X' (paths: …)`.

Groups: one hunk or block may belong to only one `--into` group (`error: hunk gnh selected twice`). `--all` means every change, including untracked files that are not ignored.

**Commit refs (`C`):**
1. Hex of at least 4 characters matching a commit in the range.
2. Else an exact full subject that is unique in the range (or in the `--from` range for pick). Ambiguity lists candidates.
3. Else `rev-parse --verify X^{commit}` (branch names such as `review-fix` give the tip). Mutations still require the result to be in range.

`A..B` means an inclusive span in range order (squash and move), or a git range (pick). Subjects with commas are fine because groups use `--into`.

## 4. Verb contract

`MutationOpts` (flattened into every mutating verb): `--branch B` (rewrite B without checkout; worktree untouched) and `-q/--quiet` (result line only). All mutating verbs take the operation lock and refuse while a sync is paused (except `sync --continue` and `sync --abort`). Unaffected older commits keep their ids: a commit with the same parent, tree and metadata is reused.

| Verb | Semantics |
|---|---|
| `log [--branch B] [--limit N=20] [--no-hunks]` | Branch view plus uncommitted changes (3.2) and the paused-sync line. Read-only. |
| `show [REV\|A..B] [SEL…] [--full] [--branch B]` | Hunks of a commit, range or the uncommitted changes. By default only changed lines, at most 12 per hunk, then `… +k`. `--full` adds 3 lines of context and no cap. |
| `commit -m M [-b NEW] (SEL…\|--all)` / `commit --into M SEL… [--into M SEL…]` | New commit(s) on HEAD: tree_k = HEAD + groups 1..k, spliced exactly. `-b` creates NEW at the result (main untouched) and points HEAD at it. No hooks (D3). |
| `amend --into C SEL… [--into C SEL…]…` | Moves uncommitted hunks into existing commits (algorithm 6.4). No new commits; worktree unchanged. |
| `discard (SEL…\|--whitespace-only)` | Reverts the selected uncommitted changes in the worktree; untracked selections are deleted. Index: whole-file selection resets to the HEAD entry; partial selection with index == W gets the new W; index == T is kept; anything else is reverse-placed or refused. Old bytes are saved in the op so undo restores them exactly. |
| `split C --into M SEL… [--into M SEL…]… [--rest uncommit\|keep]` | Algorithm 6.5. Default rest is `uncommit` (leftovers move to the worktree). Parts keep C's author. |
| `uncommit C SEL…` | Equivalent to `split C --into <C's raw message> (rest of C)` with rest SEL uncommitted. If nothing remains, C is dropped. Author and message byte-exact. |
| `squash --into M C C… [--into M C C…]…` | Each group needs at least 2 commits. Adjacent groups need no merges: the result tree is the newest member's tree, later commits are re-parented, and the author is the oldest member's. `M=""` joins the member messages. Non-adjacent groups are an error in v1 ("move them together first"); S2 may implement them by replay. |
| `move C… (--before C\|--after C)` / `move --order C…` | Moved commits keep their original relative order. `--order` must list a full permutation of the range, oldest first. Replays from the first changed position with merge-tree. A conflict aborts with `commit X: P conflicts`; no paused state. |
| `pick C… [--to B \| -b NEW [--onto BASE]] [--from B]` | Replays commits (author and message kept) onto the current branch (worktree carried), onto B (not checked out), or onto NEW created at BASE (default: the base) without touching HEAD. Conflicts abort. |
| `sync [ONTO] [--prefer-branch P…] [--prefer-onto P…] [--fetch]` / `sync --continue` / `sync --abort` | Algorithm 6.6. ONTO defaults to the base. `--fetch` runs `git fetch <remote of ONTO>`. Commits already contained upstream are dropped and reported. |
| `land B… [--into T=base] [--switch]` | For each B in order: `B --not T` replayed onto T (fast-forward when possible), T updated by CAS. If T is checked out here, the worktree is carried; if checked out elsewhere, refused. `--switch` moves HEAD to T, carrying the dirty edits. |
| `switch B [-c]` | Carry worktree T→T' (3-way per path, refuse on conflict), apply the index rule, `symbolic-ref`. Recorded in the oplog. |
| `undo [OP]` / `oplog [--limit 10]` | Section 6.8. |
| `gc [--apply] [--min-age HOURS=24]`, `start NAME [--path P [--sparse DIR…]]`, `finish [--keep]` | Section 7. |

## 5. Output

**Default format.** New verbs print compact plain text even when stdout is redirected; `--json` (or `AISC_OUTPUT=json`) gives JSON (D1). Existing verbs keep their current behavior. Implemented by `Operation::prefers_text()` and `Presentation::for_operation()`. No color in compact text.

### 5.1 `log` grammar (golden-tested)
```
HEADER  := B ": " n " commits on " BASE [", " BASE " +" m " ahead (aisc sync)"] [", ↑" UP " +a/-b"] " (newest first)"
           | B ": last " n " commits (newest first)"        # no base; "HEAD (detached)" for detached
COMMIT  := ID " " SUBJECT [" | " up to 4 paths [" +" k]]
"uncommitted:" then per file (sorted):
  single-hunk file:  PATH " " HID " @" L " +" a ["-" r] " " HINT [" [ws]"]
  multi-hunk file:   PATH [" [ws]"] NEWLINE ( " " HID " @" L " +" a ["-" r] " " HINT [" [ws]"] )*
  whole-file item:   PATH " (" untracked|new|deleted|binary|mode ")" [" +" lines]
cap: 200 hunk lines, then "… N more hunks (aisc show)"
STATE   := "sync paused: " k " conflicts (aisc sync --continue | --abort)"
clean worktree: no section
```
Example (pilot 2, about 1.3 KB):
```
amend-series: 5 commits on main (newest first)
e41b document response behavior | README.md docs/response.md
9c07 add API handler | src/handler.ts tests/handler.test.ts
3a5d add lead scoring | README.md src/lead.ts tests/lead.test.ts
77f0 add app configuration | src/config.ts
b2c9 refactor validation helpers | README.md src/lead.ts tests/lead.test.ts
uncommitted:
README.md
 kqv @7 +1-1 - Requires a contact name and a likely email address before a lead can be accepted.
 pwx @19 +1 - Rejected responses include a reason that can be shown to operators.
docs/response.md tmr @4 +1 Invalid leads return `accepted: false` with a stable rejection reason.
src/config.ts hjn @4 +2-1 logLevel: "debug" | "info" | "warn";
src/lead.ts
 gnh @21 +6-1 export function isLikelyEmail(email: string): boolean {
 sjq @38 +5-1 if (email.endsWith("@enterprise.example")) {
 vrm @53 +5-1 export function debugLeadSummary(payload: LeadPayload): string {
tests/lead.test.ts
 mpq @19 +3-3 export function testIsValidLeadRejectsMalformedEmail(): void {
 trv @36 +12 export function testScoreLeadRewardsEnterpriseDomain(): void {
notes/investigation.md (untracked) +4
```

### 5.2 Mutation output
One result line, then the compact log. The compact log lists uncommitted changes as a single line, `uncommitted: P P …`, with no hunks. Result lines:
- `committed 5c2e "Add input validation" on new branch input-validation (3 files); op 1a7f`
- `amended b2c9 +3 hunks, 3a5d +2, e41b +2; rewrote 5; op 1a7f`
- `split 4c1d into 3; rest uncommitted: src/config.ts src/lead.ts notes/investigation.md; op …`
- `squashed 2 groups (7→4 commits); op …`
- `moved 2; op …`
- `picked 1 onto revive-worktree-no-graph (new, from main); op …`
- `synced notify-retry onto main a91c (2 commits, 0 dropped); op …`
- `discarded 3 files; op …`
- `undid amend 1a7f`, then one `<branch> <id7> <subject>` line per restored tip (`<branch> deleted` for a removed one). Undo prints nothing else: the rest is the state an earlier command reported, and a redo is `aisc undo` again (the new op id is in JSON and `aisc oplog`). `--stack` prints the compact log.

Dependent branch moves are listed as `moved branch stacked: 3e1a→9b0c`.

### 5.3 Paused sync (exit 0, D2)
```
CONFLICTS: sync notify-retry onto main a91c paused, nothing committed. Edit the marked files, then: aisc sync --continue (or --abort)
src/notify.ts in 4b2e "Add sms channel"
<<<<<<< main
export const activeChannels: Channel[] = ["email", "push"];
||||||| base
export const activeChannels: Channel[] = ["email"];
=======
export const activeChannels: Channel[] = ["email", "sms"];
>>>>>>> 4b2e Add sms channel
```
Only the conflict regions are printed, with at most 3 lines of context between regions.

### 5.4 JSON
- **log**: `{"branch","head","base":{"ref","oid","ahead","behind"}|null,"upstream":…|null,"commits":[{"id","oid","subject","files":[…],"files_omitted"}],"changes":[{"path","status":"M|A|D|?|B|T","ws",hunks:[{"id","line","added","removed","hint","ws"}]}],"omitted_hunks","state":null|{"sync":{"conflicts":k}}}`
- **mutation**: `{"op","verb","branch","old","new","rewritten":[{"old","new","subject"}],"moved_refs":[{"ref","old","new"}],"head_changed_to"?,"uncommitted":[paths],"log":{…}}`
- **paused sync**: `{"verb":"sync","status":"conflicts","onto","conflicts":[{"commit","subject","path","file","regions":[{"line","onto":[…],"base":[…],"branch":[…]}]}],"next":["aisc sync --continue","aisc sync --abort"]}`
- **errors**: `{"error":msg,"code":"usage|selector|stale|dirty|conflict|unsupported|state|git","hint"?}`

Human error output: `error: MSG` and optionally `hint: …`.

### 5.5 Exit codes
- 0: success, including a paused sync.
- 1: error; nothing changed.
- 2: usage (clap).
- 4: stale. Refs, index or HEAD changed concurrently or CAS failed; nothing changed; re-run `aisc log`.

## 6. Engine (slice 0 unless noted)

### 6.1 process.rs
- Add `execute_input(command, stdin: Vec<u8>, timeout, limit)`; a writer thread feeds stdin.
- Add `Batch` for a long-lived `git cat-file --batch`: write all requests from a thread, parse responses, stop on a deadline by killing the process group.

### 6.2 plumbing.rs
- `read_blobs(&[oid])`: one cat-file batch process.
- `read_commit_meta(oid)`: parse the raw `cat-file commit` for the author line, date and tz, and the raw message. Refuse an `encoding` header; drop `gpgsig`.
- `write_blob(bytes, no_filters)` and `hash_worktree(paths)`.
- `tree_with(base_tree, edits)`:
  - Uses a temporary index at `$GIT_DIR/aisc/tmp/index-<pid>-<n>` (removed on drop).
  - Runs `read-tree`, then `update-index --index-info` (deletions use mode 0), then `write-tree`.
  - The API is fixed so a later `mktree --batch` optimization does not change callers.
- `commit_tree(tree, parents, meta)`:
  - Runs `git commit-tree TREE -p … -F -` with the raw message on stdin.
  - Passes `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL` and `GIT_AUTHOR_DATE='@<unix> <tz>'`; the committer is the current identity.
  - `commit.gpgSign` is honored.
- `merge_trees(base, ours, theirs) -> {tree, conflicts:[{path, stages[1..3]}], messages}`:
  - Runs `merge-tree --write-tree -z --merge-base=BASE OURS THEIRS`.
  - Exit 1 means conflicts; any other non-zero exit is an error.
- `merge_file(base, ours, theirs, labels, favor: None|Ours|Theirs|Union, diff3)`: `git merge-file --object-id -p [--diff3] -L …`.
- `RefTx`: `update-ref --stdin -m "aisc VERB"` with `start / update R NEW OLD / create R NEW / delete R OLD / prepare / commit`. Errors containing "cannot lock ref" or "but expected" map to `Stale`.
- `require_git(2,43)`: lazy and cached; used by merge verbs.

### 6.3 Transplant (`apply.rs`; pure Rust, fully unit-tested)
- `splice(old, blocks)` and `unsplice(new, blocks)`: exact operations at the recorded positions. They assert that the removed (or added) lines match.
- `transplant(target, blocks, Forward|Reverse)`. For each block, located on the **original** target:
  - Candidates: every index i where `target[i..i+|R|] == R`, or every insertion point when R is empty.
  - Score: consecutive context matches outward on each side, plus 1 for a file-start or EOF anchor when the original block sat there.
  - A candidate is valid only if it is *significant*: R or some matched context line has at least 3 alphanumeric characters, or it uses an anchor. Pure insertions also need a score of at least 1.
  - The unique maximum wins. Ties give `Ambiguous`; no candidates give `Missing`.
  - Located regions must not overlap. Apply from the bottom up; insertions at the same index keep their original order.
- **Fast path**: if the target blob equals the hunk's base blob, use `splice`.
- **Invariant**: whatever happens in the middle of the branch, the tip is always exact by construction.

### 6.4 Amend algorithm (S1)
For commits j from the oldest target to the tip, and each path P with routed hunks targeting at or before j:
- **Text hunks**: transplant onto `C_j[P]`. If P is absent there: error "P is added by X".
- **Untracked or added file**: P must be absent in `C_j`; add it with the worktree mode.
- **Deleted file**: `C_j[P]` must equal `HEAD[P]`; delete it.
- **Binary**: `C_j[P]` must equal `HEAD[P]`; replace it.
- Memoize results by (blob, hunk set).
- On failure at commit k: find the smallest j > k where the transplant succeeds and report `hunk gnh (src/lead.ts) edits lines from 3a5d "add lead scoring"; target 3a5d or later`. Nothing changes.
- New trees are built with `tree_with`; commits keep their metadata; worktree policy is Keep.

### 6.5 Split algorithm (S1)
- D = diff(X^, X). X_i tree = X^ + G1..Gi, spliced exactly. Rest R = D minus all groups.
- **`--rest uncommit`**: every descendant becomes D_j − R.
  - Fast path: if `D_j[P] == X[P]`, use the `X_n[P]` blob.
  - Otherwise reverse-transplant R.
  - The tip loses R and the worktree is unchanged, so R becomes uncommitted. Untracked-added R files disappear from the index (by the index rule) and become untracked.
- **`--rest keep`**: append X_{n+1} with tree X and the original message; descendants are re-parented only.
- **Errors**: an empty group, overlapping groups, or a no-op split.

### 6.6 Sync algorithm (S3)
- **Replay**: for each commit c (oldest first), compute `merge_trees(c^, newparent, c)`.
  - On conflict in P: if a stored resolution exists for (c, P), use its blob. Else if `--prefer-*` matches P, use `merge_file --theirs` (branch) or `--ours` (onto).
  - Otherwise record a pending conflict and use the marker blob as a provisional result so later commits keep replaying.
  - Conflicts in a later commit on a path that is already pending are deferred and reported as "more conflicts may follow".
  - An empty result where the original commit was non-empty is dropped.
- **Pending conflicts**:
  - Write a diff3 marker file (from `merge_file`, labels `ONTO`, `base`, `<id> <subject>`) to P if `W[P] == T[P]`, else to `P.aisc-conflict`.
  - Save `$GIT_DIR/aisc/sync.json`: `{branch, old_tip, onto_spec, onto_oid, commits, resolutions:{commit:{path:blob}}, prefer, materialized:[{path, file, commit, original_blob}]}`.
  - Refs are untouched. Exit 0 with status "conflicts".
- **`--continue`**:
  - Each materialized file must contain no line starting with `<<<<<<< `, `=======` or `>>>>>>> `; otherwise error with code `conflict`.
  - Hash the files into resolutions, then replay from scratch with all resolutions. New conflicts pause again.
  - On success: worktree policy is Carry, except materialized paths, which are set to `T'[P]`. `P.aisc-conflict` files are deleted. CAS checks against `old_tip` (stale gives exit 4).
- **`--abort`**: restore materialized paths to their original blobs, delete side files and state.
- **`--branch B` (not checked out)**: conflicts abort with a report; no paused state.

### 6.7 Rewrite plan executor (`rewrite.rs`)
```rust
Plan {
    verb, summary, argv,
    target: RefMove { refname, old, new },   // refs/heads/x or HEAD (no-deref)
    extra: Vec<RefMove>,                     // created branches, dependent branches
    head: Option<(from_ref, to_ref)>,
    rewritten: Vec<(old, new)>,
    worktree: Untouched | Keep | Carry | Explicit(Vec<Write>),
    expect_tip_tree: Option<oid>,
}
```
Steps:
1. **Session open**: acquire `aisc-operation.lock`, then `index.lock` if the index will change. Snapshot HEAD (symbolic and oid), the branch oid, the index bytes and mtime, and the status; require `writable()`.
2. **Dependent refs**: local branches whose tip is a rewritten old oid move to the new oid (D5). If such a branch is checked out in another worktree, refuse.
3. **Worktree**:
   - *Carry*: for each path where T ≠ T':
     - `W == T`: write `T'`.
     - `W == T'`: skip.
     - Otherwise run `merge_file(T, W, T')`; a conflict, or an untracked or ignored collision, aborts with code `dirty`.
     - Skip-worktree entries get an index update only, no file write.
   - *Keep*: no writes. Asserted by construction for commit, amend, split, uncommit and squash.
4. **Index**: for each path where T ≠ T': an entry equal to `T[P]` becomes `T'[P]`; anything else is kept.
   - Build on a copy that keeps the original mtime (PR 5 rule).
   - Check the real index is still byte-identical, then install by renaming over the lock.
5. **Oplog commit** (6.8), then one `RefTx` covering the target, extra refs and the oplog ref (CAS).
6. If `head` is set, `symbolic-ref` after re-checking that HEAD still points at the old ref.
7. Write worktree files (temp file in the same directory, rename, preserve mode, symlinks supported, never follow symlinked parents).
8. Install the index. Return `Applied {op, post_view}`.

**Failure behavior**:
- Before step 5: nothing changed (exit 1, or 4 for stale).
- After step 5: report the partial state together with `aisc undo OP`. The op lists the planned writes.

### 6.8 Oplog and undo (`oplog.rs`, `undo.rs`)
- **Ref**: `refs/aisc/oplog/<wt>`, where `<wt>` is `main`, or the basename of `.git/worktrees/<id>`.
- **Op commit**:
  - Tree: `op.json` plus `wt/<n>` blobs holding pre-op bytes of every path the op writes or deletes.
  - Parents: the previous op, plus every unique old and new oid of moved refs, so everything stays reachable through gc.
  - Message: `aisc VERB: summary`.
- **`op.json`** (v1): `{v:1, verb, argv, summary, time, root, refs:[{ref, old, new}], head:{from, to}|null, worktree:[{path, before:blob|null, after:blob|null, mode}], index_before_tree, undo_of}`.
- **`undo [OP]`** (default: the latest op; undoing an undo is a redo):
  - Every op ref must currently equal its `new` value (else exit 4). Worktree paths must equal their `after` blob (else refuse).
  - Restore refs and HEAD, rewrite worktree files to `before`, and apply the index rule in reverse.
  - Records an `undo` op.
- **`oplog`** prints `ID AGE VERB SUMMARY`, newest first.

**Convergence (D4)**: core's S4a journal (`refs/aisc/operations`, `op plan/apply`) is a separate effort. Keep this API narrow (`record(plan)`, `list`, `undo`) so it can later be backed by that journal.

## 7. Disk-space requirements (S4)

1. **No worktree by default.**
   - History verbs operate on the single checkout. `--branch` edits other branches without a checkout.
   - `start NAME` with neither `--path` nor `--in-place` defaults to in-place (D7; slice 0 changes the clap group).
   - Running parallel tasks inside one checkout (GitButler-style virtual branches) remains tracker item S6.
2. **Registry.**
   - `start --path` writes `$GIT_COMMON_DIR/aisc/worktrees/<name>.json`: `{name, path, branch, base, created_at, aisc_version, sparse:[…]|null}` (atomic write).
   - Slice 0 adds `Repo::common_dir()`.
3. **`finish` removes the worktree.** For worktree sessions without `--keep`:
   - Runs after evidence validation; `require_clean` is already required, so untracked or modified files block removal.
   - Runs `git worktree remove P` (never `--force`) from the main checkout's cwd.
   - Keeps the `aisc/NAME` branch (commits are not lost) and deletes the registry entry.
   - Output adds `removed_worktree`, `freed_bytes` and `main_checkout` ("continue in …").
   - Ignored files go with the worktree, so removal is automatic only when **all** ignored content is known to be regenerable (an anchored allowlist):
     - Tool dirs count anywhere: `node_modules`, `__pycache__`, `.pytest_cache`, `.mypy_cache`, `.ruff_cache`, `.tox`, `.venv`, `.gradle`, `.next`, `.nuxt`, `.svelte-kit`, `.turbo`, `.parcel-cache`, `DerivedData`, `.dart_tool`. So do `*.pyc`, `*.pyo` and `*.o` files, and OS/package-manager junk (`.DS_Store`, `Thumbs.db`, `desktop.ini`, `npm-debug.log*`, `yarn-debug.log*`, `yarn-error.log*`, `pnpm-debug.log*`, `lerna-debug.log*`). Other `*.log` files do not count.
     - Generic names (`build`, `dist`, `out`, `target`, `coverage`, `.cache`) count only at the worktree root or as a direct child of a directory holding a project manifest (`Cargo.toml`, `package.json`, `pyproject.toml`, `setup.py`, `setup.cfg`, `go.mod`, `pom.xml`, `build.gradle(.kts)`, `settings.gradle(.kts)`, `Package.swift`, `Gemfile`, `composer.json`, `mix.exs`, `CMakeLists.txt`, `*.csproj`, `*.fsproj`, `*.sln`). `bin` and `obj` count only beside a `*.csproj`/`*.fsproj`/`*.sln`, never just for being at the root (an ignored root `bin/` often holds personal scripts). So `src/build/notes.txt`, `docs/dist/x.md` or `bin/deploy.sh` keeps the worktree.
     - Any other ignored file keeps the worktree with `ignored files not known to be regenerable: <first 3>`.
   - Secret-like content keeps the worktree, inside allowlisted dirs too. That covers secret-like names (`.env`, `.env.*`, `*.env`, `.envrc`, `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.jks`, `*.keystore`, `*.ovpn`, `*.kdbx`, `id_rsa*`, `id_ed25519*`, `id_ecdsa*`, `id_dsa*`, `credentials*`/`*credential*`, `service-account*`, `secrets.*`, `terraform.tfstate*`, `.npmrc`, `.netrc`, `.pgpass`, `.git-credentials`; all case-insensitive) and anything under `.aws`, `.kube`, `.ssh`, `.gnupg`, `.docker`, `gcloud`, `secrets` or `secret`. Exception: names are not checked inside installed third-party packages (`node_modules`, `.venv/**/site-packages`, `.gradle`, `.dart_tool`), because lockfiles bring names like `.env.example` and `server.key`. **Accepted trade-off:** a real secret someone puts inside `node_modules` (or those dirs) is deleted with the worktree; such a file is not a project's own and lives in a dir every package manager rewrites. A project's own build output (`target/`, `build/`, `dist/`) is always checked.
   - Nested repositories keep the worktree too: a `.git` dir or file, or a bare repo with `HEAD` + `objects/` + `refs/`. Exception: `node_modules/**` is not checked for nested repos, because git dependencies carry `.git` dirs.
   - Ignored dirs are listed with `git ls-files --others --ignored --exclude-standard --directory` and walked without following symlinks. The walk is bounded at 2,000,000 entries or 30 s; `AISC_IGNORED_SCAN_LIMIT` overrides the entry count and, like `AISC_GIT_TIMEOUT` and `AISC_OUTPUT`, is not part of the verify evidence. If the walk hits the bound, it sets `ignored_scan_truncated: true` and keeps the worktree (fails closed).
   - `--delete-ignored` (finish and gc) overrides all of the above; `--delete-ignored-secrets` and `--delete-nested-repos` are hidden aliases. The output always reports `ignored_files`, `ignored_bytes` and `ignored_scan_truncated`.
   - A HEAD that no branch, tag, remote or `refs/aisc/*` ref contains (detached work) is saved as `refs/aisc/rescue/<name>` before removal; output field `rescue_ref`.
   - If Git deletes only part of the files (it drops its admin dir anyway), the output says "partially removed" with the leftover path to delete by hand, and the registry entry is flagged `partially_removed`. gc lists an unlisted registered directory as `leftover` only if it has that flag or a `.git` file pointing at the admin dir recorded at start (`gitdir`). Otherwise it is `unknown`: never sized, and never given "delete by hand" advice.
4. **`aisc gc [--apply] [--min-age H]`.**
   - Lists only registry entries that are cross-checked: the path appears in `git worktree list --porcelain`, and that worktree's gitdir has an `aisc-session.json` with the same name. Unverified or foreign worktrees are never touched; they are counted as "N foreign worktrees ignored".
   - Per entry:
     - Size: a std::fs walk without following symlinks, apparent and allocated (`st_blocks*512`), 10 s budget, `≥` if partial.
     - Age, branch.
     - State: missing / dirty / clean, plus merged (tip is an ancestor of the base) or finished.
   - Candidates are clean and (merged or finished) and at least min-age old, or missing and at least min-age old (their HEAD is rescued first).
   - Registry names must match `[A-Za-z0-9._-]` (no `..`) and the file name; registered paths that are relative, contain `.`/`..` components, are `/`, `$HOME` or one of its ancestors, a system dir (`/Users`, `/home`, `/Volumes`, `/private`, `/var`, `/tmp`, `/System`, `/opt`, `/usr`), or contain the main worktree or Git dir (checked lexically, with the parent resolved, and by file identity so case variants on case-insensitive filesystems match) are `invalid`, never walked or acted on; the reason names the registry file to remove. A registered path that is a symlink is `unverified` and kept; paths are compared with only the parent resolved.
   - Dry run by default. `--apply` runs `git worktree remove` on candidates, then `git worktree prune`, then deletes their registry entries.
   - Text example:
     ```
     aisc worktrees: 3 (1.2 GiB); reclaimable 2 (1.1 GiB) (dry run; pass --apply)
     fix-a  ../fix-a  812 MiB  3d  clean merged    remove
     wip    ../wip     92 MiB  2h  dirty           keep
     ```
5. **Sparse.**
   - `start NAME --path P --sparse DIR…` runs `git worktree add --no-checkout -b aisc/NAME P BASE`, then `git -C P sparse-checkout set --cone DIR…`, then `git -C P checkout`.
   - Like Git's own `sparse-checkout` in a linked worktree, this sets `extensions.worktreeConfig=true` in the shared config. AISC adds a `note` when it is the one enabling it and never reverts it (other worktrees may depend on it).
   - S4 relaxes `Repo::writable()` to allow sparse checkouts, `paths()` to reject paths outside the cone, and the `task.rs` fingerprint to skip skip-worktree entries.
   - History Keep verbs work unchanged; Carry updates only the index for skip-worktree entries (already handled by slice 0).
6. **Disk metric.**
   - AISC `benchmarks/disk.py`: synthetic repo of about 5k files / 50 MB. Measures bytes added (apparent and allocated, workspace plus `.git` plus extra worktrees plus tool data) for: `git worktree add`, `aisc start --path`, `aisc start --path --sparse src`, in-place AISC history ops (object growth only), `but setup` plus app data, `jj git init --colocate` plus `.jj`.
   - vcbench `scripts/lib/disk.mjs` records `disk.{before, after, delta}_{apparent, allocated}_bytes` per run. Coverage: workspace including `.git`/`.jj`, worktrees outside it (from `git worktree list`), and the but app-data dir.

## 8. vcbench `aisc+skill` arm (S5, in a fork; upstream PR later)

- **`tool-policy.mjs`**:
  - `aiscReads = log show oplog context diff github gitlab help version`; `gc` counts as a read without `--apply`.
  - `subcommand()` takes value options `--color` and `--branch`.
  - `policyBlocked`: `if (tool==='aisc') return arm!=='aisc+skill'`. Raw git writes are already blocked in non-git arms; but and jj are already blocked.
  - `taskPolicyBlocked`: aisc has no publish verbs.
- **`command-guards.mjs`**: add `aisc` to the wrapped tools and its basename to `nativeNames`, so git spawned by aisc counts as internal.
- **`run-pilot-agent.mjs`**:
  - Add the arm, `--aisc-bin`, and `installAiscSkill` (copies `skills/aisc/SKILL.md` into `.codex/skills/aisc` and `.claude/skills/aisc`; records skill sha256 and `aisc --version`).
  - Preparation is identical to the git arm (dirty fixture created directly, no tool setup), so the fairness equivalence check passes trivially.
  - Scenario libs for pilots 7-13 replace `arm==='git'` with `gitLike(arm)` for state checks.
- **AGENTS.md policy block**:
  ```
  ## Version control
  - Use AISC (`aisc`) for all version-control writes. Do not use raw `git` writes, `but`, or `jj`.
  - Read-only git is allowed, but `aisc log` / `aisc show` already show what you need.
  ```
- **`skills/aisc/SKILL.md`** (history section, keep it short):
  ```
  ---
  name: aisc
  description: One-call Git history edits for agents: commit hunks, amend into older commits, split, squash, reorder, sync dirty branches, undo.
  ---
  # AISC
  Use `aisc` for every version-control write. Run `aisc log` once: it shows the branch's commits (hex ids) and uncommitted hunks (letter ids). Don't also run git status/log/diff.
  Refs: COMMIT = hex id or exact subject in quotes. SEL = PATH | hunk id | PATH:START-END (new-side lines; a deletion counts at the line after it, so a range ending on the line just before a deletion also selects it) | "PATH~text in the change".
  - Commit: aisc commit -m MSG [-b NEW_BRANCH] SEL...   (--all = everything)
  - Parallel branch: aisc commit -b NAME --onto BASE -m MSG PATHS   (HEAD stays; the changes move to NAME; no Git hooks run)
  - Fold changes into existing commits: aisc amend --into COMMIT SEL... [--into COMMIT SEL...]
  - Split: aisc split COMMIT --into MSG SEL... [--into MSG SEL...]   (unselected parts become uncommitted)
  - Remove part of a commit: aisc uncommit COMMIT SEL...
  - Squash: aisc squash --into MSG COMMIT COMMIT... [--into MSG ...]
  - Reorder: aisc move COMMIT... --before|--after COMMIT
  - Rebase onto moved base, keeping uncommitted work: aisc sync [ONTO]. On CONFLICTS, edit the marked files, then aisc sync --continue
  - Copy commits: aisc pick COMMIT... [-b NEW --onto BASE]; integrate: aisc land BRANCH... --into main --switch
  - Discard uncommitted changes: aisc discard SEL... ; undo the last operation: aisc undo
  Every write prints the resulting log; don't re-check. Anything not selected stays uncommitted. Use `aisc show COMMIT` to see a commit's hunks.
  ```
- **Reference `solve-aisc.sh`** (proves the tasks are solvable; added to the `pilot*:check` scripts):
  1. `aisc commit -b input-validation -m "Add input validation" README.md tests/handler.test.ts "src/handler.ts~validateLeadInput"`
  2. `aisc amend --into "refactor validation helpers" "README.md~likely email" "src/lead.ts~isLikelyEmail" "tests/lead.test.ts~MalformedEmail" --into "add lead scoring" "src/lead.ts~@enterprise.example" "tests/lead.test.ts~EnterpriseDomain" --into "document response behavior" "README.md~reason that can be shown" docs/response.md`
  3. `aisc split "add lead workflow" --into "refactor validation helpers" "src/lead.ts~isLikelyEmail" "tests/lead.test.ts~MalformedEmail" --into "tune lead scoring" "src/lead.ts~@enterprise.example" "tests/lead.test.ts~EnterpriseDomain" --into "document lead workflow" README.md docs/lead-workflow.md`
  4. `aisc move "add retry policy" "add notification sender" --before "add customer model"`
  5. `aisc squash --into "add parser pipeline" "extract parser helpers" "wire parser helpers" --into "add retry support" "add retry option" "test retry option" "document retry option"`
  6. `aisc sync main --prefer-branch src/config.ts`, then `apply-pilot6-state resolved-notify`, then `aisc sync --continue`
  7. `aisc show seed-rewrite`
  8. `aisc commit -b app-checkpoint -m "Checkpoint app" --all`
  9. `aisc discard src/catalog.ts src/format.ts test/catalog.test.ts`
  10. `aisc uncommit review-fix notes/private.md`
  11. `aisc sync origin/main --fetch`, then `aisc pick origin/feature-service`
  12. `aisc land greeting docs --into main --switch`
  13. `aisc pick --from revive-worktree-command "Revive worktree command" -b revive-worktree-no-graph --onto main`
- **Trial protocol**: same agents and models as the published runs, k ≥ 5 per arm per task. Report commands, wall time, transcript KB, provider tokens (kept separate from transcript size), pass rate and disk delta. Keep negative results.

## 9. Slices (file ownership is disjoint after slice 0)

Every slice runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`. Scenario tests port fixture content from `scripts/lib/pilotN-content.mjs` (MIT, attributed in a header comment), inline in the slice's own test file. They act like an agent: if a log call is needed, they parse `aisc log --json` and pick hunk ids by hint or content. They assert that **at most 2 aisc invocations** were used, then assert the vcbench verifier checks (subjects and order, per-commit snippets and paths via `git show rev:path`, leftovers on disk and absent from history, untracked files still untracked, `git diff HEAD -U0` free of targets, no stash, no `rebase-*`, no unmerged entries, protected refs equal).

Concurrency tests use the PR 5 PATH-wrapper technique: a fake `git` intercepts one subcommand (for example `update-ref`) and first mutates the repo.

**Slice 0, `feat/history-core`** (sequential; merges right after PR 5)
- **Owns**: `src/cli.rs` (all verbs and flags in section 4, `MutationOpts`, `start --sparse`, `finish --keep`, in-place default), `src/main.rs` (dispatch, `Outcome{json, text, exit}`, error-code downcast), `src/output.rs` (per-operation text default, `code`/`hint` in JSON errors), `src/process.rs` (`execute_input`, `Batch`), `src/git.rs` (`common_dir`, helpers).
- **New**: `src/history/{mod,model,view,diff,selector,apply,plumbing,rewrite,oplog,render,log,commit,undo}.rs`, plus stubs returning `bail!("not implemented")` in `amend.rs`, `discard.rs`, `split.rs`, `squash.rs`, `reorder.rs`, `pick.rs`, `sync.rs`, `land.rs` and `src/worktree.rs`.
- **Tests**: unit tests in apply/selector/diff (splice round-trips, every transplant score, tie and anchor case, no-EOL, CRLF refusal, quoted paths, hunk-id stability). `tests/support/mod.rs` (fixture builder, `aisc()` with an invocation counter, git helpers, tree/subject helpers).
  - `tests/history_core.rs`: log golden text and JSON; ids stable across two logs and across an unrelated edit; an edited hunk gets a new id and the old id is rejected; selector errors (unknown, ambiguous, overlapping, `~` with zero matches); `show` caps.
  - `tests/history_core.rs` engine tests: concurrent `update-ref` gives exit 4 with refs, index and oplog byte-identical; an index changed during the operation is refused; lock held; undo of commit restores branch, HEAD and index; undo after an external move is refused; redo; objects survive `git reflog expire --all --expire=now && git gc --prune=now`.
  - `tests/history_commit.rs`: **pilot 1** end-to-end in 2 calls; pilot 8 (`--all` respects ignores); `--into` stack; untracked, deleted, binary and mode changes; `-b` with an existing name fails; detached HEAD.

**S1 `feat/history-amend-split`**: amend, discard, split, uncommit.
- **Owns**: `src/history/{amend,discard,split}.rs`, `tests/history_{amend,discard,split}.rs`.
- **Tests**:
  - **Pilot 2** (2 calls); dependency error names the later commit and changes nothing; an untracked file routed into a commit where the path exists later is refused; author, date and message preserved with committer updated; dependent stacked branch moved; unrelated staged index entries preserved; amending into a base commit refused.
  - **Pilot 9** (`--whitespace-only` and path forms; worktree and index reset; undo restores bytes).
  - **Pilot 3** (2 calls); `--rest keep`; a split of the top commit.
  - **Pilot 10** (1 call; message and author byte-exact; file untracked).

**S2 `feat/history-replay`**: squash, move, pick.
- **Owns**: `src/history/{squash,reorder,pick}.rs`, `tests/history_{squash,move,pick}.rs`.
- **Tests**: **pilot 5** (1 call; tree identical; old subjects unreachable); **pilot 4** (1 call); a move conflict aborts atomically with a report; `--order` permutation validation; **pilot 13** (HEAD and scratch file untouched); a pick conflict aborts; `--to` a branch checked out elsewhere is refused.

**S3 `feat/history-sync`**: sync, land, switch.
- **Owns**: `src/history/{sync,land}.rs`, `tests/history_{sync,land}.rs`.
- **Tests**:
  - **Pilot 6** (2 calls; markers; exit 0 with status conflicts; a second call with markers left is refused; final verifier checks).
  - `--prefer-*` flags; `--abort` restores files byte-for-byte; a dirty worktree that conflicts with incoming changes aborts before any change; other verbs refused while paused; branch moved during the pause gives stale; an already-upstream commit is dropped.
  - **Pilot 11** with a local bare remote and `--fetch` (README edit unstaged and intact); **pilot 12** (main checked out and clean); `switch` carries edits and refuses on conflict.

**S4 `feat/worktree-disk`**
- **Owns**: `src/worktree.rs`, `src/task.rs`, the `writable`/`paths` parts of `src/git.rs`, `tests/worktree.rs`, `tests/lifecycle.rs` (adjust tests that rely on finish keeping the worktree), `benchmarks/disk.py`, `benchmarks/test_disk.py`.
- **Tests**: start registers; finish removes the worktree (path gone, branch kept, absent from `git worktree list`); `--keep`; dirty or untracked worktree means finish fails and nothing is removed; gc dry-run lists size > 0, age and status; gc `--apply` removes only clean merged or finished entries; foreign `git worktree add` is untouched and unlisted; dirty registered worktree kept; missing path pruned; sparse start has only cone paths on disk; checkpoint outside the cone refused; verify and finish work in a sparse worktree; size equals the bytes written within a tolerance.

**S5 `test/vcbench-aisc`** (fork plus AISC docs)
- **Owns** (fork): `scripts/lib/{tool-policy,command-guards,disk}.mjs`, `scripts/run-pilot-agent.mjs`, `tasks/*/solution/solve-aisc.sh`, `scripts/run-pilot*-checks.mjs`, `scripts/lib/scenario-*.mjs` (`gitLike`).
- **Owns** (AISC): `skills/aisc/SKILL.md` (history section), `benchmarks/vcbench/README.md`.
- **Acceptance**: every `pilot*:check` passes with the aisc reference solution; policy unit tests (aisc blocked in other arms; raw git write blocked in the aisc arm; aisc-spawned git counted as internal); disk fields present.
- **Timing**: written in parallel against the contract; real trials start after S1-S3 merge.

## 10. Expected aisc commands per scenario
| Scenario | Calls | `but` (published mean 4.5) |
|---|---|---|
| 1 selective | 2 (log, commit) | 2 |
| 2 multi-amend | 2 (log, amend) | about 5 |
| 3 split | 2 (show, split) | about 9 |
| 4 reorder | 1-2 | 2-4 |
| 5 squash | 1-2 | 3-7 |
| 6 dirty sync | 2 (sync, continue) plus edits | 7 |

Mean about 1.7-1.8. Expansion tasks: 7: 1, 8: 1 (no setup needed), 9: 1-2, 10: 1, 11: 2, 12: 1, 13: 1-2. The VC output budget is about 2-3 KB per task (log about 1.3 KB plus mutation about 0.5 KB).

## 11. Decisions needing lead approval, and risks
- **D1** New verbs print compact text by default even when redirected; JSON via `--json`.
- **D2** A paused sync exits 0 with status conflicts (jj precedent; avoids agents retrying a "failed" command and inflating failure counts).
- **D3** Plumbing commits run no hooks (like rebase); `checkpoint` keeps running hooks.
- **D4** Separate `refs/aisc/oplog/<wt>` journal, converged with core's S4a journal later.
- **D5** Auto-move local branches that point at rewritten commits.
- **D6** `finish` auto-removes worktrees (`--keep` keeps them).
- **D7** `start` defaults to in-place.
- **D8** Git ≥ 2.43 required.

Risks:
1. A wrong fuzzy transplant (mitigated by the unique-maximum and significance rules, tip exactness, and pilot tests).
2. 3-line-context hunks can merge two topics (`PATH:L-R` is the fallback).
3. A ref transaction is not crash-atomic across loose refs, and worktree writes happen after the ref update (the oplog has the recovery data).
4. Clap `Vec<Vec<String>>` per-occurrence parsing (verify in slice 0 first; fall back to `ArgMatches::get_occurrences`).
5. The agent's cwd is deleted when `finish` removes the worktree.
6. vcbench fairness acceptance: the arm lives in a fork, internal-git detection relies on the process name, and trials must match the published agent and model.
7. Agents may still re-run `aisc log` after a write; the skill tells them not to.

### Critical Files for Implementation
- /Users/samuelgja/Documents/PRIVATE/aisc/src/cli.rs
- /Users/samuelgja/Documents/PRIVATE/aisc/src/main.rs
- /Users/samuelgja/Documents/PRIVATE/aisc/src/task.rs
- /Users/samuelgja/Documents/PRIVATE/aisc/src/git.rs
- /Users/samuelgja/Documents/PRIVATE/aisc/tests/lifecycle.rs
## 12. Slice 0 implementation notes (feat/history-core)

Shipped: `log`, `show`, `commit`, `oplog`, `undo`, the engine below them, and wired stubs for every other verb.

Deviations and decisions made while implementing:
- Repeatable `--into` groups: clap derive cannot build `Vec<Vec<String>>` from a flat `Vec<String>` arg, so the flat field is registered and `Operation::attach_groups` fills `groups: Vec<Vec<String>>` from `ArgMatches::get_occurrences` right after parsing (risk 4 resolved). Slices S1-S3 read `args.groups`.
- Selector order: an exact changed path wins before `PATH:L-R` and `PATH~TEXT`.
- Oplog ref for a linked worktree is `refs/aisc/oplog/wt-<id>` (a worktree named `main` cannot collide with the main checkout's log). The first op hangs off an empty "aisc oplog" genesis commit so a first-parent walk never leaks into branch history.
- `op.json` additionally records the index rule changes (`index`) so undo reverses them exactly; `index_before_tree` is always null.
- `Batch` was not needed: `read_blobs` uses one `cat-file --batch` process via `execute_input`.
- `index.lock` is held for the whole mutation; the index is compared byte for byte before the ref transaction and again before installation. Index entries created by the rule carry no stat data, so a later `git diff` may refresh and rewrite the index; the installed file itself keeps the original mtime (tested through a hard link).
- Process safety: SIGINT/SIGTERM kill and reap the running child process group, are deferred during the ref transaction + HEAD + worktree + index installation, and then exit 130/143. Reads time out after 30 s; hook-running commands and `update-ref` after 1 h (`AISC_GIT_TIMEOUT` overrides). Human output escapes C0/C1 controls and bidi overrides.
- Crash consistency (SIGKILL at any step): `aisc-operation.lock` holds `aisc pid N` plus an exclusive flock; `index.lock` is a hard link of `aisc/tmp/index-token-PID`; `aisc/journal.json` (op id, oplog ref, prepared index path and snapshot digest, cleanup lists) is written just before the ref transaction. The next `Session::open` takes over a lock only when its flock is free and its tag is aisc's, removes `index.lock` only when it is the dead process's token (same inode), and then rolls back (journal op not on the oplog ref: nothing had changed, the command proceeds) or rolls forward (HEAD, worktree files from their recorded `before`, prepared index over an unchanged snapshot) and exits 4. Foreign or live locks are always refused. Writes run deletions first; preflight checks each write against the earlier writes of the same plan. Deletions keep their directories; a file replaces a directory only once the plan emptied it.
- Follow-ups: roll-forward installs the prepared index whenever the index content still equals the snapshot (under a fresh `index.lock` if the user removed the stale one). A journal without its operation lock is touched only when its pid is dead and it is older than 5 s. Side files are removed only while their digest matches what the plan saw. `recover_if_needed()` also runs before `log`, `show` and `oplog`. Every worktree write or delete is compare-before-write: a file whose bytes are neither the planned `before` nor `after` (edited while aisc worked) is first renamed to `PATH.aisc-conflict[-N]` and reported on stderr.
- `start` defaults to in-place; `--sparse` and `finish --keep` are parsed (finish still always keeps worktrees).
- `commit -b NEW --onto BASE` (parallel branch, one call where Git needs `switch -c NEW BASE; commit; switch back`): NEW is created as BASE plus one commit with the selection, built with plumbing only; HEAD never moves. Each selected block goes onto BASE's version of its file where the diff BASE..HEAD carries it (the engine's `follow`); a block whose lines differ on BASE, a file missing (or, for a new file, already present) on BASE, or a whole-file change of a file that differs on BASE is refused with nothing changed. In the same operation the selection leaves the worktree and index exactly as `discard` would remove it (compare-before-write, journal, oplog), which is Git's end state after switching back; `aisc undo` deletes NEW and restores the changes. `--keep` leaves the changes in place (GitButler's end state). NEW must not exist; `--existing` allows it only with `--onto NEW` (the commit goes on top, the ref moves by compare-and-swap) and never for a checked-out branch. Text output is one line (`new branch NEW: ID "subject"`); JSON adds `onto`, `base`, `kept`, `removed` and the full log. Like every aisc commit it runs no Git hooks (D3); the equivalent `git switch` + `git commit` would run pre-commit and commit-msg.
