#!/usr/bin/env bash
# Runs every command form in skills/aisc/SKILL.md against a real aisc binary on a
# throwaway repository and checks the exit codes and effects the skill promises.
# Usage: benchmarks/commands/skill_check.sh path/to/aisc
set -u
AISC=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_AUTHOR_NAME=B GIT_AUTHOR_EMAIL=b@example.invalid \
  GIT_COMMITTER_NAME=B GIT_COMMITTER_EMAIL=b@example.invalid GIT_EDITOR=true NO_COLOR=1 LC_ALL=C
fail=0; n=0
ok() { # ok EXPECTED_EXIT DESCRIPTION -- command...
  local want=$1 what=$2; shift 3
  n=$((n + 1))
  out=$("$@" </dev/null 2>&1); got=$?
  if [ "$got" != "$want" ]; then fail=$((fail + 1)); printf 'FAIL %s (exit %s, want %s)\n%s\n' "$what" "$got" "$want" "$out"; fi
}
has() { # has DESCRIPTION -- condition...
  local what=$1; shift 2
  n=$((n + 1))
  if ! "$@" >/dev/null 2>&1; then fail=$((fail + 1)); printf 'FAIL %s\n' "$what"; fi
}
a() { "$AISC" "$@"; }

git init -q --bare -b main "$ROOT/origin.git"
git init -q -b main "$ROOT/repo"; cd "$ROOT/repo"
printf 'import sys\n\n\ndef load(path):\n    return open(path).read()\n\n\n\n\n\n\ndef main():\n    print(load(sys.argv[1]))\n' > app.py
printf '# Demo\n' > README.md; printf 'CHANNELS = ["email"]\n' > notify.py
git add -A; git commit -qm initial; git remote add origin ../origin.git; git push -q origin main; git fetch -q origin
git switch -qc feature

# Read
ok 0 "log" -- a log
# Commit: path~text hunk, PATH:A-B lines, hunk id, --all, -b, -b --onto, -q
printf 'import sys\n\n\ndef load(path):\n    assert path\n    return open(path).read()\n\n\n\n\n\n\ndef main():\n    print("x", load(sys.argv[1]))\n' > app.py
ok 0 "commit PATH~text" -- a commit -m "assert path" "app.py~assert path"
git diff --quiet HEAD -- app.py && { fail=$((fail + 1)); echo "FAIL other hunk was committed"; }
ok 0 "commit PATH:A-B (new-side lines)" -- a commit -q -m "log result" app.py:14-14
echo one > a.txt; ok 0 "commit -b NEW (switches)" -- a commit -b topic -m "add a" a.txt
has "HEAD switched to topic" -- test "$(git branch --show-current)" = topic
git switch -q feature
echo guide > guide.md; echo wip >> README.md
ok 0 "commit -b NEW --onto BASE (stays)" -- a commit -b docs --onto main -m "add guide" guide.md
has "HEAD stayed on feature" -- test "$(git branch --show-current)" = feature
has "docs is based on main" -- test "$(git rev-parse docs^)" = "$(git rev-parse main)"
git diff --quiet HEAD -- README.md && { fail=$((fail + 1)); echo "FAIL README edit lost"; }
ok 0 "show REV" -- a show "assert path"
ok 0 "show A..B" -- a show "$(git rev-parse --short HEAD~1)..$(git rev-parse --short HEAD)"
ok 0 "show BRANCH" -- a show docs
id=$(a log | awk '/README.md/ {for (i = 1; i <= NF; i++) if ($i ~ /^[a-z][a-z][a-z]$/) {print $i; exit}}')
ok 0 "commit by hunk id from log ($id)" -- a commit -m "readme wip" "$id"
# History edits
echo fix >> notify.py; echo more >> README.md
ok 0 "amend --into twice" -- a amend --into "readme wip" README.md --into "assert path" notify.py
printf 'u\n' > user.py; printf 'o\n' > order.py; a commit -q -m "add models" user.py order.py >/dev/null
echo api > api.py; a commit -q -m "add api" api.py >/dev/null
ok 0 "split" -- a split "add models" --into "add user" user.py --into "add order" order.py
has "split made add user, add order" -- test "$(git log --format=%s -3 | tr '\n' ,)" = "add api,add order,add user,"
ok 0 "uncommit (file stays on disk)" -- a uncommit "add api" api.py
has "api.py on disk, untracked" -- test -f api.py -a -z "$(git ls-files api.py)"
ok 0 "move --after" -- a move "add user" --after "add order"
ok 0 "move --before" -- a move "add user" --before "add order"
ok 0 "squash" -- a squash --into "add models" "add user" "add order"
ok 0 "undo" -- a undo
has "undo restored two commits" -- test "$(git log --format=%s -2 | tr '\n' ,)" = "add order,add user,"
ok 0 "undo again = redo" -- a undo
has "redo squashed again" -- test "$(git log --format=%s -1)" = "add models"
# Discard and switch
echo junk >> app.py; ok 0 "discard" -- a discard app.py
ok 0 "switch -c" -- a switch -c side
ok 0 "switch carrying changes" -- a switch feature
# Sync: --fetch onto a moved upstream, conflict exits 0, --continue; --abort; --prefer-branch
git clone -q ../origin.git ../up; (cd ../up && printf 'CHANNELS = ["email", "push"]\n' > notify.py && git commit -qam "add push" && git push -q origin main)
out=$(a sync origin/main --fetch </dev/null 2>&1); code=$?
n=$((n + 1)); case "$code:$out" in 0:CONFLICTS*) ;; *) fail=$((fail + 1)); echo "FAIL sync conflict: exit $code $out";; esac
ok 0 "sync --abort" -- a sync --abort
a sync origin/main >/dev/null 2>&1 </dev/null
printf 'CHANNELS = ["email", "push"]\nfix\n' > notify.py
ok 0 "sync --continue" -- a sync --continue
has "feature now on origin/main" -- git merge-base --is-ancestor origin/main feature
(cd ../up && printf 'CHANNELS = ["email", "push", "sms"]\n' > notify.py && git commit -qam "add sms" && git push -q origin main)
ok 0 "sync --fetch --prefer-branch" -- a sync origin/main --fetch --prefer-branch notify.py
# Copy and integrate
ok 0 "pick --from -b --onto" -- a pick --from feature "add models" -b copy --onto main
(cd ../up && git switch -qc remote-topic && echo r > r.txt && git add r.txt && git commit -qm "remote topic" && git push -q origin remote-topic)
git fetch -q origin
ok 0 "pick origin/BRANCH" -- a pick origin/remote-topic
ok 0 "land --into main --switch" -- a land docs --into main --switch
has "land switched to main" -- test "$(git branch --show-current)" = main
# Exit codes: errors change nothing
before=$(git rev-parse HEAD)
ok 1 "unknown selector is an error (exit 1)" -- a commit -m x "app.py~no such text"
ok 2 "usage error (exit 2)" -- a commit --no-such-flag
has "errors changed nothing" -- test "$(git rev-parse HEAD)" = "$before"
echo q >> README.md; ok 0 "-q" -- a commit -q -m "r" README.md
echo "$n checks, $fail failed"
exit $((fail > 0))
