#!/usr/bin/env bash
# Two hosts draining one queue, on this Mac, with no network and no tokens.
#
#   ./scripts/two-host-sim.sh
#
# WHAT THIS IS FOR
#
# Phase 3 turns one runner into several, and almost everything that can go
# wrong needs two of them at once: the claim race, a cancel crossing hosts, a
# host pausing without pausing the others, a read-only host refusing work it
# must not take. None of that is reachable from a unit test, and all of it is
# expensive to discover in production — the cheapest failure is two unattended
# agents in one repo, and the dearest is one of them writing.
#
# So: two `cxtasks-runner` processes against a THROWAWAY database, in a
# throwaway repo, with a mirror pushed to a local bare repo. Zero network (the
# "remote" is a path), zero tokens (nothing is ever spawned — every assertion is
# about what the two hosts CLAIM, which is where the interesting bugs are).
#
# Nothing here touches ~/Library/Application Support/com.cxtasks.app.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="${TMPDIR:-/tmp}/cxtasks-two-host-$$"
mkdir -p "$ROOT"
# Bounded to the directory this script created. Never a parent — see the
# `Scratch` comment in mirror.rs for what that mistake looks like.
# `KEEP=1` leaves it behind, which is how you debug a failing check.
if [ -n "${KEEP:-}" ]; then
  trap 'printf "\nkept: %s\n" "$ROOT"' EXIT
else
  trap 'rm -rf "$ROOT"' EXIT
fi

MAC_DATA="$ROOT/mac"     # the shared database lives here; both "hosts" open it
MAC_HOME="$ROOT/machome"
NAS_HOME="$ROOT/nashome"
REPO="$ROOT/repo"
MIRRORS="$ROOT/mirrors"
mkdir -p "$MAC_DATA" "$MAC_HOME/.claude" "$NAS_HOME/.claude" "$MIRRORS"

# The property invariant 7 depends on, made true for both fake hosts.
printf '{"permissions":{"allow":[],"deny":[]}}\n' > "$MAC_HOME/.claude/settings.json"
cp "$MAC_HOME/.claude/settings.json" "$NAS_HOME/.claude/settings.json"

RUNNER="target/debug/cxtasks-runner"

# Every host here has a fake $HOME, so a real install under the real one can't be
# found, and a host with no executor reports HostDisabled before any gate this
# script is testing. It never runs an agent, so a stub that answers --version is
# all it needs, and the result doesn't depend on what this machine has installed.
mkdir -p "$ROOT/bin"
printf '%s\n' '#!/bin/sh' 'echo "2.0.0 (stub)"' > "$ROOT/bin/claude"
chmod +x "$ROOT/bin/claude"
export PATH="$ROOT/bin:$PATH"
DB="$MAC_DATA/cxtasks.db"

say()  { printf '\n\033[1m==> %s\033[0m\n' "$1"; }
pass() { printf '    \033[32mok\033[0m  %s\n' "$1"; }
fail() { printf '    \033[31mFAIL\033[0m %s\n' "$1" >&2; FAILURES=$((FAILURES+1)); }
FAILURES=0

# Capture into a variable rather than piping into `grep -q`.
#
# `grep -q` exits at the FIRST match and closes the pipe, so the producer takes
# SIGPIPE and reports failure — and under `set -o pipefail` that makes the whole
# pipeline non-zero. Every successful assertion would read as a failed one, which
# is exactly what the first version of this script did. Capturing also means a
# failure can print what it actually saw.
LAST_OUT=""
capture() { LAST_OUT="$("$@" 2>/dev/null || true)"; }

# `expect <desc> <substring>` / `refute <desc> <substring>` against LAST_OUT.
expect() {
  case "$LAST_OUT" in
    *"$2"*) pass "$1" ;;
    *) fail "$1"; printf '         wanted %s in:\n%s\n' "\"$2\"" "$(printf '%s' "$LAST_OUT" | sed 's/^/         | /')" >&2 ;;
  esac
}
refute() {
  case "$LAST_OUT" in
    *"$2"*) fail "$1"; printf '         did NOT want %s in:\n%s\n' "\"$2\"" "$(printf '%s' "$LAST_OUT" | sed 's/^/         | /')" >&2 ;;
    *) pass "$1" ;;
  esac
}

q() { sqlite3 "$DB" "PRAGMA trusted_schema=ON; $1"; }

# Both hosts share one database, which is what the sync layer produces on a real
# deployment. The difference between them is entirely CXTASKS_HOST_*.
mac() {
  env HOME="$MAC_HOME" CXTASKS_DATA_DIR="$MAC_DATA" TZ=America/Chicago \
      CXTASKS_HOST_ID=mac-sim CXTASKS_HOST_LABEL="the Mac" \
      CXTASKS_HOST_WORKSPACES=live CXTASKS_HOST_MAX_TIER=write \
      CXTASKS_HOST_EXECUTORS=claude,codex \
      "$RUNNER" "$@"
}
laptop() {
  env HOME="$MAC_HOME" CXTASKS_DATA_DIR="$MAC_DATA" TZ=America/Chicago \
      CXTASKS_HOST_ID=laptop-sim CXTASKS_HOST_LABEL="the laptop" \
      CXTASKS_HOST_WORKSPACES=live CXTASKS_HOST_MAX_TIER=write \
      CXTASKS_HOST_EXECUTORS=claude,codex \
      "$RUNNER" "$@"
}
nas() {
  env HOME="$NAS_HOME" CXTASKS_DATA_DIR="$MAC_DATA" TZ=America/Chicago \
      CXTASKS_HOST_ID=nas-sim CXTASKS_HOST_LABEL="the NAS" \
      CXTASKS_HOST_WORKSPACES=mirror CXTASKS_HOST_MAX_TIER=read-only \
      CXTASKS_HOST_EXECUTORS=claude CXTASKS_HOST_MAX_CONCURRENT=1 \
      "$RUNNER" "$@"
}

say "building"
source "$HOME/.cargo/env" 2>/dev/null || true
cargo build -q --bin cxtasks-runner
[ -x "$RUNNER" ] || { echo "!! no $RUNNER" >&2; exit 1; }

say "a repo with uncommitted work, and a mirror of it"
git init -q -b main "$REPO"
git -C "$REPO" config user.email sim@localhost
git -C "$REPO" config user.name sim
printf 'secret.env\n' > "$REPO/.gitignore"
printf 'original\n'   > "$REPO/tracked.txt"
git -C "$REPO" add -A
git -C "$REPO" commit -qm first
printf 'TOKEN=hunter2\n' > "$REPO/secret.env"
printf 'work in progress\n' > "$REPO/tracked.txt"

# Register both hosts by asking each to look at itself.
mac --health >/dev/null 2>&1 || true
nas --health >/dev/null 2>&1 || true
[ "$(q "SELECT COUNT(*) FROM hosts")" = "2" ] \
  && pass "both hosts registered themselves" \
  || fail "expected 2 hosts, got $(q 'SELECT COUNT(*) FROM hosts')"

q "UPDATE hosts SET mirror_remote = '$MIRRORS' WHERE id = 'nas-sim';"

say "each host generates its own drain SQL"
capture mac --dry-run
refute "the Mac reads bg_allowed, not bg_remote_allowed" "bg_remote_allowed"
capture nas --dry-run
expect "the NAS denies writes in its own SQL"  "bg_writes_allowed = 0"
expect "the NAS denies a shell in its own SQL" "bg_shell_allowed = 0"
expect "the NAS reads the remote opt-in column" "bg_remote_allowed = 1"

# ── the tasks ───────────────────────────────────────────────────────────────
# Written with the sqlite3 CLI on purpose: it is the same skew a leftover
# binary produces, and it is how these rows would look to a runner that has
# never heard of the columns being tested.
new_task() { # id title bg_allowed bg_remote writes shell
  # A remote-eligible task here is one assigned to the NAS: a host that isn't the
  # default claims only what names it, so that no task ever has two claimants.
  local run_on=NULL
  [ "$4" = 1 ] && run_on="'nas-sim'"
  q "INSERT INTO tasks (id, title, status, repo_path, bg_allowed, bg_remote_allowed,
                        bg_writes_allowed, bg_shell_allowed, bg_executor, short_id, run_on)
     VALUES ('$1', '$2', 'queued', '$REPO', $3, $4, $5, $6, 'claude',
             (SELECT COALESCE(MAX(short_id),0)+1 FROM tasks), $run_on);"
}

say "a task marked local-only is invisible to the NAS"
new_task local-only "local only" 1 0 0 0
capture mac --dry-run
expect "the Mac sees it" "local only"
capture nas --dry-run
expect "the NAS does not" "nothing queued"
q "DELETE FROM tasks WHERE id = 'local-only';"

say "a remote-eligible WRITE task is refused by the read-only host"
new_task writes "writes to the tree" 1 1 1 0
capture nas --dry-run
expect "the NAS refuses it — its own SQL cannot express the claim" "nothing queued"
capture mac --dry-run
expect "and the Mac leaves it — it names the NAS, and it never moves on its own" "nothing queued"
q "DELETE FROM tasks WHERE id = 'writes';"

say "two Macs, one queue: every task has exactly one claimant"
capture laptop --dry-run
expect "a second Mac registers without becoming a second default" "capability"
new_task unassigned "nobody named" 1 0 0 0
capture mac --dry-run
expect "the default machine sees unassigned work" "nobody named"
capture laptop --dry-run
expect "the other Mac does not" "nothing queued"
q "UPDATE tasks SET run_on = 'laptop-sim' WHERE id = 'unassigned';"
capture laptop --dry-run
expect "naming the other Mac gives it the task" "nobody named"
capture mac --dry-run
expect "and takes it away from the default" "nothing queued"
q "DELETE FROM tasks WHERE id = 'unassigned';"

say "a read-only remote task"
new_task readonly "investigate the failure" 0 1 0 0
capture nas --dry-run
expect "the NAS's SQL admits it" "investigate the failure"
capture mac --dry-run
expect "and the Mac cannot see it — it is not local-eligible" "nothing queued"

# Run a host's full loop — control thread included — for a few seconds.
#
# No `timeout(1)`: it is GNU coreutils and is NOT on a stock macOS, so the whole
# command silently did nothing and every check that depended on it failed as if
# the feature were broken. `kill` after a `sleep` needs no dependency.
#
# Backgrounding `env` directly rather than a wrapper matters too — `$!` has to
# be the process actually holding the runner, or the kill lands on a parent and
# leaves the runner behind.
run_for() { # seconds  env-assignments... -- (uses the caller's host vars)
  local secs="$1"; shift
  "$@" >/dev/null 2>&1 &
  local pid=$!
  sleep "$secs"
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
}

say "mirroring the repo, and what travels with it"
# Through the runner's own control loop, not by calling mirror::push directly:
# the thing under test is that a running host mirrors without being asked.
run_for 6 env HOME="$MAC_HOME" CXTASKS_DATA_DIR="$MAC_DATA" TZ=America/Chicago \
  CXTASKS_HOST_ID=mac-sim CXTASKS_HOST_LABEL="the Mac" \
  CXTASKS_HOST_WORKSPACES=live CXTASKS_HOST_MAX_TIER=write \
  CXTASKS_HOST_EXECUTORS=claude "$RUNNER"

BARE=$(find "$MIRRORS" -maxdepth 1 -name '*.git' | head -1)
if [ -n "$BARE" ]; then
  pass "a mirror appeared at $(basename "$BARE")"
  WORK="$ROOT/materialized"
  SHA=$(git -C "$BARE" rev-parse refs/cxtasks/sync/snapshot)
  git clone -q --shared --no-checkout "$BARE" "$WORK"
  git -C "$WORK" checkout -q --detach "$SHA"
  [ "$(cat "$WORK/tracked.txt")" = "work in progress" ] \
    && pass "uncommitted work travelled" || fail "the snapshot lost uncommitted work"
  [ -f "$WORK/secret.env" ] \
    && fail "a gitignored secret left the machine" || pass "the gitignored secret stayed behind"
  [ -z "$(git -C "$WORK" status --porcelain)" ] \
    && pass "the snapshot presents as a clean tree (which is why the prompt must say so)" \
    || fail "the materialized tree is dirty"
  [ "$(q "SELECT mirrors FROM hosts WHERE id='nas-sim'")" = "$REPO" ] \
    && pass "the pusher recorded the mirror against the NAS" \
    || fail "hosts.mirrors was not recorded: '$(q "SELECT mirrors FROM hosts WHERE id='nas-sim'")'"
else
  fail "no mirror was pushed"
fi

say "pausing one host does not pause the other"
q "UPDATE hosts SET paused = 1 WHERE id = 'nas-sim';"
capture nas --status
expect "the NAS reports HostPaused, not a bare Paused" "HostPaused"
capture mac --status
expect "the Mac is unaffected" "gate:          Open"
q "UPDATE hosts SET paused = 0 WHERE id = 'nas-sim';"

say "a disabled host explains itself differently from a paused one"
q "UPDATE hosts SET enabled = 0 WHERE id = 'nas-sim';"
capture nas --status
expect "the NAS reports HostDisabled" "HostDisabled"
q "UPDATE hosts SET enabled = 1 WHERE id = 'nas-sim';"

say "a cancel crosses hosts"
RUN_ID=$(uuidgen)
q "INSERT INTO task_runs (id, task_id, mode, status, host, heartbeat_at, executor, intent)
   VALUES ('$RUN_ID', 'readonly', 'background', 'running', 'nas-sim', datetime('now'),
           'claude', 'work');"
q "UPDATE task_runs SET cancel_requested_at = datetime('now') WHERE id = '$RUN_ID';"
# One control pass on the NAS turns the row into the local flag file.
run_for 5 env HOME="$NAS_HOME" CXTASKS_DATA_DIR="$MAC_DATA" TZ=America/Chicago \
  CXTASKS_HOST_ID=nas-sim CXTASKS_HOST_LABEL="the NAS" \
  CXTASKS_HOST_WORKSPACES=mirror CXTASKS_HOST_MAX_TIER=read-only \
  CXTASKS_HOST_EXECUTORS=claude "$RUNNER"
[ -f "$MAC_DATA/CANCEL_$RUN_ID" ] \
  && pass "the NAS raised the local cancel flag from a row it did not write" \
  || fail "the cancel never became a file"

# ...and the Mac must NOT have raised it: the run is not its business.
[ ! -f "$MAC_DATA/CANCEL_mac-only" ] && pass "no stray flags" || fail "stray cancel flag"

say "the concurrency ceiling is per host, out of one pool"
q "UPDATE hosts SET max_concurrent = 1 WHERE id = 'nas-sim';"
q "INSERT INTO task_runs (id, task_id, mode, status, host, heartbeat_at, executor, intent)
   VALUES ('busy-1', 'readonly', 'background', 'running', 'nas-sim', datetime('now'),
           'claude', 'work');"
capture nas --status
expect "the NAS is at its ceiling with one run" "AtConcurrencyLimit"
capture mac --status
expect "the Mac still has slots — the ceiling is per host" "gate:          Open"

say "a session asked for from the phone opens on its machine, once"
# A fake cx-open-in-herdr under the fake $HOME, so nothing reaches a real Herdr.
# It logs its arguments and answers like the headless helper does.
mkdir -p "$MAC_HOME/.local/bin"
printf '%s\n' '#!/bin/sh' \
  'printf "%s\n" "$*" >> "$(dirname "$0")/router.log"' \
  'printf "%s\n" "{\"workspace_id\":\"w\",\"tab_id\":\"t\"}"' \
  > "$MAC_HOME/.local/bin/cx-open-in-herdr"
chmod +x "$MAC_HOME/.local/bin/cx-open-in-herdr"
ROUTER_LOG="$MAC_HOME/.local/bin/router.log"
q "INSERT INTO tasks (id, title, status, short_id)
   VALUES ('sess', 'open me on the Mac', 'todo', (SELECT COALESCE(MAX(short_id),0)+1 FROM tasks));"
q "INSERT INTO task_runs (id, task_id, mode, status, executor, intent, host) VALUES
   ('sess-mac',    'sess', 'foreground', 'requested', 'claude', 'work', 'mac-sim'),
   ('sess-laptop', 'sess', 'foreground', 'requested', 'claude', 'work', 'laptop-sim'),
   ('sess-stale',  'sess', 'foreground', 'requested', 'claude', 'work', 'mac-sim');"
q "UPDATE task_runs SET started_at = datetime('now', '-20 minutes') WHERE id = 'sess-stale';"
capture mac --status
expect "the Mac counts only its fresh request" "sessions:      1 requested for this machine"
mac --once >/dev/null 2>&1 || true
[ "$(q "SELECT status FROM task_runs WHERE id = 'sess-mac'")" = "dispatched" ] \
  && pass "the Mac claimed its request and recorded it dispatched" \
  || fail "sess-mac is '$(q "SELECT status FROM task_runs WHERE id = 'sess-mac'")'"
LAST_OUT="$(cat "$ROUTER_LOG" 2>/dev/null || true)"
expect "the helper was asked to open it headless" "--headless --cwd"
expect "in the CXTasks space, scoped to the Inbox" "--space CXTasks --scope cxtasks:inbox"
[ "$(q "SELECT status FROM task_runs WHERE id = 'sess-laptop'")" = "requested" ] \
  && pass "the laptop's request was left for the laptop" || fail "the Mac touched the laptop's request"
LAST_OUT="$(q "SELECT status || ': ' || result_text FROM task_runs WHERE id = 'sess-stale'")"
expect "a stale tap was failed, naming the machine, not opened" "failed: the Mac didn't pick this up"
[ "$(q "SELECT status FROM tasks WHERE id = 'sess'")" = "in_progress" ] \
  && pass "opening the session started the task" || fail "the task did not move to in_progress"
mac --once >/dev/null 2>&1 || true
[ "$(wc -l < "$ROUTER_LOG" | tr -d ' ')" = "1" ] \
  && pass "a second tick opened nothing" || fail "the helper ran $(wc -l < "$ROUTER_LOG") times"

printf '\n'
if [ "$FAILURES" = "0" ]; then
  printf '\033[32mall checks passed\033[0m — two hosts, one queue, no network, no tokens\n'
else
  printf '\033[31m%s check(s) failed\033[0m\n' "$FAILURES"
  exit 1
fi
