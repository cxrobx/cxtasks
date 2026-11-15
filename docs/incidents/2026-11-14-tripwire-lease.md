# Tripwire result discarded after a foreign health diagnostic

## Confirmed incident

On November 14 UTC, T651's assigned Mac mini started check run
`1662b8d7-73e8-4a2f-9a32-e31a036873a2` at 09:07:09 and took its local
runtime lease. At 09:07:12 the NAS independently claimed its replica and
recorded a missing-owner error. At 09:07:17 reconciliation preferred the
NAS's later whole-row runtime image, erasing the mini's lease. The mini's
09:08:08 TRIPPED result was consequently discarded. Its finding was real.

The runtime had two errors. A 360-minute cadence with two failures backs off
to 1,440 minutes: the apparent 21-hour stall was part of a 24-hour backoff,
not proof of a permanently dead runner. A November 15 06:32 UTC manual check
succeeded and left an open sticky trip. This fix does not complete/rearm
T651 or apply its proposals. Other November 13–14 discards exist in the
logs; this ordering is established for the named run, not all three discards.

## Failure class

A replica-local lock was mistaken for distributed exclusion, and an observer
wrote the same mutable row as the executor. A later diagnostic could erase a
valid attempt and penalize it with backoff. Separately, resetting runtime
history made an old task look inactive since creation; the repeated orphan-note
shortcut could sustain the diagnosis. Run success was persisted before result
acceptance, hiding the discard from consumers.

## Fix and bounds

Health warnings now append independent, deduplicated comments. They never
claim or modify the owner's runtime or task state. Their deterministic identity
includes the exact spec, assignment and clock inputs; the shared task query
derives current warning visibility from that identity. Open warnings surface
in Open/Today and task detail; done monitors remain in Monitors. A real answer,
reset, request or changed spec/assignment expires a stale observation even if
it arrives late. Visibility does not authorize background work through an armed
gate. Repeated warnings create no fake failures and impose no retry backoff.

Reset starts a fresh inactivity clock. Candidate scans filter all due tasks
before limiting executable work so old orphans cannot starve local checks.
Claims enforce assignment/spec; settlement rechecks assignment, cancellation,
spec and the existing lifecycle/lease protections. Agent result acceptance and
the run receipt commit together. Discards have their own terminal status and
keep the full report and token usage; settlement failures cannot report success.

No schema/protocol migration is needed. This is not a new distributed execution
lock: two genuinely eligible unassigned executors still use replica-local
leases, and general whole-row user-edit/request races and equal-second conflict
ties retain the existing sync limitations. Older runners can still write the
old diagnostic, so protection requires replacing every reporting runner.

## Regression evidence

`runner::tripwire::tests` drives eight two-replica warning/check interleavings
(warning before/after claim, both push orders, settlement before/after sync),
replays stale warning images and checks convergence, accepted trips, zero added
failures and sticky behavior. Other regressions cover warning visibility,
deduplication, fresh reset/request clocks, returning owners, more than 200
orphans before executable work, cancellation/reassignment/spec edits, durable
discarded receipts and rollback when receipt persistence fails. Existing DB
lease reclaim, lifecycle, monitor, sticky and transaction tests remain in place.
UI tests cover the health warning and hold badge. All use synthetic tasks and
throwaway databases, with fake executors where needed; no paid check calls.

Adversarial review caught the initial warning visibility and scan-starvation
gaps; both were fixed and encoded before release. Final validation and host
installation evidence are recorded in the release follow-up.

## Release evidence — November 15, 2026

Source commit: `5556b51` (clean tree at each build). No origin push was made.
The laptop's clean `ac930cc` checkout was fast-forwarded using a local Git
bundle; its signed build ran in its own logged-in Aqua session. Its temporary
build LaunchAgent was unloaded afterward, with the non-secret log retained at
`/tmp/cxtasks-tripwire-laptop-build.log` and its previous app bundle retained
at `/tmp/CXTasks-before-tripwire-5556b51.app`.

Validation: 1,066 Rust workspace tests passed (two existing ignored tests),
549 UI tests passed across 48 files, TypeScript passed, the Linux all-targets
compile check passed, and both `agent-check-sim.sh` and `two-host-sim.sh` passed.
The agent simulation's two stale `inbox` assertions were updated to the `todo`
vocabulary already established in `ac930cc`; an accepted-result receipt
assertion was added. No assertion was removed to hide a failure. Validation
used throwaway databases and fake executors, not paid check calls.

Final review also caught warning visibility leaking into `/next` admission.
The strict armed gate was restored there, including project-board next-work
selection (which calls the same function), and a regression now proves a
warned gate is visible without becoming executable work.

Installed protection:

| Host | Updated components | Verification |
| --- | --- | --- |
| Mac mini | app, runner, MCP; runner and shared HTTP MCP restarted | `scripts/ship.sh`, runner health `ok`; installed bundle matched build, strict signature verification and `flags=0x0(none)`; launchd paths point into `/Applications/CXTasks.app` |
| Laptop | app, runner, MCP; app relaunched and runner restarted | local Aqua `scripts/ship.sh`, `LAPTOP_TRIPWIRE_SUCCESS=5556b51`, exit 0, runner health `ok`; installed binaries matched its own signed build; `flags=0x0(none)` |
| NAS | runner and its sibling MCP; canonical server | repository deploy scripts, runner health `ok`, live server `/health` answered over tailnet with schema 30/protocol 1/push enabled; LAN exposure check refused; running binaries matched built image bytes |

SHA-256 receipt prefixes (each installed binary was compared against its own
build, rather than comparing separately signed Macs to each other):

| Host | App | Runner | MCP | Server |
| --- | --- | --- | --- | --- |
| Mini | `60d4dba2d5be628a` | `9b66bd87dd41c5e8` | `e045df51a983c8df` | — |
| Laptop | `4f7919d80761ea54` | `585b81c8188a6a34` | `66e463e6ee7364d3` | — |
| NAS | — | `8f835b657769a6ac` | `dac38d553d18722f` | `0e7a0d5ef72986c6` |

No separate mini server LaunchAgent was loaded. Existing stdio MCP sessions
keep their already-running executable until reopened; the mini's shared HTTP
MCP was restarted. No live model check was triggered to validate the release.
No new cross-machine production task was created as a test: behavioral
acceptance is the synthetic two-replica harness plus the deployed binary and
health checks.

The live T651 row was read before and after installation: still `todo`,
`tripped`, with the original `2026-11-15 06:32:27` trip timestamp. It was not
completed, rearmed or used for testing, and its proposals were not applied.
The separate review-consumption mitigation was left intact. The shared
operating-model hunt list also gained the observer-overwrites-executor class
and its executable checks.
