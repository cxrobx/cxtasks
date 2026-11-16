# Direct Ask on the Mac mini

The iOS Ask tab uses `https://cxs-mac-mini.tailbff47a.ts.net:8788` directly.
The board, widgets, capture outbox and reminders retain the paired NAS URL.
`AskAPIClient` pins the mini hostname and port, reads the current phone board
credential on each call, reuses its connection, and never retries a request on
the NAS. A mini 401 does not unpair the working board.

This retains Ask's task-backed conversation model: new requests are filed as
tasks; follow-ups run on the same task with its existing context. Progress is
polled from the mini's local run/event tables. This is not a native persistent
CLI session or token-by-token answer stream. A runner may take up to its normal
20-second drain interval to pick up the work. The existing runner remains the
only executor and enforces host pins, tool tiers, cancellation and budgets.

The mini's normal sync sends tasks and results to the NAS. With the NAS offline,
chat connectivity and local execution can continue as long as the mini has its
existing replica, available task-number lease and budget. Board changes and
results appear on other clients after reconciliation. A task explicitly pinned
to another host still waits for that host and replication.

## Install

Build `cxtasks-server` from a clean checkout when the working tree contains
unrelated edits. Install the NAS's current **board** token as
`~/.config/cxtasks/ask-board-token` (0600), transferring file-to-file over SSH,
never through chat or logs. The full token is generated separately on the mini;
never copy the NAS full token for this service. The iOS app retains its existing
pairing; no phone credential changes are required.

Run `scripts/install-ask-server.sh`, optionally setting `CXTASKS_SERVER_BIN`.
It signs and installs a stable executable under `~/.local/libexec/`, registers
`com.cxtasks.ask-server` in the Aqua session, binds only `127.0.0.1:8788`, and
publishes it with Tailscale Serve HTTPS. This is launchd-managed, not a dev
server, so the dev reaper does not manage its lifetime. Port 443 is untouched.

After a board-token rotation, replace the mini's board-token file and restart
`launchctl kickstart -k gui/$(id -u)/com.cxtasks.ask-server` as well as updating
the phone pairing. Token values never belong in this repo.

## Verify / undo

Probe `/health` through the HTTPS hostname, verify unauthenticated `/rpc`
returns 401, and make a read-only authenticated board call using the token file.
Verify the listener is loopback-only. iOS routing tests cover every Ask verb,
a changed token, unpairing, unavailable mini and no fallback/double send. The
Ask UI fixture uses the DEBUG-only `-cxtasksAskAPIURL` loopback override.

Undo the service with `launchctl bootout gui/$(id -u)/com.cxtasks.ask-server`
and `tailscale serve --https=8788 off`. Reinstall the previous iOS build to
restore Ask through the NAS. Keep the shared task database and NAS unchanged.

## Validation (2026-11-16)

- 28 iOS tests passed (AskRoutingTests + existing AskTests).
- Four UI checks passed across iPhone 18 Pro and iPhone Duo. Separate real
  `cxtasks-server` fixture databases recorded two requests on the execution
  host and zero on the board. No agent or production task was started.
- Installed endpoint: valid tailnet HTTPS; unauthenticated RPC 401; board
  `view_counts` succeeds; board `reset_budget_window` refused with 403.
- Listener verified at `127.0.0.1:8788` only. Release server built from clean
  core commit `9d6617a`, excluding unrelated work in progress.
- Signed Duo release build succeeded using existing local signing profiles,
  without provisioning updates or Apple account API calls.
