//! `aisc agent setup --print`: the block an agent's AGENTS.md or CLAUDE.md carries, so the agent
//! starts with the one-call workflow instead of Git habits, without opening help or the skill.

pub const SETUP_BLOCK: &str = "\
<!-- aisc-agent-setup:start -->
## Version control

Use AISC (`aisc`) for all version-control inspection and writes. Most tasks take two calls: `aisc log`, then one command below. If the task already names the commits (exact subject) and the changes (path or text), skip `aisc log`. This syntax is complete: do not run `--help`, open the skill, or use `git status`/`diff`/`log`.

- look: `aisc log [BRANCH]` (commits, every uncommitted hunk with an id; BRANCH may be `origin/NAME`); `aisc show COMMIT`
- commit some changes: `aisc commit -m MSG SEL...` (`--all`; `-b NEW` new branch; `-b NEW --onto BASE` without leaving this branch)
- fold into older commits: `aisc amend --into COMMIT SEL... [--into COMMIT SEL...]`
- split: `aisc split COMMIT --into MSG SEL... [--into MSG SEL...]`
- squash: `aisc squash --into MSG COMMIT COMMIT... [--into MSG COMMIT...]`
- reorder: `aisc move COMMIT... --before|--after COMMIT`
- take out of a commit, keep on disk: `aisc uncommit COMMIT SEL...`
- throw away uncommitted changes: `aisc discard SEL...`
- copy commits: `aisc pick [--from BRANCH] COMMIT... [-b NEW --onto BASE]`
- update with moved base and own upstream, keeping uncommitted work: `aisc sync [ONTO] [--fetch]`; on CONFLICTS edit the marked files, then `aisc sync --continue`
- integrate branches: `aisc land BRANCH... --into main [--switch]`
- undo the last operation: `aisc undo`

SEL is a hunk id, a path, `PATH:A-B` (new-side lines) or `\"PATH~text\"` (the hunk containing text). COMMIT is an id or exact subject.

Writes print the result, new ids and `clean` when nothing is left; do not verify, answer and stop. Exit 4: run `aisc log`, retry. Exit 1: read `error:`/`hint:`. These replace raw `git checkout`, `reset`, `merge`, `rebase`, `cherry-pick` and `stash`. Leave unrelated changes uncommitted; do not push unless asked.
<!-- aisc-agent-setup:end -->
";
