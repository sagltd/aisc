# Contributing

Thanks for helping improve AISC. Keep changes focused, explain the user-facing
problem, and include evidence for behavior that can fail or change over time.

## Before opening a pull request

- Search existing issues and pull requests for related work.
- For a bug report, include the AISC version, operating system, Git version,
  command and sanitized output needed to reproduce it. Remove repository
  contents, session transcripts, tokens, and private paths.
- For a change, describe the behavior before and after. Include tests for new
  logic and regression cases for fixed bugs.
- Run the repository checks locally:

  ```sh
  python3 -m unittest discover -s benchmarks -v
  cargo fmt --check
  cargo clippy --all-targets -- -D warnings
  cargo test
  ```

- Keep `Cargo.lock` committed. This repository ships an executable, so the lock
  file is part of the reproducible build.
- Keep benchmark claims tied to checked-in or linked results. Report provider
  usage separately from estimates made by tokenizing transcript text, and label
  incomplete or failed runs.

## Pull requests

Feature work goes in a pull request into `dev`. Releases are a pull request from
`dev` into `main` only, and `main` is protected: the Main guard check rejects
any other source branch.

Open pull requests against `dev`. Keep the title specific and the description
short: state the problem, the resulting behavior, and the checks run. Do not
include user data, local agent sessions, credentials, or machine-specific
paths.

By submitting a contribution, you agree that it is provided under the MIT
License in this repository.
