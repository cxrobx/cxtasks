#!/usr/bin/env bash
# Prove the platform-neutral crates still build for Linux.
#
# This is the only check that verifies the thing the crate split exists for.
# Everything else — `cargo test`, the bundle, the app — runs on macOS and would
# stay green through a macOS-only call sneaking back into core. The crate
# boundary makes that a compile error; this script is what runs the compiler.
#
# Cross-compiling from the host is not enough: rusqlite's `bundled` feature
# builds SQLite from C in a build script, so a real Linux toolchain has to be
# present. Hence a container rather than `rustup target add`.
#
# Caches the registry and target dir in named volumes, so the first run is slow
# (~900 crates) and later ones are seconds.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE="${CXTASKS_LINUX_IMAGE:-rust:1-bookworm}"

exec docker run --rm \
  -v "$REPO":/repo \
  -v cxtasks-linux-target:/target \
  -v cxtasks-linux-registry:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/target \
  -w /repo \
  "$IMAGE" \
  cargo check -p cxtasks-core -p cxtasks-server --all-targets --locked "$@"
