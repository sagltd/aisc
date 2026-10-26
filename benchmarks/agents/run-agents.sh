#!/bin/sh
# Re-run the real-agent comparison: every task, every tool, REPS times, rotating tool order.
#
# Needs: a clone of GitButler's version-control-bench at 4886fd5 with
# benchmarks/vcbench/aisc-arm.patch applied, Node 20+, the Claude CLI logged in, and the three
# tools. Paths come from the environment, nothing is machine-specific:
#
#   VCBENCH=~/src/version-control-bench   # patched harness checkout
#   AISC_BIN=~/.local/bin/aisc            # e.g. the v0.1.1 release binary
#   AISC_SKILL=./skills/aisc              # this repository's skill directory
#   BUT_BIN=/path/to/but                  # GitButler CLI 0.22.3
#   JJ_BIN=/path/to/jj                    # only needed by the harness's version probe
#   PINS=./benchmarks/agents/pins.json    # expected tool versions
#   OUT=./agent-runs                      # where per-run result.json files go
#   REPS=3  MODEL=claude-sonnet-5-5  EFFORT=medium
#   TASKS="heldout-7-amend-and-reword ..."  # optional: explicit task ids instead of every pilot-* task
#   ARMS="aisc+skill"                      # optional: only these arms (default: all three, rotated per rep)
#
# The harness runs every arm in the same sandbox (Claude accept-edits plus an allowlist; no
# permission bypass) and writes one result.json per run. Summarize with summarize.py.
set -eu
: "${VCBENCH:?} ${AISC_BIN:?} ${AISC_SKILL:?} ${BUT_BIN:?} ${JJ_BIN:?} ${PINS:?} ${OUT:?}"
REPS=${REPS:-3}; MODEL=${MODEL:-claude-sonnet-5-5}; EFFORT=${EFFORT:-medium}
OUT=$(cd "$(dirname "$OUT")" && pwd)/$(basename "$OUT")
cd "$VCBENCH"
tasks=${TASKS:-$(ls tasks | grep '^pilot-' | sort -t- -k2 -n)}
rep=1
while [ "$rep" -le "$REPS" ]; do
  case $((rep % 3)) in
    1) arms="aisc+skill but+skill git" ;;
    2) arms="but+skill git aisc+skill" ;;
    0) arms="git aisc+skill but+skill" ;;
  esac
  arms=${ARMS:-$arms}
  for task in $tasks; do
    for arm in $arms; do
      run="$OUT/rep$rep/$task--$arm"
      [ -f "$run/result.json" ] && continue
      mkdir -p "$OUT/rep$rep"
      node scripts/run-pilot-agent.mjs --task "$task" --arm "$arm" --agent claude \
        --model "$MODEL" --claude-effort-level "$EFFORT" \
        --aisc-bin "$AISC_BIN" --aisc-skill-dir "$AISC_SKILL" \
        --but-bin "$BUT_BIN" --jj-bin "$JJ_BIN" --version-pins "$PINS" \
        --out "$run" </dev/null >"$run.log" 2>&1 || true
      echo "rep$rep $task $arm"
    done
  done
  rep=$((rep + 1))
done
