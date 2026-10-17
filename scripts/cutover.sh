#!/usr/bin/env bash
# Pair THIS Mac with the canonical node on the NAS. The real one.
#
# Everything here has already been rehearsed by `sync-dress-rehearsal.sh`
# against a `.backup` copy. This is the same sequence against the database that
# matters, with the same gates, plus the parts a rehearsal cannot have: a backup
# stored outside the app data dir, the background runner stopped, and a refusal
# to continue past any gate that does not hold.
#
# ROLLBACK, at any point, including after it finishes:
#
#     ./scripts/sync-control.sh pause     # stop talking to the node, keep everything
#     ./scripts/sync-control.sh unpair    # forget it entirely; the database stays whole
#
# The Mac is not a cache and never was, so unpairing removes bookkeeping and
# never data. That decision is what makes this reversible rather than a
# migration.
set -euo pipefail

DATA="$HOME/Library/Application Support/com.cxtasks.app"
DB="$DATA/cxtasks.db"
BACKUPS="$HOME/Backups/cxtasks"
MAC_NODE="${CXTASKS_MAC_NODE:-http://100.84.253.20:8787}"
NAS_NODE="${CXTASKS_NAS_NODE:-http://100.88.68.22:18870}"
MAC_TOKEN_FILE="$HOME/.config/cxtasks/full-token"
NAS_TOKEN_FILE="/volume2/docker/cxtasks/secrets/full-token"
NAS_SSH="${CXTASKS_NAS:-nas}"

fail() { echo; echo "!! $1" >&2; echo "   Nothing further was done. The database is untouched by this script beyond the backup." >&2; exit 1; }
say()  { printf '\n==> %s\n' "$1"; }
q()    { sqlite3 "$DB" "PRAGMA trusted_schema=ON; $1"; }

mac_auth() { printf 'header = "Authorization: Bearer %s"\n' "$(tr -d '\n' < "$MAC_TOKEN_FILE")"; }
# The node's own token, fetched up front. The two ends hold DIFFERENT secrets —
# querying one with the other's is not an auth failure worth diagnosing, it is
# the wrong key in the wrong lock, and the first draft of this script did
# exactly that.
NAS_TOKEN=""
nas_auth() { printf 'header = "Authorization: Bearer %s"\n' "$NAS_TOKEN"; }

# ── 0. refuse to start from a state that cannot be reasoned about ────────────

say "checking both ends agree before touching anything"
[ -f "$DB" ] || fail "no database at $DB"
NAS_TOKEN=$(ssh "$NAS_SSH" "cat $NAS_TOKEN_FILE" 2>/dev/null | tr -d '\n') \
  || fail "could not read the canonical node's token over ssh ($NAS_SSH)"
[ -n "$NAS_TOKEN" ] || fail "the canonical node's token file is empty at $NAS_TOKEN_FILE"

MAC_V=$(curl -s -m 10 "$MAC_NODE/health" | python3 -c 'import sys,json; print(json.load(sys.stdin)["schema_version"])' 2>/dev/null) \
  || fail "the Mac's node is not answering at $MAC_NODE"
NAS_V=$(curl -s -m 15 "$NAS_NODE/health" | python3 -c 'import sys,json; print(json.load(sys.stdin)["schema_version"])' 2>/dev/null) \
  || fail "the canonical node is not answering at $NAS_NODE"
DB_V=$(q "SELECT value FROM app_meta WHERE key='schema_version';")

echo "    mac node v$MAC_V · canonical node v$NAS_V · this database v$DB_V"
[ "$MAC_V" = "$NAS_V" ] || fail "schema skew: the Mac speaks v$MAC_V and the node speaks v$NAS_V.
   Pairing across versions makes columns silently stop travelling, which whole-row
   sync cannot detect. Update the older one first."
[ "$DB_V" = "$MAC_V" ] || fail "this database is v$DB_V but its own node is v$MAC_V — restart the node so it migrates."

# The refusal that matters most, checked BEFORE the backup so a wrong node
# costs nothing.
SEEDED=$(nas_auth | curl -s -K - -m 15 "$NAS_NODE/sync/info" | python3 -c 'import sys,json; d=json.load(sys.stdin); print(d.get("err") or ("SEEDED:"+str(d["ok"]["seeded_from"]) if d["ok"]["seeded_from"] else "UNSEEDED:"+("canonical" if d["ok"]["canonical"] else "NOT-CANONICAL")))' 2>/dev/null || echo "unreachable")
case "$SEEDED" in
  UNSEEDED:canonical) echo "    the node is canonical and has never been paired — good" ;;
  UNSEEDED:NOT-CANONICAL) fail "that node is not running as the canonical role, so there is nothing to pair WITH." ;;
  SEEDED:*) fail "that node was already seeded from ${SEEDED#SEEDED:}.
   If that is this Mac, you are already paired — use sync-control.sh, not this.
   If it is not, STOP: joining two databases that both issued T-numbers from 1
   either collides or renumbers, and neither is repairable." ;;
  *) fail "could not read the node's state — is the Mac's full token the right one for it? ($SEEDED)" ;;
esac

if [ "$(q "SELECT COUNT(*) FROM task_runs WHERE status='running';")" != "0" ]; then
  fail "a background run is in flight. Let it finish, or stop it, then run this again."
fi

# ── 1. stop the things that write ───────────────────────────────────────────

say "stopping background runs (the kill switch, checked between spawns)"
touch "$DATA/STOP_BACKGROUND"
echo "    $DATA/STOP_BACKGROUND"

# ── 2. the backup, OUTSIDE the app data dir ─────────────────────────────────

say "backing up, outside the data directory"
mkdir -p "$BACKUPS"
STAMP=$(date +%Y%m%d-%H%M%S)
BACKUP="$BACKUPS/cxtasks-pre-cutover-$STAMP.db"
# `.backup`, never `cp`: there is a live WAL and a plain copy can catch it
# mid-transaction. Outside the data dir so that wiping that dir — which is the
# recovery for "I paired the wrong machine" — does not take the backup with it.
sqlite3 "$DB" ".backup '$BACKUP'"
[ "$(sqlite3 "$BACKUP" 'PRAGMA integrity_check;')" = "ok" ] || fail "the backup failed its own integrity check"
echo "    $BACKUP  ($(du -h "$BACKUP" | cut -f1), integrity ok)"

# ── 3. record what must still be true afterwards ────────────────────────────

say "recording the facts the gate will check"
TABLES="tasks projects sections tags task_tags task_deps task_comments
        project_updates project_repos task_attachments task_runs"
BEFORE="$BACKUPS/cutover-$STAMP-before.txt"
: > "$BEFORE"
for t in $TABLES; do printf '%s=%s\n' "$t" "$(q "SELECT COUNT(*) FROM $t;")" >> "$BEFORE"; done
MAXSHORT=$(q "SELECT MAX(short_id) FROM tasks;")
echo "    $(grep -c . "$BEFORE") tables · $(q 'SELECT COUNT(*) FROM tasks;') tasks · highest T$MAXSHORT"

# ── 4. pair, through the tested path ────────────────────────────────────────

say "pairing"
RESULT=$(mac_auth | curl -s -K - -m 120 -X POST "$MAC_NODE/sync/pair" \
  -H 'Content-Type: application/json' \
  -d "$(python3 -c 'import json,sys; print(json.dumps({"url": sys.argv[1], "token": sys.argv[2]}))' "$NAS_NODE" "$NAS_TOKEN")")
echo "$RESULT" | grep -q '"ok"' || fail "pairing refused: $(echo "$RESULT" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("err","?"))' 2>/dev/null || echo "$RESULT")"
echo "    $(echo "$RESULT" | python3 -c 'import sys,json; print(json.load(sys.stdin)["ok"])')"

say "reconciling"
R=$(mac_auth | curl -s -K - -m 600 -X POST "$MAC_NODE/sync/reconcile")
python3 -c '
import json, sys
raw = json.loads(sys.argv[1])
if "ok" not in raw:
    print("    refused:", raw.get("err")); raise SystemExit(1)
d = raw["ok"]
print("    swept=%d pushed=%d failed=%d held_back=%s" % (d["swept"], d["pushed"], len(d["failed"]), d["held_back"]))
for f in d["failed"]:
    print("      seq %s: %s" % (f["local_seq"], f["err"]))
raise SystemExit(1 if d["failed"] or d["held_back"] else 0)' "$R" || fail "the first reconcile did not complete cleanly"

# ── 5. the gate ─────────────────────────────────────────────────────────────

# Read the node's state through its OWN API rather than reaching into its
# container: what matters is what it will serve, not what is on its disk.

say "gate 1/4 — this database still passes its own integrity check"
[ "$(q 'PRAGMA integrity_check;')" = "ok" ] || fail "the LIVE database failed integrity_check after pairing"
echo "    ok"

say "gate 2/4 — the node holds the same number of tasks"
NAS_TASKS=$(nas_auth | curl -s -K - -m 120 "$NAS_NODE/sync/snapshot" > "$BACKUPS/cutover-$STAMP-tasks.json"; python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["ok"]["tables"]["tasks"]["rows"]))' "$BACKUPS/cutover-$STAMP-tasks.json")
LOCAL_TASKS=$(q "SELECT COUNT(*) FROM tasks;")
[ "$NAS_TASKS" = "$LOCAL_TASKS" ] || fail "task count: this Mac has $LOCAL_TASKS, the node has $NAS_TASKS"
echo "    $LOCAL_TASKS on both"

say "gate 3/4 — per-table row counts, every synced table"
# To a FILE, never a shell variable piped through `echo`: zsh's echo interprets
# backslash escapes, so JSON's `\n` becomes a real newline inside a string and
# the payload stops being valid JSON. It presents as "Invalid control character"
# at a byte offset, which reads like the server emitting bad JSON — it is not.
SNAPFILE="$BACKUPS/cutover-$STAMP-node-snapshot.json"
nas_auth | curl -s -K - -m 120 "$NAS_NODE/sync/snapshot" > "$SNAPFILE"
while IFS='=' read -r t want; do
  [ -n "$t" ] || continue
  got=$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[2]))["ok"]["tables"].get(sys.argv[1],{"rows":[]})["rows"]))' "$t" "$SNAPFILE")
  [ "$want" = "$got" ] || fail "$t: this Mac has $want, the node has $got"
done < "$BEFORE"
echo "    all $(grep -c . "$BEFORE") tables match"

say "gate 4/4 — a settled reconcile moves nothing"
R2=$(mac_auth | curl -s -K - -m 300 -X POST "$MAC_NODE/sync/reconcile")
python3 -c '
import json, sys
d = json.loads(sys.argv[1])["ok"]
quiet = d["pushed"] == 0 and d["pulled"]["inserted_or_updated"] == 0 and d["pulled"]["deleted"] == 0
print("    pushed=%d pulled=%s" % (d["pushed"], d["pulled"]))
raise SystemExit(0 if quiet else 1)' "$R2" || fail \
  "a settled reconcile still reported movement — it would redraw the UI on a timer and discard in-progress edits"

# ── 6. hand back the switch ─────────────────────────────────────────────────

say "releasing the background kill switch"
rm -f "$DATA/STOP_BACKGROUND"

cat <<DONE

PAIRED. Every gate held.

  backup      $BACKUP
  counts      $BACKUPS/cutover-$STAMP-before.txt
  rollback    ./scripts/sync-control.sh pause      (keeps everything)
              ./scripts/sync-control.sh unpair     (forgets the node; data stays)

  The Mac still holds the real database — it is not a cache, and every
  documented sqlite3 incantation still works against it.

  Next: point the phone at the canonical node instead of this Mac, so it
  works with the lid shut. Settings -> node URL:

      cx.tailbff47a.ts.net:18870

  with that node's BOARD token:

      ssh $NAS_SSH "cat /volume2/docker/cxtasks/secrets/board-token" | pbcopy
DONE
