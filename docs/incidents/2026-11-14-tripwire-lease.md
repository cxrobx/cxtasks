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
