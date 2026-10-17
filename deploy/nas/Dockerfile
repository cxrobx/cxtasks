# The canonical node, built for the NAS.
#
# Built on the Mac and shipped as an image, never compiled on the NAS: that box
# is a 4-core Celeron, and a release build of this workspace there would take
# the better part of an hour and evict the page cache every other service is
# using.
#
# Two stages so the runtime image carries a binary and nothing else — no cargo,
# no registry, no source. rusqlite's `bundled` feature compiles SQLite into the
# binary, so the runtime needs only glibc.

FROM --platform=linux/amd64 rust:1-bookworm AS build
WORKDIR /src

# The whole workspace, because `cxtasks-server` depends on `cxtasks-core` by
# path and cargo needs every member's manifest to resolve the graph — including
# `src-tauri`, which is never built here. `.dockerignore` keeps `target/` and
# `node_modules/` out.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY src-tauri ./src-tauri

# `--locked`, so a deploy can never quietly resolve a different dependency tree
# than the one that passed the tests.
RUN cargo build --release --locked -p cxtasks-server

FROM --platform=linux/amd64 debian:bookworm-slim
# `tini` because the server is PID 1 in this container and PID 1 does not get
# default signal handlers — without it a `docker stop` waits the full ten
# seconds and then SIGKILLs, which is a hard stop in the middle of a write.
RUN apt-get update \
 && apt-get install -y --no-install-recommends tini ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/cxtasks-server /usr/local/bin/cxtasks-server

# Everything the server keeps lives under one root — `paths.rs` owns that, and
# `CXTASKS_DATA_DIR` moves all of it together or none of it.
ENV CXTASKS_DATA_DIR=/data
VOLUME ["/data"]

# Inside the container this is fine: the exposure decision is the host-side port
# publish, and on this NAS binding the host side to loopback makes it reachable
# over the tailnet and refused on the LAN.
ENV CXTASKS_BIND=0.0.0.0:8787
EXPOSE 8787

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/cxtasks-server"]
