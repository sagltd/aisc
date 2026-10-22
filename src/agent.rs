//! `aisc agent setup --print`: the block an agent's AGENTS.md or CLAUDE.md carries, so the agent
//! starts with the one-call workflow instead of Git habits. The skill holds the full syntax.

pub const SETUP_BLOCK: &str = "\
<!-- aisc-agent-setup:start -->
## Version control

- Use AISC (`aisc`) for version-control inspection and writes: commits, branches, history edits, sync and undo.
- Start with `aisc log`. It shows the branch, its commits and every uncommitted hunk with an id, so do not run `git status` or `git diff` first. `aisc log BRANCH` views another local or `origin/` branch.
- Then do the task in one command, using hunk ids, paths, `PATH:L-R` or `\"PATH~text\"` as selectors and a commit's id or exact subject:
  - commit only some changes on a new branch: `aisc commit -b NEW -m MSG SEL...`
  - fold fixes into older commits: `aisc amend --into COMMIT SEL... --into COMMIT SEL...`
  - split, squash, reorder, uncommit: `aisc split`, `aisc squash`, `aisc move`, `aisc uncommit`
  - throw away selected uncommitted changes: `aisc discard SEL...`
  - copy commits onto another branch: `aisc pick [--from BRANCH] COMMIT... [-b NEW --onto BASE]`
  - rebase onto a moved base, keeping uncommitted work: `aisc sync [--fetch]`
  - integrate finished branches: `aisc land BRANCH... --into main`
  - undo the last aisc operation: `aisc undo`
- These replace raw `git checkout`, `reset`, `merge`, `rebase`, `cherry-pick` and `stash`.
- Writes print their result. Do not re-run log, status or diff to verify; stop and summarize.
- Exit 4 means something changed underneath: run `aisc log` and retry. Read `error:` and `hint:` lines before retrying.
- Commit only this session's changes and leave the rest uncommitted. Do not push unless asked.
- Exact syntax for every command is in the installed `aisc` skill.
<!-- aisc-agent-setup:end -->
";
