#!/usr/bin/env bash
# Build the executor image on THIS Mac and ship it to the NAS.
#
#   ./deploy/nas/runner/deploy.sh            # build, ship, load — do not start
#   ./deploy/nas/runner/deploy.sh --start    # ...and bring it up
#
# WHAT THIS DEPLOYS
#
# A second host that runs unattended agents. It is declared read-only, mirror-
# only, one-at-a-time, with half the daily token pool reserved for the Mac —
# but it is still a machine that spends money without anybody watching. Hence
# the two-step: loading the image changes nothing, and `--start` is a separate
# decision.
#
# Never `docker build` on the NAS. It is a 4-core Celeron; a release build of
# this workspace there takes the better part of an hour and evicts the page
# cache every other service is using.
set -euo pipefail
cd "$(dirname "$0")/../../.."

NAS="${CXTASKS_NAS:-nas}"
DOCKER="/usr/local/bin/docker"
REMOTE_DIR="/volume2/docker/cxtasks/runner"
IMAGE="cxtasks-runner:latest"
START=0
[ "${1:-}" = "--start" ] && START=1

say() { printf '\n==> %s\n' "$1"; }

say "checking the tests pass before shipping anything"
source "$HOME/.cargo/env" 2>/dev/null || true
cargo test --workspace -q >/dev/null
./scripts/check-linux.sh >/dev/null
echo "    workspace green, core still builds for Linux"

say "building for linux/amd64 (on this Mac, never on the NAS)"
docker buildx build --platform linux/amd64 \
  -f deploy/nas/runner/Dockerfile \
  -t "$IMAGE" --load .

say "shipping the image"
# Straight down the pipe rather than `docker save` to a file then `scp`: the
# tarball is ~1 GB and writing it twice on a volume that is 66% full is a lot of
# I/O for no benefit.
docker save "$IMAGE" | gzip -1 | ssh "$NAS" "gunzip | $DOCKER load"

say "placing the compose file and the directories it mounts"
ssh "$NAS" "mkdir -p $REMOTE_DIR /volume2/docker/cxtasks/runner-home /volume2/docker/cxtasks/mirrors"
# `ssh cat >` rather than scp: this NAS has no sftp subsystem, so scp fails
# with a bare "Connection closed" that reads like a network problem — and it
# does so AFTER the image has already shipped, which is the worst place in
# this script to be confused. The canonical node's deploy.sh learned this
# first; this one had to learn it twice.
ssh "$NAS" "cat > $REMOTE_DIR/docker-compose.yml" < deploy/nas/runner/docker-compose.yml

# The mounted home SHADOWS the empty settings.json the image ships, so the
# property invariant 7 rests on has to exist on the host side too. Seeded only
# when absent — overwriting would discard a deliberate change, and `--health`
# is what actually enforces this either way.
ssh "$NAS" "
  f=/volume2/docker/cxtasks/runner-home/.claude/settings.json
  if [ ! -s \"\$f\" ]; then
    mkdir -p \"\$(dirname \"\$f\")\"
    printf '{\n  \"permissions\": {\n    \"allow\": [],\n    \"deny\": []\n  }\n}\n' > \"\$f\"
    echo '  seeded an empty permissions file'
  else
    echo '  permissions file already present, left alone'
  fi"

if [ "$START" = "0" ]; then
  cat <<DONE

Image loaded. NOTHING is running.

  Start it:   ssh $NAS "cd $REMOTE_DIR && $DOCKER compose up -d"
  Check it:   ssh $NAS "$DOCKER exec cxtasks-runner cxtasks-runner --health"
  Dry run:    ssh $NAS "$DOCKER exec cxtasks-runner cxtasks-runner --dry-run"
  Logs:       ssh $NAS "$DOCKER logs cxtasks-runner --tail 50"

Before the first real run:

  1. \`--health\` must print ok. It asserts this container's \$HOME grants no
     permission rules, which is what invariant 7 rests on here.
  2. \`--dry-run\` prints the drain SQL this host generates, the workspace it
     would use and the exact prompt. Read the prompt.
  3. \`claude\` in the container needs a login. Two details, both easy to get
     wrong, and neither fails in a way that says so:

       ssh -t $NAS "$DOCKER exec -it cxtasks-runner claude auth login"

     \`ssh -t\` — without it there is no pseudo-terminal for \`exec -it\` to
     attach to, and the error is "the input device is not a TTY", which reads
     as a docker problem rather than an ssh one.

     \`auth login\`, not \`login\` — Claude Code 2.x has no top-level \`login\`
     subcommand, so \`claude login\` is parsed as a PROMPT: it starts an
     interactive session and answers the word "login" instead of performing
     one. It looks like it worked, because the session greets you by name.

     Verify non-interactively — this prints JSON and needs no terminal:
       ssh $NAS "$DOCKER exec cxtasks-runner claude auth status"

     Credentials land on the mounted home, so they survive a redeploy.
  4. Mirror a repo to it before expecting any task to route there — a host with
     no mirror of a repo is refused for that repo, by design.
DONE
  exit 0
fi

say "starting"
ssh "$NAS" "cd $REMOTE_DIR && $DOCKER compose up -d"
sleep 5
ssh "$NAS" "$DOCKER exec cxtasks-runner cxtasks-runner --health" || {
  echo "!! the container is up but unhealthy — read the reason above" >&2
  exit 1
}
say "up and healthy"
