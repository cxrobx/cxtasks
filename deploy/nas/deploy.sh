#!/usr/bin/env bash
# Ship the canonical node to the NAS.
#
# Build here, ship the IMAGE. The NAS is a 4-core Celeron; a release build of
# this workspace there would take the better part of an hour and evict the page
# cache every other service on the box is using. `docker save | ssh docker load`
# moves ~40 MB over the LAN instead.
#
# Idempotent: re-running rebuilds, re-ships and restarts. It never touches the
# data directory, so the canonical database survives every redeploy.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NAS="${CXTASKS_NAS:-nas}"
# Every other stack on this NAS lives under /volume2/docker, and that is also
# the only one of the two volumes the `cx` account can write to — /volume1 is
# root-owned. Trust `df -h /volume2` over DSM's number: btrfs subvolume metadata
# inflates what the UI reports.
REMOTE_DIR=/volume2/docker/cxtasks
DOCKER=/usr/local/bin/docker
PORT=18870
TAILNET_IP=100.88.68.22

say() { printf '\n==> %s\n' "$1"; }

say "building cxtasks-server for linux/amd64 (on this Mac, never on the NAS)"
docker buildx build \
  --platform linux/amd64 \
  -f "$REPO/deploy/nas/Dockerfile" \
  -t cxtasks-server:latest \
  --load \
  "$REPO"

say "preparing $REMOTE_DIR on the NAS"
# The data dir starts EMPTY and stays that way across redeploys. An adopt is a
# deliberate act performed by a paired client, never a side effect of shipping.
ssh "$NAS" "mkdir -p $REMOTE_DIR/data $REMOTE_DIR/secrets && chmod 700 $REMOTE_DIR/secrets"

say "installing tokens (generated on the NAS; values never cross this terminal)"
ssh "$NAS" "
  for t in full-token board-token; do
    f=$REMOTE_DIR/secrets/\$t
    if [ ! -s \"\$f\" ]; then
      # Created with the right mode BEFORE anything is written into it.
      # The server refuses a token under 32 characters or one that is
      # group/other-readable, so this has to be right the first time.
      ( umask 077; openssl rand -hex 32 > \"\$f\" )
      chmod 600 \"\$f\"
      echo \"  generated \$t\"
    else
      echo \"  \$t already present, left alone\"
    fi
  done"

say "shipping the image (~40 MB, gzipped over the LAN)"
docker save cxtasks-server:latest | gzip -1 | ssh "$NAS" "gunzip | $DOCKER load"

say "installing the compose file"
# `ssh cat >` rather than scp: this NAS has no sftp subsystem, and scp fails
# with a bare "Connection closed" that reads like a network problem.
ssh "$NAS" "cat > $REMOTE_DIR/docker-compose.yml" < "$REPO/deploy/nas/docker-compose.yml"

say "starting"
# Build first, stop second, up third — the sequence the house rules insist on,
# because stopping before a failure turns a bad deploy into an outage. Here the
# image is already built and loaded, so `up -d` is the only moving part.
ssh "$NAS" "cd $REMOTE_DIR && $DOCKER compose up -d"

say "verifying"
sleep 4
ssh "$NAS" "$DOCKER ps --filter name=cxtasks-server --format '  {{.Names}}  {{.Status}}  {{.Ports}}'"

# From HERE, over the tailnet — which is the path that actually has to work.
if curl -sf -m 10 "http://$TAILNET_IP:$PORT/health" | grep -q '"ok":true'; then
  echo "  /health answers over the tailnet:"
  curl -s -m 10 "http://$TAILNET_IP:$PORT/health" | sed 's/^/    /'
else
  echo "  !! /health did not answer at http://$TAILNET_IP:$PORT" >&2
  ssh "$NAS" "$DOCKER logs cxtasks-server --tail 30" >&2
  exit 1
fi

# And the LAN must refuse it. This is the exposure claim, so it is checked
# rather than asserted.
if curl -sf -m 5 "http://192.168.4.22:$PORT/health" >/dev/null 2>&1; then
  echo "  !! reachable on the LAN — the port publish is wrong" >&2
  exit 1
fi
echo "  LAN correctly refuses it (loopback publish + userspace tailscaled)"

cat <<NOTE

  Canonical node is up but NOT seeded. It holds no data and nothing points at
  it yet — pairing is a separate, deliberate act.

  Board token, for the phone:
    ssh $NAS "cat $REMOTE_DIR/secrets/board-token" | pbcopy
NOTE
