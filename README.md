# AISC

**Less source-control work for coding agents.** AISC gives an agent a separate
worktree, bounded context and diff output, explicit commits, and verification
tied to the exact clean commit that was checked.

### Current evidence

| Status | Claim | Evidence |
| --- | --- | --- |
| Measured baseline | A Git-only inspection run took a median 39.08 ms over five runs. Its transcript was 926 bytes and 409 `o200k_base` tokens. | [Method and results](docs/benchmarks.md) |
| Goal | Reduce model context and time spent on routine source-control work. | AISC and Git will be compared on matched tasks in the [benchmark](docs/benchmarks.md). |
| Not established | No AISC speed or token savings, or production-readiness claim, is established by the baseline. | A paired AISC comparison and safety review are still required. |

The 409-token figure is a text-size proxy, not provider-reported inference
usage. The [benchmark methodology and results](docs/benchmarks.md) keep provider
usage, transcript estimates, Git-only tasks, and the full agent workflow
separate.

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

For parallel work that needs its own branch and working directory, create a
worktree at a complete destination path that does not already exist, then run
the same lifecycle commands there:

```sh
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

Run `aisc --help` and `aisc <command> --help` for the installed version's full
syntax and limits.

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
