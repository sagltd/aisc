# Evidence before implementation

Reviewed 2026-10-22. These are primary sources, not independent replications.

| Source | What it establishes | Implication for AISC |
| --- | --- | --- |
| [vc-bench](https://vcbench.dev/) | Compares Git, Jujutsu and GitButler with deterministic final Git-state grading, repeated fixed-model runs, setup excluded. Its publisher is GitButler, one measured tool. | Reuse outcome grading and disclose provenance. Published time, command and transcript-KB figures do not establish AISC savings or provider token usage. |
| [Git status](https://git-scm.com/docs/git-status) | Porcelain output is stable for scripts; v2 carries branch and index/worktree metadata. | Compare against concise machine-readable Git, with optional targeted diff. Never use gratuitously verbose Git as the baseline. |
| [Git filesystem monitor](https://git-scm.com/docs/git-fsmonitor--daemon) | Git already avoids full scans with filesystem notifications. | A wrapper is not inherently faster than Git. Document configuration; measure command time separately from model latency. |
| [GitButler CLI](https://docs.gitbutler.com/cli-overview) | Existing CLI supports stacked/parallel branches, inspection, operation restoration and conflict handling. | Fewer agent decisions are an existing design direction, not a novel storage model. |
| [Jujutsu Git compatibility](https://docs.jj-vcs.dev/latest/git-compatibility/) | Git backend and colocated workflows exist; Git's staging area is ignored. | Git interoperability alone does not prove preservation of the user's index. Test staged and unstaged content explicitly. |
| [AgentGit](https://agent-git.com/) | Existing AgentGit/agit saves, shares and resumes agent sessions in Git repositories. | Retain the AISC name and focus on source-control task operations. Do not import or publish private sessions to make a benchmark. |

The first local measurement precedes Rust implementation: five optimized Git inspection runs on generated public data, all state-preservation checks passed. Each transcript contained 409 `o200k_base` tokens and 926 bytes; command-time median 39.08 ms (36.87–42.34 ms). This is a baseline, not evidence that AISC saves tokens. [Raw measurements](../benchmarks/results/git-baseline.json).

The project will keep Git as storage and transport. The initial complete operation is isolated task start, bounded inspection, explicit-path checkpoint, command verification and finish. Finish returns a verified commit/ref; it does not integrate or publish automatically. [Measurement protocol](benchmarks.md).
