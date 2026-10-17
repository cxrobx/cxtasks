#!/usr/bin/env bash
# An `agent:` tripwire evaluated END TO END by the real runner binary, against
# a fake `claude`, with zero tokens and nothing touching the live database.
#
#   ./scripts/agent-check-sim.sh
#
# WHAT THIS IS FOR
#
# Release 2 of tripwires (docs/roadmap.md § Phase 5) adds the agent kind: a
# bounded, read-only background run that answers a condition in prose. The
# unit tests pin every piece — the verdict grammar, the admission arithmetic,
# the pinned grant, the run row being the lease — but the thing that bites in
# production is the SEAM: `tick` → `agent_pass` → `start_check` → `ready_for`
# → `run_one_recorded` → `finish_check` → `record`, across two threads and a
# real subprocess, with the runner's own environment. So this drives
# `cxtasks-runner --once` at a throwaway data dir with `CXTASKS_CLAUDE_CLI`
# pointed at a shell script that speaks stream-json, and asserts on what
# landed in the database afterwards.
#
# Nothing here touches ~/Library/Application Support/com.cxtasks.app.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="${TMPDIR:-/tmp}/cxtasks-agent-check-$$"
mkdir -p "$ROOT"
if [ -n "${KEEP:-}" ]; then
  trap 'printf "\nkept: %s\n" "$ROOT"' EXIT
else
  trap 'rm -rf "$ROOT"' EXIT
fi

DATA="$ROOT/data"
HOME_DIR="$ROOT/home"
REPO="$ROOT/repo"
mkdir -p "$DATA" "$HOME_DIR/.claude" "$REPO"
# Gotcha #41: the runner must inherit no permission rules.
printf '{"permissions":{"allow":[],"deny":[]}}\n' > "$HOME_DIR/.claude/settings.json"
printf 'v1.9.3 shipped\n' > "$REPO/CHANGELOG.md"

RUNNER="target/debug/cxtasks-runner"
DB="$DATA/cxtasks.db"
[ -x "$RUNNER" ] || { echo "build first: cargo build" >&2; exit 2; }

say()  { printf '\n\033[1m==> %s\033[0m\n' "$1"; }
pass() { printf '    \033[32mok\033[0m  %s\n' "$1"; }
fail() { printf '    \033[31mFAIL\033[0m %s\n' "$1" >&2; FAILURES=$((FAILURES+1)); }
FAILURES=0

# Capture into a variable, never `| grep -q` — gotcha #37.
expect() { # label needle
  if [[ "$OUT" == *"$2"* ]]; then pass "$1"; else fail "$1 — wanted '$2' in: $(printf '%s' "$OUT" | head -c 400)"; fi
}
refute() { # label needle
  if [[ "$OUT" != *"$2"* ]]; then pass "$1"; else fail "$1 — did not want '$2'"; fi
}
q() { sqlite3 "$DB" "PRAGMA trusted_schema=ON; $1"; }
runner() { # args...
  env HOME="$HOME_DIR" CXTASKS_DATA_DIR="$DATA" TZ=America/Chicago \
      CXTASKS_CLAUDE_CLI="$FAKE" "$RUNNER" "$@"
}

# ── the fake claude ─────────────────────────────────────────────────────────
# Speaks `--output-format stream-json`, records its argv, and answers with
# whatever $ROOT/verdict.txt holds. `printf`, not `echo`: macOS /bin/sh's echo
# expands the \n inside the JSON and splits the result event in two.
FAKE="$ROOT/claude"
cat > "$FAKE" <<'EOF'
#!/bin/sh
printf '%s\036' "$@" > "$(dirname "$0")/argv.txt"
msg=$(cat "$(dirname "$0")/verdict.txt")
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sim-check"}'
printf '%s\n' '{"type":"assistant","message":{"model":"claude-haiku-4-5-20251001","content":[{"type":"text","text":"reading CHANGELOG.md"}],"usage":{"input_tokens":1000,"cache_read_input_tokens":40000,"output_tokens":20}}}'
printf '%s\n' "{\"type\":\"result\",\"result\":$msg,\"usage\":{\"output_tokens\":30}}"
exit 0
EOF
chmod +x "$FAKE"
verdict() { printf '%s' "$1" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))' > "$ROOT/verdict.txt"; }
argv_after() { # flag — the value is one RS-delimited record, however many lines it holds
  awk -v f="$1" 'BEGIN{RS="\036"} p{printf "%s", $0; exit} $0==f{p=1}' "$ROOT/argv.txt"
}

# ── the database ────────────────────────────────────────────────────────────
say "migrating a throwaway database"
OUT=$(runner --status 2>&1)
expect "the runner opened it" "tripwires:"
expect "the status line for check tokens exists" "check tokens:  0 /"

say "an armed agent tripwire, hourly, on a task with a repo"
q "INSERT INTO tasks (id, title, body, status, repo_path, bg_executor, model, short_id,
                      tripwire, tripwire_every_minutes, tripwire_state, bg_run_token_cap)
   VALUES ('sim-agent', 'SIM: the changelog mentions 2.0', 'A dress rehearsal for an agent tripwire.',
           'inbox', '$REPO', 'claude', 'claude-haiku-4-5-20251001', 900,
           'agent:CHANGELOG.md in this repo mentions a 2.0 release', 60, 'armed', 0);"
OUT=$(runner --status 2>&1)
expect "the runner sees it as due" "1 armed · 1 due now"

# ── NOT_YET ─────────────────────────────────────────────────────────────────
say "a NOT_YET verdict leaves the task armed and records the attempt"
verdict $'Looked at CHANGELOG.md.\n\nVERDICT: NOT_YET\nEVIDENCE: CHANGELOG.md tops out at 1.9.3; no 2.0 anywhere.'
OUT=$(runner --once 2>&1)
expect "the agent lane started a check" "agent check starting as run"
expect "and read the verdict" "agent check → not_yet"
OUT=$(q "SELECT intent || '|' || status || '|' || mode || '|' || COALESCE(timeout_minutes,'') FROM task_runs;")
expect "one run row, intent=check, succeeded, on the check clock" "check|succeeded|background|10"
OUT=$(q "SELECT last_outcome || '|' || last_note || '|' || failures || '|' || COALESCE(lease_id,'none') FROM tripwire_runtime WHERE task_id='sim-agent';")
expect "the attempt landed on the runtime row" "not_yet|CHANGELOG.md tops out at 1.9.3; no 2.0 anywhere.|0|none"
OUT=$(q "SELECT tripwire_state || '|' || status FROM tasks WHERE id='sim-agent';")
expect "the task is still armed and untouched" "armed|inbox"
OUT=$(q "SELECT COUNT(*) FROM task_comments WHERE task_id='sim-agent';")
expect "a not-yet is silent on the task" "0"

say "what the fake was actually handed"
OUT=$(argv_after --allowedTools)
expect "the pinned check grant" "Read,Glob,Grep,mcp__cxtasks__get_task"
refute "no Edit, whatever the task's tier" "Edit"
OUT=$(argv_after --disallowedTools)
expect "and the whole dangerous list denied" "Bash"
OUT=$(argv_after --max-turns)
expect "12 turns, not the global 150" "12"
OUT=$(argv_after --model)
expect "the task's model, because the check runs on Claude" "claude-haiku-4-5-20251001"
OUT=$(argv_after -p)
expect "the prompt is the CONDITION" "CONDITION: CHANGELOG.md in this repo mentions a 2.0 release"
expect "with the notes" "A dress rehearsal for an agent tripwire."
expect "and the verdict grammar" "VERDICT: TRIPPED | NOT_YET | UNSURE"
OUT=$(tr '\036' '\n' < "$ROOT/argv.txt")
expect "strict MCP config" "--strict-mcp-config"
OUT=$(cat "$DATA/claude-sessions/sim-agent/mcp.json" 2>/dev/null || echo "no mcp.json")
expect "the write-back router is started in CHECK mode" "CXTASKS_CHECK_RUN"

say "tokens land in both counters"
OUT=$(runner --status 2>&1)
expect "the day's total includes the check" "tokens today:  41050 /"
expect "and the sub-cap line shows the same slice" "check tokens:  41050 /"

# ── backed off ──────────────────────────────────────────────────────────────
say "just checked, so a second tick starts nothing"
OUT=$(runner --once 2>&1)
refute "no second check inside the hour" "agent check starting"
OUT=$(q "SELECT COUNT(*) FROM task_runs;")
expect "still one run row" "1"

# ── TRIPPED ─────────────────────────────────────────────────────────────────
say "a 'check now' request runs it again, and a TRIPPED verdict surfaces the task"
printf 'v2.0.0 shipped\n' >> "$REPO/CHANGELOG.md"
verdict $'VERDICT: TRIPPED\nEVIDENCE: CHANGELOG.md line 2 says v2.0.0 shipped.'
q "UPDATE tripwire_runtime SET requested_at = strftime('%Y-%m-%d %H:%M:%f','now') WHERE task_id='sim-agent';"
OUT=$(runner --once 2>&1)
expect "the request was honoured" "agent check → tripped"
OUT=$(q "SELECT tripwire_state || '|' || status || '|' || COALESCE(tripwire_tripped_at,'') FROM tasks WHERE id='sim-agent';")
expect "the task surfaced, still open" "tripped|inbox|20"
OUT=$(q "SELECT author || ': ' || body FROM task_comments WHERE task_id='sim-agent';")
expect "with the evidence as a comment under the tripwire author" "line 2 says v2.0.0 shipped"
OUT=$(q "SELECT COUNT(*) FROM task_runs WHERE intent='check' AND status='succeeded';")
expect "two check runs on the task's history" "2"
OUT=$(q "SELECT COUNT(*) FROM tripwire_runtime WHERE lease_id IS NOT NULL;")
expect "no lease left behind" "0"

# ── the sub-cap ─────────────────────────────────────────────────────────────
say "a spent sub-cap admits no check, and says so"
q "UPDATE tasks SET tripwire_state='armed', tripwire_tripped_at=NULL WHERE id='sim-agent';"
q "UPDATE tripwire_runtime SET requested_at = strftime('%Y-%m-%d %H:%M:%f','now') WHERE task_id='sim-agent';"
q "INSERT OR REPLACE INTO app_meta (key, value) VALUES ('bg_tripwire_daily_token_cap', '100000');"
OUT=$(runner --once 2>&1)
expect "refused with the arithmetic" "daily sub-cap and this one reserves"
OUT=$(q "SELECT COUNT(*) FROM task_runs;")
expect "no run row was written for a refused check" "2"
OUT=$(q "SELECT COALESCE(requested_at,'none') FROM tripwire_runtime WHERE task_id='sim-agent';")
refute "and the request is still pending for a later tick" "none"

# ── UNSURE ──────────────────────────────────────────────────────────────────
say "UNSURE is an error, not a not-yet"
q "INSERT OR REPLACE INTO app_meta (key, value) VALUES ('bg_tripwire_daily_token_cap', '0');"
verdict $'VERDICT: UNSURE\nEVIDENCE: the repo has no changelog I can read.'
OUT=$(runner --once 2>&1)
expect "recorded as an error" "agent check → error"
OUT=$(q "SELECT last_outcome || '|' || failures FROM tripwire_runtime WHERE task_id='sim-agent';")
expect "one failure, counted" "error|1"
OUT=$(q "SELECT tripwire_state FROM tasks WHERE id='sim-agent';")
expect "a cadenced check gets five tries before it surfaces" "armed"

say "done"
if [ "$FAILURES" -gt 0 ]; then
  printf '\n\033[31m%d check(s) failed\033[0m\n' "$FAILURES" >&2
  exit 1
fi
printf '\n\033[32mall checks passed\033[0m\n'
