# Working on AISC

Before continuing work, read `.lead/handoff.md` when present; checkpoint progress there after meaningful changes. Local handoff files do not ship with a clone.

- Rust 2024; keep Git as the storage and transport backend.
- Prefer the smallest complete operation and measured improvements. Never equate transcript-token counts with billed model usage.
- Preserve unrelated work, the user's index and refs. Test failure, conflict and concurrent-change paths before claiming safety.
- Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` for Rust changes.
- Never publish private agent sessions, credentials or machine-specific paths in benchmark artifacts.
