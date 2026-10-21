# AISC

**Less source-control work for coding agents.** AISC gives an agent bounded
context and diff output, explicit commits, and verification tied to the exact
clean commit that was checked. Work in place or create a separate Git worktree
for isolation.

### Current evidence

| Status | Claim | Evidence |
| --- | --- | --- |
| Measured baseline | A Git-only inspection run took a median 39.08 ms over five runs. Its transcript was 926 bytes and 409 `o200k_base` tokens. | [Method and results](docs/benchmarks.md) |
| Measured command comparison | On one compact inspection, AISC context took 192.99 ms median and emitted 143 transcript-proxy tokens; GitButler status plus Git numstat took 38.60 ms and 106 tokens. | [Raw command comparison and limits](benchmarks/results/commands-v1.json) |
| Dependency audit | `cargo-audit` 0.22.2 reported zero vulnerabilities and warnings across 49 dependencies. This applies only to the audited lockfile snapshot, not to overall security. | `Cargo.lock` SHA-256 `4f810ab783b022fa613ddce254233d45526d38c12e140a1ac15fe5594967daf7`; advisory database `4b587f00996ebe5c05a73eaef105ef241e876414` (2026-10-21). Rerun after lockfile changes. |
| Goal | Reduce model context and time spent on routine source-control work. | AISC and Git will be compared on matched tasks in the [benchmark](docs/benchmarks.md). |
| Not established | The measured command comparison was slower and used more text-proxy tokens than GitButler for that inspection. It does not establish whole-workflow or provider-usage savings, or production readiness. | The benchmark describes the task, candidate, and measurement limits; broader provider comparisons remain separate. |

These token figures are text-size proxies, not provider-reported inference
usage. The [benchmark methodology and results](docs/benchmarks.md) distinguish
provider usage, transcript estimates, Git-only tasks, and the full agent
workflow.

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
create parent directories or change `.gitignore` for you:

```sh
mkdir -p .worktrees
aisc start parser-fix --path .worktrees/parser-fix
cd .worktrees/parser-fix
```

In a terminal, AISC prints readable human output; when redirected, it emits
stable JSON by default. Use `--json` to request machine output explicitly, or
`--human --color always` to keep formatted text in a pipe. `NO_COLOR` disables
ANSI color. For example:

```sh
aisc --json context
aisc --human --color never diff -- src/parser.rs
```

`context` reports bounded branch, commit, index, worktree, and conflict data.
`diff` accepts literal paths and limits output size. `checkpoint` commits only
the named paths while preserving unrelated staged changes. `verify` runs the
given program directly; use an explicit shell command such as `sh -c` only when
shell behavior is needed. `finish` succeeds only when the same clean candidate
commit and environment have a successful verification record. It reports the
resulting commit and reference; it never pushes, merges, or deletes a worktree.

`aisc github` is an explicit, read-only GitHub summary using the authenticated
GitHub CLI (`gh pr status`). It summarizes the current branch and related pull
requests, review decision, and checks. It needs network access; the Git context
commands above work offline. GitHub's own results can be partial, and AISC does
not claim fewer API requests than an optimized `gh` query.

`aisc gitlab` is an explicit, read-only summary of open merge requests whose
source branch exactly matches the current branch. It uses the authenticated
GitLab CLI (`glab`), requires network access, and may receive incomplete
pipeline or mergeability fields from GitLab. It does not include a review inbox
or every project merge request. Both hosting commands are optional; local Git
operations do not require `gh` or `glab`.

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
