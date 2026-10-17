# CXTasks — remaining work (Phases 1c → 4)

Self-contained handoff. You have none of the conversation that produced this.
Everything you need is here or named by path.

---

## 0. Where things stand

Two repos:

| Repo | What it is |
|---|---|
| `~/Projects/cxtasks` | Rust workspace + Tauri app + `cxtasks-server`. Branch `main`. |
| `~/Projects/xcode/cxtasks` | The iOS client. Branch `main`. Separate git repo, no remote. |

**Verify the starting state before changing anything:**

```bash
cd ~/Projects/cxtasks
source "$HOME/.cargo/env" && cargo test          # expect 526 passing, 0 failing
./scripts/check-linux.sh                          # cxtasks-core + cxtasks-server build for Linux
npx vitest run                                    # 257 frontend tests

cd ~/Projects/xcode/cxtasks
xcodebuild -project cxtasks.xcodeproj -scheme cxtasks -sdk iphonesimulator \
  -destination 'platform=iOS Simulator,name=iPhone 16 Pro' -only-testing:cxtasksTests test
./scripts/e2e.sh                                  # full stack: tap → HTTP → verb table → SQLite
```

`./scripts/e2e.sh` is the important one. It copies the live database with
`.backup`, starts a server against the copy, drives the real UI with XCUITest,
and asserts on **SQLite** — because a UI can happily show a change that never
landed, which is what two earlier bugs did.

### Completed

**Phase 0 — the platform split.** `crates/cxtasks-core` holds db, dispatch,
runner, mcp, checkpoint, recurrence, paths, error, executor, intent, launchd,
attachments, and the two helper binaries. It has no tauri/objc2/window-vibrancy
in its dependency graph, so a macOS-only call there is a compile error during an
ordinary `cargo test`. `src-tauri` keeps only what genuinely needs macOS. Where
core needs the platform it asks through a hook: `notify.rs` for banners,
`platform.rs` for host facts.

**Schema is at v20.** `task_runs.host` and `task_runs.heartbeat_at`, both
nullable, nothing backfilled. Host-scoped sweep, per-host concurrency gate, and
`db::runs::ensure_local` guarding `run_diff`/`restore_checkpoint`.

**Phase 1a — one verb table.** Every operation is a `match` arm in
`crates/cxtasks-core/src/api.rs::dispatch`. The 83 Tauri commands are wrappers
over it; so is the server's `POST /rpc`. There is nowhere for a second
implementation to live.

**Phase 1b — `cxtasks-server`.** axum. `/health` (auth-optional on purpose) and
`/rpc`. Two-scope bearer auth from `0600` token files. `TZ` required and refused
if unset. Builds for Linux.

**Phase 1c — the iOS app (partial).** Five tabs (Today, Inbox, Review, Agents,
Search), task detail, run detail with a folded timeline, offline outbox AND read
cache, queueing with scope refusal, 60 unit tests + 6 UI tests + the e2e harness.

### The live database is untouched

Still schema v19, 89 tasks. Every test in this work ran against `.backup`
copies. It migrates to v20 the first time a new build is installed — additive
columns only.

### Two things needing Chris, not code

- **App Group `group.app.cxtasks` is not registered** in the developer portal,
  so Xcode strips the entitlement. The simulator is permissive so it works
  there; widgets and the Share Extension will not see captures on a device.
- **The iOS repo is not registered in `project_repos`** (invariant 17). There is
  no `add_project_repo` MCP tool; do it from the app.

---

## 1. Conventions that bite

Read `CLAUDE.md` and the matching file in `.claude/rules/` before editing a
layer. The ones that have actually cost time here:

- **Arguments to `api::dispatch` are snake_case.** Tauri converts camelCase from
  JavaScript before it reaches core.
- **`TaskPatch` means absent = leave alone, explicit null = clear**, and it
  round-trips through JSON. Every field carries `skip_serializing_if`. Without
  it an untouched field encodes as `null`, the server reads "clear this column",
  and a patch setting one field wipes the rest of the task. Asserted in
  `api::tests::a_patch_survives_a_json_round_trip_without_clearing_anything` and
  end-to-end in `scripts/e2e.sh` (which lives in the iOS repo, not this one).
- **`NewTask.id` is optional and `create` is idempotent on it.** That is what
  makes an offline capture safe: a retry after a lost response returns the
  existing row instead of duplicating.
- **`api::HOST_LOCAL`** names the twelve verbs a server must refuse. The refusal
  says *which kind* of mistake it is — "only runs on the host that owns the
  files" is actionable, "unknown command" sends you hunting a typo.
- **P0 is the HIGHEST priority.** Every `ORDER BY priority` is ASC.
- **`queued` is the runner's atomic claim key**, never offered to a person.
- **Invariant 1:** wrap BOTH operands in `datetime()` for every due/defer
  comparison.
- **Invariant 8:** `paths.rs` owns every shared path.
- **Invariant 14:** a T-number is permanent and never reissued.
- **`sqlite3` CLI needs `PRAGMA trusted_schema=ON;`** in the same invocation to
  write to `tasks` at all.
- **A version number is spent the moment any binary stamps it**, including a
  `tauri dev` run. Check the local DB before folding columns into v21.

### Test-writing traps found the hard way

- A **skipped XCUITest reports `TEST SUCCEEDED`**. A bare env var reaches
  `xcodebuild` but not the test runner — the prefix is `TEST_RUNNER_`. The
  harness greps for "Test skipped" and fails on it rather than trusting the exit
  code.
- **Match UI elements by `accessibilityIdentifier`, never by label.**
  `label CONTAINS 'T'` also matches the "Today" tab, which is always on screen,
  so the assertion can never fail. This happened twice.
- A **`List` renders section headers uppercased**, so asserting on the string as
  written never matches.
- **Assert on visible text, not on a container's identifier** — a SwiftUI stack
  does not necessarily surface as an accessibility element.
- **Write the negative control.** Both times a test was sabotaged deliberately,
  one of them turned out to be vacuous. A test never seen to fail is not
  evidence.

---

## 2. Phase 1c — finish the iOS app

Not blocking Phase 2; can proceed in parallel.

**Pairing / Settings.** Node URL + token entry (QR or paste), "Test connection"
using the auth-optional `/health`, and an explicit **"Use offline capture only"**
escape. That escape is a real product decision: it means the app ships useful
even if sync slips. Also surface `OutboxService.usingSharedContainer` — false
means widgets will never see captures, and today it is false because the App
Group is unregistered.

**Tags, comments, attachments.** Verbs already exist in the table:
`task_tags`/`set_task_tags`, `list_comments`/`add_comment`,
`list_task_attachments`. The two attachment ADD verbs are in `HOST_LOCAL`
because they take local file paths — upload needs its own HTTP route, not the
verb table.

**GSD board.** `project_board` returns `BoardRow` with the derived
`ActivityState`. Render as cards, one per node. Keep the 14-bar strip scaled
**per project** — one busy project otherwise flattens every other row. Health is
a dot shown ONLY when reported; null ≠ on_track.

**Native surfaces.** Local due notifications first — they need zero server work
and are strictly better than the Mac can do (`lib.rs` says outright its reminder
loop only runs while the app is open and the Mac is awake). Cap pending
notifications at 60; iOS allows 64 and silently drops your *newest* beyond it.
Then APNs, App Intents, widgets, Share Extension, iPad `NavigationSplitView`.

**Known gap:** `cxtasks://run/<id>` cannot resolve a run older than the recent
window, because `recent_runs` is the only verb returning runs. Fine while
notifications fire for runs that just finished. Fixing it needs a `get_run`
verb — do not work around it from `recent_runs`.

---

## 3. Phase 2 — the canonical role (sync) — ⚠️ BUILT, AND NOT AS DESCRIBED BELOW

> **Read `docs/roadmap.md` § Phase 2 before this section.** Phase 2 shipped, and
> the design in the rest of this section is the one that was *planned*, not the
> one that exists. The important difference: this section says every canonical
> verb is recorded and replayed on the peer. What shipped is **state as
> post-images, deletions as replayed verbs** — because replaying a create makes
> the peer mint its own row, and replaying `complete_task` on a recurring task
> materializes the next instance twice. The roadmap has the full reasoning and
> the two other corrections (the `>=` cursor, deferred foreign keys).
>
> Everything below about *why* — lease blocks, adopt-or-refuse, whole-table
> snapshots, the skew sweep, the rejected alternatives — is still accurate and
> still worth reading. It is the mechanism sketch that drifted, not the
> argument.



**Already filed as task T90** with a full seed prompt. `cxtasks` MCP:
`get_task` with `task_id: "T90"`.

**This is the dangerous phase, and it is dangerous because it is the
interesting one.** Three consistency stories in one system is how a task app
becomes a bespoke CRDT project. The mitigations are structural. Hold the
implementation to being boring.

### Write path when sync is on

Every canonical verb, in **one transaction**: apply locally through
`api::dispatch` → append `(local_seq, cmd, args_json, host)` to `sync_outbox`.
Same transaction, same file — appending the intent and applying it must be
atomic or a crash silently drops a change. That forces `sync_outbox` to be a
table in `cxtasks.db`, which is also what lets `cxtasks-mcp` and
`cxtasks-runner` append without a second lock domain. **The UI never waits on
the network.**

### Reconcile — one code path for five seconds and five days

```
push:  drain sync_outbox in local_seq order → POST /rpc; collect failures
pull:  GET /sync/snapshot → apply post-images row-by-row
clear: delete drained rows; surface failures, never drop them
```

**Whole-table snapshot in v1, not a delta.** At 2.7 MB gzipped over a tailnet
this is a few hundred KB. A delta is not worth its correctness risk: a watermark
cannot see a hard delete, `datetime('now')` is second-resolution so a `>` cursor
can skip a row written in the same second, and `tags` / `task_tags` /
`task_deps` / `project_repos` have no `updated_at` at all. Design the
`?since=<opaque cursor>` parameter now; let the server always answer "full" in
v1. `run_events` is excluded and fetched on demand.

**Classify at the table level** (14 tables), not the function level (~105
sites). Synced: tasks, projects, sections, tags, task_tags, task_deps,
task_comments, project_updates, project_repos, task_attachments, task_runs.
Host-local: run_events, tasks_fts (derived), sync_outbox, app_meta. Column
exception: **`tasks.reminded_at` is host-local** — a pull must not un-arm or
re-fire a reminder on the wrong machine.

`app_meta` excluded wholesale (clobbering `schema_version` across a version
skew, rewinding `next_short_id`) except a named allowlist of user settings. Add
a test that the snapshot serializer **refuses** to emit `schema_version`,
`next_short_id`, `sync_*`, `role`.

### The skew sweep

A leftover pre-sync `cxtasks-mcp` writes to `tasks` without appending to the
outbox; its change is applied locally then silently reverted by the next pull —
silent data loss on the app's most-used entry point. Defence: before pushing,
enqueue a synthetic full-row upsert for every synced row whose `updated_at`
beats the last push watermark with no outbox entry. This also covers the two
raw-SQL bypasses (`commands/tasks.rs` reorder, empty_trash) without refactoring
them, plus `tags::gc` and the `initialize` heals.

**Hard deletes are the one case the sweep cannot cover** — nothing is left to
sweep and the pull resurrects the row. So `purge_expired` runs **only** in the
canonical role, and `destroy_task` / `empty_trash` **require a connection**. The
button already says irreversible behind a native confirm; adding "needs a
connection" costs nothing and preserves invariant 13.

### T-numbers under two writers — lease blocks

`MAX(local, canonical)` has a race: both nodes create a task in the same window,
both claim 91, the merge takes 92, and there are two T91s — **unrepairable**
under invariant 14, because T-numbers get written into commit messages.

**Lease blocks.** The canonical node hands out reserved ranges; the Mac leases
500 at seed time and refills below 100. No hot-path coordination, no rewind. If
a lease runs out offline, tasks are created with `short_id = NULL` — an
already-supported state that `backfill_short_ids` heals.

### First enable — adopt or refuse

The canonical node records `app_meta['sync_seeded_from']`. Enabling against an
**unseeded** node performs an adopt: full snapshot upload, `sync_seeded_from`
recorded, `next_short_id` adopted, first lease carved. Enabling against an
**already-seeded** node whose id does not match, when the Mac has local data:
**refuse loudly, naming both ids.**

That refusal is deliberate. Merging two independently-grown CXTasks databases is
a data-loss generator — both issued T-numbers from 1, so a merge either collides
or renumbers, and renumbering breaks invariant 14 permanently. Recovery for "I
paired the wrong machine" is wipe and re-seed, thirty seconds. Recovery for a
silent merge is nothing.

**Pause ≠ unpair.** Pause stops pushing and pulling but **keeps**
`sync_peer_id` and the outbox; re-enabling reconciles through the identical
offline-window code path. Unpair clears both behind a confirm.

### Cutover is a pairing, and reversible by construction

Quit app → `touch STOP_BACKGROUND` → bootout the runner → `sqlite3 .backup`
(**never `cp`** — there is a live WAL) stored outside the app data dir → record
`integrity_check`, row counts, `MAX(short_id)`, `next_short_id` → deploy server
with empty data dir, verify `/health` matches schema fingerprint and TZ →
enable, adopt → **verification gate**: per-table counts equal, `MAX(short_id)`
equal, ten sampled tasks byte-equal on every column, five fixed FTS queries
returning identical id sets → rollback at any point is `sync_enabled = 0`.

Note what does *not* change: `~/Library/Application Support/com.cxtasks.app/cxtasks.db`
stays the Mac's real database, so every documented `sqlite3` incantation in
`.claude/rules/database.md` keeps working.

### `cxtasks-mcp` stays local, always

Spawned by every Claude session, registered user-scope. If it required a
reachable peer, `/task`, `/next` and every write-back would fail whenever the
NAS was down or the laptop was in a bag. It writes through `api::dispatch`
against the local DB, appends to the outbox in the same transaction, and
best-effort kicks the drain.

### Attachments — do it now

`stored_name` points at bytes under `paths::attachments_dir()` — declared
(`paths.rs:134`), covered by the invariant-8 test (`paths.rs:222`) and created
on demand (`attachments.rs:113`), but still holding essentially nothing.
Bytes canonical on the peer, Mac keeps a content-addressed cache,
`prune_orphaned_files` becomes host-local. Defer this and you migrate files
later instead of making a design decision now.

### Server endpoints to add

```
POST /sync/push      [ intents ]
GET  /sync/snapshot  ?since=…      → post-image tables
GET  /events         (SSE)         → mcp::bridge::Envelope frames
GET/POST /attachments/:id
```

Plus **server-side timestamp normalization**: every timestamp leaves the API as
ISO-8601 UTC with `Z` and ms (`strftime('%Y-%m-%dT%H:%M:%fZ', …)` at the SQL
boundary). This removes the two-format problem at the wire rather than in every
client.

### Verification

Pairing gate (above); a test that the snapshot serializer refuses excluded
`app_meta` keys; a test that lease blocks never overlap; a post-apply assertion
that every synced task satisfies `(status='blocked') = (blocked_reason IS NOT NULL)`;
and one subtle one — **the pull applier must not emit a change notification when
the post-image is byte-identical**, because gotcha #24 resets `TaskDetailForm`
drafts on `[task.id, task.updated_at]` and a sync round-trip bumps it twice.
That presents as "typing in Notes loses characters" and gets rediscovered as a
mystery for weeks.

---

## 4. Phase 3 — a second executor host

**Blocked by Phase 2.** SQLite over SMB/NFS is not an option — WAL needs shared
memory a network filesystem cannot provide, and the documented failure is
corruption, not an error. So the sync layer is a hard prerequisite, not a
parallel track.

### Capability matching replaces `bg_locus`

A locus enum naming a host puts routing knowledge on every task row, so adding a
Mac mini means rewriting rows. Instead:

- **`RunRequirement::for_task`** — derived, stored nowhere, from
  `(ToolTier, bg_executor, repo_path)`.
- **`HostCapability`** — a row in a new `hosts` table, split into **declared**
  (`max_tier`, `executors`, `workspaces`, `can_checkpoint`, `max_concurrent`,
  `headroom_pct`, `priority`, `enabled`) and **observed** (`last_seen_at`,
  `paused`, `gate`, `executors_ready`). Effective capability is the
  **intersection** — that split *is* the preflight split, landed in data.
- **`routing::satisfies(cap, req, mirrors) -> Result<WorkspaceKind, Unmet>`** —
  pure and testable beside `gate()`.

`max_tier` is a **policy ceiling, not a probe**. The NAS is ReadOnly because
Chris chose that, not because the code knows what a NAS is. A Mac mini set to
`Write` needs no code change.

Preference: **`Live` beats `Mirror` always** (freshness dominates), then lower
`priority`. Hosts **defer** to any strictly-preferred host that is alive,
unpaused and last gated `Open`. Anti-starvation backstop at 10 minutes.
`claim_for_background` stays atomic.

### Fail-closed, two mechanisms

**`tasks.bg_remote_allowed`** — named for the *class* of remote execution, not
one machine. A pre-v20 runner cannot see a remote-only task
(`bg_allowed = 0`); it *can* claim a dual-eligible read-only task and run it
against the live tree, which is what a new local runner does anyway since `Live`
beats `Mirror`. That asymmetry is deliberate.

**Capability-generated drain SQL** is strictly stronger and subsumes the column
trick. One builder parameterised by capability — `routing::queue_sql(cap)` —
exactly as `enabled_sql()` is generated from `ALL`. A read-only host's own SQL
carries `bg_writes_allowed = 0 AND bg_shell_allowed = 0`, so it can never claim
beyond what it declares, including task shapes invented after it shipped.

### The sweep: liveness, not age

Already partly built in v20. `runs::start` seeds `heartbeat_at`;
`progress.rs`'s writer refreshes it every 30s on both branches ungated on
`dirty`. Three rules, and the third is an omission that must stay written down:

1. My own legacy rows aged past their own clock — today's rule verbatim.
2. Any host's row that has stopped heartbeating — liveness, not age.
3. **A foreign row with a NULL heartbeat is never swept.** Cost is a stranded
   slot; the alternative destroys a live run.

**Test the pair, not the half**: one sweep call must spare a fresh-heartbeat
foreign run *and* reclaim a stale one. Already done in
`db::runs::tests::a_live_run_on_another_host_is_never_swept_and_a_stale_one_is_reclaimed`.

### Budget: one pool, per-host ceilings

Splitting the budget is what `gate()`'s own doc forbids: two budgets that each
look fine while together doubling the burn. A **ceiling** reserves headroom for
the visible host without ever creating headroom. `gate()` takes a
`&HostCapability` and applies host limits **after** the global ones. NAS
`headroom_pct = 50`, `max_concurrent = 1`, `run_token_cap = 10M`. **Do not lower
`max_turns`** — gotcha #31 is explicit that turns are a bad spend proxy.

**Timezone**: `date('now','localtime')` moves to Rust —
`app_meta['bg_budget_tz']` + `meta::window_start_utc` using `chrono-tz`, passed
as a bound parameter to all three counters. It is currently spread across
`db/tasks.rs`, `db/search.rs`, `db/meta.rs`, `db/projects.rs`, and a UTC
container computes a different "Today", silently.

**The concurrency race gets fixed properly**: fold the check into the claim —
merge `claim_for_background` and `runs::start` into one `BEGIN IMMEDIATE`
transaction re-checking the running count inside the write lock. That forces the
checkpoint **after** the run row, which fixes two bugs on the way:
`ref_for_run` gets `run.id` instead of `task.id` (two runs of one task currently
share a ref name), and a failed checkpoint becomes a recorded `failed` run
rather than a task parked with no run row.

### Mirrors

A checkpoint commit *is* the uncommitted tree, as a real commit object. So the
mirror transports a commit, never a dirty tree.

- `checkpoint::capture_to(repo, ref_name)` extracted from `capture`;
  `SYNC_REF_PREFIX = "refs/cxtasks/sync"`.
- `git push <remote> +refs/heads/*:refs/heads/* +<sync_ref>:<sync_ref>` —
  **never `--mirror`**, which would delete the sync refs.
- SSH over Tailscale to `nas-ts`; bare repos at
  `/volume1/cxtasks/mirrors/<repo_key>.git`. `repo_key` = basename + short hash
  of the absolute path, so `~/Projects/a/web` and `~/Work/a/web` cannot collide.
  `gc.auto 0`.
- Triggers: runner tick every 20s when `(HEAD sha, hash of git status --porcelain)`
  changed and the mirror is older than 15 min; **`NSWorkspaceWillSleepNotification`**
  for one final capped sync, which converts "up to 15 minutes stale" into
  "exactly as you left it"; queue-time; on demand.
- A failed sync **degrades** the queue with a staleness note; **no mirror at
  all** refuses.
- Materialization: `git clone --shared --no-checkout /mirrors/<key>.git /run/<run_id>`
  then `checkout --detach <snapshot_sha>`. `--shared` lets `/mirrors` be mounted
  **read-only**.

**Three mirror properties that produce confidently wrong findings unless the
prompt says so**: `.gitignore`d files are absent (an agent will report "the
project has no environment configuration" as a *finding*); files untracked on
the Mac are *tracked* in the snapshot, so `git status` reads clean; submodules
are absent. The Claude `prompt_tail` needs a mirror-context paragraph.

**Staleness is surfaced**: the remote host writes a `run_events` **notice** at
run start — *"executing against mirror snapshot 3f2a91c taken 14:20 · 7
uncommitted files included · gitignored files absent"* — which renders with zero
schema change (`kind = 'notice'` and `TraceEvent::notice()` already exist). "3
commits behind local main" is computed **lazily on the Mac at render time**,
because it is a fact about *now*.

### Preflight split

`admit(conn, task)` at queue time — opt-in, requirement derivation, `repo_path`
**string** check (invariant 12 preserved), review-has-something-to-review, and
`routing::satisfies` against every registered host. No filesystem, no
subprocess.

`ready(task, workspace_kind)` at execute time on the host about to spawn —
resolve or materialize the workspace, `Provider::preflight()` on *this* host,
checkpoint if needed, plus a belt-and-braces tier refusal.

**Legibility is the requirement.** A **capability miss on every host** refuses
the queue naming the best near-miss; a **liveness miss** admits with a warning.
`QueueOutcome` grows `hosts: Vec<HostReadiness>` and **every** entry point
renders it. The task row gets a **derived badge, not a new status** — `queued`
must stay the runner's one atomic claim key (invariant 16).

### Stop, cancel, notifications

Keep the file as the *interface*, add the DB as the *transport*. `process.rs`
stays byte-identical. A control thread in `bin/runner.rs` loops every 3s on its
own connection: cancel requests for me → `paths::request_cancel`; `hosts.paused`
→ touch/remove a file; `bg_paused_global` → touch/remove a file; write my own
`last_seen_at` / `gate` / `executors_ready`.

**The bridge cannot cross hosts and should not try** — its security model is
same-user filesystem perms. Add a durable `run_notices` outbox drained by the
app, which also fixes an existing gap: today, if the app is closed when a run
finishes, it learns nothing on next launch. With an outbox you get **"4 runs
finished on the NAS while you were away"**.

### The container

Build on the Mac (`docker buildx --platform linux/amd64`, `docker save` → `scp`
→ `docker load`) — never compile Rust on a 4-core Celeron with 11 of 13 GiB
swap consumed.

`node:22-bookworm-slim` + git + openssh-client + tini;
`@anthropic-ai/claude-code`; **`cxtasks-runner` and `cxtasks-mcp` in the same
directory** (`writeback_binary()` resolves via `sibling_binary`). No Codex.

**A dedicated `$HOME`**, not the shared `~cx/.claude` — three containers sharing
one `.claude/projects` tree is a documented concurrency hazard on that box.

**And a property worth protecting**: a fresh `$HOME` has **no
`permissions.allow` rules**. Invariant 7 exists because `--allowedTools` is
additive over `~/.claude/settings.json`, and Chris's holds ~70 rules including
`Bash(bash:*)` and `Bash(git push:*)`. Ship an explicit empty allow/deny block
and assert it in the healthcheck — *the container's `$HOME` must never grow
one.*

`mem_limit: 2g` with **`memswap_limit: 2g`** (equal ⇒ no swap),
`NODE_OPTIONS=--max-old-space-size=1536`, `cpus: 2.0`, `TZ` explicit, subnet
**`10.224.0.0/24`** (10.222 and 10.223 are taken). Mounts: `/mirrors`
**read-only**, `/run` rw, data dir rw, `$HOME` rw — nothing else. **The mount set
is the filesystem guard.**

**Sonnet, not Haiku** — inference is remote so the CPU is irrelevant, and a
read-only exploration's entire deliverable is a plan; a weaker model produces a
confidently wrong brief.

Two new runner flags: `--health` and **`--dry-run`** (claim, materialize, build
argv, print the prompt, roll back the claim, spawn nothing) — how you validate
the container without spending a token.

### Sequencing

**P3.0** de-risk · **P3.1** host identity, heartbeat, sweep, budget window —
**ships to the Mac alone with multi-host off, and gets a week of normal use** ·
**P3.2** atomic claim + per-host ceilings + the two checkpoint bug fixes ·
**P3.3** mirrors, Mac side only, verified by `git log` on the NAS with nothing
running there · **P3.4** container in `--dry-run` · **P3.5** enable, observe,
one real run on a throwaway repo.

### Verification

`scripts/two-host-sim.sh`: two `cxtasks-runner` processes on one Mac against a
throwaway DB with a **fake CLI** (reuse the fake-Codex harness in
`runner/tests.rs`), exercising the claim race under a cap of 2, cross-host
cancel latency, pause scoping, deferral and starvation.

Named tests: `a_foreign_run_with_no_heartbeat_is_left_for_its_own_host`;
`a_legacy_runner_query_cannot_claim_a_remote_only_task` beside its deliberate
opposite `a_legacy_runner_still_claims_a_dual_eligible_task`;
`queue_sql_for_the_local_host_is_byte_identical_to_the_pre_v20_filter` (a blunt
string comparison, and exactly right — it makes any future edit to the Mac's
drain query a deliberate act); `the_local_host_refuses_a_detached_task`;
`the_budget_window_is_the_same_instant_from_two_timezones`.

Mirror tests need **zero network** — extend `checkpoint.rs`'s temp-repo harness:
assert the untracked file is present, the gitignored file absent, `refs/heads`
unchanged, and the source's `git status` byte-identical before and after.

---

## 5. Phase 4 — repo-less remote tasks

`WorkspaceKind::Detached` is a **declared capability, not a special case in
code**. The MacBook does not list it, and the reason is written down: an
unattended run at `$HOME` gets unsupervised `Read`/`Glob`/`Grep` over every
client repo, `~/.ssh`, and the notes vault, with no mount boundary.

**Invariant 12 is not weakened — it is satisfied differently:**

> Background execution is refused outside a **declared workspace**. A repo is
> one. A container mount set the app authored is another. `$HOME` is neither.

The enforcement points each grow an explicit `Detached` **branch**, never a
deleted check, with the fallback arm byte-identical to today. There are **two**,
and neither is in `db/tasks.rs`:

| Where | What it does today |
|---|---|
| `crates/cxtasks-core/src/dispatch.rs:746` | `if task.repo_path.is_none()` — refuses at QUEUE time. The only repo refusal in the workspace |
| `crates/cxtasks-core/src/mcp/server.rs:1197` and `:1378` | the MCP's own pair — refuses `bg_allowed` with no repo, and again at queue. Must move in step, or a detached task filed through `/cxtasks:task` is refused by a surface nobody thought to update |

*(An earlier revision of this file also listed `db/tasks.rs:1180` and `:1333`.
Both were wrong. `:1180` is inside `TaskPatch::is_empty()`, a no-op-patch check
— the real `bg_allowed` resolution is `:1399-1404` and has no `repo_path` guard
at all. `:1333` refuses nothing: it skips the project→`repo_path` prefill when
the caller passed an explicit repo. Editing either would be editing a dead end.)*

The fail-closed encoding is free: the drain filter carries
`repo_path IS NOT NULL AND TRIM(repo_path) <> ''`, so a detached task is
invisible to a pre-existing runner with no new column. **The existing guard is
the skew guard.** Worth stating because the next reader will look for a third
column and not find one. There is exactly **one** such filter,
`db/tasks.rs:1751` — an earlier revision claimed a second copy at
`db/schema.rs:826`, which is v11's one-time `project_repos` backfill and has
nothing to do with draining.

### What Phase 4 still needs decided

Genuinely open, and small enough that discovering it in code is fine:

- **`tasks.bg_mcp_servers`** is a new column, so Phase 4 opens **schema v21**.
  Check the local DB's stamped version before claiming the number — a version is
  spent the moment any binary writes it, including a `tauri dev` run.
- **The registry** it validates against. Start with `vault` and nothing else;
  an empty default is the safe one.
- **Where a detached run's working directory actually is** inside the container.
  It is not `$HOME` and it is not a repo — most likely a scratch dir under the
  data-dir mount, which keeps "the mount set is the filesystem guard" true.

**What replaces the repo gate is the tool surface, not the filesystem.**
`--strict-mcp-config` means "only what `--mcp-config` names". So a detached run
gets `cxtasks` (3 write-back tools) plus an explicitly allow-listed set of
read-only MCP servers per task, defaulting to **empty**
(`tasks.bg_mcp_servers`, validated against a registry; `vault` first).
**`WebFetch`/`WebSearch` stay denied at every tier** — network research is an
egress question deserving its own round.

---

## 6. What a write-capable remote host would need later

Not building it; not foreclosing it. Four things ReadOnly dodges: where its tree
comes from; **where its edits land** — not in the MacBook's tree, but committed
onto `refs/cxtasks/results/<run_id>` and pushed back, so the remote agent
produces **a patch, not a mutation**; how `run_diff` reaches a remote ref (the
Mac fetches it, then both commands work unchanged); and conflict with the user's
own edits (the result carries `snapshot_sha`).

Three cheap things now that make it a feature later rather than a rewrite: keep
`snapshot_sha` / `snapshot_at`; make `run_diff` take its repo source from the
run row rather than assuming locality; make `restore_checkpoint` **explicitly
refuse** a non-local run (already done — `db::runs::ensure_local`). Note the verb
for a remote result is **apply**, not restore — the local tree was never touched.

---

## 7. Rejected, with reasons — do not re-invent these

| Rejected | Why |
|---|---|
| **A change-journal / oplog table** | ~105 write sites = 105 chances to forget, failing as "one field silently stops syncing" with no greppable call sites. As triggers it compounds gotchas #1 and #2 and sits one step from invariant 3. At 2.7 MB a snapshot is correct by construction. |
| **Field-level merge** | Manufactures rows `tasks::update` would refuse to write (`status='blocked'` from one side, `blocked_reason=NULL` from the other), and nothing catches them because they weren't written by `update`. |
| **Merging two grown databases** | Both issued T-numbers from 1. No repair exists. Refuse the pairing and say why. |
| **`MAX(local, canonical)` for `next_short_id`** | Race produces duplicate T-numbers — unrepairable under invariant 14. |
| **Feature flags instead of a workspace** | The constraint would only be checked by a command nobody runs. |
| **Making the Mac a thin cache** | `/task` from a terminal with the lid shut would fail, and every documented `sqlite3` incantation would silently read a cache. |
| **SQLite over SMB/NFS** | WAL needs shared memory a network filesystem cannot provide. Documented failure is corruption, not an error. **Makes Phase 2 a hard prerequisite for Phase 3.** |
| **Wake-on-LAN in the routing path** | `pmset womp` wakes from Ethernet only; Wi-Fi wake needs a Bonjour Sleep Proxy on the LAN, and "away" means not on the LAN. Silent failure, miserable diagnosis. The deferral rule is already self-correcting. |
| **Splitting the budget per host** | `gate()`'s own doc forbids it: two budgets that each look fine while together doubling the burn. Ceilings, not budgets. |
| **`if host == nas { tier = ReadOnly }`** | Invariant 7's "refuse rather than quietly downgrade" and `intent.rs`'s "never derive a capability". A host does not *claim* what it cannot run. |
| **Lowering `max_turns` on the remote host** | Gotcha #31: turns are a bad spend proxy and a low cap is a tax. Tighten tokens instead. |
| **Sharing `~cx/.claude` with the existing containers** | Documented session-lock precedent on that exact box. |
| **A `cancel_requested` column read by the 250 ms poll** | `paths.rs:96-105` already argued this. DB as transport, file as interface. |
| **Codex on the NAS** | A fourth column for zero value. |
| **Building Rust on the NAS** | 4-core Celeron, 11 of 13 GiB swap consumed. |
| **SwiftData / GRDB on iOS** | A second source of truth with no conflict policy; `bg_allowed` is computed, not a column. |
| **Optimistic UI on `queue_background`** | Spending against a budget you cannot see. |
| **A "Sync now" button** | Teaches the user the sync is unreliable. Automatic drain plus an honest status strip. |
| **`tailscale funnel` / any public exposure** | This endpoint can start write-enabled agents in 25 repos. |
| **Letting the server's `TZ` default** | Silent, and it changes what "Today" means. |

---

## 8. Risks

1. **Phase 2 is the dangerous phase**, and it is dangerous because it is the
   interesting one. Mitigations are structural: whole-row merge, snapshot pull,
   single-writer intents, no merge logic anywhere.
2. **The pairing moment** is the single highest-risk operation — mitigated by
   the `.backup`, the verification gate, and a rollback that is one toggle.
3. **NAS hardware.** 12 Gi of 19 used, 11 of 13 swap consumed, before adding
   anything. Hence `memswap_limit`, `max_concurrent = 1`, 50% headroom.
4. **Scope.** Months of work across three phases and two languages. Local-first
   is what makes that survivable: every phase is independently valuable and
   independently abandonable.

---

## 9. Working agreements

- Commit straight to `main` — both repos are on it. No feature branches,
  no PRs. Commit at bookends — complete AND verified.
- Stage precisely; parallel sessions leave unrelated work in the tree.
- Never push unless asked.
- **Never open the live database directly in a test.** Copy it with
  `sqlite3 .backup` — there is a live WAL and `cp` can catch it mid-transaction.
- Verify beyond the build. A passing build does not mean the feature works; run
  `./scripts/e2e.sh` — from `~/Projects/xcode/cxtasks`, the only repo it is in —
  and read what it asserts.
