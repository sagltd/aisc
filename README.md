# AISC

**Less source-control work for coding agents.** AISC gives an agent bounded
context and diff output, explicit commits, and verification tied to the exact
clean commit that was checked. Work in place or create a separate Git worktree
for isolation.

### Current evidence

| Status | Claim | Evidence |
| --- | --- | --- |
| Measured baseline | A Git-only inspection run took a median 39.08 ms over five runs. Its transcript was 926 bytes and 409 `o200k_base` tokens. | [Method and results](docs/benchmarks.md) |
| Dependency audit | `cargo-audit` 0.22.2 reported zero vulnerabilities and warnings across 34 dependencies. This applies only to the audited lockfile snapshot, not to overall security. | `Cargo.lock` SHA-256 `18ef0d289ea4c5f7cb2527f007bcc45ef3214e6b5dc2315e3fd0f354a552ecea`; advisory database `4b587f00996ebe5c05a73eaef105ef241e876414` (2026-10-21). Rerun after lockfile changes. |
| Goal | Reduce model context and time spent on routine source-control work. | AISC and Git will be compared on matched tasks in the [benchmark](docs/benchmarks.md). |
| Not established | No AISC speed or token savings, or production-readiness claim, is established by the baseline. | A paired AISC comparison and safety review are still required. |

The 409-token figure is a text-size proxy, not provider-reported inference
usage. The [benchmark methodology and results](docs/benchmarks.md) keep provider
usage, transcript estimates, Git-only tasks, and the full agent workflow
separate.

CI targets Ubuntu 24.04 and macOS 15. Windows is not in the current matrix, so
these checks cannot establish Windows support.

## Install

You need Git and Rust 1.91.1 or later. Install the current repository revision
with Cargo:

```sh
cargo install --git https://github.com/sagltd/aisc --locked
```

## Try it

For a small task, start in the current directory and branch. In-place mode
requires a clean starting state and does not isolate filesystem changes:

```sh
aisc start parser-fix --in-place
aisc context
aisc diff -- src/parser.rs
aisc checkpoint -m "Fix parser edge case" -- src/parser.rs
aisc verify -- cargo test
aisc finish
```

For parallel work that needs its own branch and working directory, create an
ignored parent directory first, then create a worktree at a complete destination
path that does not already exist. The parent must already exist; AISC will not
create directories or change `.gitignore` for you:

```sh
mkdir -p .worktrees
aisc start parser-fix --path .worktrees/parser-fix
cd .worktrees/parser-fix
```

`context` returns bounded JSON about the branch, commit, index, worktree, and
conflicts. `diff` accepts literal paths and limits output size. `checkpoint`
commits only the named paths while preserving unrelated staged changes. `verify`
runs the given program directly; use an explicit shell command such as
`sh -c` only when shell behavior is needed. `finish` succeeds only when the
same clean candidate commit and environment have a successful verification
record. It reports the resulting commit and reference; it never pushes, merges,
or deletes a worktree.

`aisc github` is an explicit, read-only GitHub summary using the authenticated
GitHub CLI (`gh pr status`). It summarizes the current branch and related pull
requests, review decision, and checks. It needs network access; the Git context
commands above work offline. GitHub's own results can be partial, and AISC does
not claim fewer API requests than an optimized `gh` query.

Verification is supported on macOS and Linux. It requires an existing `HEAD`
and a clean committed checkout. AISC binds the candidate to tracked files, the
index, Git configuration, environment, and executable identity; it excludes
the per-call `CODEX_TOOL_CALL_ID` orchestration variable. Ignored files,
interpreters, transitive tools, and external services are not hermetic inputs.
The command runs directly without implicit shell fallback; use `sh -c` only
when shell behavior is intended. AISC terminates and reaps the command and
remaining processes in its Unix process group, but deliberately detached
processes are outside that boundary. Configured Git hooks and signing programs
run normally and are trusted programs with their own possible side effects.
Concurrent agents in one checkout are unsupported; use an isolated worktree
when agents need to run in parallel.

Run `aisc --help` and `aisc <command> --help` for the installed version's full
syntax and limits.

### Sandbox permissions

AISC uses Git, so the agent host must permit the required writes to Git
metadata. In a non-interactive Codex run, explicitly allow the repository's
`.git` directory with `--add-dir /absolute/path/to/repo/.git` while keeping the
normal workspace sandbox enabled. For a linked worktree, allow the resolved
Git and common Git directories instead:

```sh
git -C /path/to/worktree rev-parse --path-format=absolute --git-dir
git -C /path/to/worktree rev-parse --path-format=absolute --git-common-dir
```

Pass those specific directories to the host's write-permission mechanism.
Interactive agents can use their normal approval flow. Do not disable the
sandbox to grant Git access.

## Agent skill

Install the AISC skill with the [Skills CLI](https://skills.sh/):

```sh
npx skills add sagltd/aisc --skill aisc
```

The skill teaches an agent to inspect repository state before acting, preserve
unrelated work, and check the result of each AISC operation.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and checks. Read
[SECURITY.md](SECURITY.md) to report a vulnerability privately. AISC is
licensed under the [MIT License](LICENSE).
