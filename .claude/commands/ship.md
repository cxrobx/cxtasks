---
description: Build, re-sign and install CXTasks into /Applications, then relaunch it
---

Ship the current tree of this repo to /Applications. Chris has already asked for
it — don't ask for a go-ahead, and don't push (shipping is local; pushing is a
separate ask).

1. `git status --short`. If there are uncommitted changes, name them. Ship only
   work that is committed and verified: if the changes are this session's and
   finished, commit them first (stage by name); if they belong to another
   session or are half-done, stop and tell Chris rather than shipping them.
2. Run the tests, and stop on a failure — a failing build is not shipped:
   `npx tsc --noEmit -p . && npx vitest run`, plus
   `source "$HOME/.cargo/env" && cargo test` when anything under `crates/` or
   `src-tauri/` changed since the last ship.
3. Run `scripts/ship.sh` (timeout 600000 ms). It does every step of CLAUDE.md
   § "Shipping a build" and exits non-zero on any check that fails — the three
   bundled binaries, the `flags=0x0(none)` signature, the install, the runner
   restart and `--health`. Don't re-do its steps by hand, and if it fails, fix
   the cause it names rather than working around the check.
4. If `crates/cxtasks-server` changed, say so: the server LaunchAgent runs from
   `target/release`, outside the bundle, and the script doesn't touch it
   (gotcha #43).
5. Report in a few lines: the commit shipped, that the flags check passed,
   runner health, and anything skipped.
