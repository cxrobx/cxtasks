#!/usr/bin/env bash
# Turn sync off, back on, or off for good — without needing anybody's help.
#
#   ./scripts/sync-control.sh status
#   ./scripts/sync-control.sh now       # reconcile once, right now
#   ./scripts/sync-control.sh pause     # stop talking to the peer, keep everything
#   ./scripts/sync-control.sh resume
#   ./scripts/sync-control.sh unpair    # forget the peer entirely, back to standalone
#
# WHY THIS EXISTS
#
# A rollback that needs a session running is not a rollback you own. The plan's
# safety story is "it is one toggle" — this is the toggle.
#
# PAUSE vs UNPAIR
#
#   pause   keeps the peer, the outbox and the T-number lease. Nothing is sent
#           and nothing is pulled. Resuming reconciles through the identical
#           code path an offline window uses, so a week paused and a week in a
#           bag are the same situation to the reconciler.
#
#   unpair  forgets all of it. The database stays complete and standalone — the
#           Mac was never a thin cache, which is exactly why coming back is
#           cheap — but anything the peer knows and this machine has not pulled
#           is gone, and un-pushed deletions in the outbox are discarded.
#           So it reconciles first, or refuses.
set -euo pipefail

DB="${CXTASKS_DB:-$HOME/Library/Application Support/com.cxtasks.app/cxtasks.db}"
[ -f "$DB" ] || { echo "!! no database at $DB" >&2; exit 1; }

# `trusted_schema=ON` in the SAME invocation: the CLI cannot write to `tasks`
# without it (gotcha #1). `app_meta` has no triggers and does not need it, but
# using one helper for every query means never wondering which kind this was.
q() { sqlite3 "$DB" "PRAGMA trusted_schema=ON; $1"; }

meta() { q "SELECT COALESCE((SELECT value FROM app_meta WHERE key='$1'), '');"; }

status() {
  local enabled peer url outbox hi next role
  enabled=$(meta sync_enabled)
  peer=$(meta sync_peer_id)
  url=$(meta sync_peer_url)
  role=$(meta role)
  outbox=$(q "SELECT COUNT(*) FROM sync_outbox;")
  hi=$(meta sync_lease_hi)
  next=$(meta next_short_id)

  echo "  database      $DB"
  echo "  schema        v$(meta schema_version)"
  if [ "$enabled" = "1" ]; then
    echo "  sync          ON"
  elif [ -n "$peer" ]; then
    echo "  sync          PAUSED (peer remembered, nothing sent or pulled)"
  else
    echo "  sync          OFF — standalone, exactly as it shipped"
  fi
  [ -n "${role:-}" ] && echo "  role          $role"
  [ -n "$peer" ] && echo "  peer          $peer at ${url:-<no url>}"
  echo "  tasks         $(q 'SELECT COUNT(*) FROM tasks;')"
  if [ -n "$hi" ]; then
    echo "  T-numbers     next T$next, lease ends T$hi ($((hi - next + 1)) left)"
  else
    echo "  T-numbers     next T$next, no ceiling"
  fi
  echo "  outbox        $outbox waiting to send"
  [ "$outbox" != "0" ] && echo "                (these are deletions — reconcile before unpairing)"

  # The three lines that tell a stall from a failure. `last pull` alone cannot:
  # four days old reads identically whether nothing is trying or every attempt
  # is being refused, and that ambiguity is exactly what hid T143's bug.
  local pulled attempt err
  pulled=$(meta sync_last_pull_at)
  attempt=$(meta sync_last_attempt_at)
  err=$(meta sync_last_error)
  [ -n "$pulled" ] && echo "  last pull     $pulled"
  if [ -n "$attempt" ]; then
    echo "  last attempt  $attempt"
  elif [ "$enabled" = "1" ]; then
    echo "  last attempt  never — nothing has tried since this database was opened"
  fi
  [ -n "$err" ] && echo "  last error    $err"
  return 0
}

# The reconcile itself needs Rust — `sync_now` is api::HOST_LOCAL and is refused
# over /rpc on purpose, so there is no HTTP route to borrow. `cxtasks-runner`
# carries it, and the INSTALLED one is preferred for the same reason gotcha #3
# gives about the MCP binary: that is the build actually in use.
#
# It must also check the binary KNOWS the flag. An older build — the installed
# app, before the next ship — parses its argv by looking for flags it has heard
# of and ignores the rest, so handing `--sync-now` to one starts the DRAIN LOOP
# in the foreground instead: unattended agents, no reconcile, no error. (Newer
# builds exit 2 on an unknown flag; this guard is for the ones that predate
# that.) `grep -c` reads the binary directly rather than piping `strings`, since
# `strings | grep -q` under `set -o pipefail` reports failure on SUCCESS —
# gotcha #37.
runner_bin() {
  for candidate in \
    "/Applications/CXTasks.app/Contents/MacOS/cxtasks-runner" \
    "$(dirname "$0")/../target/release/cxtasks-runner" \
    "$(dirname "$0")/../target/debug/cxtasks-runner"
  do
    [ -x "$candidate" ] || continue
    if [ "$(grep -a -c -- '--sync-now' "$candidate" || true)" != "0" ]; then
      echo "$candidate"
      return 0
    fi
    echo "  skipping $candidate — it predates --sync-now" >&2
  done
  return 1
}

backup() {
  local dir="$HOME/Backups/cxtasks"
  mkdir -p "$dir"
  local out="$dir/cxtasks-$1-$(date +%Y%m%d-%H%M%S).db"
  # `.backup`, never `cp`: there is a live WAL and a plain copy can catch it
  # mid-transaction.
  sqlite3 "$DB" ".backup '$out'"
  echo "  backup        $out"
}

case "${1:-status}" in
  status)
    status
    ;;

  now)
    [ "$(meta sync_enabled)" = "1" ] || {
      echo "!! sync is not on here — nothing to reconcile with. Try: $0 status" >&2
      exit 1
    }
    bin=$(runner_bin) || {
      echo "!! no cxtasks-runner found. Install the app, or:" >&2
      echo "   source \"\$HOME/.cargo/env\" && cargo build --release --bin cxtasks-runner" >&2
      exit 1
    }
    echo "  reconciling via $bin"
    "$bin" --sync-now
    ;;

  pause)
    q "INSERT OR REPLACE INTO app_meta (key, value) VALUES ('sync_enabled','0');"
    echo "  sync PAUSED. The peer, the outbox and the lease are all still here."
    echo "  Resume with: $0 resume"
    ;;

  resume)
    [ -n "$(meta sync_peer_id)" ] || { echo "!! nothing to resume — this database has no peer" >&2; exit 1; }
    q "INSERT OR REPLACE INTO app_meta (key, value) VALUES ('sync_enabled','1');"
    echo "  sync ON again, peer $(meta sync_peer_id)."
    ;;

  unpair)
    outbox=$(q "SELECT COUNT(*) FROM sync_outbox;")
    if [ "$outbox" != "0" ] && [ "${2:-}" != "--force" ]; then
      cat >&2 <<WARN
!! $outbox change(s) have not reached the peer yet, and unpairing discards them.

   They are deletions — the only thing that cannot travel as state — so losing
   them means a task you destroyed here comes back the next time these two
   databases meet.

   Reconcile first, then unpair. Or, if you have decided this peer is going
   away and you do not care what it thinks:  $0 unpair --force
WARN
      exit 1
    fi
    backup pre-unpair
    q "DELETE FROM app_meta WHERE key IN
         ('sync_enabled','sync_peer_url','sync_peer_id','sync_pushed_through',
          'sync_last_pull_at','sync_swept_through','role','sync_lease_hi',
          'sync_last_attempt_at','sync_last_error');
       DELETE FROM app_meta WHERE key LIKE 'sync_digest_%';
       DELETE FROM sync_outbox;"
    # The peer's token, which core deliberately leaves alone — it cannot tell a
    # test's sandbox from a live pairing, and this script can.
    rm -f "$HOME/.config/cxtasks/peer-token"
    echo "  unpaired. This database is standalone again:"
    echo
    status
    echo
    echo "  Nothing was removed from it — the Mac was never a thin cache, which"
    echo "  is why coming back costs nothing. T-numbers continue from where they"
    echo "  are; gaps are fine and were always possible."
    ;;

  *)
    sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
    ;;
esac
