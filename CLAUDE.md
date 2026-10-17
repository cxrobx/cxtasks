# CXTasks

Agentic todo app for macOS. A real task system (due dates, recurrence, priorities,
reminders) where **any task can optionally carry an execution context** and be
dispatched to an agent — foreground in Herdr (displayed in Ghostty) when the
shared `cx-open-in-herdr` router is installed, or unattended by a background
runner that writes the result back onto the task.

**Publishing.** Repo `cxrobx/cxtasks` is prepared for **public** release as one
fresh commit (2026-10-17, Business Source License 1.1). The earlier history is
the local `private-history` branch, the private `cxrobx/cxtasks-history` repo
(remote `history`) and `~/Archives/cxtasks-history-2026-10-17.bundle`; **never
push `private-history` to `origin`**. Keep client and contact names out of
code, tests, mocks and docs: fixtures use Acme / Globex / Initech and @dana /
@sam.

The shared router lives in `integrations/herdr/`. It selects a space by canonical
checkout path, creates one when missing, and starts each handoff in a fresh tab.
Repo-less foreground tasks use per-task scratch directories grouped by project
or Inbox. Without the router, foreground retains its standalone Ghostty path.

Foreground is always **Claude Code**. Background is per-task: **Claude Code**
(the default) or **Codex** (`codex exec`), chosen by `tasks.bg_executor`.

**It is no longer only a Mac app.** `cxtasks-core` builds for Linux,
`cxtasks-server` exposes the verb table over HTTP, and `~/Projects/xcode/cxtasks`
is a native iOS client. Sync itself is not built yet — the server is a live
surface onto whichever database it is pointed at, and the Mac app is unchanged
and still entirely standalone.

The gap it closes: `TodoWrite` is session-scoped and dies with the session. CXTasks
is the durable layer between capture (the vault, `wn`/`ytn`) and execution (Claude
Code across ~25 repos).

## Tech Stack

| Layer | Technology |
|-------|------------|
| Desktop | Tauri 2 (macOS-private-api) |
| Frontend | React 19, TypeScript, Vite, Tailwind CSS 4, Radix UI |
| Backend | Rust, Tokio, rusqlite (bundled SQLite + FTS5) |
| State | Zustand |
| MCP | rmcp with stdio transport |

Forked from cxmail's skeleton (Ghostty handoff, MCP binary shape, live-refresh
bridge, LaunchAgent install, migration pattern). Deliberately **not** carried over:
licensing/`LicenseGate`, updater signing, IMAP/SMTP, the plugins runtime, the tracker.

## Golden Commands

```bash
source "$HOME/.cargo/env" && npx tauri dev          # Dev mode (hot reload)
source "$HOME/.cargo/env" && npm run tauri build    # Production build
source "$HOME/.cargo/env" && cargo test             # Rust tests (from the repo root)
source "$HOME/.cargo/env" && cargo build --release --bin cxtasks-mcp   # Rebuild the live MCP
python3 scripts/make-icon.py                        # Icon set, downscaled from icons/icon_source.png
target/release/cxtasks-runner --status              # What the runner would do right now
target/release/cxtasks-runner --once                # Drain one batch and exit
target/release/cxtasks-runner --health             # One line + exit code, for a healthcheck
target/release/cxtasks-runner --dry-run            # Claim nothing, spawn nothing, print the prompt
target/release/cxtasks-runner --sync-now           # Reconcile with the peer once; `sync-control.sh now` wraps it
target/release/cxtasks-runner --prose-audit        # Score a week of agent notes/comments/results; exit 0 = tripped
target/release/cxtasks-runner --tag-audit          # Near-duplicate / misplaced / burst-of-one-off tags; exit 0 = tripped (T240)
./scripts/two-host-sim.sh                          # Two hosts, one queue, no network, no tokens
./scripts/agent-check-sim.sh                        # An agent: tripwire end to end — real runner, fake claude, zero tokens
codex login status                                  # Is the Codex executor usable?
./scripts/check-linux.sh                            # Does cxtasks-core still build for Linux?

# The sync server, against a THROWAWAY database — never point it at the live one
# by accident. TZ is required and refused if unset: it decides what "today" means.
TZ=America/Chicago CXTASKS_DATA_DIR=/tmp/cx CXTASKS_TOKEN_FILE=/tmp/cx/token \
  target/debug/cxtasks-server

# The iOS client (separate repo, ~/Projects/xcode/cxtasks)
xcodebuild -project cxtasks.xcodeproj -scheme cxtasks -sdk iphonesimulator \
  -destination 'platform=iOS Simulator,name=iPhone 16 Pro' test
python3 scripts/make-icon.py    # ITS make-icon.py — same master, extended to full bleed
```

**This is a Cargo WORKSPACE, so build output is `target/` at the repo root — not
`src-tauri/target/`, which no longer exists.** Every path above changed for that
reason and that reason only. Cargo commands now run from the root; running them
from `src-tauri/` still works and still writes to the same place.

**Always `source "$HOME/.cargo/env"` before cargo/tauri** (cxmail gotcha #1).
**Kill any old `tauri dev` before restarting** — two instances on one SQLite file
deadlock (cxmail gotcha #12): `pkill -f "target/debug/cxtasks"`.

**Codex is a prerequisite, not a bundled dependency** — a Codex task is refused at
queue time when the CLI is missing or logged out. Before testing anything by hand,
note gotcha #19 (the node shim vs the LaunchAgent's PATH) and gotcha #20 (`codex`
is a shell function that injects an approval policy — `command codex` is the real
one). Full detail: `.claude/rules/execution.md`.

**Slash commands** (plugin `cxtasks@cxtasks`, user-scope): `/cxtasks:task` (file a
task with the current repo attached) · `/cxtasks:tasks` (board, read-only) ·
`/cxtasks:next` (pull the top task and start) · `/cxtasks:breakdown` (plan → file
tasks across dated phases). Neither filing command derives `project` from the
directory name (invariant 17), a seed prompt is the default expectation whenever
`repo_path` is set, and the plugin is installed as a **copy** — editing
`claude-plugin/` does nothing until the version is bumped and re-installed
(gotcha #14). Full behaviour: `.claude/rules/mcp-plugin.md`.

**Stop all background runs without opening the app:**
```bash
touch ~/Library/Application\ Support/com.cxtasks.app/STOP_BACKGROUND
```

That switch is checked **between spawns**, so it stops the NEXT run starting and
lets an in-flight one finish. To stop a run that is already going, use the
**Stop** button on it (task detail → Runs, or Mission Control), or from a
terminal:
```bash
touch ~/Library/Application\ Support/com.cxtasks.app/CANCEL_<run_id>
```

## Shipping a build

**`/ship` (or `scripts/ship.sh` from a shell) does all of this** and fails on
every check below; the steps are kept here so the script has something to be
checked against.

The extra steps are about the **SIGNATURE**, not about copying files:

```bash
source "$HOME/.cargo/env" && npm run tauri build          # WAIT for "Finished 2 bundles"
cd target/release/bundle/macos
ls CXTasks.app/Contents/MacOS/     # expect all three: cxtasks, cxtasks-mcp, cxtasks-runner
                                   # NEVER copy one in by hand — the two helpers arrive via
                                   # `externalBin`, staged by scripts/build-helpers.sh. If one
                                   # is missing the STAGING failed; fix that, don't paper over
                                   # it (gotcha #5)
codesign --force --deep --sign "Developer ID Application: Christopher Robinson (CCYV5HQZCM)" CXTasks.app
codesign -dv --verbose=2 CXTasks.app 2>&1 | grep flags   # MUST read flags=0x0(none)
rm -rf /Applications/CXTasks.app && cp -R CXTasks.app /Applications/
```

The re-sign is required **even when you copy nothing**: `tauri build` (CLI 2.10.1)
signs with hardened runtime (`flags=0x10000`), which silently kills the Ghostty
AppleScript route (gotcha #25); the re-sign strips it back to `0x0`. **Verify the
flag, not the command.** Installing before the build finishes installs the
*previous* build (presents as "the migration didn't run"). Why each step exists:
`.claude/rules/macos-native.md`.

## Layout

**Two crates, and the split is the point.** `cxtasks-core` holds everything that
must also run on Linux — the runner and the MCP have to work in a container. It
has no tauri, objc2, block2 or window-vibrancy in its dependency graph, so a
macOS-only call added there is a **compile error during an ordinary
`cargo test`**, not something that surfaces under a flag nobody passes. When
core needs the platform it asks through a hook: `notify` for banners,
`platform` for host facts. `src-tauri/` keeps only what genuinely needs macOS.

`cxtasks_lib` re-exports every core module under its original path, so
`crate::db::…`, `crate::runner::…` and `crate::paths::…` resolve unchanged from
the Tauri side.

| Path | Purpose | Detail |
|------|---------|--------|
| `crates/cxtasks-core/src/db/` | `schema.rs` (migrations), `tasks.rs`, `runs.rs`, `search.rs` (FTS + `FilterSpec`), `meta.rs`, relations (`tags`/`deps`/`comments` plus **`relations.rs`** for `supersedes` / `relates_to`), **`retrospective.rs`** (cited read-only recall), `projects.rs` (PARA `ProjectKind` + `expand_scope`, incl. the GSD board's derived `ActivityState`), `project_updates.rs`, **`tripwires.rs`** (the leased attempt: `list_due` → `claim` → `record`, one transaction — invariant 26) | `rules/database.md` |
| `crates/cxtasks-core/src/api.rs` | **The one verb table** — every operation, keyed by name | (section above) |
| `crates/cxtasks-server/` | axum over `api::dispatch`: `/health`, `/rpc`, `/sync/*`, `/attachments/*`, two-scope bearer auth. Builds for Linux | |
| `crates/cxtasks-core/src/sync/` | Whole-row replication: `tables` (classification), `snapshot`, `apply`, `sweep`, `outbox`, `leases`, `pair`, `reconcile`, `http`, `state`, **`blobs`** (attachment bytes — invariant 23), **`auto`** (the five-minute timer's entry points, and the record of the last attempt) | `rules/database.md` |
| `crates/cxtasks-core/src/host.rs` | This machine's identity, as a FILE — never `app_meta`, which travels with a copied database | `rules/database.md` |
| `crates/cxtasks-core/src/mcp/` | `server.rs` (22 tools, `file_followup` background-only), `bridge.rs` (Unix-socket live refresh) | `rules/mcp-plugin.md` |
| `crates/cxtasks-core/src/routing.rs` | **Capability matching** — `RunRequirement` × `HostCapability` → `satisfies`; `queue_sql` generates each host's own drain predicate | `rules/execution.md` |
| `crates/cxtasks-core/src/db/hosts.rs` | The host registry; declared policy survives a restart, observed is refreshed by the heartbeat | `rules/execution.md` |
| `crates/cxtasks-core/src/mirror.rs` | A working tree moved as a checkpoint COMMIT; `MIRROR_CONTEXT` is the load-bearing part | `rules/execution.md` |
| `crates/cxtasks-core/src/mcp_registry.rs` | What a detached run may reach — names, never command lines | `rules/execution.md` |
| `crates/cxtasks-core/src/runner/` | Drain loop, budget gates, tiers, providers/{claude,codex}, salvage, `control_pass`, `ready`/`release`; **`tripwire.rs`** holds BOTH tripwire lanes — script and `jev:` checks run before the budget gate's early return (gotcha #42), while the AGENT lane (`agent_pass`) runs below it, takes one slot, and evaluates `agent:` specs as a `RunKind::Check` — read-only pinned, `get_task` only, 12 turns · 3M · 10 min, under its own daily sub-cap; an `on_trip = interpret` trip gets one zero-tool run reading its full evidence (`interpret_one`, 2 turns · 1M · 5 min). **`completion.rs`** parses the `CXTASKS_RESULT:` report every ordinary work run ends with — exit 0 is not "done" | `rules/execution.md` |
| `crates/cxtasks-core/src/dispatch.rs` | Shared prompt/scratch-dir builder, repo resolution | `rules/execution.md` |
| `crates/cxtasks-core/src/executor.rs` + `intent.rs` | `BackgroundExecutor` and `BackgroundIntent` — read as a pair | `rules/execution.md` |
| `crates/cxtasks-core/src/checkpoint.rs` | Working-tree snapshots onto `refs/cxtasks/` for write-enabled runs | `rules/execution.md` |
| `crates/cxtasks-core/src/paths.rs` | **Single source of truth** for every shared on-disk path (invariant 8); `CXTASKS_DATA_DIR` moves the root | |
| `crates/cxtasks-core/src/recurrence.rs` | RRULE-lite (`daily` · `weekly:MO,TH` · `monthly:15` · `quarterly:1`) + `complete_with_recurrence` | |
| `crates/cxtasks-core/src/tripwire.rs` | The tripwire VOCABULARY — `script:<name> [arg]`, `jev:<condition>`, and `agent:<condition>` grammar, states, cadence floors, `is_due`/backoff arithmetic, `script_path` (no symlinks, beneath the trusted dir, exec bit), the check and interpretation bounds (`CHECK_*`, `INTERPRET_*` + `min_nonzero`), the `OnTrip` policies (surface · complete · monitor · interpret; unknown reads as surface) and the verdict grammar (`parse_verdict`: last `VERDICT:` line wins). Pure; the DB half is `db/tripwires.rs`, the process half `runner/tripwire.rs` | `rules/database.md` |
| `crates/cxtasks-core/src/jev.rs` | Optional TypeSafe/Jev client for readiness, post-run review, `jev:` tripwires and local-result reranking, plus CAPTURE, which defaults to OpenAI Decisions (`capture_engine`; falls back to Jev, then code) with per-engine bars and per-question refusal handling. Credentials are host-local; all features default off behind `jev_enabled` | `docs/jev.md` |
| `crates/cxtasks-core/src/prose.rs` + `markdown_allowlist.json` | The write-time RENDER CHECK (what the sanitizer drops, and walls of text) on every MCP write reply, with a per-path remedy; the weekly `--prose-audit`. The JSON is the ONE tag + attribute list, also imported by `src/lib/markdownSchema.ts` | `rules/mcp-plugin.md` |
| `crates/cxtasks-core/src/notify.rs` + `platform.rs` | The two hooks core uses to reach the platform without depending on it | |
| `src-tauri/src/commands/` | Tauri IPC handlers — `tasks.rs`, `dispatch.rs` | |
| `src-tauri/src/bin/` | `mcp.rs` → `cxtasks-mcp`, `runner.rs` → `cxtasks-runner`; thin `main`s that install the platform hooks | |
| `src-tauri/src/ghostty_macos.rs` | `find_ghostty_app`, `open_in_running_ghostty` — foreground dispatch only | `rules/execution.md` |
| `src-tauri/src/launchd.rs` | LaunchAgent install/uninstall; logs → `~/Library/Logs/CXTasks/` | |
| `src-tauri/Info.plist` | Additive plist keys (`NSAppleEventsUsageDescription`, the `cxtasks://` scheme) — real builds only, never `tauri dev` | `rules/macos-native.md` |
| `src-tauri/src/links.rs` | `cxtasks://task/<T-number or id>` → the notification-click reveal latch (`RunEvent::Opened` in `lib.rs`) | |
| `src-tauri/src/{notify,appearance,glass,vibrancy}_macos.rs` | Native layer: notifications + bundle guard, DarkAqua pin, the CGS desktop blur (+ Reduce Transparency), and the `NSVisualEffectView` it falls back to | `rules/macos-native.md` |
| `src/` | React frontend — component map in the rules file | `rules/frontend.md` |
| `src/mocks/RedesignMock.tsx` | Design-exploration page, `?mock` only, lazy-loaded | `rules/frontend.md` |
| `claude-plugin/` | `/cxtasks:*` slash commands | `rules/mcp-plugin.md` |
| `integrations/alfred/` | The Alfred `t` workflow: read-only hybrid search (T-number · FTS · Ollama embeddings, RRF-fused), ↩ opens `cxtasks://task/<id>`. Alfred runs an installed COPY — re-run `install.sh` after editing | `integrations/alfred/README.md` |

## App Data

| Path | Contents |
|------|----------|
| `~/Library/Application Support/com.cxtasks.app/cxtasks.db` | SQLite (incl. the `tasks_fts` index) |
| `~/Library/Application Support/com.cxtasks.app/ipc.sock` | Live-refresh socket |
| `~/Library/Application Support/com.cxtasks.app/STOP_BACKGROUND` | Background kill switch — presence = paused |
| `~/Library/Application Support/com.cxtasks.app/CANCEL_<run_id>` | Per-run cancel request — presence stops ONE in-flight run |
| `~/Library/Application Support/com.cxtasks.app/attachments/` | Managed image copies — a CACHE on a synced host (invariant 23); `.incoming/` holds transfers in flight |
| `~/Library/Application Support/com.cxtasks.app/claude-sessions/` | Per-task dispatch scratch dirs (`prompt.txt` for the work; `check-prompt.txt` beside it for an agent tripwire check — never over it) |
| `~/Library/Application Support/com.cxtasks.app/tripwires/` | **Trusted** executables a `script:<name>` tripwire names — exit 0 = tripped (stdout = evidence), 1 = not yet, else error; `$1` is the one argument and is untrusted data. Symlinks refused; the runner is the only thing that runs them |
| `~/Library/Logs/CXTasks/runner.{log,err.log}` | Runner stdout/stderr, redirected by the LaunchAgent |

## Data Model — quick facts

Full schema (table-by-table, tags/deps/comments, migration discipline):
`.claude/rules/database.md`. Always-relevant:

- `schema.rs` is at `CURRENT_VERSION = 29`; the stamp only moves **forward**
  (three binaries share one DB and don't upgrade in lockstep). Don't hardcode the
  number in a test — compare against `schema::CURRENT_VERSION`.
- **A version number is spent the moment any binary stamps it on a real DB —
  including a `tauri dev` run.** Before folding columns into the newest migration,
  check the local DB:
  `sqlite3 "$HOME/Library/Application Support/com.cxtasks.app/cxtasks.db" "SELECT value FROM app_meta WHERE key='schema_version'"`
- The `sqlite3` CLI needs `PRAGMA trusted_schema=ON;` in the same invocation to
  write to `tasks` at all (gotcha #1).

## Critical Invariants (index)

One line here; the full argument lives in the named rules file. **Numbers are
global** — code comments, tests and the gotchas cite them as "invariant N".

| # | Invariant | Full text |
|---|-----------|-----------|
| 1 | Wrap BOTH operands in `datetime()` for every due/defer/reminder comparison | database |
| 2 | Never write `tasks_fts` directly — only `db::search::rebuild`; user keywords via `build_match_expr` | database |
| 3 | Never enable `PRAGMA recursive_triggers` | database |
| 4 | `repo_path` is an explicit MCP parameter — the server can't see the caller's cwd | mcp-plugin |
| 5 | Cost scales with TURNS, not context size — a warm session makes it WORSE; the real lever is a precise seed prompt | execution |
| 6 | Background cost is TOTAL tokens, not output tokens; `daily_token_cap` is the primary guard and is NOT per-task overridable; background defaults to Sonnet | execution |
| 7 | Tool grants enforced by `--disallowedTools` (complement over `DANGEROUS_TOOLS`) / Codex `--sandbox`; Shell and Write both checkpoint; per-run token ceiling bounds Claude but NOT Codex | execution |
| 8 | `paths.rs` owns every shared path — app, MCP and runner must resolve the same DB; never `app_data_dir()` in one place and `dirs::data_dir()` in another | (that's the whole rule) |
| 9 | P0 is the HIGHEST priority, P3 the lowest; every `ORDER BY priority` is ASC; "worth showing" thresholds are `< PRIORITY_LOWEST` | database |
| 10 | Anything that hands work back must subtract blocked tasks — including hidden (trashed/archived) blockers | database |
| 11 | Grouping must never destroy what it groups — rename/delete carry all four text joins | database |
| 12 | Missing `repo_path` = "run at $HOME" for FOREGROUND only; background is refused outside a **declared workspace** — a repo is one, a container mount set is another, `$HOME` is neither | execution |
| 13 | Deletion is recoverable; exactly two code paths hard-delete; `db::tasks::LIVE` keeps the filter greppable; batch keys are never time-derived | database |
| 14 | A T-number is permanent, never reissued; `projects.health` moves only via a written update — but the BOARD reads `activity` (derived from the clock), never health, because a signal needing an input nobody is prompted for reads as an accusation | database |
| 15 | A second background agent fails CLOSED — per-executor enabled flags; switching executor clears the model; tiers mean the same to both | execution |
| 16 | `status` answers ONE question on one axis (six values — `blocked` is a HOLD on `blocked_reason` since v27, not a status); `queued` is the runner's claim key; the only closed status is `done`; `not_held_sql` is the complement of `waiting_sql`, is the WHOLE of `actionable_sql`, and every actionable surface carries it — a due date is a DEADLINE and hides nothing, which is why Upcoming was retired and Waiting is the one "not yet" list | database |
| 17 | A project's NAME and its repo's FOLDER are never derived from each other — `project_repos` stores the mapping; rename onto an existing name MERGES | database |
| 18 | A delegated run leaves a handoff, the next dispatch replays it, and the write-back MCP is a REDUCED router (3 tools); a FOREGROUND session leaves the same handoff through the same split — full text onto a run row, derived summary into the log — via `complete_task`'s `result` or `comment_task(handoff: true)`, which background is refused | execution |
| 19 | A run has an INTENT, continuation is DERIVED, and intent must never decide a capability | execution |
| 20 | `task_runs.status = 'running'` means THIS machine is executing NOW — foreground runs are terminal at `dispatched`; the sweep is host-scoped and judges LIVENESS (heartbeat) not age, and never reclaims a foreign row with no heartbeat; the concurrency gate counts THIS host's rows | database |
| 21 | ONE query builder (`db::search::filtered`; `tasks::list` is its adapter) — chips REFINE the view, the "not done" exclusion is a removable chip rather than six WHERE clauses, every negation is NULL-safe, deferred/armed rows SINK under every sort, and MCP facets ride on `search_tasks`, never a new tool | database |
| 23 | Attachment BYTES are canonical on the peer and the local store is a CACHE; rows travel first and bytes follow; nobody stores bytes for a row they don't have; fetch on presence, send on the digest; a failed transfer is one missing image, never a failed reconcile — which is why `prune_orphaned_files` is host-local in EFFECT, and why scope splits on the METHOD (board may GET, only full may POST) | database |
| 24 | A host row means "a machine that runs tasks" — only the RUNNER writes one, on the machine it describes; declared is policy and observed is what it saw, effective is the intersection, and absence of an observation is not a negative one | execution |
| 25 | A detached run is bounded by its TOOL SURFACE, not its filesystem — `bg_mcp_servers` holds names validated against a registry, defaults to empty, and there is no "is detached" column because every pre-existing drain query already carries `repo_path IS NOT NULL` | execution |
| 26 | A tripwire hides like a defer and never stays hidden after a failure — only `armed` hides; an attempt is a leased, CAS-recorded transition (claim → run → record in ONE transaction, notify after commit); volatile bookkeeping lives on `tripwire_runtime`, never on `tasks`; the defer gate is evaluated first; scripts are names plus one hostile `$1`, never argv; one-shot across recurrence. An AGENT check is the same attempt with a RUN ROW as its lease: `intent = check` is machine-only (never on a task), its grant/bounds are pinned by the lane that started it and never read off a row, a verdict is not a handoff, and UNSURE is an error. A MONITOR (`on_trip = 'monitor'`) is a standing check: done does not stop it (only archive does), completing it re-arms it, a trip reopens it, and it can't recur. An INTERPRET trip gets one zero-tool run that reads its evidence and never acts on it | database |
| 22 | A grouping node is an AREA or a PROJECT and only a project ENDS; areas nest in areas to ANY depth, nothing nests in a project (it holds sections), and nothing nests inside its own subtree; `parent_id` is the one join keyed by ID not name; the roll-up lives ONLY in `expand_scope`, and project→directory may inherit while directory→project may not. A session can run the WHOLE lifecycle: `create_project` starts one, `update_project` ends it | database |
| 27 | `supersedes` preserves a conclusion without leaving old work actionable: replacement → older task, both directions readable, cycles refused, and every path that hands out work subtracts the older task; pure plan churn is deleted instead | database |
| 28 | A self-initiated filing declares WHY it isn't being done now (`why`), is tagged `agent-filed`, and asks Chris before the row exists; "so it isn't lost" is not a reason — Chris asking is the only ungated path, and a reviewer or another agent is not Chris | mcp-plugin |
| 29 | A PERSON is a tag spelled `@name` and a DECISION is the tag `decision` — conventions on `tags`, never columns; what makes them real is BEHAVIOUR: a decision is on Today (the Decide strip), off Inbox, skipped by `next_task` and both runner claim paths, and refused by the queue; people ride `search_tasks(people)` and the retrospective's mix. WAITING is one predicate (`waiting_sql`) shared by the sidebar view and the facet, and a negated predicate must `COALESCE` its nullable column | database |

## Gotchas (index)

Symptom-level index; the full entries live verbatim in the rules files.

**database** — #1 `sqlite3` CLI can't write to `tasks` without
`PRAGMA trusted_schema=ON` · #2 FTS5 triggers use plain `DELETE`/`UPDATE`, not the
`'delete'` command · #12 comments need millisecond timestamps AND a `rowid`
tiebreaker · #30 raising a `DEFAULT_*` changes nothing once the key is stored in
`app_meta` · #36 a column on a `created_at`-swept table must be written WITH the
row — a late fill can't travel, the next pull reverts it, and it flaps forever.

**execution** — #40 a mirror fingerprint built from `git status --porcelain`
is BLIND to edits of already-dirty files (porcelain names paths, not contents)
· #41 the runner container's `$HOME` must grant no `permissions.allow` rules —
`--allowedTools` is additive over it and Chris's holds **175** · #18 queueing
into a runner that isn't running needs its own
warning (both routes) · #19 `codex` on PATH is a node shim; the LaunchAgent's PATH
has no node · #20 `codex` in the interactive shell is a function injecting an
approval policy · #21 Codex's network-disable override is defence in depth, not a
seal · #25 foreground dispatch has TWO routes (Ghostty AppleScript, then `open -na`
fallback) and the good one fails silently · #31 a stopped run must say WHICH guard
fired; the turn cap is a loop detector, tokens are the cost guard · #42 the
drain tick returns early when the gate is closed — the free (script tripwire)
lane must run BEFORE that return, or a spent token budget silently stops
zero-token checks.

**scripts** — #37 `cmd | grep -q x` under `set -o pipefail` reports FAILURE on
success (grep closes the pipe, the producer takes SIGPIPE) — capture into a
variable instead · #38 `timeout(1)` is GNU coreutils and is NOT on a stock
macOS, so those lines silently do nothing and the feature looks broken ·
#39 `remove_dir_all(dir.parent())` on a dir created directly under `$TMPDIR`
names the WHOLE temp directory · #46 macOS `/bin/sh` is bash in POSIX mode
with `xpg_echo`, so `echo '…\n…'` EXPANDS the escape — a fake CLI emitting a
JSON string with `\n` in it splits the event across two invalid lines, and the
run reads as "no verdict" / "no result" with nothing erroring. Use
`printf '%s\n'`.

**frontend** — #45 density is CSS variables + one `data-density` attribute
(applied pre-paint), what each mode DROPS is the design, and
`DENSITY_ROW_ESTIMATE` must move with the padding or the scrollbar jumps ·
#44 an UNLAYERED rule in `globals.css` outranks every Tailwind
utility, so `text-*` on a `<button>` was inert and 17 buttons painted the
inherited colour · #6 `<select>` popups render light-grey (Radix Select instead) ·
#10 every flex/grid child in the pane chain needs `min-h-0` · #13 the sectioned
list is deliberately NOT virtualized · #15 window dragging is the
`data-tauri-drag-region` attribute + an ACL grant, and drag regions swallow
`mousedown` (register click-outside handlers in the CAPTURE phase) · #16 a
shortcut that fires while typing must be non-destructive · #23 theme CSS
silent-fallback traps (`woff2` format string, pre-hydrate alphas) · #24 a field
that UNMOUNTS loses its blur — commit drafts before closing an editor ·
#28 agent prose AND Notes render through ONE markdown surface, HTML/SVG
only through the sanitize allowlist (`lib/markdownSchema.ts`), links never navigate · #29 the leading box CLOSES, the ROW selects; ⌘Z is a stack that
outlives its toast · #32 a task row is a frosted CARD whose states compose
through VARIABLES, never whole declarations · #33 the row gap belongs to the
row's CONTAINER (padding), never the row — a margin collapses through the
virtualizer's measured element.

**macos-native** — #4 `UNUserNotificationCenter` TERMINATES any process outside an
.app bundle · #5 the re-sign is about hardened runtime, not the copy · #7
notification authorization is silent when denied · #8 native chrome follows
`NSApp.appearance` — set the theme in TWO places via `uiStore::applyTheme` · #9
right-click menus are RENDERED (Radix), not `NSMenu` — which is what makes ⌘Z
work there, and the row must NOT `stopPropagation` · #11 bind the bridge socket inside the Tokio
runtime · #17 `dialog:default` does NOT grant `confirm` · #22 window transparency
is NATIVE (`NSVisualEffectView`) — `backdrop-filter` can't see past the window,
and no pane may sit on another pane's background · #26 `set_focus` is a no-op on
a minimized window + three other ways a notification click dies · #34 only the
bundle's MAIN executable may post a notification — helpers delegate to
`cxtasks --notify` · #35 **retired** — the helpers are built by `cxtasks-core`,
which has no `tauri-build`, so they no longer carry an embedded `__info_plist`;
`build-helpers.sh` asserts it rather than trusting it. · #43 a rebuilt
`target/release/cxtasks-server` is killed at spawn by launchd
(`OS_REASON_CODESIGNING`) although `codesign --verify` passes and it runs from a
shell — cargo's linker-signed ad-hoc signature isn't enough; re-sign with the
Developer ID before `kickstart`. · #47 the glass must be turned on AFTER
the first paint (a transparent window with an empty webview is bare wallpaper —
the old material hid that gap by painting something, a CGS blur paints nothing);
clear the background to alpha 0.01 not 0, or AppKit chamfers the corners.
· #49 `NSPasteboard` is main-thread only — a clipboard read in
`spawn_blocking` races WebKit's own ⌘V read and SIGSEGVs; read through
`run_on_main_thread`.

**mcp-plugin** — #3 the live MCP binary is the one in /Applications, not
`target/release` · #14 `claude plugin update` compares the VERSION, not file
content · #27 `#[tool_handler]` calls the STATIC router unless told
`router = self.tool_router` · #48 Claude Code shows only the first 2,048
characters of the server instructions — a rule past that is invisible, so
formatting rides on the field descriptions.

## One verb table

Every operation the app can perform lives in **`cxtasks_core::api::dispatch`**,
a single `match` keyed by the names the frontend already invokes. The Tauri
commands in `src-tauri/src/commands/` are wrappers over it; so is
`cxtasks-server`'s `POST /rpc`.

The point is what that makes impossible. A server with its own implementation of
"complete a task" drifts within a month — the second copy forgets that
completion materializes the next recurrence instance, or that a `project` change
prefills `repo_path`. There is now nowhere for a second answer to live.

- **Arguments are snake_case.** Tauri converts camelCase from JavaScript before
  it reaches core, so the wire format matches the database.
- **`api::HOST_LOCAL`** names the verbs a server must refuse — foreground
  dispatch, the launchd registration, anything resolving a local path. A refusal
  says *which* kind it is, because "only runs on the host that owns the files"
  is actionable and "unknown command" sends you hunting a typo. The two
  attachment ADD verbs are in it because they take source paths on local disk,
  which is why uploading bytes has its own HTTP route rather than a verb.
- **`api::DELETES_ROWS`** is the only other list, and it is *discovered by test*
  rather than declared — a verb that removes rows and isn't in it never reaches
  the peer, and the next pull resurrects what it removed. That test is only as
  good as its fixture: `delete_task_attachment` sat outside the list for as long
  as the fixture had no attachment row to delete.
- **A patch round-trips through JSON**, so `TaskPatch` carries
  `skip_serializing_if` on every field. Without it an untouched field encodes as
  `null`, the server reads "clear this column", and a patch setting one thing
  wipes the rest of the task.
- **`NewTask.id` is optional and `create` is idempotent on it.** That is what
  makes an offline capture safe: a retry after a lost response returns the
  existing row instead of making a duplicate.

## Conventions

- Package manager: npm. Dev server on **5273** (cxmail owns 5173).
- Rust errors: `thiserror` (`AppError`) + `anyhow` for ad-hoc.
- IPC: `#[tauri::command] async fn -> Result<T, AppError>`, typed wrappers in
  `src/lib/tauri.ts`.
- Timestamps: **two formats coexist in one DB** — Rust writes ISO-8601 UTC with a
  trailing `Z` for instants the user set; SQLite writes `datetime('now')` for
  bookkeeping. Never compare either form byte-wise (invariant 1). Full rules +
  the two millisecond exceptions: `.claude/rules/database.md`.
- `TaskPatch` semantics: absent key = leave alone, explicit `null` = clear.

## Where this is going

[`docs/roadmap.md`](docs/roadmap.md) holds the phase plan — what is done, what is
next, the settled decisions, and the list of approaches already rejected with
their reasons. **Read it before starting multi-host or sync work**, because the
rejected list is the expensive half: field-level merge, an oplog table,
`MAX()` for `next_short_id` and a thin-cache Mac all look reasonable until you
know why they were ruled out.

**Every phase is now built.** Phase 3 (a second executor host) and Phase 4
(repo-less runs) landed 2026-08-21; the NAS executor is packaged and
deliberately **not started** — `deploy/nas/runner/deploy.sh` loads the image
and `--start` is a separate decision. Phase 5's second release (agent
tripwires, `agent:<condition>`) landed 2026-09-02 with no schema change.

Historically: Phase 2 (sync) is built, deployed
and proven between two nodes: `crates/cxtasks-core/src/sync/`, the `/sync/*`
routes, `/attachments/*` for the bytes (invariant 23), and a canonical node on
the NAS. What it still owes is the cutover itself. **`docs/roadmap.md` § Phase 2 records where the shipped
design departs from the planned one** — state travels as post-images and only
deletions replay as verbs — and `docs/remaining-plan.md` §3 is the older sketch,
now banner-flagged.

Sync is **on** on the live database, paired to the NAS canonical node over
the tailnet (`sync_peer_url` in `app_meta`), and **reconciles on a five-minute
timer while the app is open** — `cxtasks_core::sync::auto`, spawned as its own
thread in `lib.rs`. Before that loop existed the Mac only reconciled when
somebody pressed the Settings button, and the failure had no symptom: the live
DB was found seven days behind, twice, with nothing erroring. Settings and
`sync-control.sh status` now report the last **attempt** beside the last pull,
because a stale pull time alone cannot tell "nothing is trying" from "every
attempt is refused". Pausing and unpairing are in the app's Settings, or
`scripts/sync-control.sh`; `sync-control.sh now` (or
`cxtasks-runner --sync-now`) reconciles once from a shell.
`scripts/sync-dress-rehearsal.sh` runs the whole pairing against a `.backup`
copy with the cutover's five gates.

## Documentation map

**Path-scoped rules** (`.claude/rules/`, loaded when editing matching paths —
read them directly when a task spans layers; invariant/gotcha numbers in the
indexes above name the file):

| File | Holds | Scope |
|------|-------|-------|
| `database.md` | Data model, migrations, tags/deps/comments, timestamps; invariants 1–3, 9–11, 13–14, 16–17, 20–21; gotchas 1, 2, 12, 30 | `src-tauri/src/{db,commands,mcp}/**` |
| `execution.md` | Runner, dispatch, executors, intent, budgets; invariants 5–7, 12, 15, 18–19; gotchas 18–21, 25, 31 | `runner/**`, `dispatch.rs`, `executor.rs`, `intent.rs`, `checkpoint.rs`, `launchd.rs`, `bin/**` |
| `frontend.md` | Component map; gotchas 6, 10, 13, 15, 16, 23, 24, 28, 29 | `src/**` |
| `macos-native.md` | Shipping/signing detail; gotchas 4, 5, 7–9, 11, 17, 22, 26 | `*_macos.rs`, `lib.rs`, `Info.plist`, `tauri.conf.json`, `capabilities/**` |
| `mcp-plugin.md` | Slash commands, MCP surface, writing prose that renders; invariant 4; gotchas 3, 14, 27, 48 | `src-tauri/src/mcp/**`, `claude-plugin/**` |
