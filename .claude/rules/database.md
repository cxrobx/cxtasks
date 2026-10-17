---
paths:
  - "crates/cxtasks-core/src/db/**"
  - "src-tauri/src/commands/**"
  - "crates/cxtasks-core/src/mcp/**"
---

# Data Model & Database Rules

Schema, tables, and every DB-side invariant and gotcha. Invariant and gotcha
numbers are **global across the rules files** — commits, tests and other entries
cite them as "invariant N" / "gotcha #N", and the index in `CLAUDE.md` maps every
number to its file. Numbers here are therefore non-sequential; that's deliberate.

## Data Model

`schema.rs` is at `CURRENT_VERSION = 29`. Every migration is `IF NOT EXISTS` or
version-gated, and the stamp only ever moves **forward** — three binaries share
one DB and don't upgrade in lockstep, so a leftover older `cxtasks-runner` must
not re-stamp a v4 file back to v3 and send the next new-binary launch through
migrations against columns that already exist. Don't hardcode the number in a
test; compare against `schema::CURRENT_VERSION` (a literal there broke a test on
every migration for no signal).

**A version number is spent the moment any binary stamps it on a real DB —
including a `tauri dev` run.** So adding columns to an existing
`create_tables_vN` is only safe while N is genuinely unshipped, and the local
database is the thing to check, not the git log. v9 exists because of exactly
this: the batch columns were briefly folded into v8 on the reasoning that both
halves ship in one build (the v6 argument), but a dev build carrying only v8's
Codex half had already run against the real DB, leaving it stamped `8` with
`bg_executor` present and no batch columns. `version < 8` never fires again
there, so the folded columns would never have been added — and the symptom is
not a migration error but every query naming them failing at runtime, on the one
machine that matters. Check with
`sqlite3 "$HOME/Library/Application Support/com.cxtasks.app/cxtasks.db" "SELECT value FROM app_meta WHERE key='schema_version'"`
before folding anything into the newest migration. A spare integer is cheap; a
silently missing column is not.

| Table | Since | Notes |
|---|---|---|
| `projects` | v4 | Gives a project **independent existence**. Before v4, `tasks.project` was a bare text column, so a project only existed while some task referenced it — which makes a "+ New project" button impossible: you'd create one and it would vanish before filing anything into it. The text column remains the join key, and the migration backfills from the distinct values already in use, so no existing project disappears. |
| `sections` | v4 | Subdivisions for grouping. `project = <name>` scopes a section to that project; `project IS NULL` scopes it to a **view** — so Today can split into "morning" / "after standup" without inventing a fake project. Names are unique per project, so "phase 1" can exist in two. `tasks.section_id` references it `ON DELETE SET NULL`. |
| `tasks.deleted_at` / `.archived_at` | v5 | Trash and Archive, as **nullable timestamps rather than statuses** — a trashed task must remember it was `in_progress` so restore is exact rather than a guess. Millisecond precision so the Trash can order two deletions made inside one second (same reason as gotcha #12); since v9 they are **plain timestamps and nothing more** — batch identity moved to `delete_batch`/`archive_batch`. Four states, and archived-*and*-trashed is deliberate: restore returns such a task to the Archive it came from. |
| `tasks.delete_batch` / `.archive_batch` | v9 | The **batch key** tying a subtree's deletion together, as a UUID. v5 used `deleted_at` itself, which made batch identity a function of *when* rather than *which*: two `trash()` calls in the same millisecond produced a byte-identical stamp and merged into one restore batch, so restoring either resurrected the other — the exact thing the batch key exists to prevent. Reachable from the app on any fast multi-delete or MCP loop; the regression test for the property failed ~4 runs in 5. Milliseconds only narrowed the window, and no timestamp precision closes it, because the three binaries sharing this DB stamp on their own connections. **The effective key is `COALESCE(batch, stamp)` everywhere** (`tasks::Hidden::batch_key`) — that fallback is what makes v9 safe against binary skew, since a leftover pre-v9 writer keeps stamping with a NULL batch and those rows must degrade to v5's behaviour rather than collapsing into one NULL batch where restoring anything empties the Trash. It is also why no ongoing backfill is needed (compare `short_id`): the migration converts the rows already in the Trash, and skew-written rows behave exactly as they did before until they're next re-trashed. |
| `tasks.short_id` | v6 | The speakable reference, rendered **T42** (Meta convention; `T`/`t` both parse). Nullable with no default, which is the opposite of under-specification: three binaries share this file and a leftover pre-v6 `cxtasks-mcp` goes on writing NULLs until the ship cycle replaces it, so the number can't be a schema-level invariant. `tasks::backfill_short_ids` runs on **every** `initialize`, not inside the v6 gate — the v6 backfill and the skew heal are the same code path. The counter lives in `app_meta['next_short_id']` and is claimed by one atomic `UPDATE … RETURNING`; **never reuse is the invariant, gaps are fine**. |
| `projects.health` | v6 | Nullable, no default — NULL is "never reported", a different and more honest claim than `on_track`. Written in exactly ONE place (`project_updates::post`); there is deliberately no bare setter, so a status always has an explanation behind it. **No longer the board's primary signal** — see `BoardRow.activity` below. |
| `BoardRow.activity` (derived, no column) | — | **How alive a project is, from the clock alone**: `Active` (touched within `ACTIVE_WITHIN_DAYS` = 3, which spans a weekend so Friday's work still reads as active on Monday) · `Idle` · `Dormant` (`DORMANT_AFTER_DAYS` = 30) · `Empty` (nothing has ever happened). Computed from the newest of `tasks.updated_at`, `tasks.created_at`, `task_comments.created_at` and `task_runs.started_at`, every branch filtered to `LIVE` so a trashed task's history can't keep a dead project looking alive. **It exists because `health` did not survive contact with a firm-of-one**: after two months the real DB held four projects, zero health values and zero updates, while the board rendered all four in warning colour for a lapse nobody had opted into. Health measured a *reporting habit*; this measures the project. The state is resolved in Rust (`ActivityState::from_idle_days`) rather than in each renderer, because the app and the MCP both print it and duplicated thresholds drift — the same argument `tasks::actionable_sql` makes for the date gates. `Empty` is deliberately not `Dormant`: "you have left this project" and "this project has never begun" are different sentences, and only the first is a nudge. Health still renders **when it was deliberately set** — a written "at_risk" outranks "something moved yesterday" — it is simply never invented, and its absence is now silent. |
| `project_updates` | v6 | The written log behind every health change. Keyed by project **name** (matching `sections.project`), which makes two cascades mandatory rather than optional — see invariant 11. Millisecond `created_at` + a `rowid` tiebreaker for the same reason as `task_comments` (gotcha #12), ordered newest-first because a status log is read for the latest. |
| `sections.target_date` / `.completed_at` | v6 | **A section carrying a target date IS a milestone** — no new grouping concept. Full ISO-8601 UTC with a trailing `Z` like `due_at`; a bare `YYYY-MM-DD` shifts a day once JS parses it as local. `completed_at` is set by hand, deliberately decoupled from the task counts: deriving it would un-complete a milestone the moment a follow-up task is filed into the phase. |
| `tasks.bg_shell_allowed` | v7 | The middle tool tier: a shell without `Edit`/`Write`. `INTEGER NOT NULL DEFAULT 0` — additive, and the safe value and the default value are the same, so the binary skew this DB always has needs no backfill (compare `short_id`): a leftover pre-v7 writer simply never sets it and every task it files lands at the most restrictive tier. Writes outrank shell — the pair is a precedence, not a combination. |
| `tasks.bg_executor` / `.bg_codex_allowed` | v8 | The second background agent, and the reason it takes **two** columns instead of one. `bg_executor` alone would be unsafe: a leftover pre-v8 `cxtasks-runner` drains with `... AND bg_allowed = 1` and has never heard of the new column, so a Codex task stored as `bg_allowed = 1` gets claimed by it and run with **Claude** — wrong agent, wrong prompt, wrong tool grants, silently. So the enabled flag lives per-executor (`bg_allowed` stays Claude's and keeps its exact legacy meaning; `bg_codex_allowed` is Codex's), which makes a Codex task simply invisible to the old runner. `Task.bg_allowed` is then **computed** at row-read time, so nothing downstream knows. See invariant 15. |
| `task_runs.executor` | v8 | Which agent produced this run. On the **run**, not derived from the task: a task's executor can be switched later, and deriving it would retroactively relabel every historical run. Foreground runs are always `claude`. |
| `tasks.status` (reshaped) | v12 | Eight statuses became **seven on one axis** — see invariant 16. `next`→`todo`, `needs_review`→`in_review`, and `cancelled` is **gone**: dropping a task is archiving it, so the migration archives those rows rather than remapping them to `done` (which would claim work finished that wasn't) or relabelling them to `todo` (which would resurrect every abandoned task, since `View::All` is `status NOT IN CLOSED_STATUSES` and that set just shrank to `["done"]`). `normalize_status` accepts the old spellings forward, and `heal_legacy_statuses` runs on **every** `initialize`, outside the version gates — same reason as `backfill_short_ids`: a leftover pre-v12 `cxtasks-mcp` keeps writing `'next'` long after `version < 12` stops firing, and an unknown status renders as a blank pill with nothing to explain it. |
| `tasks.bg_intent` / `task_runs.intent` | v16 | What a background run is **for** — do the work, or audit the last handoff. `TEXT NOT NULL DEFAULT 'work'` on both, so the safe value and the default coincide and there is nothing to backfill (v7's `bg_shell_allowed` argument) and no ongoing heal — a heal would be actively *wrong*, since a run a pre-v16 runner recorded as `work` genuinely ran work framing. **One column, where `bg_executor` needed two**, and that asymmetry is the whole design: see invariant 19. The task's copy is a **one-shot arming** that `finish_run` resets on success, which is precisely why the run needs its own — otherwise nothing durably records that a review ever happened, and `history_block` could not label a critique as one. Sharper than `task_runs.executor`, which exists only because an executor can be *switched* later. No index (nothing filters on intent — that absence IS the skew argument), and not in the FTS trigger's `OF` list. |
| `tasks.blocked_reason` | v12, a HOLD since v27 | What the task is waiting on, outside CXTasks. Non-NULL means **held** — off every actionable view, in Waiting — independent of `status` (invariant 16). Released by the caller, or by `update` on entering `done` or `queued`. Nullable with no default, so binary skew needs no backfill; the v27 migration only moved `status = 'blocked'` rows back to `todo`, and the heal keeps doing so for a pre-v27 writer. It replaced the runner **appending `"Background run refused: …"` to `tasks.body`**, which was the exact thing the comments module warns against — body is the definition, comments are history, and folding them gives you a body nobody dares edit. |
| `tasks.repo_path` (backfilled) | v13 | No new column — the **one-shot fill** that gives every pre-existing task the repo its project already owned. v11 stored the mapping but only `TaskDetail.commitProject` ever read it in the project → path direction, so a task filed any other way sat blank while `project_repos` held the answer. `create`/`update` resolve it now; this is the rows that predate them. Version-gated on purpose — a blank workspace is a *choice* (not every task in a project is code work), so an ongoing heal would refill it every time it was cleared. See invariant 17. |
| `projects.kind` / `parent_id` / `target_date` / `closed_at` | v19 | **PARA: an area is a standing responsibility, a project ENDS.** All four additive, `kind` defaulting to `'area'` so every existing row backfills to what it already behaved as. `parent_id` is the one join in this schema keyed by ID rather than name (`ON DELETE SET NULL`) — see invariant 22 for why, and for the merge-branch line that failure mode costs. Nesting is capped at one level in `projects.rs`, not by a CHECK: the rule is a cross-row check on the parent's own `parent_id`, which a column constraint cannot see. |
| `project_repos` | v11 | **Which directories a project lives in** — the mapping that stops a project's NAME and its repo's FOLDER being derived from each other. `pocketbuddy` is at `~/Projects/finance-app`, and until v11 nothing recorded that, so three places papered over it by treating the folder name AS the project name (the detail pane's project autocomplete was fed by a `read_dir` of `~/Projects`; `file_task`'s docs and `/cxtasks:task` both said "conventionally the directory name"). Since `tasks::create` calls `ensure_exists`, every such string *became* a project — one phantom per repo whose folder doesn't match its product, permanently split from the real one. **Many repos per project** (half the portfolio is a web+iOS pair), **at most one project per repo** — the UNIQUE index on `repo_path` is what makes `for_repo_path` a function rather than a guess, and adding a repo another project owns is a named error, not a silent move. Keyed by project name, so it is the **fourth text join** and invariant 11 grows a member. The backfill is evidence-only (`HAVING COUNT(DISTINCT project) = 1`): a path two projects disagree about is left unmapped, because unmapped is visible and fixable where a confidently wrong owner is not. See invariant 17. |
| `tasks` | v1 | Todo fields plus a fully nullable execution block (`repo_path`, `prompt`, `bg_allowed`, `bg_writes_allowed`, `model`). NULL `repo_path` no longer disables dispatch: foreground dispatch (`Open in Claude` / `Copy Seed Prompt`) falls back to the user's home directory via `dispatch::resolve_repo_or_home`, so non-agent tasks are still first-class but not dispatch-dead. Background dispatch keeps the strict rule — `bg_allowed` can never be true without an explicit `repo_path` (enforced independently at the `list_queued_for_background` query, `file_task`, and the UI checkbox) — see invariant 12. `parent_id` = subtasks, `ON DELETE CASCADE`. |
| `task_runs` | v1 | One row per dispatch, written at spawn time so a crashed run stays visible instead of vanishing. v2 added `input_tokens`, `cache_read_tokens`, `cache_write_tokens`, `model` — v1's `output_tokens` alone understates consumption by orders of magnitude (invariant 6). Read per-task by `list_for_task`, and across every task by `recent_with_tasks` (the `recent_runs` command) — the latter joins `tasks` and is filtered to `LIVE`, so a trashed task's run never reaches Mission Control. **`status` gained `dispatched` and `abandoned` with no migration** — these are text in an existing column, and the rows that predate them are repaired by an ongoing heal rather than a version gate. They exist so `running` can mean one thing: a background run this machine is executing *now*. See invariant 20, which is also why a stranded row no longer wedges the runner shut. |
| `run_events` | v14 | Append-only, provider-neutral execution trace for background runs. Claude `tool_use`/`tool_result` and Codex command/file/MCP items are reduced to bounded summaries before persistence; raw provider JSON, reasoning, arbitrary tool inputs and tool output never enter the DB. Ordered by per-run `seq`, deduplicated by `run_id + call_id + phase`, and cascades with its run. The bridge carries only `run_id` as a refetch hint; the DB is the replay/recovery source of truth. |
| `tasks.tripwire` / `.tripwire_every_minutes` / `.tripwire_on_trip` / `.tripwire_state` / `.tripwire_tripped_at` + `tripwire_runtime` | v25 | **A task that waits for a CONDITION rather than a date** — invariant 26. The five task columns are the STABLE half: the spec (`script:<name> [arg]`, stored normalized), the cadence (NULL = manual), the on-trip policy (NULL = surface, `complete` = auto-complete so `blocked_by` releases), the state (`armed` \| `tripped` \| `error`, NULL exactly when the spec is), and the one-time tripped stamp. All nullable, no default, nothing backfilled — a pre-v25 writer never sets them and every existing row reads as "no tripwire". **`tripwire_runtime` is the VOLATILE half, on a 1:1 row of its own**, and that split is the load-bearing decision: sync applies whole-row post-images, so a "not yet, checked 14:05" written onto `tasks` would rewrite the task row the phone is editing at that moment. The runtime row holds the request, the lease (`lease_id` / `lease_host` / `lease_spec` / `lease_request_at` / `lease_expires_at`), the last terminal attempt (`checked_at` / `last_outcome` / `last_note`) and the consecutive-failure counter; it is `Synced`, swept on `updated_at`, emitted after `tasks`, created lazily and **reset — never deleted** — on clear or re-arm, so no verb joins `DELETES_ROWS` for it and the row goes only by cascade. Known limit: whole-row merge on this small row can lose a phone's request racing a Mac attempt (re-issuable) or re-check once early; if that bites, requests become an append-only `tripwire_requests` table. |
| `task_relations` | v26 | **Durable context for a changed plan** — invariant 27. `supersedes` is directional (`from_task_id` = newer replacement, `to_task_id` = older task); `relates_to` is symmetric and stored once in canonical UUID order. The table cascades from both task ends, is synced and swept as a small table, and stores no derived state. An older task with no run or comment is pure plan churn and the write is refused with instructions to delete it; an older task with a conclusion remains retrievable but is removed from `next_task` and both background-runner gates. |
| `task_attachments.sha256` | v22 | **The content address of an attachment's bytes**, which is what turns the local `attachments/` directory from a second master copy into a CACHE — deletable, re-fetchable, and checkable. Until v22 a row said only which file it meant (`stored_name`), never whether the file on disk *is* that file, so the only available repair was "wipe it and re-fetch" and nothing could ever notice one was needed. Nullable with no default, so the safe value and the absent value coincide and nothing needs backfilling (v7's argument): NULL means "no address on record", and such a fetch is accepted as served rather than refused — an unverified image is a weaker guarantee, never a wrong one. **Written once, at import, never afterwards** — see gotcha #36, which is the trap that decided it. |
| `tasks_fts` | v1 | FTS5 over `title, body, project` **only**. |
| `app_meta` | v1 | Schema version + the runner's budget settings (`db::meta`). Since v15 it also holds `bg_budget_reset_at`, the **manual reset**: the daily counters measure from `MAX(local midnight, that stamp)`, so zeroing them is one key rather than a destructive rewrite of `task_runs` — the runs keep every token they cost, only the "since when" moves. Read it only through `meta::WINDOW_START_SQL` / `meta::window_start`, which all three counters interpolate; three hand-written copies of that boundary would be three chances for the token cap and the run cap to disagree about which runs are in scope, and the symptom is one of them reopening and not the other. A stale stamp needs no cleanup job — tomorrow's midnight is later, so the window heals itself. The window filters on `started_at`, so resetting **mid-run** drops that whole run's spend from the counter, including the part it hasn't spent yet. |
| `app_meta['bg_daily_token_cap']` (raised) | v15 | The one-shot lift of a cap **nobody chose** — see gotcha #30. Gated on the exact pre-v15 default (`meta::LEGACY_DAILY_TOKEN_CAP`), never on `value < new`: "still on the default" and "deliberately set low" are different states, and only the first is the migration's to overwrite. They are distinguishable here only because there was no UI to edit the number until the same change that raised it — so an exact match can only be the default. |
| `app_meta['bg_max_turns']` (raised) | v17 | **The same lift, on the sibling setting** — gotcha #30's second instance, and required for the identical reason: the key is present in the real DB at `30`, so raising `meta::DEFAULT_MAX_TURNS` alone would have changed nothing on the one machine that matters. Gated on the exact pre-v17 default (`meta::LEGACY_MAX_TURNS`), never on `value < new`, and distinguishable for v15's reason — there was no UI for this number until the same change that raised it. Note what v17 does **not** add: `bg_run_token_cap` is a brand-new key, so `get_i64`'s default already applies and a migration would have nothing to correct. |
| `tasks.bg_timeout_minutes` / `.bg_run_token_cap` / `task_runs.timeout_minutes` | v18 | **A per-task run budget, and the record of which one a run got.** All three nullable with no default, so the safe value and the absent value coincide and nothing needs backfilling (v7's argument) — NULL means "use the global", which is what every pre-v18 row was already doing. Two on the task because the clock alone is nearly useless: a measured Opus run touched **24.6M of the 25M ceiling in 14 minutes**, so a task given 60 minutes on the default cap dies on tokens around minute 15 and the new clock never matters. Bounds ONE run; `daily_token_cap` is untouched and deliberately not overridable, so this cannot spend more than the day allows. **The run-side column is not bookkeeping** — see invariant 20, which it exists to keep true. |
| `tags`, `task_tags` | v3 | Shared many-to-many labels. |
| `projects.icon` / `tags.icon` (+ `color`, now written) | v29 | **How a node is drawn in the sidebar** — `lucide:<name>` or a small PNG `data:` URI (an app's own icon, found in its repos by `src-tauri/src/commands/icons.rs`, trimmed and scaled to 64px), and `color` as a hue NAME. Inline rather than an attachment: a few KB that rides the whole-row sync both tables already have, where the blob path (invariant 23) would be machinery for one icon. **Validated on write by `db::appearance`** — only PNG, magic bytes checked, ≤48 KB, no SVG, no URL — because every device renders the row. Only on `TagWithCount`, never the per-task `Tag`, which rides every task listing. Nullable, additive: a pre-v29 binary shows the default icon. |
| `task_deps` | v3 | `task_id` is blocked by `blocked_by_id`. |
| `task_comments` | v3 | Append-only per-task log. |

**Tags (`db/tags.rs`)** answer "what kind of thing is this" and cross projects —
`billable`, `waiting-on-someone`, `quick-win`. `tasks.project` is a different
axis: one free-text field answering "where does this live". Names are matched
case-insensitively by **both** `COLLATE NOCASE` on the UNIQUE index and
`tags::normalize` (trim, strip a leading `#`, collapse inner whitespace,
lowercase), so `#Quick Win` and `quick  win` are one tag. Skip either half and
you accumulate three spellings of one label while filters silently miss rows.
Multi-tag filtering is **AND** (narrowing), via `tags::task_ids_with_all`.
`rename` onto a name that already exists **merges** rather than failing the
UNIQUE constraint — that is what "rename A to B" means when B exists.
Tag names are **not** in the FTS index, so `search_tasks` will never find a task
by tag; use the `tags` filter on `list_tasks`.

**A tag is emergent; a project is curated — and that asymmetry is already in the
schema.** `projects` became its own table in v4 *specifically* so a project could
exist with no tasks in it, because otherwise "+ New project" is impossible. Tags
never got that argument and must not be given it: `upsert` has exactly one caller
(`attach`), so a tag comes into existence **only** by being put on a task, and
there is deliberately no "+ New tag" in the app or the MCP. `color` corroborates
it — nothing outside `tags.rs` has ever set it, so a tag holds no state that
re-typing the name would lose.

So a tag with no tasks left is **garbage, not vocabulary**, and `tags::gc` deletes
it rather than leaving it to pile up. Agents file tags ad hoc and schemes get
abandoned, so without this the sidebar fills with labels nobody will use again
and stops being worth reading.

Three things about it that fail quietly if bent:

* **GC keys on having no `task_tags` rows, NEVER on `open_count`.** They are
  different numbers, and the real DB had a case proving it: `outreach` showed 0
  in the sidebar while still being on a finished task. Collapsing the two
  destroys the labels on the Archive and silently breaks filtering over it.
  Hiding a 0 is a **display** decision the sidebar makes (and reverses the moment
  a task reopens); deleting is the data layer's, and only for a true orphan.
* **GC never touches a tag that is on tasks, so it is not a substitute for
  `delete`.** When an agent tags eight tasks `#urgnet`, cleaning that up is a
  bulk detach — that is what "Delete Tag…" in the sidebar's right-click menu is
  for, and why its confirm quotes `tags::usage_count` (every task, closed ones
  included) rather than the sidebar's open count. There is no undo, matching
  `deleteProject`: the undo stack is task-patch shaped and this spans a set the
  caller never enumerated.
* **It runs on the write paths AND as an ongoing heal in `initialize`** — both,
  for different reasons. Startup alone would leave a collected tag in the sidebar
  for the rest of the session, which is the staleness the feature exists to
  remove. The write paths alone would never reach orphans made by a leftover
  pre-GC `cxtasks-mcp` doing `tag_task` detaches. The heal sits **outside** the
  version gates, unlike v13's `repo_path` backfill, because that one refills a
  field a user may deliberately have blanked whereas a detached tag is something
  nobody is able to choose in the first place — so collecting one can never
  overrule a decision.

**A tag an AGENT coins is vetted first (`tags::vet_new`, 2026-10-02).**
Emergence alone produced 128 tags for 232 tasks, 70 used once — every session
coined its own word because nothing showed it the existing ones. `tag_task` and
`file_task` therefore refuse a tag that doesn't exist yet unless `new_tags:
true`, naming the closest existing tags (`similar`: same stem, a ≥4-char
prefix, or a typo away with the first letter matching — conservative on
purpose, since a guard that cries wolf teaches agents to override it). A tag
restating a project, a repo folder, or a field (`tripwire`, `blocked`, a
status) is refused even with the override. People, `decision` and
`agent-filed` are never vetted, and the app never calls it — Chris typing a
tag is the vocabulary's owner. `file_task` vets **before** the row exists, or
the agent's retry files the task twice.

The list renders each task's tags from `taskStore.tagsByTask`, one query for the
whole board rather than a field on `Task` — the same shape `deps::blocked_task_ids`
uses for the blocked badge, and for the same N+1 reason.

**Dependencies (`db/deps.rs`)** are *ordering*; `tasks.parent_id` is
*containment*. A subtask is part of exactly one parent and lives inside its
scope. A dependency is many-to-many between peers and can cross projects — "the
Acme deploy waits on the cxventures webhook fix" is two projects where neither
owns the other, which containment cannot express. Both directions are queryable
(`blocked_by`, `blocking`), and an edge **survives its blocker closing**:
`is_open` is derived from the blocker's status rather than by deleting the row,
so the record of what blocked what stays readable.

**Comments (`db/comments.rs`)** are append-only history; `tasks.body` is the
task's definition. Folding the second into the first is how you end up with a
body nobody dares edit because it also holds the record. `author` is free text,
and dispatched runs sign with `AUTHOR_CLAUDE` (`"claude"`) so their entries are
distinguishable from Chris's. See gotcha #12 for the timestamp precision this
needed. **Length policy lives at the MCP handler, not here** —
`mcp::server::COMMENT_MAX_CHARS` caps what an agent may file, while `add` stays
uncapped because the app's composer and `runner::finish_run` also write through
it; that is a policy about what an entry point accepts, not about what the table
stores (full text in `mcp-plugin.md`).


## Timestamps — two formats coexist in one DB

**Two formats coexist in one DB**, which is precisely why invariant 1
exists. Instants the user set or the app computed — `due_at`, `defer_until`, the
dates `recurrence` materializes — are written by Rust as ISO-8601 UTC with a
trailing `Z` and milliseconds, byte-identical to JS `Date.toISOString()`;
`recurrence::to_iso` is the emitter and `recurrence::parse_iso` the reader.
Bookkeeping timestamps are written by SQLite instead (column defaults or
`SET … = datetime('now')`): `created_at`, `updated_at`, `completed_at`,
`reminded_at`, `task_runs.started_at`/`finished_at` — UTC, space-separated, no
`Z`, second resolution. **Two columns are the exception and carry
milliseconds**: `task_comments.created_at`, because a burst of comments posted
inside one second otherwise ties and falls through to a random UUID (gotcha
#12); and `run_events.occurred_at`, because a run fires many tool calls inside
one clock tick. Note the second one is for *display* only — `run_events.seq` is
the ordering authority there, so no amount of timestamp precision replaces it.
Never compare either form byte-wise.

## Critical Invariants (database)

1. **Wrap both operands in `datetime()`** for every due/defer/reminder comparison.
   ISO-8601 `Z` timestamps compared byte-wise against `datetime('now')` fire up to a
   day late (cxmail gotcha #23). A 2020/2999 test fixture does **not** catch it — the
   regression fixture must share today's UTC date but be in the past:
   `date('now') || 'T00:00:00.001Z'`. Pinned by
   `db::tasks::tests::due_scan_fires_for_a_time_earlier_today_utc`, which asserts the
   *broken* comparison still reproduces the bug so the fixture can't silently rot.
2. **Never write `tasks_fts` directly** — only `db::search::rebuild`. User keywords
   must go through `db::search::build_match_expr` (cxmail gotcha #26).
3. **Never enable `PRAGMA recursive_triggers`.**

9. **P0 is the HIGHEST priority, P3 the lowest** — `db::tasks::PRIORITY_HIGHEST = 0`,
   `PRIORITY_LOWEST = 3`, Meta's convention. This is the *opposite* of the v1-v2
   schema, which stored 0 = none … 3 = highest, so the v3 migration remapped stored
   values `new = 3 - old` (order-preserving) rather than relabelling — a relabel
   would have silently turned every "drop everything" task into "whenever". The
   remap is behind the version gate because running it twice flips everything back
   and reads as data corruption; pinned by
   `db::schema::tests::v3_inverts_existing_priorities_exactly_once`. Two
   consequences are the easy things to get wrong: **every `ORDER BY priority` is
   ASC**, and a task created without an explicit priority defaults to
   `PRIORITY_LOWEST`, not 0. "No priority" no longer exists as a fourth value — an
   untriaged task is the least urgent one, not a separate category. The same
   inversion applies to `PRIORITY_COLORS` in `src/lib/format.ts`, which is indexed
   by priority, so index 0 is red. **A third consequence bit us for real:** any
   "is this worth showing" threshold flips too. `TaskRow` gated its priority chip
   on `priority > 0`, which under the old scale meant "has a priority" and under
   this one means "is not the most urgent" — so the chip was hidden on P0, the
   drop-everything priority, and printed on every untriaged P3. The tell was that
   `PRIORITY_COLORS[0]` (red) was unreachable dead code. The correct test is
   `priority < PRIORITY_LOWEST`, and `PRIORITY_EDGE` (the row's left edge bar)
   uses the same threshold by rendering P3 transparent — a bar on every row would
   make the scale mean nothing.
13. **Deletion is recoverable; exactly two code paths destroy data.** Every
    delete affordance — the ⌘⌫ shortcut, the detail-pane button, the native
    context menu, `trash_task` — routes to `db::tasks::trash`, a soft delete
    recoverable for `RETENTION_DAYS` (30). The hard `db::tasks::delete` has
    precisely two callers: `purge_expired` (the daily tick in `lib.rs`) and
    `destroy_task`/`empty_trash`, which are reachable only from inside the Trash
    view and only behind a native confirm. **A third caller is the thing to
    reconsider**, not to add.

    The filtering is where this feature dies if it dies. `db::tasks::LIVE`
    (`deleted_at IS NULL AND archived_at IS NULL`) exists as one shared constant
    precisely so the set of call sites is greppable, because soft delete fails
    one query at a time: miss the reminder scan and a deleted task still
    notifies you; miss `list_queued_for_background` **or** `claim_for_background`
    and a deleted task is handed to an unattended agent. Search is the one
    deliberate exception — it uses `NOT_DELETED` so archived tasks stay
    findable, since an archive you can't search is a black hole you can only
    browse. `get` is unfiltered on purpose: restore needs to fetch a trashed
    task. Pinned by
    `db::tasks::tests::a_trashed_task_disappears_from_every_path_that_hands_out_work`,
    which asserts absence from every hand-out path in one place — **a new
    listing query belongs in that test.**

    Two consequences that look like separate features but aren't: `trash` and
    `archive` stamp the **whole subtree** with one batch id so restore brings
    back exactly what went (a child trashed separately keeps its own batch and
    is not resurrected — the `{stamp} IS NULL` guard in `stamp_subtree` is what
    leaves it alone), and the recursive walks use `UNION`, not `UNION ALL`, so a
    hand-edited `parent_id` cycle terminates instead of hanging — the same
    defence `deps::reaches` needs.

    **The batch key must never be time-derived.** It was until v9 — the stamp
    *was* the key — and two deletions landing in the same millisecond therefore
    merged into one batch, so restoring one resurrected the other. Note where
    this is reachable and where it isn't, because getting that backwards is how
    you write a test that passes against the broken version: two unrelated
    *roots* can never merge however identical their stamps, since the batch walk
    descends by `parent_id` and never reaches a second root. The merge needs a
    batch whose downward walk passes **through** a row from an earlier one — a
    parent trashed over a child that was already trashed on its own. Pinned
    deterministically by `two_deletions_at_the_same_instant_are_still_two_batches`
    (and its `two_archives_…` twin), which force the collision instead of racing
    for it; the older
    `restoring_a_parent_does_not_resurrect_a_separately_trashed_child` samples
    the same failure by trashing back to back and is kept because it is the shape
    a user actually hits.

10. **Anything that hands work back must exclude blocked tasks** — including
    tasks whose only blocker has been trashed or archived. `deps::OPEN_BLOCKER`
    extends the closed-status test to the two hidden states, because a blocker
    that is merely *hidden* would go on blocking forever: `next_task` silently
    skips the dependent, the runner never picks it up, and the only symptom is
    work quietly vanishing from the board with nothing on screen to explain it.
    The edge itself survives and renders as no-longer-open, same as for a
    completed blocker. Both
    `db::tasks::next_task` and `db::tasks::list_queued_for_background` subtract
    `deps::blocked_task_ids` (one query, not N per task). Without it `/next` hands
    you a task you'd start and immediately discover you can't, and the runner burns
    a whole run to find that out. Cycles are rejected at **write** time by
    `deps::add`, which walks the graph: the table's `CHECK` can only catch
    self-blocking, and a longer cycle leaves every task in it permanently
    unactionable while `next_task` silently skips them all — the symptom is "the
    app stopped suggesting work" with nothing visibly wrong. The walk is
    `seen`-guarded so a hand-edited pre-existing cycle can't hang it.

11. **Grouping must never destroy what it groups.** Three v4 behaviours exist
    because the alternative silently loses work, and all three are pinned by tests
    in `db::projects::tests` — don't "simplify" them. (a) **`projects::rename`
    updates `tasks.project` AND `sections.project` in the same call**; those are
    text joins, so renaming only the `projects` row orphans every task in the
    project, and a rename that loses its tasks is worse than no rename.
    (b) **`projects::delete` removes only the project record** — tasks keep their
    `project` text and simply stop appearing under a sidebar entry, with unfiling
    them an explicit second argument. Deleting a grouping must not delete the work
    inside it. (c) **`tasks.section_id` is `ON DELETE SET NULL`, never CASCADE**,
    so deleting a section ungroups its tasks; tidying up how you group things must
    not be a way to lose tasks. v6 adds a fourth text join to the same set:
    `project_updates.project`. `rename` carries it (a rename that leaves the log
    behind makes the project read as never having been reported on — exactly the
    signal the board exists to make trustworthy), and `delete` **removes** it,
    unlike tasks. That asymmetry is deliberate: tasks are the work and must
    survive, while the log is a record *about* this project record, so leaving it
    behind means a later project reusing the name inherits a stranger's status
    history. v11 adds a **fourth** text join, `project_repos.project`, which
    follows the log's rule for the same reason and with a sharper failure mode:
    a rename that drops it un-fixes the very mapping the rename was performed to
    fix, and a delete that leaves it behind hands the next project reusing the
    name a stranger's repos — which then silently resolves *tasks* into them.


14. **A T-number is permanent, and health only moves in writing.** Two v6
    rules that fail quietly if bent.

    `short_id` is claimed once and never reissued — `meta::claim_short_id` is a
    single `UPDATE … RETURNING` so two of the three binaries can't hand out the
    same number, and `backfill_short_ids` scopes its UPDATE to `short_id IS
    NULL` so it can never renumber. Gaps are fine; a number claimed and burned
    by a failed insert is invisible. **Reuse is not**: "T42" gets written into
    commit messages and comments, and re-pointing it is unrepairable after the
    fact. Every task-id parameter on every MCP tool resolves both forms
    (`tasks::resolve_ref`) — a T-number that works in `get_task` but not in
    `block_task` is worse than none, because you only find the gap when you're
    relying on it. `search_tasks("T42")` is a **lookup**, not a search:
    `short_id` is deliberately outside the FTS index, and the lookup reaches
    trashed/archived tasks (marked as such) because that's the case where you
    most need to be told the task was deleted rather than told nothing. That
    branch is skipped when the call also carries filter facets (invariant 21):
    "T42 that is also done" asks to *test* a task against a predicate, and
    answering it with a bare lookup would hide whether the predicate held.

    `projects.health` is written **only** by `project_updates::post`, which
    inserts the explanation in the same call. Adding a bare `set_health` is the
    thing to reconsider, not the obvious convenience: a dot that flips without a
    reason decays into decoration within a month, everything green because
    nobody wrote down what green was standing on. The UI honours the same rule —
    the board's only health control is the update composer.

    **The staleness half of this rule was removed, and the reason is worth
    keeping.** It used to say: render staleness as loudly as bad health, because
    a project at `on_track` whose last update is six weeks old is unobserved. The
    logic is sound and the feature still died — after two months the real DB held
    four projects, **zero** health values and **zero** updates, so the only thing
    the board ever did was flag all four for a lapse nobody had opted into. An
    accountability mechanism with an audience of one is a chore, and the discipline
    it demanded had no reader. What replaced it is `BoardRow.activity`: the same
    question ("is this being observed?") answered from data that already exists.
    The lesson generalizes past this board — **a signal that requires an input
    nobody is prompted for will read as an accusation, not as information.** So
    the health dot is gone from the row, the written log stays as garnish for the
    projects where a client makes it worth writing, and `isUpdateStale` was
    deleted outright rather than left dead (`format.ts` carries a note saying not
    to bring it back).


16. **`status` answers ONE question — how far along is this work — and anything
    a rule can derive is off it.** The v12 reshape, and the discipline that
    keeps it from drifting back.

    The pre-v12 set failed MECE because it crammed **four axes** into one field:
    triage (`inbox`), scheduling (`next`), the background runner (`queued`,
    `needs_review`), and dependencies (`blocked`) — so a task could genuinely be
    untriaged AND ready AND queued at once, which is exactly why those three read
    as interchangeable. The six that remain (`inbox` · `todo` · `queued` ·
    `in_progress` · `in_review` · `done`) sit on one axis, and each has a
    one-line definition in `STATUS_HINTS` — **a status you cannot define in a
    sentence is one that will be used for whatever is nearby.**

    **`blocked` is a HOLD, not a status (v27).** v12 kept it on the axis
    "because it is real" and had nowhere else to put it — which left one hold
    on the progress axis while the other three (a defer date, an armed
    tripwire, an open dependency) were derived. The cost was visible in the
    filter menu: Status had a "Blocked" and Waiting had a "Blocked" and they
    named different subsets, so "Status is not Blocked" still showed every
    dependency-blocked task. A hold answers *why can't I act*, a status
    answers *how far along*, and a task has one of each — `in_progress` AND
    waiting on a key is a real state a single field cannot say. So
    `blocked_reason` is the hand-set hold on its own: non-NULL means held,
    independent of `status`, and it is what GTD's *Waiting For* means — the
    thing there is no task to hang it off. Dependency-blocking stays derived
    from `task_deps` via `deps::blocked_task_ids`; `blocked_sql` unions the two
    for the one row badge, because on screen they are one fact.

    **`not_held_sql` is the complement of `waiting_sql`, and every actionable
    surface carries it** — Inbox, Today, Open, `next_task`, the decision strip,
    the board's actionable count. No hold of any kind: date passed, no
    tripwire armed, no reason written, nothing open in the way. That last
    clause is the v27 addition: dependency-blocking used to be subtracted in
    Rust by `next_task` and the runner alone, so Open still listed a task
    whose blocker was open and merely painted a badge on it. `NOT_DEFERRED`
    survives as the date-and-tripwire half, for the one place a blocker
    genuinely does not apply (the tripwire lane's own gate).

    **Since the Upcoming view was retired, `not_held_sql` is the WHOLE of
    `actionable_sql`.** It used to carry a date gate as well, so a task due
    next month was off Open — which made a deadline invisible until the
    morning it fell due, and disagreed with `next_task`, which has always
    filtered on holds alone. A due date says when work must be DONE; only a
    defer, a tripwire or a blocker says it may not be STARTED. The runner's drain and claim add `blocked_reason IS
    NULL` on top of the dependency subtraction they already did.

    **A hold is written or released, never resolved against the status.**
    `update` treats `blocked_reason` as an ordinary double-option field —
    string holds, `null` or empty releases — with exactly two transitions that
    release it for the caller: entering `done` (a closed task waits on
    nothing, and a recurrence's next instance must not inherit the hold) and
    entering `queued` (most reasons are the runner's own "Background run
    refused: …"; re-queueing IS the un-park gesture, and a reason left in
    place would keep the task out of the drain forever). The runner parks a
    refused or failed run as `todo` + reason. `normalize_status` folds a
    legacy `"blocked"` to `todo` so a pre-v27 client (the iOS app, an old
    plugin copy) that still sends status + reason lands as a held todo rather
    than bouncing; `heal_legacy_statuses` does the same to any row such a
    binary writes directly, reason intact. `TaskDetail` shows the hold as an
    always-present field under Status, exactly like "Defer until" — clearing
    the box is the release, and there is no staging, because nothing bounces.

    **`queued` is still a status, and that is deliberate.** It is the runner's
    atomic claim key (`UPDATE … WHERE status = 'queued'`), which invariant 15
    turns on — moving it to a column would put the one query that must not drift
    between binaries on a migration's critical path for a cosmetic win. It is
    instead absent from `USER_STATUSES` (both `db::tasks` and `types/task.ts`),
    so it is never *offered*; hand-picking it would enter the drain queue past
    every check `queue_background` runs first.

    The enforcement is in the entry points, not the data layer —
    `normalize_status` accepts every real status, because the runner writes
    `queued` through the same `update` path. That made the MCP an unguarded
    third route for as long as `update_task` passed status straight through:
    a task queued without the opt-in never matches `enabled_sql()` and sits at
    "Queued" forever, and an armed review with no finished handoff dispatches
    against a run the prompt can't show — the two guards claim-time preflight
    never re-checks. `update_task(status = "queued")` is therefore the queue
    *action*: it routes through `dispatch::queue_for_background`, the ONE
    implementation of guards + preflight + write that the app's queue command
    is now a thin wrapper over, and `file_task` refuses `queued` outright with
    the two-step named (the `cancelled` precedent). See the MCP rules file for
    the tool-facing half.

    **There is no second closed status.** `CLOSED_STATUSES` is `["done"]`;
    dropping a task means archiving it. Re-adding `cancelled` recreates the
    original break one level up, since "archived" and "cancelled" would then be
    two ways to say one thing. `normalize_status` therefore refuses it rather
    than guessing a target — `done` would claim work finished and `todo` would
    put it back on the board — and says where it went. The cost, accepted
    knowingly: the Archive can no longer distinguish "shipped" from "gave up".
    If that distinction is ever wanted, the answer is GitHub's — a nullable
    close *reason*, not a status.

17. **A project's NAME and its repo's FOLDER are never derived from each other.**
    The v11 rule, and the reason the mapping is stored rather than guessed.

    `pocketbuddy` lives at `~/Projects/finance-app`. `Artist Advisory` lives at
    `~/Projects/artisttracker`. **Basename-as-project-name is wrong for most of
    the portfolio**, and it fails in the least visible way possible: because
    `tasks::create` calls `projects::ensure_exists`, a directory name that
    reaches `tasks.project` doesn't merely mislabel one task, it *creates a
    project*. You end up with an empty "finance-app" on the board beside the real
    pocketbuddy, work split across both, and neither one complete. It was
    encoded in three places at once — the detail pane's project `<datalist>` was
    populated from a `read_dir` of `~/Projects`, and `file_task`'s parameter docs
    and `/cxtasks:task` step 1 both instructed the caller to use the folder name
    (the MCP description even cited the app's autocomplete as corroboration,
    which was true and was the bug). Fixing one and not the others just moves
    which entry point manufactures the phantoms.

    **Resolution runs in opposite directions at the two entry points, and that
    is not an inconsistency to tidy up.** Each derives what its own caller
    cannot know. An agent knows a working directory and not what the user calls
    the project, so `file_task` goes **path → project** via
    `projects::for_repo_path`, and `project_for_repo` exposes the same lookup for
    `/breakdown`, which needs the name *before* filing in order to name a
    section. A human picks a project from a list and doesn't want to type a
    path, so setting `project` goes **project → path** from the primary repo.
    Collapsing them onto one direction breaks whichever caller doesn't have
    that half.

    **Both halves live in the data layer, and that is what v13 fixed.**
    path → project was always in `file_task`, but project → path lived in
    `TaskDetail.commitProject` — so it ran on exactly one gesture, editing that
    one field. Every other route into a project left the workspace blank with
    the answer sitting in `project_repos`: the quick-add bar's `#pocketbuddy`,
    an explicit `project` on `file_task`, a `/breakdown` phase, a row dragged
    onto a project group. `projects::primary_repo` had **zero Rust callers**.
    The tell was a launch console reading "Needs repo" for a project whose repo
    the app could name. It is `tasks::create` / `tasks::update` now, the UI
    reads the filled path back off the patched task, and v13 backfilled the
    rows that predate it.

    **It only ever fills a blank, and only replaces a repo the project being
    LEFT owns.** The iOS half of a project is chosen deliberately, so a prefill
    that overwrote it would snap the workspace back to the web repo every time
    the project was touched — which is why the replace is gated on the project
    actually *changing* (case is not a change) and on `for_repo_path` naming the
    outgoing project as the current repo's owner. Moving a task to cxventures
    while it goes on pointing at `finance-app` is a live dispatch footgun; a
    repo owned by neither project is deliberate cross-project work and survives.

    **The backfill is one-shot, unlike `backfill_short_ids` and
    `heal_legacy_statuses`.** Those repair a value nobody is allowed to choose,
    so they sit outside every version gate and re-running them is always right.
    A blank workspace **is** a choice — plenty of a project's tasks are not code
    work — so a heal would silently refill the field every time it was cleared,
    with no way to make it stick. Filling a blank once is help; filling it
    forever is an argument. The accepted cost is that a task filed by a leftover
    pre-v13 `cxtasks-mcp` inherits nothing and is never healed later.

    **An explicit `project` still wins over the resolved one** — cross-project
    work is real — but `file_task` **says so** when the two disagree, naming the
    directory's registered owner. Silence there is how a task lands somewhere
    nobody looks. Equally, an unregistered directory resolves to **nothing**, and
    both the tool output and `/cxtasks:task` say not to fall back to the folder
    name: unfiled is visible and fixable, a duplicate project is neither.

    **Renaming a project onto an existing name MERGES** (as `tags::rename`
    already did), which is what makes the phantoms cleanable at all — refusing,
    which is what v4–v10 did, left no route back except hand-editing the DB. Two
    parts of the merge are load-bearing. Sections are absorbed **before** the
    text joins move, because `sections` is `UNIQUE(project, name)` and two
    projects each holding a "phase 1" would otherwise fail the constraint having
    already moved the tasks; the collision is resolved by moving the tasks into
    the surviving section and *then* deleting the duplicate, since deleting it
    first lets `ON DELETE SET NULL` silently ungroup them (invariant 11c). And
    the absorb is gated on the **name**, not on a `projects` row existing —
    `upsert_section` creates sections for `/breakdown` before any task lands, so
    a target with sections and no record is a real state — but skipped entirely
    when the names differ only in case, because `list_sections` matches
    `COLLATE NOCASE` and both "sides" would be the same rows, so every section
    would collide with itself and be deleted.


20. **`task_runs.status = 'running'` means a background run THIS MACHINE is
    executing right now, and the app never claims more than it can observe.**
    The `tasks.status` discipline of invariant 16 — one question, nothing a rule
    can derive — applied to the run table, where breaking it wedges the runner
    rather than just reading badly.

    **A foreground dispatch is terminal the moment it is recorded**
    (`runs::FOREGROUND_STATUS` = `dispatched`). The app hands a session to
    Ghostty and then has no visibility into it: it cannot show progress, cannot
    stop it, and cannot learn the outcome. `running` was therefore a claim it
    had no way to back up **or ever retract** — and nothing in the codebase
    called `runs::finish` for a foreground run, so *every foreground run ever
    recorded* sat at `running` forever, reported as live work on the task and in
    Mission Control. Found in the real DB at 5 of 5. This is less a bug that was
    introduced than one that could not not happen, which is why the fix is to
    stop making the claim rather than to track the session: what is knowable is
    that a session was opened. The live signal was always the TASK moving to
    `in_progress` — real, durable, and the thing you close when the work is done.

    **A background run whose process vanished is swept to `abandoned` by
    `db::runs::sweep_abandoned`, and this one is not cosmetic.**
    `runner::gate` reads `count_running_background`, which counts DB rows rather
    than the runner's in-process state, so a row stranded by a crash holds a
    concurrency slot **forever** — two of them at the default
    `max_concurrent = 2` and background execution stops permanently, the only
    symptom being that nothing ever runs again. The regression test asserts the
    **gate reopens**, not merely that the row count drops; the count is the
    mechanism, the gate is the bug.

    **The soundness of an age-based rule is the wall clock the runner already
    enforces**, and nothing else. `runner::process` hard-kills at
    `timeout_minutes`, so a background run still `running` well past that bound
    is not a long run — it is a row whose writer is gone. That argument holds
    whichever of the three binaries sweeps, which is what makes it safe to call
    from `initialize` while the runner is mid-batch in another process.
    **Consequently the cutoff MUST be read from settings at call time**: a
    hardcoded 20 against a configured 120 starts sweeping healthy runs half an
    hour in — the guard's own failure mode, inverted and silent. Pinned by
    `the_cutoff_follows_the_configured_timeout`.

    **Since v18 that clock is PER TASK, so the cutoff is per ROW.** The
    sentence above is the whole reason: if the soundness of the rule is "the
    runner would already have killed it", then the number to compare against is
    the one *that run* was started with, not the setting as it stands now. One
    global cutoff applied to a batch reclaims a healthy 60-minute run at 35
    minutes — freeing a slot that is genuinely occupied and destroying the
    record of a live run — which is the same failure a hardcoded constant
    produces, reached by a different route. So `runner::timeout_for` resolves
    the clock, `db::runs::start` **records it on the run row**, and the sweep
    compares each row against `COALESCE(timeout_minutes, <global>)`. Resolving
    it and forgetting to persist it silently restores the old behaviour.

    Two things about that SQL fail quietly. The `COALESCE` is not politeness:
    `||` on a NULL yields NULL, a NULL date modifier makes `datetime()` NULL,
    and a NULL comparison matches **nothing** — so a missing fallback doesn't
    sweep too much, it stops sweeping entirely and wedges the gate shut again,
    with every test that only asserts "the long run survived" still green. That
    is why `a_run_is_judged_against_its_own_clock_not_the_global_one` asserts
    the pair — a long run spared **and** a default-clock run swept in the same
    call — and why `a_pre_v18_row_with_no_recorded_clock_falls_back_to_the_global`
    checks its fixture really has a NULL before proving anything.

    **The accepted skew cost, written down so it isn't rediscovered as a bug.**
    A leftover pre-v18 binary — most likely a `cxtasks-mcp` from a session that
    started before the ship — runs `sweep_abandoned` on `initialize` with the
    old single-cutoff query, which cannot see the new column. It can therefore
    still reclaim a healthy long run early. The new schema has no defence
    against a query that predates it; the consequence is a lost run record and
    at most one slot of over-concurrency, it is recoverable, and it disappears
    as those sessions end. Same graceful-degradation shape as invariant 19, not
    invariant 15's executable divergence: nothing runs as the wrong agent.

    **`abandoned` is a third thing, not a synonym.** `failed` asserts the work
    broke, which is usually false — the common cause is the *runner* dying
    (gotcha #4) while the agent was fine. `timeout` asserts our own wall-clock
    guard fired, which is a specific event that writes its own row. What is
    true is that nobody is left to say what happened. Same argument as gotcha
    #31's "a run we stopped on purpose is not a run that broke", one step
    further along; and like every other run status it gets its name and colour
    **only** in `RUN_STATUS_LABELS` / `RUN_STATUS_CLASS`, or it renders as raw
    snake_case in `TaskDetail`. `dispatched` is muted rather than green on
    purpose: opening a session is not an accomplishment, and colouring it as
    success makes a task look worked the moment it was looked at.

    **Where each recovery runs is the difference between self-healing and
    restart-healing.** `heal_foreground_runs` is an ongoing heal in
    `initialize`, outside every version gate and needing no migration (these
    are text statuses) — the invariant-17 test applies: a blank `repo_path` is
    a choice so its backfill is one-shot, while a foreground row at `running`
    is not a state anyone can be in, so healing it can never overrule a
    decision. It dates the row to `started_at`, never to now, or a two-day-old
    session floats to the top of every newest-first list. `sweep_abandoned`
    runs in `initialize` **and on every `runner::tick`, before the gate is
    consulted** — its predecessor ran only at runner startup, which recovers a
    runner that *died* (the process exits, launchd restarts it, the reap fires)
    but not one that stays up for days and loses track of a run without dying.

    **One implementation, not two.** `reap_stale_running` did this job too,
    and merging it was the point rather than a tidy-up: it recorded `failed`,
    and it took the cutoff as a *parameter* so the policy lived at the call
    site as `timeout_minutes * 2` — a second place to get it right, and one
    that scales badly at both ends (2 minutes for a 1-minute timeout, 4 hours
    for a 2-hour one). Two functions with the same job in one file is how the
    constant that matters gets fixed in only one of them.

21. **There is ONE query builder, and a chip REFINES the view rather than
    replacing it.** Every part of this fails silently.

    **`db::search::filtered` is the builder; `db::tasks::list` is an adapter
    over it.** They were two, and that was tenable only while the chips
    *replaced* the view — one query knew about views, the other about facets,
    and they never had to agree. The moment a chip composes with a view they
    do, forever, about every facet: what `project` means with an area
    selected, whether a status excludes closed work, which of them applies the
    subtask rule. Two builders would have answered separately and drifted
    within a month, which is the argument `db::tasks::LIVE` already makes one
    layer down. `list` survives because the view/project/tag shape is what the
    MCP, `view_counts` and the older callers speak; it fills a `FilterSpec` and
    that is all it does. **The parity test is the load-bearing one** — for
    every view, the adapter and the builder must return the same rows in the
    same order — because an adapter that quietly drops a predicate shows work a
    view used to hide with every other test still green.

    **A view is a query, so an empty spec is only empty for a view-LESS
    caller.** `has_facets`' short-circuit guards the search scope, where an
    empty spec really is a caller bug; `filtered` with a view and nothing else
    is the ordinary case (it is what clicking a sidebar row does).

    **Closure is stated by the caller once a view is set.** The six per-view
    `status NOT IN ('done')` lines left `view_where` and became an explicit
    `exclude_statuses` — that is what makes the exclusion *removable*, since a
    predicate baked into the view can never be a chip you take off. So
    `exclude_closed` defaults to `view.is_none()`: a view-scoped caller has
    already said everything it wants about closure, and a second implicit rule
    would silently re-apply what the user just removed. `Done` and `InReview`
    keep their status predicate, because there the status IS the view.

    **Every negation must be NULL-safe, and this is the trap.** `NOT IN` over a
    nullable column is NULL for a NULL row, `NOT (… OR NULL)` is NULL, and NULL
    matches nothing — so a naive negative facet silently drops exactly the rows
    with nothing to exclude: "project is not Acme" hides every unfiled task.
    `project` gets `COALESCE(project,'')`, tags and people negate as a
    correlated `NOT EXISTS`, and the tripwire half of `waiting_sql` keeps the
    `COALESCE` it was already given for this. Pinned in both directions —
    `every_negative_facet_is_null_safe` builds the row that has none of the
    things being excluded, which is the row the wrong implementation loses.
    **Exclusion is ANY, not ALL**: "not tagged A, B" said out loud means
    neither, and ALL-negation would return a task tagged only A.

    **`waiting` is a set of KINDS, one per row badge** — `deferred`,
    `tripwire`, `blocked`, `*`. It was a bool, and the Waiting view groups by
    *why*, so "hide the deferred ones but keep what is blocked" was unsayable.
    The MCP still speaks the bool and maps it to `*`, so the sidebar row and
    the facet still share `db::tasks::waiting_sql` and cannot disagree
    (invariant 29). `blocked` means the badge — a written `blocked_reason` OR
    an open dependency — because a filter that matched only one of them would
    disagree with what is on screen. `hide_blocked` stays separate: it is
    deps-only.

    **Deferred and armed rows SINK to the bottom of their section, under every
    sort.** "Not yet" outranks "soon" when you are reading a list to decide
    what to pick up. The band sits under the section keys and over the sort
    keys, so a held row falls to the bottom of its own section rather than out
    of it; Waiting is exempt (every row there is held, and the view already
    groups by why) and so are Trash and Archive. It is invisible in Inbox,
    Today and Open, which exclude those rows outright — a project drill-down
    and `View::All` are where it shows, now that Upcoming is gone.

    **Sort is a band, not a replacement.** ORDER BY assembles as sections (only
    under the default sort, never in a hidden view) → sink → the sort keys →
    tiebreakers ending in `rowid` so the order is total. `Default` is each
    view's own order, and it is the only sort under which a manual drag means
    anything — `sort_order` is a tiebreaker, so a drop under an explicit sort
    would write positions derived from a sorted sequence. Undated rows sort
    LAST whichever way `Due` runs, or reversing the direction floats every one
    of them to the top.

    **Values within a facet OR; facets AND. Tags are the exception and narrow**,
    because that is already what the sidebar filter and `list_tasks` mean by a
    multi-tag filter — one gesture meaning two things in two places is worse
    than a visible inconsistency.

    **Date bounds are LOCAL days and both EXCLUDE the named day**, so
    before+after compose into a range without double-counting the boundary.
    Both operands wrapped in `datetime()` with the column side shifted to
    `'localtime'` (invariant 1), and the fixtures are built relative to *now* —
    a literal date cannot catch a local/UTC day shift. **An unparseable bound
    is a loud `Validation` error, never an empty result**: `datetime()` of
    garbage is NULL, NULL comparisons match nothing, and "no tasks due before
    then" is exactly what a typo must not read as. Same class of silence
    `build_match_expr` exists to prevent on the FTS side.

    **The MCP facets ride on `search_tasks`, not a new tool.** Invariant 18's
    argument one level up: `list_tasks` and `search_tasks` already both return
    task lists, and a third makes the choice a coin flip an agent sometimes
    loses — the tool count is pinned so nobody adds one. The negative facets
    and `sort` are deliberately app-only for the same reason. `query` is
    optional (facets alone are a query) and the T-number lookup branch is
    skipped when facets are present, since "T42 that is also done" asks to
    *test* a task against a predicate. **The result line names every filter it
    applied**, or "nothing is due before Aug 3" and "you typed a project that
    doesn't exist" are byte-identical strings, and an agent reading the second
    as the first reports nothing-found about a filter that never had a chance
    to match. `search_tasks` stays **absent** from `BACKGROUND_TOOLS`, asserted
    by test: growing a tool must never grow what an unattended run can reach.

    **On the UI side** (full detail in `frontend.md`): the chips are a strip
    under the header, not inside the search box; a LOCKED chip labels the
    view's own predicate and cannot be removed, a DEFAULT chip (`Status is not
    Done`) can; navigation resets the chips to the scope's saved-or-baseline
    filters rather than to nothing, and so does the initial state, because the
    first paint is a navigation too. The sidebar no longer dims — it is
    applying now. **The badge and the list can now honestly disagree**: an
    area's `open_count` counts open work (invariant 22) and knows nothing about
    a saved "hide blocked" default. The header count beside the visible chips
    is the number that describes the list.

22. **A grouping node is an AREA or a PROJECT, only a project ENDS, and areas
    nest to any depth.** (v19, PARA; depth lifted 2026-09-27.)

    **Since 2026-09-27 the one-level cap is gone** — Chris: "the whole idea is
    a nested structure". An area may hang off an area, carrying everything
    under it (Products → Artist Advisory → Monthly-Listener Retitle). Three
    rules remain, all in `projects::set_parent`: nothing nests inside a
    PROJECT (it holds sections, the level below), nothing nests inside itself
    or its own subtree (a cycle takes the loop off the sidebar), and a node
    keeps its kind when it moves (dragging an area in no longer demotes it).
    Everything that walked one level now walks the tree: `expand_scope` and
    `IN_SCOPE_OF_P` are recursive CTEs (`UNION`, so a hand-edited cycle
    terminates), `primary_repo` inherits from the NEAREST ancestor with a
    repo, promoting a project to an area keeps its parent, and the MCP board,
    the app's GSD board and the sidebar render a tree. Where the text below
    says "one level", read it as the history of the rule.

    One row type was doing two jobs. `CXVentures` never finishes; a wave of
    government-contracting certifications filed into it does. Sharing a row type
    meant sharing one board row, one badge and one activity strip — so the
    sprint's signal was averaged into the bucket's, and an area with thirty tasks
    read as permanently `Active` because *something* in a grab bag always moved.
    `ProjectKind` splits them: an **area** is a standing responsibility with no
    finish line, a **project** is finite and carries `target_date` / `closed_at`.
    Area → project → section, and a dated section is still the milestone concept,
    unchanged.

    **`tasks.project` is untouched — the NAME is still the join key for both
    kinds** (invariant 17), which is why not one task row had to move.

    **Everything unclassified defaults to `area`**, including the v19 backfill,
    `ensure_exists`, and an MCP `create_project` with no `kind`. The other
    direction would mark five permanent things as finite and start asking when
    each will be done — invariant 14's warning arriving from the far side: a
    signal needing an input nobody chose reads as an accusation.

    **Parenthood is keyed by `parent_id`, the one deliberate exception to
    name-as-join-key.** A fifth name join would be another line in
    `projects::rename`'s "every text join moves in the same breath" block, whose
    failure mode is silent. Parenthood is the one field meaningless without a row
    — a phantom project cannot have been given a parent — so the id costs nothing
    the name would have bought, and renaming an area carries its children for
    free. The price is one line in the MERGE branch: children must be re-pointed
    at the survivor *before* the source row is deleted, because `parent_id` is
    `ON DELETE SET NULL` and skipping it doesn't error, it silently flattens the
    hierarchy. Deleting an area orphans its projects to the top level for the
    same reason invariant 11b keeps a deleted project's tasks.

    **The roll-up lives in exactly one function, `projects::expand_scope`.** An
    area's badge, the list clicking it opens, its board counts, its activity
    strip and `next_task` all route through it. The badge's contract is that the
    number describes the list it opens (see `Project::open_count`), so a roll-up
    added to one side and not the other is a lie in both directions — pinned by
    `an_areas_badge_and_its_list_agree_after_nesting`. Archived children drop out
    of the scope, or closing a project would make its area look busier.

    **Project → directory may inherit; directory → project may not.**
    `primary_repo` falls back to the parent area's primary, so nesting a task
    doesn't silently strip its workspace. `for_repo_path` is deliberately NOT
    changed to match: that lookup has to be a function, and two projects
    answering for one directory is the exact ambiguity `project_repos` exists to
    prevent.

    **The same `ActivityState` gets different WORDS per kind** (in
    `mcp::server::activity_label` and `GsdBoard`, never a second threshold). A
    finite project untouched for a month is *stalled*; an area untouched for a
    month is *quiet*, and an area never wears warning colour for being quiet. One
    shared vocabulary would have to pick one reading and be wrong about half the
    board — the dilution this whole split exists to end, arriving in the label
    instead of the counts.



### Invariant 20 with more than one host (v20)

`running` still means "a background run this machine is executing now" — v20 is
what makes that enforceable rather than merely intended. Two columns:
`task_runs.host` and `task_runs.heartbeat_at`, both nullable, no default,
**nothing backfilled**.

**NULL host is a value, not a gap.** It means the run predates v20 — predates
there being more than one machine — so it came from the only host there was.
Backfilling it with the migrating machine's id would be wrong, because
migrations run wherever the DB is first opened by a v20 binary, which after a
`.backup` restore or a seed onto a second host is not necessarily the machine
that produced those rows.

**The sweep has three rules and the third is an omission**, so it reads as a
missing case unless you know:

1. Mine, or legacy (`host IS NULL`), past its own clock — the pre-v20 rule
   verbatim.
2. Any row whose heartbeat has gone stale (`HEARTBEAT_STALE_MINUTES`, ten missed
   beats). **Liveness, not age** — a healthy hour-long run on another host is
   indistinguishable from a dead one if you only look at `started_at`.
3. A **foreign** row with no heartbeat is **never** swept. It is either pre-v20
   or from a host that died before its first beat, and this process cannot tell
   which. Leaving it strands one slot on a machine that may be gone — visible in
   Mission Control, recoverable by hand. Sweeping it destroys a live run on a
   machine this one cannot see. That asymmetry is the whole argument, and it is
   the same one `ABANDON_GRACE_MINUTES` already makes.

Test the **pair**, never the half: one sweep call must spare a fresh-heartbeat
foreign run *and* reclaim a stale one. "The live one survived" also passes
against a sweep that reclaims nothing.

**`count_running_background` counts THIS host's rows.** It feeds `runner::gate`,
whose question is "do I have a free slot", and a slot is a local process.
Counting globally would put every other host permanently at `max_concurrent` the
moment this one is busy — draining nothing, showing no reason.
`count_running_background_all_hosts` exists separately for Mission Control,
which genuinely wants the other number.

**`schema::initialize` no longer sweeps.** It runs on every `cxtasks-mcp` launch
— every Claude session — and each one evaluated `datetime('now')` in whatever
process opened the DB. With a second host that is a process reaching across the
network to judge runs it knows nothing about. The sweep belongs to the runner,
which owns the slots.

**`db::runs::ensure_local` guards anything that resolves a run against local
disk.** `run_diff` and `restore_checkpoint` both take a checkpoint ref and hand
it to git in a locally-resolved repo. For `run_diff` a foreign run produced a
misleading "checkpoint no longer exists"; for `restore_checkpoint`, which resets
a working tree, it would have reset *this* machine's tree on the strength of a
run done elsewhere. The refusal names the host, because "this run executed on
mini" is actionable and "checkpoint no longer exists" sends you hunting a bug.

### Invariant 26 — a tripwire hides like a defer and never stays hidden after a failure (v25)

`defer_until` waits for a **date**; a **tripwire** waits for a **condition**.
While `tripwire_state = 'armed'` the task is hidden in exactly the places a
deferral hides it — Open, Inbox, Today, `next_task`, the background queue, the
GSD board's actionable count — and shows in **Waiting** with a badge (it
showed in Upcoming until that view was retired; Waiting is the one "not yet"
list). That is one edit, not five: `tasks::NOT_DEFERRED` became the **waiting** predicate,
`DEFER_PASSED AND TRIPWIRE_NOT_ARMED`, and `planned_sql` grew the complement.
`next_task` used to carry an inline copy of the defer clause; it now names the
constant, because an inline copy is precisely how one view would have gone on
handing out a task the others were hiding.

**Only `armed` hides.** `tripped` and `error` both surface the task — a check
that cannot run must never keep work out of sight, because a hidden task with
no way out is the failure a tripwire exists to prevent. A manual tripwire's
first error surfaces it (the user asked and deserves the answer now); a
cadenced one is allowed five consecutive, backing off `every × 2^failures`
capped at a day and never shorter than the cadence itself. Any success resets
the count. A spec this build cannot read at all is recorded as an error by
the runner, for the same reason.

**An agent check is the same attempt with a RUN ROW as its lease (Release 2,
2026-09-02, no schema change).** `agent:<condition>` is evaluated by a
bounded, read-only `claude -p` recorded on `task_runs` with `intent = 'check'`
— text in an existing column, which is why nothing migrated. `db::runs::
start_check` inserts the row AND takes the `tripwire_runtime` lease in one
`BEGIN IMMEDIATE`, with the run id as `lease_id`, so a run never exists
without its lease nor a lease without its run; it admits one check per host
on top of `max_concurrent` and changes NO task status — only `record` moves
the task, exactly as for a script. `check` is **machine-only**: `tasks::update`
refuses `bg_intent = check`, the picker and the MCP never offer it, and a
pre-R2 binary reads the row back as `work` and merely prints it (invariant
19's argument holds because no capability is ever derived from the stored
intent — the lane that STARTED the check pinned its grant). A verdict is not
a handoff: `history_block` and `reviewable_run` skip `check` rows, a not-yet
writes no comment, and the final message lives on the run row for anyone who
wants the reasoning. `UNSURE` is an ERROR, never a not-yet — a task must not
stay hidden on a shrug, and five in a row surface it. Tokens count in BOTH
`total_tokens_today` and the `tripwire_tokens_today` breakdown; the sub-cap
key `bg_tripwire_daily_token_cap` (default 5M) travels with the other budget
keys.

**The date gate is evaluated first.** `list_due` carries `DEFER_PASSED` alone,
so an armed tripwire on a deferred task is not checked until the date passes —
which is what keeps `on_trip = complete` from closing a task before the day it
was deferred to. "Check now" on such a task is refused naming the date.

**An attempt is a leased, CAS-recorded transition.** `claim` is one `UPDATE …
WHERE lease_id IS NULL OR lease_expires_at < now` — expiry is the ONE reclaim
rule, so a runner that crashes mid-attempt leaves a lease to expire and the next
tick re-runs it, and two runner processes (the LaunchAgent plus a hand-run
`--once`) cannot both run one attempt. `record` opens `BEGIN IMMEDIATE`, re-reads
the task (not live, closed, no longer armed, spec changed → DISCARD, releasing
the lease and writing nothing else), CAS-updates the runtime row on the lease id
(a reclaimed attempt's late result is dropped), and then — in the same
transaction — moves `tripwire_state`, writes the evidence as a comment under
`AUTHOR_TRIPWIRE`, and on `complete` calls `complete_with_recurrence`, which
opens no transaction of its own and so composes here (its short-id claim rolls
back with it, a gap-free version of the burn it already accepts). The
notification is posted by the CALLER after commit: a banner inside a transaction
that then rolls back announces something that did not happen. `requested_at`
is cleared only when it `IS` the `lease_request_at` the attempt served, so a
"check now" that lands mid-attempt survives it.

**Volatile bookkeeping lives on `tripwire_runtime`, never on `tasks`** — see the
v25 row for why (whole-row post-images). The consequence for the verb table is
that `update_task` is the whole write surface: set/replace/clear the spec,
cadence and policy (resolved TOGETHER, the `status + blocked_reason` shape, so a
floor is checked against the kind the task will actually have), `tripwire_state:
'armed'` as the ONE state a caller may write and only from `tripped`/`error`,
and `tripwire_check_requested: true` to stamp a request. Nothing evaluates a
tripwire inline — not the MCP, not the app. The runner is the only execution
path.

**Scripts are NAMES plus one hostile `$1`, never argv.** `tasks.tripwire` holds
`script:<name>` where the name is `^[a-z0-9][a-z0-9._-]{0,63}$` and resolves
under `paths::tripwires_dir()` (`tripwire::script_path`: `symlink_metadata`
refuses symlinks, the canonical path must sit beneath the canonical dir, regular
file, exec bit). The optional argument is bounded and printable, passed through
`Command::arg` and never a shell, and the script's contract says it is
untrusted data — the row it came from could be written by any client. Existence
on THIS host is the MCP's check (path named on refusal), not the DB layer's: a
synced row may legitimately name a script only the Mac has.

**One-shot across recurrence.** `complete_with_recurrence` builds the next
instance with no tripwire. Conditions are level-triggered — "an outside user
exists" stays true forever — so a carried tripwire would fire the moment the
next instance was created, and with `complete` would close every future
instance unread.

**A monitor is a standing check, and completing it means "handled".**
`tripwire_on_trip = 'monitor'` (no migration: the column is TEXT, and an older
build reads the value as `surface`) is for a tripwire that should fire every
time something breaks — the weekly CLAUDE.md-paths audit (T107), the context
budget (T203) — as against a gate that waits once. Before it existed those
tasks were gates with "do NOT complete this" in their notes, and completing one
ended its schedule without a word, because `list_due` skips closed tasks. The
rule is enforced where the check RUNS, not at the doors that set `done`:
`list_due` and `record` accept a done monitor, so no route — the app, the MCP,
the iPhone through an older server, a synced post-image — can switch the check
off by completing it. Only archive, trash or clearing the spec does. Around
that: `tasks::update` re-arms a tripped or errored monitor when it closes (the
Re-arm write, runtime reset included, so the fix is re-checked on the next
tick); `record` reopens a done monitor to `todo` on a trip or a surfacing
error; and `tripwires::rearm_handled_monitors`, run at the top of every tick,
re-arms a done-and-tripped monitor that arrived without passing through that
update. A monitor can't carry a recurrence (refused on create and update):
completing a recurring task materializes an instance with no tripwire. It is
not a hold — `isTripwireArmed` stays false for a done monitor, and the app's
"watching" state is `isTripwireWatching`.

**An interpretation reads a trip's evidence; it never acts on it.**
`tripwire_on_trip = 'interpret'` (no migration, the TEXT column again; an
older runner reads it as `surface` and just posts the raw evidence) answers
what a script's own output could not: `io-write-report` writes ~5,300
characters and `last_note` keeps 500. After the trip is recorded, the script
lane's worker starts one `claude -p` with intent `interpret`
(machine-only, like `check`) on the evidence the script actually wrote, in an
empty app-owned scratch dir, with **zero tools** (not even `Read`), so there
is no workspace for it to be bounded by; 2 turns, 1M tokens, 5 minutes
(`tripwire::INTERPRET_*`). The Mac still refuses repo-less WORK at every tier.
It is one of four `OnTrip` values, so it can't also be a monitor. Every
refusal to interpret is a log line only, because the evidence is already on
the task as a comment.

Pinned by `db::tripwires::tests` (the CAS pair, discard on a changed spec, the
request-retention rule, the thresholds, the trip transaction releasing a
dependent, and an injected mid-transaction failure leaving nothing half-applied)
and by `db::tasks::tests`' hide/surface set, which the trashed-task sweep test
also covers because `list_due` is now on its list.

### Invariant 27 — superseding preserves the conclusion, not the chore (v26)

Dependencies answer ordering; `task_relations` answers history. A
`supersedes` edge points from the newer replacement to the older task. Both
directions are rendered in the app and MCP, search labels the older row as
superseded, and retrospective evidence records its closure as `superseded`
rather than `completed`. The older row keeps its body, runs and comments, but
`next_task`, the background list and the final background claim all subtract
it. The claim repeats the predicate deliberately: a runner holding a stale list
must still be unable to execute replaced work.

Only work with a conclusion earns that history. `relations::add` requires at
least one run or comment on the older task, rejects self-links and cycles, and
is idempotent. A title/body that nobody acted on is plan churn; delete it rather
than growing a second archive disguised as a graph. `relates_to` is symmetric,
so canonical endpoint order gives one durable row no matter which task the
caller names first.

### Invariant 29 — people and decisions are tag conventions, and Waiting is one predicate

Three things the task data could not say before 2026-08-25, and what was
found when it could: ~40% of the open set was hidden behind a date or a
condition with no view that listed it; five P1 rows that only Chris could
answer had sat in Inbox for weeks because they fit neither "dispatch" nor
"date"; and the one metric the Focus Map's named risk turns on — is the
quarter's output code, or introductions? — was uncomputable because every
task carries a `repo_path`, including "ask Dana for a testimonial".

**A person is a tag whose name starts with `@`** (`@sam`). **A decision is
the tag `decision`.** Both are conventions on the existing `tags` table, on
purpose: no migration under three binaries that don't upgrade in lockstep,
sync for free (`tags`/`task_tags` are small synced tables), an agent sets one
with `tag_task`, and removing the tag is the whole undo. The constants are
`db::tasks::DECISION_TAG` and `db::tags::PERSON_PREFIX`; `tags::is_person`
and `tags::person_tag` are the only two places the `@` rule is spelled.

What makes a convention more than a naming habit is the behaviour it changes:

- **A decision is on Today** (`View::Today` ORs `is_decision_sql()`) — **even
  when it is a subtask**, which is how older agent follow-ups were filed and
  how three P1 decisions sat unseen under their parents; Today alone relaxes
  the `parent_id IS NULL` rule for them. **Off Inbox** (the same reason
  in-flight work is — one row must not read as two), still in Open, and
  **never handed to an agent**: `next_task`,
  `list_queued_for_background` and `claim_for_background` all carry
  `not_decision_sql()` beside `NOT_SUPERSEDED`, and `queue_for_background`
  refuses with the remedy named. The MCP's `next_task` lists the open
  decisions under its answer so a session knows what only Chris can move.
- **People are a facet** — `FilterSpec.people` ORs (unlike `tags`, which
  narrows), `["*"]` means "anyone", and `retrospective` reports THE MIX:
  closed tasks with a person on them vs. nobody. That count is the October
  check.
- **Waiting is `db::tasks::waiting_sql()`** — a future `defer_until`, an
  `armed` tripwire, a written `blocked_reason`, or an open blocker (deps' rule: a
  trashed or archived blocker holds nothing). `View::Waiting` and
  `FilterSpec.waiting` both use it, so the sidebar row and the MCP facet
  cannot disagree; the view sorts oldest-first because age is what it exists
  to show. **The predicate is used negated** (`waiting: false`), which is why
  `tripwire_state` is `COALESCE`d: `NULL = 'armed'` is NULL, `NOT (… OR NULL)`
  is NULL, and a task with no tripwire silently vanished from "not waiting".
  The false-direction test is what caught it — keep it.

Rejected: a `kind` column (a migration for a value with one behaviour, and a
second axis on `status` — invariant 16); a contacts table (the question is
"what is open with Sam", which a tag facet answers; a CRM is cxmail's
problem); showing decisions in Inbox as well (two rows for one task).

### Invariant 23 — attachment BYTES are canonical on the peer (v22)

The metadata was always going to sync: `task_attachments` is an ordinary synced
table. The bytes could not, because `snapshot::to_json` **refuses a BLOB on
purpose** rather than base64-ing one, so a 25 MB image has no representation in
a snapshot at all. Hence a separate channel — `crates/cxtasks-core/src/sync/blobs.rs`
plus `GET`/`POST /attachments/:stored_name` — and four rules that hold it up.

**1. The peer is canonical; a local store is a cache.** Deleting the whole
`attachments/` directory on a paired host must cost nothing but a reconcile.
That is a claim about repairability, and three things make it true rather than
aspirational: `stored_name` is a UUID minted once, so it addresses one immutable
byte sequence forever; `sha256` says what those bytes are; and
`attachments::store_bytes` verifies **before** it writes and writes by renaming
out of `attachments/.incoming/`, so a file that exists is a file that is
complete. Drop the third and "the file is present" eventually means "a truncated
download is present" — permanently, because nothing looks at a file it already
has.

**2. Rows travel first, bytes follow, and no node stores bytes for a row it does
not have.** `reconcile` pushes, pulls, and *then* moves bytes; the receiving end
refuses a `stored_name` it has no row for. Reversed, a crash between "bytes
uploaded" and "row pushed" leaves the peer holding a file nothing claims, which
its own pruning collects — destroying the only copy that ever left the
originating machine. This ordering leaves a row with no bytes instead: a missing
image, visible, refilled next pass. Exactly the trade the push-before-pull rule
makes one layer up, and the refusal is the half that makes pruning sound.

**3. Verification is asymmetric on purpose.** What to FETCH is decided on
presence alone, because hashing every attachment on a timer is a full read of
the store to re-check bytes that were verified when they were written. What to
SEND is decided on the digest, because a bad upload overwrites the canonical
copy while a bad download spoils one cache. Both boundaries refuse rather than
repair, and the retry is free: the work is re-derived from what is on disk
rather than from a queue.

**4. A failure here is one missing image, never a failed reconcile.**
`BlobReport.failed` is reported and never raised — the same shape as
`PushResult`. Raising would let one unreachable file stop a whole database
syncing, which is the failure this design is least willing to have.

**`prune_orphaned_files` is therefore host-local in EFFECT, not merely in where
it runs.** "A file no row names is garbage" is true of a standalone database and
false of a synced one, because a row and its bytes stop arriving together: on a
local node a pull applies the peer's whole database and `apply::delete_absent`
removes rows the peer does not have, so an attachment created here between
another process's push and its pull is deleted by that pull and re-created by
the next sweep — and under the old rule its image was destroyed in between. So
on a synced host an untracked file gets a one-hour grace, and with sync off the
behaviour is byte-identical to what it always was. Partial downloads need no
grace at all: they live in a subdirectory and the walk skips anything that is
not a file, which is what stops one process deleting a transfer another process
is still writing.

**Scope splits on the METHOD, not the route.** `GET` — the bytes and the
`missing` inventory — accepts any authenticated scope, because a board token
already reads titles, bodies and comments and an image attached to one of those
is the same class of thing; refusing it is a broken image on the phone, not a
tighter posture. `POST` stays full-scope, and for a different reason than the
verbs `Scope::may_call` refuses: it writes into the copy every other node
treats as canonical, and nothing on iOS uploads today. The asymmetry is
deliberate and is pinned by
`attachments::tests::a_board_token_may_read_an_attachment_and_may_not_write_one`
— asserted in **both** directions, since a gate that denies too much is a gate
nobody uses and the failure mode is the full token ending up on a phone.

### The schema fingerprint

`db/schema_fingerprint.txt` snapshots all 78 objects from `sqlite_master`, and
`the_schema_matches_its_checked_in_fingerprint` compares against it. It catches
a migration that alters something it did not mean to, or an index an `ALTER`
quietly dropped — nothing else in the suite notices a schema change that leaves
every query working. It later becomes the proof that two hosts' databases are
structurally identical, which whole-row sync assumes and whose failure is not a
crash but columns silently not travelling.

Regenerate deliberately, never reflexively: a failure here is either a bug you
just caught or a change you should be able to describe in a sentence.

## Gotchas (database)

### 1. `sqlite3` CLI can't write to `tasks` without `PRAGMA trusted_schema=ON`
**Symptom:** `sqlite3 cxtasks.db "DELETE FROM tasks WHERE ..."` fails with
`Error: in prepare, unsafe use of virtual table "tasks_fts"` — and it fails on
*any* INSERT/UPDATE/DELETE against `tasks`, not just search queries.
**Cause:** the FTS sync triggers touch the `tasks_fts` virtual table. The sqlite3 CLI
runs with `trusted_schema` off (defensive mode), which forbids virtual-table access
from inside a trigger. `rusqlite` leaves `trusted_schema` on, so the app, the MCP and
the runner are all unaffected — this is purely a CLI-side restriction.
**Fix:** prefix any manual maintenance with `PRAGMA trusted_schema=ON;` in the same
invocation. It does not persist, so it must be repeated per command.

### 2. FTS5 delete/update triggers must use plain `DELETE`/`UPDATE`, not the `'delete'` command
**Symptom:** every `DELETE FROM tasks` fails with a bare `SQL logic error`.
**Cause:** the `INSERT INTO fts(fts, rowid, ...) VALUES('delete', ...)` form shown in
the FTS5 docs is for **external-content** tables. `tasks_fts` is an ordinary FTS5
table, where that command is rejected. The error names neither FTS nor the trigger,
so it presents as "task deletion is broken."
**Fix:** `DELETE FROM tasks_fts WHERE rowid = old.rowid` and a scoped
`UPDATE tasks_fts SET ... WHERE rowid = new.rowid` — the form cxmail's
`messages_fts_ad`/`_au` already used.


### 12. `datetime('now')` is second-resolution — comments need milliseconds AND a rowid tiebreaker
**Symptom:** several comments added in quick succession render in random order.
**Cause:** `datetime('now')` resolves to one second. Two comments posted in the
same second tie on `created_at`, and the sort then falls through to `id`, which is
a random UUID. **A test that sleeps between inserts passes against the broken
ordering** — the regression test
(`db::comments::tests::add_and_read_in_chronological_order`) adds eight in a tight
loop, which is what actually reproduces it.
**Fix:** `task_comments.created_at` defaults to
`strftime('%Y-%m-%d %H:%M:%f','now')` for millisecond precision, and
`comments::for_task` orders by `datetime(created_at) ASC, rowid ASC`. `rowid` is
insertion order and monotonic, so it's the definitive tiebreaker even if two
inserts land inside the same millisecond — don't drop it on the grounds that
milliseconds are "surely enough".


### 36. A column on a `created_at`-swept table must be written with the row, or it flaps forever
**Symptom (avoided, not suffered):** a value filled in after the fact is correct
locally, never reaches the peer, is overwritten back to NULL by the next pull,
counts as a change, redraws the UI — and is filled in again on the next pass.
Forever, on a timer, discarding whatever was being typed each time (gotcha #24).
**Cause:** two things that are individually reasonable. `task_attachments` is
swept on `created_at` (`sync::sweep::SWEEP`), which never moves — so a later
UPDATE to any column on that row is invisible to the sweep and cannot travel.
And `apply::upsert`'s difference test is `IS NOT`, so the peer's NULL genuinely
differs from the local value and is genuinely written back. The loop needs no
bug in either half.
**Fix:** `task_attachments.sha256` is written **once, inside the INSERT** that
creates the row, and there is deliberately no heal and no backfill for the rows
that predate it — a NULL is read as "unverified", which every consumer already
handles. **The general rule: before adding a column, check its table's
watermark.** A table swept `Whole`, or on a column that moves (`tasks.updated_at`,
`task_runs`' `MAX(...)`), can carry a late write; one swept on `created_at`
cannot, and the failure is not a missing value but a permanent oscillation.

### 30. Raising a `DEFAULT_*` changes nothing once the key is in `app_meta`
**Symptom:** the constant says 50M, the doc comment explains why it is 50M, the
tests assert 50M — and the running app still caps at 2M.
**Cause:** `budget_settings` reads `get_i64(conn, KEY, DEFAULT)`, so a
`DEFAULT_*` is a **fallback for an absent key**, not a value. The moment
anything writes that key — one `save_budget_settings` call, one hand-edit —
the constant stops being the source of truth for that database forever. The
real DB had `bg_daily_token_cap = 2000000` stored while none of the *other*
budget keys existed, so the whole settings block looked like it was running on
defaults and one row silently wasn't. Same shape as v9's folded columns: no
error, just the old behaviour, on the one machine that matters.
**Fix:** a version-gated migration that rewrites the stored value (v15), *and*
a way to change it from the UI — because the number will be wrong again. The
check before believing a default is
`sqlite3 "$HOME/Library/Application Support/com.cxtasks.app/cxtasks.db" "SELECT key, value FROM app_meta"`,
which is also how you tell "never set" from "set to the same thing".

**The number itself is the second lesson, and it is about measurement.** The
2M cap was derived from real measurements whose top end was ~234k — a few
tool-using turns. A genuine agentic run on a genuine repo (4 minutes, Sonnet 5,
one task) touched **14.6M**, of which 13.3M was cache *reads*: invariant 5's
"every turn re-reads the whole accumulated conversation" compounds over minutes,
not over the handful of turns the original figures came from. So the cap could
not fit a single honest run, and it tripped on the day's first task with the
meter reading `14.6M / 2.0M`. **A guard that can only fire after the spend it
was meant to prevent tells you nothing about the budget you have left** — and
the gate is checked before a run starts and never during one, so the cap is
where the day *stops*, not a ceiling on where it lands.

**And a spend guard must never mislabel its own scope.** The manual reset means
the counters are no longer "today", so `BudgetStatus` carries `window_reset` and
both surfaces (sidebar + Mission Control) switch to "since reset". A meter
reading 0 on a day that has burned 15M, still captioned "today", is worse than
no meter. Same rule one level down, and it is why the per-run ceiling records a
normalized `spent` (invariant 7): a ceiling that reports a number the daily
counter disagrees with is a guard lying about its own units.

**It happened again, on the sibling setting, and that is the real lesson.**
`bg_max_turns` had the *identical* latent bug — stored at `30`, so the constant
was already not the source of truth — and *also* had no UI control, because v15
applied both halves of the fix to the token cap and neither to the setting
beside it. It went on to cost two whole background runs (gotcha #31). When a
`DEFAULT_*` turns out to be wrong, **check its neighbours in the same struct
before assuming they're fine**: they were written at the same time, from the
same measurements, and they are stored in the same table.
