# A second executor host: `cxtasks-runner`, in a container, on the NAS.
#
# Built on the Mac and shipped as an image, for the same reason the canonical
# node is (see ../Dockerfile): compiling this workspace on a 4-core Celeron
# takes the better part of an hour and evicts the page cache every other service
# on the box is using.
#
# # Why the runtime image is node and not debian-slim
#
# The canonical node ships a Rust binary and needs only glibc. This one has to
# SPAWN `claude`, which is a Node program — so node is a runtime dependency
# rather than a build one, and installing it on top of debian-slim would produce
# the same image with more steps.
#
# # No Codex
#
# Deliberate, and not a gap. `codex` on PATH is a node shim whose real
# dependency is a logged-in session tied to a machine (gotcha #19, #20), and
# there is nothing to gain from a second unattended agent on a host whose whole
# point is a narrow, read-only capability. The host declares `claude` and the
# capability-generated drain SQL then makes a Codex task structurally
# unclaimable here — no special case anywhere.

FROM --platform=linux/amd64 rust:1-bookworm AS build
WORKDIR /src

# The whole workspace: `cxtasks-core` is a path dependency and cargo needs every
# member's manifest to resolve the graph, `src-tauri` included even though it is
# never built here.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src-tauri ./src-tauri

# BOTH binaries, and they must end up in ONE directory. `writeback_binary()`
# resolves `cxtasks-mcp` via `sibling_binary`, so a run that cannot find it
# beside the runner gets no write-back tools — which presents as an agent that
# does its work and then silently fails to record it (invariant 18).
#
# `--locked` so a deploy can never quietly resolve a different dependency tree
# than the one the tests passed against.
RUN cargo build --release --locked -p cxtasks-core --bin cxtasks-runner --bin cxtasks-mcp

FROM --platform=linux/amd64 node:24-bookworm-slim

# git: mirrors are materialized with `git clone --shared`.
# openssh-client: unused today (the Mac pushes TO here), kept because the
#   alternative when it is wanted is a rebuild during an incident.
# tini: PID 1 gets no default signal handlers, so without it `docker stop`
#   waits the full ten seconds and then SIGKILLs a running agent.
RUN apt-get update \
 && apt-get install -y --no-install-recommends git openssh-client tini ca-certificates \
 && rm -rf /var/lib/apt/lists/*

RUN npm install -g @anthropic-ai/claude-code && npm cache clean --force

COPY --from=build /src/target/release/cxtasks-runner /usr/local/bin/cxtasks-runner
COPY --from=build /src/target/release/cxtasks-mcp    /usr/local/bin/cxtasks-mcp

# ── the $HOME that must stay empty ──────────────────────────────────────────
#
# This is a security property, not tidiness. Invariant 7 rests on
# `--disallowedTools` beating the settings file — but `--allowedTools` is
# ADDITIVE over `~/.claude/settings.json`, and Chris's holds 175 rules including
# `Bash(bash:*)` and `Bash(git push:*)`. A fresh $HOME has none, which is what
# makes the tier ceilings real here.
#
# Shipped as an explicit empty block rather than an absent file, so the property
# is asserted rather than incidental — and `cxtasks-runner --health` FAILS if
# anything ever grows one, which is how a mounted-over home is caught.
#
# Its own directory, never a shared `~cx/.claude`: three containers sharing one
# `.claude/projects` tree is a documented session-lock hazard on this box.
ENV HOME=/home/cxrunner
RUN mkdir -p $HOME/.claude \
 && printf '{\n  "permissions": {\n    "allow": [],\n    "deny": []\n  }\n}\n' > $HOME/.claude/settings.json \
 && chown -R node:node $HOME

# `paths.rs` owns every shared path and CXTASKS_DATA_DIR moves all of them
# together. This one points at the SAME directory the canonical node uses —
# see the compose file for why that is safe here and not in general.
ENV CXTASKS_DATA_DIR=/data
ENV CXTASKS_MIRRORS_DIR=/mirrors

# Declared capability, read by `HostCapability::declared_for_this_host` on first
# start ONLY. After that the row in `hosts` is authoritative and a restart does
# not re-assert these — so a host downgraded from the app stays downgraded.
ENV CXTASKS_HOST_ID=cxtasks-nas
ENV CXTASKS_HOST_LABEL="the NAS"
ENV CXTASKS_HOST_WORKSPACES=mirror
ENV CXTASKS_HOST_MAX_TIER=read-only
ENV CXTASKS_HOST_EXECUTORS=claude
ENV CXTASKS_HOST_MAX_CONCURRENT=1
# Leave half the daily token pool for the machine Chris is sitting at. A
# ceiling, never a second budget — see `runner::gate`.
ENV CXTASKS_HOST_HEADROOM_PCT=50
ENV CXTASKS_HOST_PRIORITY=100

# Sonnet, not Haiku: inference is remote so the container's CPU is irrelevant,
# and a read-only exploration's entire deliverable is a written brief. A weaker
# model produces a confidently wrong one, which costs more to discover than the
# run saved.
ENV CXTASKS_BG_MODEL=claude-sonnet-5

# 1536, against a 2 GiB container limit. Node's default heap is a fraction of
# HOST memory, which in a container means it happily grows past the cgroup limit
# and gets OOM-killed — presenting as an agent that dies mid-run with no error.
ENV NODE_OPTIONS=--max-old-space-size=1536

USER node
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/cxtasks-runner"]
