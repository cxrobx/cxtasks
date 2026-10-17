# CXTasks — multi-host execution and the iOS client

The spine of the work, written down because it otherwise exists only in one
session's context. Phases are sequential; each is independently valuable and
independently abandonable, and stopping after any of them leaves a working
system rather than a half-migration.

**Current position: every phase is built. Phases 0-2 are running; Phase 3's
second host is packaged and deliberately not started.** The Mac remains a
complete, standalone database throughout — that was the first requirement and
nothing here changed it.

## The four requirements this serves

1. **The local Mac app keeps working standalone, always.** Sync is a switch, off
   by default; turning it off returns the app to exactly what it is today.
2. **A canonical node can be enabled** so the phone works lid-shut.
3. **It must absorb a Mac mini later** as an always-on host — configuration, not
   a refactor.
4. **A complete native iOS app**, not a port of the three-pane desktop grid.

## Settled decisions

| Question | Decision |
|---|---|
| Source of truth | Canonical node with a **full Mac replica**. The Mac holds a real, complete, standalone DB at all times. |
| iOS offline | **Read cache + offline capture.** No local database. |
| iOS scope | A complete native app with a mobile-native IA. |
| Remote execution ceiling | **Read-only for now** — expressed as a declared host capability, never baked into code. |

## Architectural principles

1. **Local-first, remote-optional.** With sync off, behaviour is byte-identical
   to today. The Mac is never *dependent* on a peer.
2. **Canonical is a role, not a machine.** One config-driven binary.
3. **Hosts declare capabilities; work routes by capability match.** A task derives
   a `RunRequirement`; a host declares a `HostCapability`; routing is a pure
   function. Adding a host is a registry row plus a deployment.
4. **Push intents, pull post-images.** An intent executes exactly once, on one
   node, through `tasks::update`, so derived writes are computed correctly.
5. **Merge whole rows, never fields.** A row is internally consistent because
   only `tasks::update` produces one. Field-level LWW is not a finer-grained row
   merge; it is a machine for manufacturing illegal rows.
6. **Read-only runs need no checkpoint** — `ToolTier::ReadOnly.needs_checkpoint()`
   is false, so the host-local git problem *disappears* rather than needing a
   solution.

---

## Phase 0 — the enabling split ✅

Platform-neutral core, zero behaviour change.

- `crates/cxtasks-core` (no tauri/objc2/window-vibrancy) + `src-tauri` unmoved.
  A **workspace, not feature flags**: under `#[cfg(feature)]` the next
  unconditional macOS call only breaks under a command nobody runs by reflex.
- `paths.rs` gets ONE root override (`CXTASKS_DATA_DIR`), never per-path.
- `host.rs` — machine identity as a **file**, never `app_meta` (a database gets
  copied between machines; two would claim one identity).
- **Schema v20**: `task_runs.host`, `task_runs.heartbeat_at`. Nothing backfilled
  — NULL means "predates multi-host", which is a value and not a gap.
- Host-scoped sweep (three rules, incl. *never* reclaim a foreign row with no
  heartbeat), per-host concurrency, `ensure_local` guarding `run_diff` /
  `restore_checkpoint`.
- `scripts/check-linux.sh` is the only thing that proves the split worked.

## Phase 1 — one verb table, a server, and the iOS app

### 1a — `api::dispatch` ✅
All 83 commands collapse onto one `match`. Tauri commands and the server both
route through it, so a second implementation has nowhere to live. **Phase 2's
size depends entirely on this.**

### 1b — `cxtasks-server` ✅
axum over the verb table: `/health` (auth-optional, so a client can tell
"sync is off" from "not paired"), `/rpc`, two-scope bearer auth. TZ required and
refused if unset. `/sync/*` and `/attachments/*` landed in Phase 2. Still to add
here: `/events` (SSE).

### 1c — the iOS app 🔨 (current)
Done: wire models with three-state `Patchable`, design tokens, Geist Mono,
API client, offline outbox **and** read cache, five tabs, task detail, run
detail with folded timeline, queueing with scope refusal, a four-phase e2e
harness that asserts on SQLite.

Open: pairing/settings, tags/comments/attachments, GSD board, APNs, App Intents,
widgets, Share Extension, iPad.

## Phase 2 — the canonical role ✅

**The dangerous phase, and it is dangerous because it is the interesting one.**
Three consistency stories in one system is how a task app becomes a bespoke CRDT
project. The mitigations are structural — hold the implementation to being
boring:

- **Whole-row merge**, never field-level.
- **Snapshot pull**, not a delta. The DB is 2.7 MB; a delta cannot see a hard
  delete, `datetime('now')` is second-resolution so a `>` cursor skips rows, and
  four tables have no `updated_at` at all. Design the `?since` cursor; let the
  server always answer "full" in v1.
- **One transaction**: apply locally through `api::dispatch` AND append to
  `sync_outbox` together, in `cxtasks.db`, or a crash drops a change.
- **No merge logic anywhere.**

> ### What was actually built, and where this section is now wrong
>
> The plan above said **every** canonical verb is recorded as an intent and
> replayed on the peer, with the sweep covering only rows no intent touched.
> That is not what shipped, because the two halves conflict — and they conflict
> silently:
>
> * `create_task` replayed on the peer makes the peer generate its **own** uuid
>   and claim its **own** T-number, so the swept post-image of the local row
>   then collides with the row that is supposed to *be* it. It announced itself
>   as `UNIQUE constraint failed: tasks.short_id`, which was the lucky version.
> * `complete_task` on a recurring task materializes the next instance at
>   **both** ends. The duplicate future task is silent.
>
> A post-image can express every state change there is. The one thing it cannot
> express is a row that is **gone**, since a swept table reports rows that
> exist. So:
>
> **State travels as post-images. Deletions travel as replayed verbs.**
>
> `api::DELETES_ROWS` is that list, and it is **discovered rather than
> declared**: a test runs every verb against a seeded database and fails naming
> any that made a synced table shrink without being listed. It found ten,
> including `rename_tag` — which removes a row only when the rename *merges* —
> and `set_task_tags`, which replaces a whole set. Neither was guessable.
>
> Two more corrections earned the same way:
>
> * The sweep cursor is **`>=`, not `>`**. Every watermark is `datetime('now')`,
>   which is second-resolution, so a strict cursor permanently skips rows
>   written in the watermark's own second. Same reason a delta snapshot was
>   rejected; it applied to the sweep too.
> * The sweep emits in the **applier's** table order, and foreign keys are
>   deferred inside an apply. `tasks.section_id` references `sections`, and
>   `projects.parent_id` references itself — so ordering that satisfies the
>   finished batch does not satisfy every step of it.

Also: the skew sweep (a leftover pre-sync MCP writes without appending to the
outbox → silent data loss); T-numbers via **lease blocks** (MAX() races produce
duplicate T-numbers, unrepairable under invariant 14); first-enable **adopt or
refuse loudly** (merging two grown databases is a data-loss generator); hard
deletes only in the canonical role; attachments designed now, while they still
store zero bytes.

`cxtasks-mcp` stays local, always — and note it writes through `db::tasks::*`
directly rather than the verb table, so it never appends an intent. **The skew
sweep is load-bearing for the app's busiest entry point, not a defence against a
stale binary.**

### What Phase 2 still owes

* ~~**Attachments.**~~ Done. Metadata always synced as an ordinary post-image;
  the **bytes** now travel over `GET`/`POST /attachments/:stored_name` plus
  `GET /attachments/missing` — reads at board scope, writes full only — driven from
  `sync::blobs::reconcile_bytes` at the end of every reconcile. **Bytes are
  canonical on the peer and a local store is a CACHE** — schema v22 adds
  `task_attachments.sha256` as the content address, `store_bytes` verifies
  before writing and renames out of `attachments/.incoming/` so a file that
  exists is complete, and `prune_orphaned_files` became host-local in effect
  (a synced host gives an untracked file an hour's grace, because a row and
  its bytes stop arriving together). Full argument: invariant 23 in
  `.claude/rules/database.md`. Found on the way: `delete_task_attachment` was
  missing from `api::DELETES_ROWS`, so deleting an attachment never travelled
  and the next pull resurrected it.
* ~~**Reconciling on a timer.**~~ Done (2026-09-01). `sync::auto` reconciles
  every **5 minutes** while the app is open — its own OS thread in `lib.rs`,
  with its own connection, because `HttpPeer` is blocking with a 120-second
  timeout and holding `AppState.db` across an unreachable peer would freeze
  every Tauri command. **This was the phase's real hole**, and it had no
  symptom: a button reconciles only when pressed, so the Mac was found seven
  days behind twice with nothing erroring and Settings still reading "Syncing
  with the-nas". So the loop shipped with the thing that makes its absence
  visible — `sync_last_attempt_at` / `sync_last_error`, shown in Settings and
  by `sync-control.sh status`, because a stale pull time alone cannot tell
  "nothing is trying" from "every attempt is refused". Rejected: a frontend
  `setInterval` (WKWebView throttles timers in a hidden window, and ⌘W *hides*
  this app — sync would stop in exactly the state where the app looks closed
  and isn't) and a configurable interval (a cadence with a slider gets turned
  down during one debugging session and left there). The shell's way in is
  `cxtasks-runner --sync-now` / `sync-control.sh now`, which exists because
  `sync_now` is `api::HOST_LOCAL` and rightly refused over `/rpc` — that
  refusal had left a shell with no route at all.
* **The cutover.** Pairing the real Mac to the NAS node. Rehearsed end to end by
  `scripts/sync-dress-rehearsal.sh`, which pairs a `.backup` copy to a throwaway
  canonical node and applies the same five gates. Reversible from Settings, or
  `scripts/sync-control.sh`.

### What exists now, that this document predates

* Schema **v21**: `sync_outbox` (AUTOINCREMENT — a reused sequence number
  replays two intents out of order) and `short_id_leases`. **v22**:
  `task_attachments.sha256`, the content address that makes the local
  attachment store a cache rather than a second master.
* `crates/cxtasks-core/src/sync/`: `tables` (classification, exhaustive by
  test), `snapshot`, `apply`, `sweep`, `outbox`, `leases`, `pair`, `reconcile`,
  `http`, `state`, `auto` (the timer's two entry points — `now` errors with no
  peer, `pass` returns `Ok(None)`, and both record the attempt).
* Server: `/sync/{info,snapshot,push,lease,seed,reconcile}` full scope only;
  `/attachments/{missing,:stored_name}` readable at board scope, writable only
  at full.
* `CXTASKS_ROLE=canonical` makes a node canonical by configuration.
* The canonical node runs on the NAS at `/volume2/docker/cxtasks`, published to
  **127.0.0.1:18870** — tailscaled runs `--tun=userspace-networking` there, so a
  loopback publish is reachable over the tailnet and refused on the LAN.
* `scripts/`: `sync-dress-rehearsal.sh`, `sync-control.sh`,
  `deploy/nas/deploy.sh`.

## Phase 3 — a second executor host ✅

`bg_locus` was the wrong shape; **capability matching** replaced it.
`RunRequirement::for_task` (derived, stored nowhere) × `HostCapability`
(declared vs observed) → `routing::satisfies`. `max_tier` is a **policy
ceiling, not a probe** — the NAS is ReadOnly because Chris chose that.

Fails closed two ways: `tasks.bg_remote_allowed` in a column no earlier binary
reads, and capability-generated drain SQL. Budget is ONE pool with per-host
**ceilings**. Mirrors transport a checkpoint *commit*, never a dirty tree.
Preflight split into `admit` (queue time) and `ready` (execute time), and a
capability miss refuses while a liveness miss warns.

### What was actually built, where it departs from the plan

* **No `run_notices` table.** The plan wanted a durable outbox so the app could
  say "4 runs finished on the NAS while you were away". `task_runs` already is
  one — it replicates whole-row and carries `host` and `finished_at` — so one
  `app_meta` cursor gives the same feature with no second table to classify,
  sweep, purge and keep in step. `db::runs::finished_elsewhere_since`.
* **The container shares the canonical node's data directory** instead of
  running a sync client. "SQLite over a network filesystem is not an option" is
  about SMB/NFS, where WAL needs shared memory the protocol cannot provide; two
  containers on one host over one local volume is the ordinary multi-process
  case, and the same one the app, the MCP and the runner on the Mac have shared
  since the first version.
* **`hosts.mirrors` is written by the PUSHER, not the holder** — the one field a
  host does not write about itself. A bare repo is named by a hash of its source
  path, so the NAS can enumerate its mirrors and still not know that
  `web-a1b2c3d4.git` is `~/Projects/artisttracker/web`.
* **`admit` never writes a host row.** A row means "a machine that runs tasks",
  and the only process entitled to write one is the runner. `cxtasks-server`
  calls `admit`; a row from it would claim to do everything and drain nothing.
  An empty registry falls back to this machine's declared capability without
  persisting it.
* **No `cpus:` on the NAS.** That kernel fails container CREATE on NanoCPUs,
  after the network exists, so it reads as a networking problem. Memory limits
  work; CPU quota does not.
* **`--health` asserts the container's `$HOME` grants no permission rules.**
  Invariant 7 holds there because a fresh `$HOME` has none — and against
  Chris's real home the check reports **175** rules, not the ~70 the older docs
  record.
* **`satisfies` keeps the most SPECIFIC refusal, not the last tried.** A host
  declaring both Mirror and Detached reported "no workspace kind fits" where
  "has no mirror of /repo yet" was available.
* **The mirror fingerprint hashes content.** The first version used HEAD plus
  `git status --porcelain`, which names changed files without their contents —
  so editing an already-dirty file, which is what working in a repo looks like,
  was invisible.

### Verification

`scripts/two-host-sim.sh` — two runners on one Mac against a throwaway
database, a throwaway repo and a local-path "remote". **20 checks, zero
network, zero tokens**, because every assertion is about what the two hosts
*claim*, which is where the interesting bugs are. `KEEP=1` leaves the scratch
root for debugging.

## Phase 4 — repo-less remote tasks ✅

`WorkspaceKind::Detached` as a **declared capability**, which the MacBook does
not list: an unattended run at `$HOME` gets unsupervised read over every client
repo, `~/.ssh` and the vault. Invariant 12 is satisfied differently rather than
weakened — a repo is a declared workspace, a container mount set is another,
`$HOME` is neither.

What replaces the repo gate is the **tool surface**: `--strict-mcp-config` plus
`tasks.bg_mcp_servers`, an allow-list of NAMES validated against
`mcp_registry::REGISTRY` and defaulting to empty. `WebFetch`/`WebSearch` stay
denied at every tier.

* **No "is detached" column, and that is the point.** A detached task is one
  with no `repo_path`, and every drain query that predates Phase 4 already
  carries `repo_path IS NOT NULL`. The existing guard IS the skew guard.
* **Names, never command lines.** If the column held commands, "which servers
  may this task use" would be "what may this task execute", and a task row
  would be a place to write arbitrary argv for an unattended process.
* **An unknown name is an error, never a silent drop.** A task filed asking for
  the vault that quietly runs without it reports confidently on nothing.
* A detached run's working directory is a scratch dir under the data-dir mount,
  which keeps "the mount set is the filesystem guard" true.

Schema: v23 (hosts, `bg_remote_allowed`, `task_runs.cancel_requested_at`),
v24 (`bg_mcp_servers`).

## Phase 5 — tripwires: a task that waits for a CONDITION

**Release 1 — script tripwires ✅ (2026-08-24, schema v25).** `defer_until`
waits for a date; a tripwire waits for a check. `tasks.tripwire = "script:<name>
[arg]"` names an executable in `tripwires/` (a NAME, never argv; `$1` is
untrusted data), `tripwire_every_minutes` is the cadence (NULL = manual),
`tripwire_on_trip = 'complete'` opts a pure gate task into auto-completing so
`blocked_by` releases. While `armed` the task hides exactly where a deferral
does; `tripped`/`error` both surface it. The attempt is a leased, CAS-recorded
transition on its own `tripwire_runtime` row (invariant 26), run by the
runner's free lane before the budget gate's return (gotcha #42). Solves T118
with zero token risk and proves hiding, leasing, notification, dependency
release, sync classification and the UI.

The phone has it too (`~/Projects/xcode/cxtasks` 38ebe66, 2026-08-25): the
five columns and the runtime row decode, an armed row wears the badge (red
once unchecked), and the detail screen sets, clears, re-arms and requests a
check through the same one-field patches. The script name is TYPED there —
`tripwire_scripts` is host-local, so a picker on the phone would offer files
it cannot see — and the runner's answer arrives on pull-to-refresh, since
the phone has no live bridge.

**Release 2 — agent tripwires ✅ (2026-09-02, no schema change).**
`agent:<condition>` is evaluated by a bounded, read-only `claude -p` — a
`BackgroundIntent::Check` run row that IS the tripwire lease. Built exactly
on the seams below; the departures from the design as written are: the agent
lane runs BELOW the budget gate's return (it costs tokens, unlike a script)
and takes at most ONE slot before the drain so neither lane starves the
other; a check's cwd is the task's repo (`Live`) or, on a host declaring
`Detached`, a throwaway workspace — the Mac does not, so an agent tripwire
there needs a `repo_path`; `UNSURE` is recorded as an ERROR (five in a row
surface the task) rather than as not-yet; the sub-cap key is
`bg_tripwire_daily_token_cap` and travels with the other budget keys; and a
check's `--model` is the task's own override when its executor is Claude,
else the global Claude default. `scripts/agent-check-sim.sh` is the dress
rehearsal — the real runner binary, a fake `claude`, zero tokens, both
verdicts, the sub-cap refusal and an UNSURE. The gate R1 set ("one script
tripwire tripped for real first") was NOT met when this was built: all 19
armed script tripwires had only ever recorded `not_yet`; leasing and sync
had been exercised for a week, the trip transition and its banner only by
tests and by the rehearsal. The design, as built:

* **Intent:** `BackgroundIntent::Check` (`"check"`), machine-only — not in the
  UI picker, not settable over MCP, refused by `queue_for_background`. Driven by
  `tripwire_runtime`, never by `status = 'queued'`, so a pre-R2 runner can
  neither produce nor drain one and the one-column argument (invariant 19)
  holds. `dispatch::history_block` / `reviewable_run` filter `is_check()` runs —
  a verdict is not a handoff.
* **Run row:** `runs::start_check` — `BEGIN IMMEDIATE`, one check in flight per
  host, under `max_concurrent`, no task status change; the run id IS the
  `lease_id`, and the lease expiry is the check timeout plus
  `ABANDON_GRACE_MINUTES`.
* **Admission:** in `agent_pass`, after `gate` is `Open` and taking at most one
  slot first so neither lane starves the other:
  `check_tokens_today + CHECK_TOKEN_CEILING ≤ app_meta['tripwire_daily_token_cap']`
  (5M) — the running check's whole ceiling is RESERVED so the sub-cap cannot be
  overshot. Claude only, until Codex reports usage incrementally.
* **Bounds:** 12 turns · 3M tokens · 10 min, each `min_nonzero(task/global,
  constant)` so a task's `bg_run_token_cap = 0` never disables the check
  ceiling. `ToolTier::ReadOnly` pinned, no checkpoint, no salvage turn. Cadence
  floor 60 min (5-minute checks ≈ 288 × ~100k ≈ 29M tokens/day for one task —
  the refusal says so).
* **Tool grant:** MCP mode `CXTASKS_CHECK_RUN=1` serves `CHECK_TOOLS =
  ["get_task"]`; `--allowedTools` = Read/Glob/Grep + `mcp__cxtasks__get_task`;
  extra servers must be registry `read_only: true` — a curated-list gate.
* **Prompt / verdict:** `dispatch::write_check_scratch` → `check-prompt.txt`
  (task context + condition + verdict tail, no history). `VERDICT: TRIPPED |
  NOT_YET | UNSURE` + `EVIDENCE:`; last `VERDICT:` line wins; UNSURE, no
  verdict, or any non-success = error → `db::tripwires::record` with
  `lease_id = run.id`.
* **Budget UI:** `KEY_TRIPWIRE_DAILY_TOKEN_CAP` in `SYNCED_META_KEYS`,
  `BudgetStatus.tripwire_tokens_today`, a `CapField` in `BudgetPanel`, a
  `--status` line. No schema change: `intent` is text, the cap is an
  `app_meta` key.
* **MCP:** `parse` accepts `agent:`; descriptions add the 60-min arithmetic,
  "prefer `script:` when a command can answer", `model:
  claude-haiku-4-5-20251001`, "prefer `surface` for `agent:`".

## What is built but not switched on

The NAS executor is **packaged and proven, and nothing is running**.
`deploy/nas/runner/deploy.sh` loads the image and starts nothing; `--start` is
a separate decision. Before a first real run: `--health` must pass, `--dry-run`
must print a prompt worth reading, `claude` in the container needs a login, and
a repo must be mirrored to it — a host with no mirror of a repo is refused for
that repo, by design.

---

## Rejected, so nobody re-invents them

| Rejected | Why |
|---|---|
| A change-journal / oplog table | ~105 write sites = 105 chances to forget; fails as "one field silently stops syncing". |
| Field-level merge | Manufactures rows `tasks::update` would refuse to write. |
| Merging two grown databases | Both issued T-numbers from 1. No repair exists. |
| `MAX(local, canonical)` for `next_short_id` | Race produces duplicate T-numbers. |
| Feature flags instead of a workspace | The constraint would only be checked by a command nobody runs. |
| Making the Mac a thin cache | `/task` with the lid shut would fail, and every documented `sqlite3` incantation would silently read a cache. |
| Wake-on-LAN in the routing path | Wi-Fi wake needs a Bonjour proxy on the LAN, and "away" means not on the LAN. |
| Splitting the budget per host | Two budgets that each look fine while together doubling the burn. |
| `if host == nas { tier = ReadOnly }` | A host does not *claim* what it cannot run. |
| Lowering `max_turns` remotely | Gotcha #31: turns are a bad spend proxy. |
| SwiftData / GRDB on iOS | A second source of truth with no conflict policy. |
| Optimistic UI on `queue_background` | Spending against a budget you cannot see. |
| `tailscale funnel` / public exposure | This endpoint can start write-enabled agents in 25 repos. |
| A `run_notices` outbox table | `task_runs` already replicates whole-row with `host` and `finished_at`; a cursor is the same feature with nothing new to classify, sweep or purge. |
| A "this task is detached" column | Every pre-existing drain query carries `repo_path IS NOT NULL`. The existing guard is the skew guard. |
| Command lines in `bg_mcp_servers` | "Which servers may this task use" would become "what may this task execute" — arbitrary argv for an unattended process, on a task row. |
| A sync client in the executor container | It is on the canonical node's own disk. Two containers, one host, one local volume is the case SQLite locking is for. |
| Letting the app register a host | A row means "a machine that runs tasks". Every process that opened the database would claim to run everything and drain nothing. |
| Inline shell commands as tripwire specs | A task row would hold argv for an unattended process, writable by any synced client. `script:<name>` resolves under a trusted directory instead, with one bounded `$1` the script treats as data. |
| Tripwire bookkeeping columns on `tasks` | Sync applies whole-row post-images; a "not yet, checked 14:05" every six hours would rewrite the task row the phone is editing. `tripwire_runtime` is its own 1:1 row. |
| A per-attempt log table for tripwires | Volume (a 5-minute cadence is 288 rows a day per task), and the agent kind's tokens must land where `total_tokens_today` already looks — `task_runs`. The last attempt on the runtime row plus a comment on each transition is enough. |
| Re-arming a tripwire across recurrence | Conditions are level-triggered: "an outside user exists" stays true, so a carried tripwire would fire on the next instance the moment it was created and, with `complete`, close every future instance unread. |
| A "Watching" / "Recurring" sidebar view (2026-09-22) | Recurring tasks already close — each instance is a real task, and the next one is deferred to its due day, so it sits in Waiting until then. The rows that never close are the armed tripwires, and they are already a group ("Until a condition") inside Waiting. A second "not yet" list is the exact shape Upcoming was retired for. |
| Folding the `inbox` status into `todo` (2026-09-22) | Nothing reads it — the Inbox view is "unfiled, undated, not held" whatever the status, and the badge is that list's length — but the fold is not a wording pass: the status heal does not bump `updated_at`, so the rewrite would never travel and the canonical node would revert it on every pull (gotcha #36); three binaries plus the phone keep writing `inbox` until reinstalled. Decided not worth it while the bit is merely unmaintained. |
| `planned` / `blocked` as stored statuses, or hold names under the Status filter (2026-09-22) | "In progress AND waiting on a key" is one task with one of each, which one field cannot say; `planned` would need a clock-driven status write, which is how `next` drifted. Offering Blocked under Status while the Waiting facet offers Blocked recreates the two-menus-one-subset bug v27 removed. If the holds should be louder on the row, that is pill prominence, and it must use the app's existing words (Deferred, Tripwire, Blocked) — `planned_count` on the board already means deferred OR armed. |
