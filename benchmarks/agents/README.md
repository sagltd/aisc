# Real-agent benchmark: everything needed to check it

The README's real-agent numbers come from this folder and two result files. Nothing in them is
hand-entered: the table is recomputed from per-run data, and the runs can be repeated with the
script below.

| File | What it is |
| --- | --- |
| [`tasks.md`](tasks.md) | The 13 task prompts, verbatim, and the wrapper text every agent saw |
| [`setup-blocks/`](setup-blocks) | The version-control instructions each tool's agent got in `AGENTS.md`/`CLAUDE.md` |
| [`run-agents.sh`](run-agents.sh) | Runs every task with every tool, N repetitions, rotating tool order |
| [`summarize.py`](summarize.py) | Recomputes the README table, and lists every failed run and why |
| [`pins.json`](pins.json) | Exact tool versions the harness checks before each run |
| [`../results/agent-runs-v0.1.0.json`](../results/agent-runs-v0.1.0.json) | 117 runs: AISC v0.1.0, GitButler 0.22.3 and Git 2.50, 3 repetitions each |
| [`../results/agent-runs-v0.1.1.json`](../results/agent-runs-v0.1.1.json) | 39 runs: AISC v0.1.1, same settings |

```sh
python3 benchmarks/agents/summarize.py benchmarks/results/agent-runs-v0.1.0.json
python3 benchmarks/agents/summarize.py benchmarks/results/agent-runs-v0.1.1.json
```

## How a run works

- **Harness.** [GitButler's version-control-bench](https://github.com/gitbutlerapp/version-control-bench) at commit `4886fd5`, plus [`../vcbench/aisc-arm.patch`](../vcbench/aisc-arm.patch), which adds the AISC arm (graded like the Git arm) and the sandboxed agent mode used for every arm. Task texts, fixtures and grading checks are unchanged.
- **Agent.** Claude CLI 2.1.306 with `claude-sonnet-5-5` at medium effort, in accept-edits mode with the same command allowlist for every tool. Approvals are never bypassed.
- **Instructions.** Each tool's agent gets that tool's own setup block, in the same `AGENTS.md` frame: `aisc agent setup --print`, `but agent setup --print`, or the harness's plain-Git text. The AISC and GitButler skills are installed the same way.
- **Grading.** The harness's verifier checks the Git-visible end state: commits, trees, refs, and the files left uncommitted. Its command policy also fails a run whenever the agent uses a command that tool's arm forbids, such as a raw `git` write in the AISC or GitButler arm.
- **Measures.** Tokens and cost are exactly what the Claude CLI reported for the session: input, cache creation, cache reads and output. Cost is the CLI's own estimate, not an invoice.

## What each run records

Every run in the result files has its task, tool and repetition, and whether it passed. A failed
run also names the failed verifier checks or the blocked command. Then come the number of tool
calls, the tokens by category, cost and wall time, and `vc_commands`. That last field holds every
version-control command the agent ran, with arguments and exit code. So you can see, for example,
that AISC's task 1 was:

```text
aisc log
aisc commit -m "Add lead input validation" -b input-validation qog guq ksv
```

Model text, prompts beyond `tasks.md`, and machine paths are not included: temporary paths read
`<tmp>`.

## What to keep in mind

- **Tuned tasks.** AISC's agent block and several commands were improved after watching AISC's
  runs on these same 13 tasks; GitButler and Git ran with their shipped setup. New tasks the
  tool was never tuned on are the stronger test, and this folder is the place to add them.
- **Separate v0.1.1 runs.** The v0.1.1 runs are AISC-only, made a few hours after the 117-run
  batch with identical settings. GitButler and Git were not re-run.
- **Variation.** Agents vary from run to run, which is why every number is reported with its
  min–max over the repetitions.
