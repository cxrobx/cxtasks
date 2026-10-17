#!/usr/bin/env bash
# The whole pairing, against a COPY, with the cutover's verification gate.
#
# WHY THIS EXISTS
#
# Pairing is the highest-risk operation in this project: it is the moment a
# database that has been standalone for a year starts taking writes from
# somewhere else. The plan's mitigations — a backup, and a rollback that is one
# toggle — are both about surviving a failure. This is about not having one. It
# runs the entire sequence against a `.backup` copy and applies the same gate
# the real cutover uses.
#
# It NEVER touches the live database and never talks to the real canonical node.
# Seeding the real node from a copy would stamp it with the COPY's host id, and
# the Mac would then be refused by the very check that exists to protect it.
# Both ends here are throwaway.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIVE_DB="$HOME/Library/Application Support/com.cxtasks.app/cxtasks.db"
WORK="$(mktemp -d)"
CANON_PORT="${CX_REHEARSE_CANON_PORT:-8951}"
LOCAL_PORT="${CX_REHEARSE_LOCAL_PORT:-8952}"
PIDS=()

cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

fail() { echo "!! $1" >&2; exit 1; }
say()  { printf '\n==> %s\n' "$1"; }

command -v sqlite3 >/dev/null || fail "sqlite3 not on PATH"
[ -x "$REPO/target/debug/cxtasks-server" ] || fail \
  "build it first: source \"\$HOME/.cargo/env\" && cargo build --bin cxtasks-server"

say "copying the live database (never opening it directly)"
# `.backup`, not `cp`: there is a live WAL, and a plain copy can catch it
# mid-transaction.
mkdir -p "$WORK/canon" "$WORK/mac"
sqlite3 "$LIVE_DB" ".backup '$WORK/mac/cxtasks.db'"

# One throwaway token, written straight to its files and never held anywhere
# else. Both ends share it because both ends are disposable.
( umask 077; openssl rand -hex 32 > "$WORK/canon/tok" )
cp "$WORK/canon/tok" "$WORK/mac/tok"
chmod 600 "$WORK/canon/tok" "$WORK/mac/tok"
auth_header() { printf 'header = "Authorization: Bearer %s"\n' "$(cat "$WORK/canon/tok")"; }

# A SECOND, board-scoped token — the one a phone would carry. It exists here
# only so the scope split on `/attachments/*` is proven at the wire rather than
# in a unit test: reading an image is reading a task, writing bytes into the
# master store is not.
( umask 077; openssl rand -hex 32 > "$WORK/canon/board-tok" )
cp "$WORK/canon/board-tok" "$WORK/mac/board-tok"
chmod 600 "$WORK/canon/board-tok" "$WORK/mac/board-tok"
board_header() { printf 'header = "Authorization: Bearer %s"\n' "$(cat "$WORK/canon/board-tok")"; }

TABLES="tasks projects sections tags task_tags task_deps task_comments
        project_updates project_repos task_attachments task_runs"

: > "$WORK/before.txt"
for t in $TABLES; do
  printf '%s=%s\n' "$t" "$(sqlite3 "$WORK/mac/cxtasks.db" "SELECT COUNT(*) FROM $t;")" >> "$WORK/before.txt"
done
echo "    $(grep -c . "$WORK/before.txt") tables, $(sqlite3 "$WORK/mac/cxtasks.db" 'PRAGMA trusted_schema=ON; SELECT COUNT(*) FROM tasks;') tasks"

start() {
  local dir="$1" port="$2" role="${3:-}"
  TZ="${TZ:-America/Chicago}" \
  CXTASKS_DATA_DIR="$WORK/$dir" \
  CXTASKS_TOKEN_FILE="$WORK/$dir/tok" \
  CXTASKS_BOARD_TOKEN_FILE="$WORK/$dir/board-tok" \
  CXTASKS_BIND="127.0.0.1:$port" \
  CXTASKS_ROLE="$role" \
    "$REPO/target/debug/cxtasks-server" > "$WORK/$dir.log" 2>&1 &
  PIDS+=("$!")
  # Disowned so the shell does not print "Terminated: 15" when cleanup kills it.
  # That notice arrives AFTER the PASS banner and makes a clean run look like a
  # failure — which matters more than usual here, because this script is the
  # gate somebody reads right before deciding to cut over.
  disown "$!" 2>/dev/null || true
}

say "starting a throwaway canonical node and a throwaway copy of this Mac"
start canon "$CANON_PORT" canonical
start mac   "$LOCAL_PORT" local
for _ in $(seq 1 30); do
  curl -sf "http://127.0.0.1:$CANON_PORT/health" >/dev/null 2>&1 \
    && curl -sf "http://127.0.0.1:$LOCAL_PORT/health" >/dev/null 2>&1 && break
  sleep 1
done
curl -sf "http://127.0.0.1:$CANON_PORT/health" >/dev/null || { cat "$WORK/canon.log"; fail "canonical node did not start"; }
curl -sf "http://127.0.0.1:$LOCAL_PORT/health"  >/dev/null || { cat "$WORK/mac.log";   fail "local node did not start"; }

sqlite3 "$WORK/mac/cxtasks.db" \
  "INSERT OR REPLACE INTO app_meta(key,value) VALUES('sync_enabled','1'),('sync_peer_url','http://127.0.0.1:$CANON_PORT');"

reconcile() { auth_header | curl -s -K - -X POST "http://127.0.0.1:$LOCAL_PORT/sync/reconcile"; }

say "reconcile #1 — the pairing"
R1=$(reconcile)
python3 -c '
import json, sys
raw = json.loads(sys.argv[1])
if "ok" not in raw:
    print("    refused:", raw.get("err")); raise SystemExit(1)
d = raw["ok"]
print("    swept=%d pushed=%d failed=%d held_back=%s" % (d["swept"], d["pushed"], len(d["failed"]), d["held_back"]))
for f in d["failed"]:
    print("      seq %s: %s" % (f["local_seq"], f["err"]))
raise SystemExit(1 if d["failed"] or d["held_back"] else 0)' "$R1" || fail "intents were refused"

say "reconcile #2 — settled, and it must be QUIET"
R2=$(reconcile)
python3 -c '
import json, sys
d = json.loads(sys.argv[1])["ok"]
quiet = d["pushed"] == 0 and d["pulled"]["inserted_or_updated"] == 0 and d["pulled"]["deleted"] == 0
print("    swept=%d pushed=%d pulled=%s" % (d["swept"], d["pushed"], d["pulled"]))
raise SystemExit(0 if quiet else 1)' "$R2" || fail \
  "a settled reconcile still reported movement — it would redraw the UI on a timer and discard in-progress edits (gotcha #24)"

# ── the verification gate the real cutover uses ──────────────────────────────

say "gate 1/5 — integrity on both ends"
for d in mac canon; do
  [ "$(sqlite3 "$WORK/$d/cxtasks.db" 'PRAGMA integrity_check;')" = "ok" ] || fail "$d failed integrity_check"
done
echo "    ok"

say "gate 2/5 — per-table row counts equal"
while IFS='=' read -r t want; do
  [ -n "$t" ] || continue
  got=$(sqlite3 "$WORK/canon/cxtasks.db" "SELECT COUNT(*) FROM $t;")
  [ "$want" = "$got" ] || fail "$t: mac=$want canonical=$got"
done < "$WORK/before.txt"
echo "    all $(grep -c . "$WORK/before.txt") tables match"

say "gate 3/5 — MAX(short_id) equal"
A=$(sqlite3 "$WORK/mac/cxtasks.db"   "PRAGMA trusted_schema=ON; SELECT MAX(short_id) FROM tasks;")
B=$(sqlite3 "$WORK/canon/cxtasks.db" "PRAGMA trusted_schema=ON; SELECT MAX(short_id) FROM tasks;")
[ "$A" = "$B" ] || fail "MAX(short_id): mac=$A canonical=$B"
echo "    T$A on both"

say "gate 4/5 — ten sampled tasks byte-equal on every column"
# `reminded_at` excluded on purpose: it is host-local, and a machine holding
# another's value would re-fire or disarm a reminder that is not its business.
COLS=$(sqlite3 "$WORK/mac/cxtasks.db" "PRAGMA table_info(tasks);" | cut -d'|' -f2 | grep -v '^reminded_at$' | paste -sd, -)
n=0
for id in $(sqlite3 "$WORK/mac/cxtasks.db" "PRAGMA trusted_schema=ON; SELECT id FROM tasks ORDER BY short_id LIMIT 10;"); do
  a=$(sqlite3 "$WORK/mac/cxtasks.db"   "PRAGMA trusted_schema=ON; SELECT $COLS FROM tasks WHERE id='$id';")
  b=$(sqlite3 "$WORK/canon/cxtasks.db" "PRAGMA trusted_schema=ON; SELECT $COLS FROM tasks WHERE id='$id';")
  [ "$a" = "$b" ] || fail "task ${id:0:8} differs between the two ends"
  n=$((n + 1))
done
echo "    $n/10 identical"

say "gate 5/5 — five fixed FTS queries returning identical id sets"
# The receiving end is never handed an index: its own triggers rebuild one from
# the rows. So this checks the applier went THROUGH the database, not around it.
for q in task sync agent repo review; do
  a=$(sqlite3 "$WORK/mac/cxtasks.db"   "PRAGMA trusted_schema=ON; SELECT t.id FROM tasks_fts f JOIN tasks t ON t.rowid=f.rowid WHERE tasks_fts MATCH '$q' ORDER BY t.id;" | shasum)
  b=$(sqlite3 "$WORK/canon/cxtasks.db" "PRAGMA trusted_schema=ON; SELECT t.id FROM tasks_fts f JOIN tasks t ON t.rowid=f.rowid WHERE tasks_fts MATCH '$q' ORDER BY t.id;" | shasum)
  [ "$a" = "$b" ] || fail "FTS '$q' returned different tasks on the two ends"
done
echo "    all five match"

say "round trip — an edit upstream must come down, a delete here must go up"
CT=$(sqlite3 "$WORK/canon/cxtasks.db" "PRAGMA trusted_schema=ON; SELECT id FROM tasks ORDER BY short_id LIMIT 1;")
sqlite3 "$WORK/canon/cxtasks.db" "PRAGMA trusted_schema=ON; UPDATE tasks SET title='changed upstream', updated_at=datetime('now') WHERE id='$CT';"
reconcile >/dev/null
[ "$(sqlite3 "$WORK/mac/cxtasks.db" "PRAGMA trusted_schema=ON; SELECT title FROM tasks WHERE id='$CT';")" = "changed upstream" ] \
  || fail "an edit on the canonical node did not reach this end"
echo "    pull: ok"

DT=$(sqlite3 "$WORK/mac/cxtasks.db" "PRAGMA trusted_schema=ON; SELECT id FROM tasks ORDER BY short_id DESC LIMIT 1;")
auth_header | curl -s -K - -X POST "http://127.0.0.1:$LOCAL_PORT/rpc" \
  -H 'Content-Type: application/json' \
  -d "{\"cmd\":\"destroy_task\",\"args\":{\"id\":\"$DT\"}}" >/dev/null
reconcile >/dev/null
[ "$(sqlite3 "$WORK/canon/cxtasks.db" "PRAGMA trusted_schema=ON; SELECT COUNT(*) FROM tasks WHERE id='$DT';")" = "0" ] \
  || fail "a destroy did not reach the canonical node — a delete cannot travel as a post-image"
echo "    push (delete): ok"

# ── attachments: the row travels as state, the BYTES follow ──────────────────
#
# The live database holds zero attachment rows, so this section MAKES one — a
# file under the data root and a row naming it, exactly the pair the app writes.
# Everything after that is the real code path.

say "attachments — bytes are canonical on the node, and the Mac keeps a cache"
mkdir -p "$WORK/mac/attachments"
AT_NAME="$(uuidgen | tr 'A-Z' 'a-z').png"
printf 'rehearsal attachment %s\n' "$(date +%s)" > "$WORK/mac/attachments/$AT_NAME"
AT_SHA=$(shasum -a 256 "$WORK/mac/attachments/$AT_NAME" | cut -d' ' -f1)
AT_TASK=$(sqlite3 "$WORK/mac/cxtasks.db" "PRAGMA trusted_schema=ON; SELECT id FROM tasks ORDER BY short_id LIMIT 1;")
AT_ID=$(uuidgen | tr 'A-Z' 'a-z')
sqlite3 "$WORK/mac/cxtasks.db" "PRAGMA trusted_schema=ON;
  INSERT INTO task_attachments (id, task_id, comment_id, name, mime_type, stored_name, sha256)
  VALUES ('$AT_ID', '$AT_TASK', NULL, 'rehearsal.png', 'image/png', '$AT_NAME', '$AT_SHA');"

reconcile > "$WORK/att1.json"
python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))["ok"]
b = d["blobs"]
print("    uploaded=%d downloaded=%d failed=%d" % (b["uploaded"], b["downloaded"], len(b["failed"])))
for f in b["failed"]:
    print("      %s: %s" % (f["stored_name"], f["err"]))
raise SystemExit(0 if b["uploaded"] == 1 and not b["failed"] else 1)' "$WORK/att1.json"   || fail "the bytes did not reach the node"

# The gate is the CHECKSUM, not the status code: a 200 proves the request was
# accepted, and only the digest proves the right bytes landed.
[ -f "$WORK/canon/attachments/$AT_NAME" ] || fail "no file on the canonical node"
GOT=$(shasum -a 256 "$WORK/canon/attachments/$AT_NAME" | cut -d' ' -f1)
[ "$GOT" = "$AT_SHA" ] || fail "checksum on the node is $GOT, expected $AT_SHA"
echo "    push: sha256 ${AT_SHA:0:12}… identical on both ends"

say "...and the cache refills itself, which is what makes it a cache"
rm "$WORK/mac/attachments/$AT_NAME"
reconcile > "$WORK/att2.json"
python3 -c '
import json, sys
b = json.load(open(sys.argv[1]))["ok"]["blobs"]
print("    uploaded=%d downloaded=%d failed=%d" % (b["uploaded"], b["downloaded"], len(b["failed"])))
raise SystemExit(0 if b["downloaded"] == 1 and not b["failed"] else 1)' "$WORK/att2.json"   || fail "a deleted local file was not re-fetched"
BACK=$(shasum -a 256 "$WORK/mac/attachments/$AT_NAME" | cut -d' ' -f1)
[ "$BACK" = "$AT_SHA" ] || fail "the re-fetched file hashes to $BACK, expected $AT_SHA"
echo "    pull: byte-identical after deleting the local copy"

say "...a settled pass moves nothing"
reconcile > "$WORK/att3.json"
python3 -c '
import json, sys
b = json.load(open(sys.argv[1]))["ok"]["blobs"]
raise SystemExit(0 if b == {"uploaded": 0, "downloaded": 0, "failed": [], "more": False} else 1)' "$WORK/att3.json"   || fail "a settled reconcile still moved bytes — it would redraw the UI on a timer (gotcha #24)"
echo "    quiet"

say "...a traversing name is refused rather than served"
TRAV=$(auth_header | curl -s -o /dev/null -w '%{http_code}' -K - "http://127.0.0.1:$CANON_PORT/attachments/..%2f..%2fetc%2fpasswd")
[ "$TRAV" = "404" ] || fail "a path traversal answered $TRAV rather than refusing"
NOAUTH=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$CANON_PORT/attachments/$AT_NAME")
[ "$NOAUTH" = "401" ] || fail "an unauthenticated fetch answered $NOAUTH rather than 401"
echo "    traversal 404, unauthenticated 401"

say "...a board token may READ an attachment and may not WRITE one"
BGET=$(board_header | curl -s -o /dev/null -w '%{http_code}' -K - "http://127.0.0.1:$CANON_PORT/attachments/$AT_NAME")
[ "$BGET" = "200" ] || fail "a board token could not read an attachment ($BGET) — that is a broken image on the phone, not a tighter posture"
BPOST=$(board_header | curl -s -o /dev/null -w '%{http_code}' -K - -X POST \
  --data-binary @"$WORK/canon/attachments/$AT_NAME" \
  "http://127.0.0.1:$CANON_PORT/attachments/$AT_NAME")
[ "$BPOST" = "403" ] || fail "a board token could write bytes into the master store ($BPOST)"
# ...and the split must not have widened the row protocol along with it.
BSNAP=$(board_header | curl -s -o /dev/null -w '%{http_code}' -K - "http://127.0.0.1:$CANON_PORT/sync/snapshot")
[ "$BSNAP" = "403" ] || fail "a board token could replicate the database ($BSNAP)"
echo "    board GET 200, board POST 403, board /sync/snapshot 403"

say "...and deleting an attachment travels, rather than coming back on the next pull"
auth_header | curl -s -K - -X POST "http://127.0.0.1:$LOCAL_PORT/rpc" \
  -H 'Content-Type: application/json' \
  -d "{\"cmd\":\"delete_task_attachment\",\"args\":{\"id\":\"$AT_ID\"}}" >/dev/null
reconcile >/dev/null
[ "$(sqlite3 "$WORK/canon/cxtasks.db" "SELECT COUNT(*) FROM task_attachments WHERE id='$AT_ID';")" = "0" ] \
  || fail "the delete did not reach the node — the row would be resurrected by the next pull"
[ ! -f "$WORK/canon/attachments/$AT_NAME" ] || fail "the node kept the file after the row was deleted"
reconcile >/dev/null
[ "$(sqlite3 "$WORK/mac/cxtasks.db" "SELECT COUNT(*) FROM task_attachments WHERE id='$AT_ID';")" = "0" ] \
  || fail "the attachment came back on the next pull"
echo "    gone on both ends, and it stayed gone"

cat <<'DONE'

PASS — a copy of the live database paired, converged, settled quiet, passed all
       five gates, round-tripped an edit down and a delete up, and moved an
       attachment's BYTES up, back down after a cache wipe, and away again on
       delete — verified by checksum at every step.

       Nothing here touched the live database or the real canonical node.
DONE
