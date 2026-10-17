//! Schema + migrations. Version-gated `create_tables_vN` in the style of
//! cxmail's `db/schema.rs`: every migration is idempotent, runs on both the
//! Tauri app and the standalone binaries, and never drops user data.

use crate::error::AppError;
use rusqlite::{Connection, Result};

/// Bump this and add a `create_tables_vN` when the schema changes.
pub const CURRENT_VERSION: i32 = 29;

/// The version of the **wire protocol** between this build and a sync peer.
///
/// Deliberately sitting next to [`CURRENT_VERSION`], because the two answer
/// different questions and the failure mode is confusing them. The schema
/// version describes a database file that every binary on this machine shares
/// and migrates in lockstep-ish fashion. This one describes a conversation
/// between two **separately deployed artifacts** — the app in `/Applications`
/// and a server on another machine — which do not upgrade together and will
/// skew, because you will ship a build and forget the container for weeks.
///
/// Without a wire version that skew shows up as rows served with fields the
/// other end doesn't know about: silent, and diagnosed by staring at JSON. A
/// version exchanged in a `/health` handshake turns it into a refusal that
/// names both numbers.
///
/// Bump on any breaking change to a request or response shape. Nothing reads
/// it yet; it is here so that when something does, the constant it needs is
/// already in the place a reader would look for it.
pub const SYNC_PROTOCOL_VERSION: u32 = 1;

/// Open-time setup + migrations. Safe to call from the app, the MCP binary and
/// the runner concurrently — every step is `IF NOT EXISTS` or version-gated.
pub fn initialize(conn: &Connection) -> Result<(), AppError> {
    conn.execute_batch("PRAGMA journal_mode=WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    conn.execute_batch("PRAGMA optimize;")?;
    // Cap the WAL at 64 MB so a long-running writer can't grow it unbounded.
    conn.execute_batch("PRAGMA journal_size_limit = 67108864;")?;

    let version = get_schema_version(conn)?;

    if version < 1 {
        create_tables_v1(conn)?;
    }
    if version < 2 {
        create_tables_v2(conn)?;
    }
    if version < 3 {
        create_tables_v3(conn)?;
    }
    if version < 4 {
        create_tables_v4(conn)?;
    }
    if version < 5 {
        create_tables_v5(conn)?;
    }
    if version < 6 {
        create_tables_v6(conn)?;
    }
    if version < 7 {
        create_tables_v7(conn)?;
    }
    if version < 8 {
        create_tables_v8(conn)?;
    }
    if version < 9 {
        create_tables_v9(conn)?;
    }
    if version < 10 {
        create_tables_v10(conn)?;
    }
    if version < 11 {
        create_tables_v11(conn)?;
    }
    if version < 12 {
        create_tables_v12(conn)?;
    }
    if version < 13 {
        create_tables_v13(conn)?;
    }
    if version < 14 {
        create_tables_v14(conn)?;
    }
    if version < 15 {
        create_tables_v15(conn)?;
    }
    if version < 16 {
        create_tables_v16(conn)?;
    }
    if version < 17 {
        create_tables_v17(conn)?;
    }

    if version < 18 {
        create_tables_v18(conn)?;
    }

    if version < 19 {
        create_tables_v19(conn)?;
    }

    if version < 20 {
        create_tables_v20(conn)?;
    }

    if version < 21 {
        create_tables_v21(conn)?;
    }

    if version < 22 {
        create_tables_v22(conn)?;
    }

    if version < 23 {
        create_tables_v23(conn)?;
    }

    if version < 24 {
        create_tables_v24(conn)?;
    }

    if version < 25 {
        create_tables_v25(conn)?;
    }

    if version < 26 {
        create_tables_v26(conn)?;
    }

    if version < 27 {
        create_tables_v27(conn)?;
    }

    if version < 28 {
        create_tables_v28(conn)?;
    }

    if version < 29 {
        create_tables_v29(conn)?;
    }

    // Stamp FORWARD only. Three binaries share this DB and they don't upgrade
    // in lockstep — an older `cxtasks-runner` left over from a previous build
    // would otherwise re-stamp a v2 database back to v1, and the next new-binary
    // launch would re-run migrations against columns that already exist.
    // (cxmail hit the same shape: "a pre-v40 binary opening a v40 DB re-stamps
    // schema_version to its own version".)
    if version < CURRENT_VERSION {
        set_schema_version(conn, CURRENT_VERSION)?;
    }

    // **Outside the version gate on purpose.** This is both the v6 backfill and
    // the ongoing skew heal, and they are the same code path: three binaries
    // share this DB and don't upgrade in lockstep, so a leftover pre-v6
    // `cxtasks-mcp` or `cxtasks-runner` goes on INSERTing rows with a NULL
    // `short_id` until the ship cycle replaces it. Gating this on `version < 6`
    // would leave those rows permanently unnumbered — unaddressable as "T42"
    // and invisible to `search_tasks("T…")` — with nothing on screen to explain
    // why some tasks have a reference and some don't.
    crate::db::tasks::backfill_short_ids(conn)?;
    // Outside the gate for exactly the reason above: a leftover pre-v12
    // `cxtasks-mcp` goes on writing `'next'` / `'needs_review'` / `'cancelled'`
    // until the ship cycle replaces it, and an unknown status renders as a blank
    // pill with nothing to explain it. Gating this on `version < 12` would leave
    // those rows stranded on a vocabulary nothing else speaks.
    crate::db::tasks::heal_legacy_statuses(conn)?;
    // Outside the gate, and an ongoing heal rather than a one-shot backfill,
    // for both of the reasons above at once.
    //
    // The v13 `repo_path` backfill is gated because a blank workspace is a
    // *choice* — refilling it forever would be an argument with the user. A tag
    // with no tasks left is the opposite: `tags::upsert` has one caller
    // (`attach`), so a detached tag is not something anyone is able to choose,
    // and collecting one can never overrule a decision.
    //
    // And it has to keep running, because a leftover pre-GC `cxtasks-mcp` goes
    // on orphaning tags through `tag_task` until the ship cycle replaces it —
    // those orphans would never be reached by a migration that has already run.
    // This is also what sweeps up whatever accumulated before GC existed.
    //
    // Note this does NOT make the write-path calls redundant: startup alone
    // would leave a collected tag sitting in the sidebar for the rest of the
    // session, which is the staleness the whole feature is against.
    crate::db::tags::gc(conn)?;
    // Outside every version gate, and for a sharper reason than the heals
    // above: this one un-wedges the runner. `runner::gate` counts `running`
    // background rows straight out of this table, so a run stranded by a
    // crashed runner (gotcha #4) holds a concurrency slot forever — two of them
    // at the default `max_concurrent = 2` and background execution stops
    // permanently, with the only symptom being that nothing ever runs again.
    //
    // NOT swept here any more. It used to be, and the argument was that any of
    // the three binaries could safely do it because the cutoff is the wall
    // clock the runner enforces. That argument holds for exactly one machine.
    //
    // `initialize` runs on every `cxtasks-mcp` launch — which is every Claude
    // session, dozens a day — and each of those evaluated `datetime('now')` in
    // whatever process happened to open the DB. With a second host that is a
    // process reaching across the network to judge the liveness of runs it
    // knows nothing about, on a clock that is not the one those runs are being
    // timed against.
    //
    // The sweep now belongs to the thing that owns the slots: the runner. It
    // sweeps at startup (`bin/runner.rs`) and on every tick (`runner::tick`),
    // and the tick is what actually matters — a runner that stays up for days
    // and loses track of a run cannot be helped by startup-only reaping. So
    // nothing is lost here except a sweep that was never this layer's job.
    // Its foreground counterpart, and deliberately a heal rather than a
    // version-gated migration: a foreground run is terminal at spawn now, so a
    // foreground row at `running` is not a state anyone can choose — it is the
    // fossil of a build where nothing closed them. Healing it can never
    // overrule a decision, which is the same test that keeps
    // `heal_legacy_statuses` outside the gates and v13's `repo_path` backfill
    // inside them.
    if let Err(e) = crate::db::runs::heal_foreground_runs(conn) {
        log::warn!("foreground-run heal failed: {e}");
    }
    Ok(())
}

fn get_schema_version(conn: &Connection) -> Result<i32> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS app_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
        [],
    )?;
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .ok();
    Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
}

fn set_schema_version(conn: &Connection, version: i32) -> Result<()> {
    conn.execute(
        "INSERT INTO app_meta (key, value) VALUES ('schema_version', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [version.to_string()],
    )?;
    Ok(())
}

fn create_tables_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        -- ── tasks ──────────────────────────────────────────────────────────
        -- Todo fields are the core. The execution block at the bottom is all
        -- nullable: NULL repo_path/prompt => a plain todo with no dispatch
        -- button. Non-agent tasks are first-class.
        CREATE TABLE IF NOT EXISTS tasks (
            id            TEXT PRIMARY KEY,
            title         TEXT NOT NULL,
            body          TEXT,
            status        TEXT NOT NULL DEFAULT 'inbox',
            priority      INTEGER NOT NULL DEFAULT 0,
            project       TEXT,
            due_at        TEXT,
            defer_until   TEXT,
            recurrence    TEXT,
            parent_id     TEXT REFERENCES tasks(id) ON DELETE CASCADE,
            sort_order    INTEGER NOT NULL DEFAULT 0,
            created_at    TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at    TEXT NOT NULL DEFAULT (datetime('now')),
            completed_at  TEXT,

            -- execution context (all nullable)
            repo_path         TEXT,
            prompt            TEXT,
            bg_allowed        INTEGER NOT NULL DEFAULT 0,
            bg_writes_allowed INTEGER NOT NULL DEFAULT 0,

            -- reminder bookkeeping (Phase 5); NULL until a reminder fires
            reminded_at   TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_tasks_status     ON tasks(status);
        CREATE INDEX IF NOT EXISTS idx_tasks_due        ON tasks(due_at);
        CREATE INDEX IF NOT EXISTS idx_tasks_project    ON tasks(project);
        CREATE INDEX IF NOT EXISTS idx_tasks_parent     ON tasks(parent_id);
        CREATE INDEX IF NOT EXISTS idx_tasks_bg_queue   ON tasks(status, bg_allowed);

        -- ── task_runs ──────────────────────────────────────────────────────
        -- One row per dispatch. `mode` = foreground|background.
        CREATE TABLE IF NOT EXISTS task_runs (
            id            TEXT PRIMARY KEY,
            task_id       TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            mode          TEXT NOT NULL,
            status        TEXT NOT NULL,
            started_at    TEXT NOT NULL DEFAULT (datetime('now')),
            finished_at   TEXT,
            session_id    TEXT,
            exit_code     INTEGER,
            result_text   TEXT,
            output_tokens INTEGER,
            scratch_dir   TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_runs_task    ON task_runs(task_id);
        CREATE INDEX IF NOT EXISTS idx_runs_status  ON task_runs(status);
        CREATE INDEX IF NOT EXISTS idx_runs_started ON task_runs(started_at);
        "#,
    )?;

    create_fts_v1(conn)?;
    Ok(())
}

/// v2 — honest token accounting + per-task model choice.
///
/// v1 recorded only `output_tokens`, which understates consumption by roughly
/// two orders of magnitude: a measured run that emitted **4** output tokens
/// touched **56,349** tokens once the reloaded `CLAUDE.md` context is counted
/// (40.8k cache-write + 15.5k cache-read). A budget guard reading the wrong
/// number is worse than no guard, since it reads as reassuring.
fn create_tables_v2(conn: &Connection) -> Result<()> {
    // ALTER TABLE ADD COLUMN is idempotent-safe only if we tolerate the
    // duplicate-column error, which is how a re-run on a partially migrated DB
    // behaves.
    for stmt in [
        "ALTER TABLE task_runs ADD COLUMN input_tokens INTEGER",
        "ALTER TABLE task_runs ADD COLUMN cache_read_tokens INTEGER",
        "ALTER TABLE task_runs ADD COLUMN cache_write_tokens INTEGER",
        "ALTER TABLE task_runs ADD COLUMN model TEXT",
        "ALTER TABLE tasks ADD COLUMN model TEXT",
    ] {
        if let Err(e) = conn.execute(stmt, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
    }
    Ok(())
}

/// v3 — tags, blocked-by dependencies, comments, and the P0/P3 inversion.
///
/// **The priority flip is a data migration, not a relabel.** v1-v2 stored
/// 0 = none … 3 = highest. Meta's convention (and Chris's muscle memory) is the
/// opposite: **P0 is highest**, P3 lowest. Simply changing the labels would
/// silently invert the meaning of every existing task — a P3 "drop everything"
/// would read as P3 "whenever". So the stored values are remapped
/// `new = 3 - old`, which preserves relative ordering exactly:
///
/// | old | meaning | new | meaning |
/// |---|---|---|---|
/// | 3 | highest | 0 | P0 highest |
/// | 2 | high    | 1 | P1 |
/// | 1 | low     | 2 | P2 |
/// | 0 | none    | 3 | P3 lowest |
///
/// "No priority" ceases to exist, which is the honest outcome: an untriaged task
/// is the lowest-urgency one, not a fourth category. Every `ORDER BY priority`
/// flips from DESC to ASC at the same time — see `db::tasks`.
fn create_tables_v3(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        -- ── tags ───────────────────────────────────────────────────────────
        -- Shared across tasks (many-to-many), unlike `project` which is one
        -- free-text field per task.
        CREATE TABLE IF NOT EXISTS tags (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
            color      TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS task_tags (
            task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            tag_id  TEXT NOT NULL REFERENCES tags(id)  ON DELETE CASCADE,
            PRIMARY KEY (task_id, tag_id)
        );
        CREATE INDEX IF NOT EXISTS idx_task_tags_tag ON task_tags(tag_id);

        -- ── dependencies ───────────────────────────────────────────────────
        -- `task_id` is BLOCKED BY `blocked_by_id`. Many-to-many and orthogonal
        -- to `tasks.parent_id`: a subtask is containment ("part of"), a
        -- dependency is ordering ("can't start until"). A task can be blocked
        -- by something in a different project entirely, which containment
        -- cannot express.
        CREATE TABLE IF NOT EXISTS task_deps (
            task_id       TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            blocked_by_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            created_at    TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (task_id, blocked_by_id),
            -- Self-blocking is always a mistake; longer cycles are rejected in
            -- `db::deps::add` because SQL CHECK can't see them.
            CHECK (task_id <> blocked_by_id)
        );
        CREATE INDEX IF NOT EXISTS idx_deps_blocker ON task_deps(blocked_by_id);

        -- ── comments ───────────────────────────────────────────────────────
        -- A running log per task. `author` is free text so an agent run can
        -- sign its own entries distinctly from Chris's.
        CREATE TABLE IF NOT EXISTS task_comments (
            id         TEXT PRIMARY KEY,
            task_id    TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            author     TEXT NOT NULL DEFAULT 'me',
            body       TEXT NOT NULL,
            -- MILLISECOND precision, unlike every other timestamp here.
            -- `datetime('now')` resolves to one second, so two comments posted
            -- in the same second tie and the sort falls back to the random
            -- UUID — they display out of order. `%f` gives `SS.SSS`.
            -- Still ISO-8601-with-Z-compatible for the `datetime()` rule.
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', 'now'))
        );
        CREATE INDEX IF NOT EXISTS idx_comments_task ON task_comments(task_id, created_at);
        "#,
    )?;

    // The inversion. Guarded by the version gate so it can only ever run once —
    // running it twice would flip priorities back and look like data corruption.
    conn.execute("UPDATE tasks SET priority = 3 - priority", [])?;
    Ok(())
}

/// v4 — first-class projects and sections.
///
/// Until now `tasks.project` was a bare text column, which meant a project only
/// existed while at least one task referenced it. That's fine for autocomplete
/// and fatal for a "+ New project" button: the project would vanish the moment
/// you created it. So projects get a table, backfilled from the distinct values
/// already in use so nothing disappears.
///
/// `sections` are subdivisions for grouping within a project **or** within a
/// view (`project IS NULL` = a section that applies wherever you are, e.g.
/// splitting Today into "morning" / "after the standup"). `tasks.section_id` is
/// `ON DELETE SET NULL`: deleting a section must un-group its tasks, never
/// delete them — losing work because you tidied up the grouping would be
/// indefensible.
fn create_tables_v4(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS projects (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
            color      TEXT,
            sort_order INTEGER NOT NULL DEFAULT 0,
            archived   INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS sections (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL,
            -- NULL = a section that groups a VIEW rather than a project.
            project    TEXT,
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE (project, name)
        );
        CREATE INDEX IF NOT EXISTS idx_sections_project ON sections(project, sort_order);
        "#,
    )?;

    // SET NULL, not CASCADE — see the doc comment.
    if let Err(e) = conn.execute(
        "ALTER TABLE tasks ADD COLUMN section_id TEXT REFERENCES sections(id) ON DELETE SET NULL",
        [],
    ) {
        if !e.to_string().contains("duplicate column name") {
            return Err(e);
        }
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_tasks_section ON tasks(section_id);")?;

    // Backfill projects from whatever `tasks.project` already holds, so every
    // existing project keeps showing in the sidebar after the upgrade.
    conn.execute(
        "INSERT OR IGNORE INTO projects (id, name)
         SELECT lower(hex(randomblob(16))), TRIM(project)
           FROM tasks
          WHERE project IS NOT NULL AND TRIM(project) <> ''
          GROUP BY TRIM(project) COLLATE NOCASE",
        [],
    )?;
    Ok(())
}

/// v5 — Trash (recoverable soft delete) and Archive.
///
/// Both are **nullable timestamps, deliberately not new `status` values.** A
/// trashed task has to remember it was `in_progress` or `blocked` so restoring
/// it puts it back where it was; folding either state into `status` destroys
/// exactly the information restore depends on and turns every recovery into a
/// guess.
///
/// Two independent columns give four states, and the fourth — archived *and*
/// trashed — is intentional rather than a hole in the model: trash something
/// out of the Archive and restoring it returns it to the Archive it came from,
/// because `archived_at` was never touched.
///
/// Both are written at **millisecond** precision (`%f`), matching
/// `task_comments.created_at` rather than the second-resolution
/// `datetime('now')` used by the other bookkeeping columns — so the Trash can
/// order two deletions made inside the same second, same as gotcha #12.
///
/// v5 also made `deleted_at` double as the **batch key** tying a subtree's
/// deletion together, on the theory that milliseconds were fine enough to keep
/// two deletions apart. They were not: see [`create_tables_v8`], which moves
/// batch identity onto its own column and leaves these two as plain timestamps.
///
/// Pure `ADD COLUMN` — every pre-existing task is live, so there is nothing to
/// backfill and no way for this migration to lose data.
fn create_tables_v5(conn: &Connection) -> Result<()> {
    for stmt in [
        "ALTER TABLE tasks ADD COLUMN deleted_at TEXT",
        "ALTER TABLE tasks ADD COLUMN archived_at TEXT",
    ] {
        if let Err(e) = conn.execute(stmt, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
    }
    // Partial indexes: the overwhelming majority of rows are live (both NULL),
    // and the only queries that read these columns want the non-NULL side.
    conn.execute_batch(
        r#"
        CREATE INDEX IF NOT EXISTS idx_tasks_deleted
            ON tasks(deleted_at) WHERE deleted_at IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_tasks_archived
            ON tasks(archived_at) WHERE archived_at IS NOT NULL;
        "#,
    )?;
    Ok(())
}

/// v6 — the GSD layer, dated milestones, and speakable T-numbers.
///
/// Three features in one migration deliberately. A second version bump would
/// widen the window in which three binaries disagree about the schema for no
/// benefit — they all ship together in one build.
///
/// The `short_id` column is **nullable with no default**, which looks like
/// under-specification and is the opposite. A pre-v6 `cxtasks-mcp` left over
/// from the previous install keeps writing rows with `short_id = NULL` until the
/// ship cycle replaces it, so the number cannot be a schema-level invariant —
/// it is maintained by [`crate::db::tasks::backfill_short_ids`], which runs on
/// **every** `initialize`, not just this one.
///
/// `projects.health` is likewise nullable with no default: NULL means "never
/// reported", which is a different and more honest claim than `on_track`. A
/// board that asserts a status nobody wrote is exactly the failure mode a status
/// board exists to prevent.
fn create_tables_v6(conn: &Connection) -> Result<()> {
    for stmt in [
        "ALTER TABLE tasks ADD COLUMN short_id INTEGER",
        "ALTER TABLE projects ADD COLUMN health TEXT",
        // A section carrying a `target_date` IS a milestone — no new grouping
        // concept, no third table. Stored as full ISO-8601 UTC with a trailing
        // `Z`, like `due_at`: a bare `YYYY-MM-DD` renders a day early or late
        // once JS parses it as local time.
        "ALTER TABLE sections ADD COLUMN target_date TEXT",
        // Manual: a milestone is done when someone says it is, not when the
        // task count happens to hit zero.
        "ALTER TABLE sections ADD COLUMN completed_at TEXT",
    ] {
        if let Err(e) = conn.execute(stmt, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
    }

    conn.execute_batch(
        r#"
        -- Partial, so the NULLs a pre-v6 writer is still producing don't all
        -- collide with each other on the way in.
        CREATE UNIQUE INDEX IF NOT EXISTS idx_tasks_short_id
            ON tasks(short_id) WHERE short_id IS NOT NULL;

        -- ── project_updates ────────────────────────────────────────────────
        -- The written record behind every health change. There is no bare
        -- health setter anywhere in the codebase: `db::project_updates::post`
        -- writes this row and `projects.health` together, so a status can never
        -- move without an explanation attached. A dot with no story behind it is
        -- how a status board rots into decoration.
        CREATE TABLE IF NOT EXISTS project_updates (
            id         TEXT PRIMARY KEY,
            -- Keyed by project NAME, matching `sections.project`. That makes two
            -- cascades mandatory rather than optional: `projects::rename` has to
            -- carry these rows with it (invariant 11a), and `projects::delete`
            -- has to remove them — otherwise deleting project "X" and later
            -- creating a new "X" resurrects the old project's update log.
            project    TEXT NOT NULL,
            author     TEXT NOT NULL DEFAULT 'me',
            body       TEXT NOT NULL,
            -- NULL = the update narrates without moving the dot.
            health     TEXT,
            -- MILLISECOND precision, same reason as `task_comments.created_at`
            -- (gotcha #12): several updates posted inside one second would tie
            -- on `created_at` and render in random order.
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_project_updates_project
            ON project_updates(project, created_at);
        "#,
    )?;

    // Seed the T-number counter past anything already stored. `OR IGNORE` rather
    // than a `WHERE NOT EXISTS` guard: the aggregate SELECT returns one row even
    // when the guard filters everything out, so the guard form would try to
    // insert a duplicate key and fail.
    conn.execute(
        "INSERT OR IGNORE INTO app_meta (key, value)
         SELECT 'next_short_id', CAST(COALESCE(MAX(short_id), 0) + 1 AS TEXT) FROM tasks",
        [],
    )?;
    Ok(())
}

/// v7 — the middle tool tier: `bg_shell_allowed`.
///
/// Before this there were two background tiers, read-only and write, and a task
/// that needed to run `git` had to be handed `Edit`/`Write` to get a shell. So
/// the honest options for "list stale branches" were *can't do it* or *can
/// rewrite the repo*, which is how a safety default gets switched off for
/// reasons that have nothing to do with safety.
///
/// A shell is NOT a halfway house on danger — `>` writes and `git push`
/// publishes — so [`crate::runner::ToolTier::needs_checkpoint`] treats this tier
/// exactly like `Write`: git repo required, working tree snapshotted first. What
/// it narrows is the accident surface, not the blast radius.
///
/// Additive with a `0` default, which is what makes it safe against the binary
/// skew this DB always has: a leftover pre-v7 `cxtasks-mcp` or `cxtasks-runner`
/// simply never sets it, and every task it writes lands at the *most* restrictive
/// tier. Compare `short_id`, where the equivalent skew needed an ongoing backfill
/// — here the safe value and the default value are the same, so there's nothing
/// to heal.
fn create_tables_v7(conn: &Connection) -> Result<()> {
    if let Err(e) = conn.execute(
        "ALTER TABLE tasks ADD COLUMN bg_shell_allowed INTEGER NOT NULL DEFAULT 0",
        [],
    ) {
        if !e.to_string().contains("duplicate column name") {
            return Err(e);
        }
    }
    Ok(())
}

/// v8 — a second background executor: OpenAI Codex.
///
/// Three columns, and the interesting one is `bg_codex_allowed`, which looks
/// redundant next to `bg_executor` and is the entire safety property of this
/// migration. The full argument is in [`crate::executor`]; the short version is
/// that a leftover **pre-v8 `cxtasks-runner`** drains the queue with
/// `... AND bg_allowed = 1` and has never heard of `bg_executor`, so storing a
/// Codex task as `bg_allowed = 1` would get it claimed and run with *Claude*.
/// Keeping the flag in a column that runner cannot see makes a Codex task
/// simply invisible to it — the failure mode is "doesn't run until the new
/// runner ships", which is recoverable, instead of "runs as the wrong agent",
/// which is not.
///
/// `task_runs.executor` is not bookkeeping for its own sake either: a task can
/// be switched from Claude to Codex later, and if the executor lived only on
/// the task then every historical run would retroactively claim to have been
/// produced by whichever agent is selected *now*. The run row is the record of
/// what actually happened.
///
/// All three are additive with a constant default, and for `bg_codex_allowed`
/// the default value and the safe value are the same `0` — so, exactly like v7's
/// `bg_shell_allowed`, there is nothing to backfill and a pre-v8 writer's rows
/// land at the most restrictive setting on their own.
fn create_tables_v8(conn: &Connection) -> Result<()> {
    for stmt in [
        // Defaults to the legacy behaviour, so every existing task — and every
        // row a pre-v8 binary goes on writing — is a Claude task.
        "ALTER TABLE tasks ADD COLUMN bg_executor TEXT NOT NULL DEFAULT 'claude'",
        "ALTER TABLE tasks ADD COLUMN bg_codex_allowed INTEGER NOT NULL DEFAULT 0",
        // Historical runs predate Codex, so 'claude' is not a guess here.
        "ALTER TABLE task_runs ADD COLUMN executor TEXT NOT NULL DEFAULT 'claude'",
    ] {
        if let Err(e) = conn.execute(stmt, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
    }
    // The Codex counterpart of `idx_tasks_bg_queue`; the drain query ORs the
    // two executor clauses, so both halves want an index.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_tasks_bg_codex_queue
             ON tasks(status, bg_codex_allowed);",
    )?;
    Ok(())
}

/// v9 — `delete_batch` / `archive_batch`: the batch key stops being the clock.
///
/// **Why this is v9 and not folded into v8, which shipped in the same build.**
/// It was folded in, briefly, on exactly the v6 reasoning — one build, one bump,
/// don't widen the window where three binaries disagree. That was wrong on the
/// facts: v8 had *already escaped*. A dev build carrying only the Codex half had
/// been run against the real database, which was therefore stamped `8` with
/// `bg_executor` present and no batch columns. `create_tables_v8` is gated on
/// `version < 8`, so on that database it never runs again and the two columns
/// would never be added — every query naming them failing at runtime, on the one
/// machine that matters.
///
/// The rule this is an instance of: **a version number is spent the moment any
/// binary stamps it on a real database.** Folding into an existing migration is
/// only safe while its number is still unshipped, and "unshipped" means no
/// `tauri dev` run either. When in doubt, take a new number — the cost is one
/// integer, and the failure it prevents is a silently missing column.
///
/// v5 used `deleted_at` itself as the key tying a subtree's deletion together,
/// which made batch identity a function of *when* rather than *which*. Two
/// `trash()` calls landing in the same millisecond therefore produced a
/// byte-identical stamp and merged into one batch — restoring either one
/// resurrected the other, which is precisely what the batch key exists to
/// prevent. Millisecond precision only narrowed the window; it never closed it,
/// and the regression test for the property failed roughly four runs in five.
/// Reachable from the app any time two tasks are trashed in quick succession —
/// a loop over the MCP, or a fast multi-delete.
///
/// A UUID needs no coordination to be unique, so the collision cannot come back
/// under a faster machine or a coarser clock. `deleted_at` keeps its
/// millisecond precision and stays a plain timestamp — it is still what
/// `purge_expired` compares and what the Trash counts down from — but it no
/// longer carries identity as a side job.
///
/// **`COALESCE(batch, stamp)` is the effective key everywhere** (see
/// `db::tasks::Hidden::batch_key`), which is what makes this safe against the
/// same binary skew the Codex columns are guarding against: a leftover pre-v9
/// `cxtasks-mcp` or `cxtasks-runner` goes on writing `deleted_at` with a NULL
/// batch, and those rows simply fall back to v5's timestamp-keyed behaviour
/// rather than becoming one giant NULL batch where restoring anything restores
/// everything. That fallback is also why this needs no ongoing backfill in the
/// style of `short_id`: the rows heal on their own the next time they are
/// restored and re-trashed, and until then they behave exactly as they did
/// before.
///
/// The one-shot backfill below is for the rows already sitting in the Trash at
/// upgrade time. It copies each row's existing stamp into the batch column, so
/// batches that exist today survive the migration byte for byte — including any
/// two that had already collided, which is not repairable after the fact and
/// must at least not change under them.
fn create_tables_v9(conn: &Connection) -> Result<()> {
    for stmt in [
        // Nullable with no default: NULL means "written by a pre-v9 binary",
        // which `Hidden::batch_key` reads as "fall back to the timestamp".
        "ALTER TABLE tasks ADD COLUMN delete_batch TEXT",
        "ALTER TABLE tasks ADD COLUMN archive_batch TEXT",
    ] {
        if let Err(e) = conn.execute(stmt, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
    }
    // Carry the batches that already exist across unchanged.
    conn.execute_batch(
        r#"
        UPDATE tasks SET delete_batch = deleted_at
         WHERE deleted_at IS NOT NULL AND delete_batch IS NULL;
        UPDATE tasks SET archive_batch = archived_at
         WHERE archived_at IS NOT NULL AND archive_batch IS NULL;
        "#,
    )?;
    Ok(())
}

/// v10 — managed image attachments for task notes and comments.
///
/// Metadata belongs in SQLite so task/comment cascades remain authoritative;
/// image bytes live in app data rather than bloating the database and WAL.
/// `comment_id = NULL` means the image is attached to the task's editable Notes
/// section. A non-null value places it beneath that specific log entry.
fn create_tables_v10(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS task_attachments (
            id          TEXT PRIMARY KEY,
            task_id     TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            comment_id  TEXT REFERENCES task_comments(id) ON DELETE CASCADE,
            name        TEXT NOT NULL,
            mime_type   TEXT NOT NULL,
            stored_name TEXT NOT NULL UNIQUE,
            created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_attachments_task
            ON task_attachments(task_id, created_at);
        CREATE INDEX IF NOT EXISTS idx_attachments_comment
            ON task_attachments(comment_id, created_at);
        "#,
    )?;
    Ok(())
}

/// v11 — a project owns its repositories, so a project NAME and a repo
/// DIRECTORY stop being the same thing.
///
/// Until v11 nothing anywhere connected "pocketbuddy" to
/// `~/Projects/finance-app`, and three separate places papered over that by
/// treating the directory name AS the project name: the detail pane's project
/// autocomplete was fed by a `read_dir` of `~/Projects`, and both `file_task`'s
/// parameter docs and `/cxtasks:task` told the caller to derive `project` from
/// the directory. Since [`crate::db::tasks::create`] calls
/// `projects::ensure_exists`, every such string became a first-class project —
/// so a repo whose folder doesn't match its product name manufactured a phantom
/// project on the board, permanently split from the real one holding the work.
///
/// **Many repos per project, at most one owner per repo.** Half of the real
/// portfolio is a web+iOS pair (PocketBuddy = `finance-app` + `xcode/pocketbuddy`,
/// and the same shape for Artist Advisory, BuildersBuddy, Optimium), so a single
/// `projects.repo_path` column would have been wrong on the day it shipped. The
/// UNIQUE index on `repo_path` is the load-bearing half: it makes
/// path → project a *function*, which is what lets
/// [`crate::db::projects::for_repo_path`] answer `file_task` without guessing.
///
/// Keyed by project **name**, matching `sections` and `project_updates` rather
/// than by id. That is a deliberate consistency call, and it makes this the
/// FOURTH text join — so invariant 11 grows a member: `rename` must carry it and
/// `delete` must remove it. An id-keyed table would need no cascade but would
/// mean two different answers to "what does a rename do", which is worse to
/// remember than one rule with a test behind it.
///
/// The backfill uses **evidence, not inference**: distinct `(project, repo_path)`
/// pairs already on tasks. That is why it is `HAVING COUNT(DISTINCT project) = 1`
/// — a path two different projects have filed against is genuinely ambiguous, and
/// silently picking one would bake a wrong mapping into the very table whose job
/// is to be authoritative. Those get left for a human. It also cannot invent the
/// pocketbuddy → finance-app link, because no task ever carried both.
fn create_tables_v11(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS project_repos (
            id         TEXT PRIMARY KEY,
            project    TEXT NOT NULL,
            repo_path  TEXT NOT NULL,
            is_primary INTEGER NOT NULL DEFAULT 0,
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        -- NOCASE because APFS is: `/Projects/CXTasks` and `/Projects/cxtasks`
        -- are one directory, and letting them be two rows would make the reverse
        -- lookup depend on how the path happened to be typed.
        CREATE UNIQUE INDEX IF NOT EXISTS idx_project_repos_path
            ON project_repos(repo_path COLLATE NOCASE);
        CREATE INDEX IF NOT EXISTS idx_project_repos_project
            ON project_repos(project, sort_order);
        "#,
    )?;

    conn.execute(
        "INSERT OR IGNORE INTO project_repos (id, project, repo_path, is_primary, sort_order)
         SELECT lower(hex(randomblob(16))), MIN(TRIM(project)), TRIM(repo_path), 0, 0
           FROM tasks
          WHERE project   IS NOT NULL AND TRIM(project)   <> ''
            AND repo_path IS NOT NULL AND TRIM(repo_path) <> ''
          GROUP BY lower(TRIM(repo_path))
         HAVING COUNT(DISTINCT lower(TRIM(project))) = 1",
        [],
    )?;
    // Exactly one primary per project. A project with repos and no primary would
    // make the detail pane's prefill silently do nothing, which reads as the
    // mapping not having been saved.
    conn.execute(
        "UPDATE project_repos SET is_primary = 1
          WHERE rowid IN (SELECT MIN(rowid) FROM project_repos GROUP BY lower(project))",
        [],
    )?;
    Ok(())
}

/// v12 — one MECE status axis, and a `blocked` that has to say why.
///
/// Until v12 `status` carried four independent axes at once, which is why
/// `inbox` / `next` / `queued` felt interchangeable: a task can genuinely be
/// untriaged AND ready AND handed to the runner, so they were never mutually
/// exclusive. Worse, `blocked` meant two unrelated things *on screen at the same
/// time* — the row badge came from `task_deps` (derived, correct) while the
/// status came from the runner refusing a preflight, and a genuinely
/// dependency-blocked task never got the status at all.
///
/// The axis is now one question — how far along is this work — and everything a
/// rule can derive is off it:
///
/// | status        | meaning                                              |
/// |---------------|------------------------------------------------------|
/// | `inbox`       | captured, untriaged                                  |
/// | `todo`        | triaged, actionable now (was `next`)                 |
/// | `blocked`     | open, can't act — waiting on something outside CXTasks |
/// | `queued`      | **machine-owned**; see below                         |
/// | `in_progress` | active                                               |
/// | `in_review`   | produced, awaiting a human's accept (was `needs_review`) |
/// | `done`        | closed                                               |
///
/// **`blocked` stays because it is real** — waiting on a client's sign-off or an
/// API key that hasn't arrived is GTD's *Waiting For*, and there is no task in
/// this system to hang it off. What it loses is the two illegitimate meanings.
/// It gains `blocked_reason`, required on entry, for the same reason
/// `projects.health` has no bare setter (invariant 14): a state that moves
/// without a written reason decays into an invisible pile within a month.
/// Dependency-blocking stays *derived* and never touches `status`.
///
/// **`queued` is deliberately still a status**, not a column. It is the runner's
/// atomic claim key (`UPDATE … WHERE status = 'queued'`), and invariant 15 turns
/// on old and new binaries agreeing about that query — so moving it would put
/// the one thing that must not drift on the migration's critical path for a
/// purely cosmetic win. It is simply not offered to a human ([`USER_STATUSES`])
/// and renders as a chip.
///
/// **`cancelled` is gone; dropping a task is archiving it.** Keeping it made
/// "archived" and "cancelled" two ways to say one thing — the same non-MECE
/// break one level up. Existing rows are archived rather than remapped to
/// `done`, which would claim work was finished that wasn't; they land on `todo`
/// underneath, since the Archive is the statement and the status beneath it is
/// not read. `CLOSED_STATUSES` shrinking to `["done"]` is why they have to be
/// archived and not merely relabelled: `View::All` is `status NOT IN (closed)`,
/// so a bare relabel would resurrect every abandoned task onto the board.
///
/// The remap is idempotent — unlike v3's priority inversion, running it twice is
/// a no-op because there are no `next` rows left to convert. The ongoing heal
/// for binary skew lives in [`crate::db::tasks::heal_legacy_statuses`], outside
/// every version gate for the same reason `backfill_short_ids` is.
fn create_tables_v12(conn: &Connection) -> Result<()> {
    if let Err(e) = conn.execute("ALTER TABLE tasks ADD COLUMN blocked_reason TEXT", []) {
        if !e.to_string().contains("duplicate column name") {
            return Err(e);
        }
    }
    // Archive the cancelled BEFORE the status remap, so the `status <> 'done'`
    // half can still tell them apart from anything else.
    conn.execute(
        "UPDATE tasks
            SET archived_at   = COALESCE(archived_at, strftime('%Y-%m-%d %H:%M:%f','now')),
                archive_batch = COALESCE(archive_batch, lower(hex(randomblob(16)))),
                status        = 'todo',
                completed_at  = NULL
          WHERE status = 'cancelled'",
        [],
    )?;
    conn.execute_batch(
        r#"
        UPDATE tasks SET status = 'todo'      WHERE status = 'next';
        UPDATE tasks SET status = 'in_review' WHERE status = 'needs_review';
        "#,
    )?;
    Ok(())
}

/// v13 — give the tasks that predate the propagation the repo their project
/// already owned.
///
/// v11 stored the mapping and wired path → project; project → path was wired
/// only into the detail pane's project field, so it ran on exactly one user
/// gesture. Every other route into a project — the quick-add bar's `#project`,
/// an explicit `project` on `file_task`, a `/breakdown` phase, a row dragged
/// onto a project group — left `repo_path` blank while `project_repos` held the
/// answer. [`crate::db::tasks::create`] and [`crate::db::tasks::update`] resolve
/// it now; this is the rows already on the board when they started to.
///
/// **One-shot, deliberately — NOT an ongoing heal** like `backfill_short_ids` or
/// `heal_legacy_statuses`. Those repair a value no one is allowed to choose, so
/// re-running them is always right. A blank workspace IS a choice: plenty of a
/// project's tasks are not code work, and a heal outside the version gate would
/// silently refill the field every time it was cleared, with no way to make it
/// stick. Filling a blank once is help; filling it forever is an argument.
///
/// Skips a project with no repos, and takes the primary via the same ordering as
/// [`crate::db::projects::list_repos`] so the backfill and the live lookup can't
/// disagree about which of a web+iOS pair is the default.
fn create_tables_v13(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE tasks
            SET repo_path = (
                    SELECT pr.repo_path
                      FROM project_repos pr
                     WHERE pr.project = tasks.project COLLATE NOCASE
                     ORDER BY pr.is_primary DESC, pr.sort_order ASC
                     LIMIT 1
                )
          WHERE (repo_path IS NULL OR TRIM(repo_path) = '')
            AND project IS NOT NULL AND TRIM(project) <> ''
            AND EXISTS (
                    SELECT 1 FROM project_repos pr
                     WHERE pr.project = tasks.project COLLATE NOCASE
                )",
        [],
    )?;
    Ok(())
}

/// v14 — a replayable, provider-neutral execution trace for background runs.
///
/// The raw Claude/Codex streams are deliberately not stored here. They can
/// contain source text, reasoning, command output and credentials; the runner
/// writes only the small normalized event summaries the UI actually renders.
/// `seq` is the ordering authority because many tool calls land inside the
/// same clock tick. `call_id + phase` makes noisy provider update events safe
/// to absorb more than once without duplicating the timeline.
fn create_tables_v14(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS run_events (
            id          INTEGER PRIMARY KEY,
            run_id      TEXT NOT NULL REFERENCES task_runs(id) ON DELETE CASCADE,
            seq         INTEGER NOT NULL,
            occurred_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d %H:%M:%f', 'now')),
            kind        TEXT NOT NULL,
            call_id     TEXT,
            tool_name   TEXT,
            phase       TEXT NOT NULL,
            summary     TEXT NOT NULL,
            detail_json TEXT,
            UNIQUE(run_id, seq)
        );

        CREATE INDEX IF NOT EXISTS idx_run_events_run_seq
            ON run_events(run_id, seq);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_run_events_call_phase
            ON run_events(run_id, call_id, phase)
            WHERE call_id IS NOT NULL;
        "#,
    )?;
    Ok(())
}

/// v15 — lift a stored daily token cap that nobody ever chose.
///
/// **A settings default only applies while the key is absent**, and this one
/// wasn't: `bg_daily_token_cap` was sitting in `app_meta` at the old `2000000`,
/// so raising [`crate::db::meta::DEFAULT_DAILY_TOKEN_CAP`] in Rust would have
/// changed nothing at all on the one database that matters — the app would go
/// on capping at 2M while the constant, the comment explaining it and every
/// test all said 50M. That is the same class of silence as a folded migration
/// column (see v9): no error, just the old behaviour.
///
/// Gated on the exact old value rather than on `value < new`, because "still on
/// the default" and "deliberately set low" are different states and only the
/// first is safe to overwrite. Here they're distinguishable: there was no UI to
/// edit this number until the same change that raised it, so an exact match
/// against the pre-v15 default can only be the default.
///
/// One-shot, unlike `backfill_short_ids` / `heal_legacy_statuses` (invariant
/// 17's distinction): those repair a value nobody is allowed to choose, while
/// this one is now editable, and a heal outside the gate would silently undo a
/// deliberate lowering every time the app started.
fn create_tables_v15(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE app_meta SET value = ?2 WHERE key = ?1 AND value = ?3",
        rusqlite::params![
            crate::db::meta::KEY_DAILY_TOKEN_CAP,
            crate::db::meta::DEFAULT_DAILY_TOKEN_CAP.to_string(),
            crate::db::meta::LEGACY_DAILY_TOKEN_CAP.to_string(),
        ],
    )?;
    Ok(())
}

/// v16: what a background run is FOR — continue the work, or audit the last
/// handoff.
///
/// Two additive columns, both `TEXT NOT NULL DEFAULT 'work'`. The safe value and
/// the default value coincide, so there is **nothing to backfill** (v7's
/// `bg_shell_allowed` argument) and no ongoing heal. A heal would be actively
/// wrong here: a run a pre-v16 runner recorded as `work` genuinely *ran* work
/// framing, so rewriting it later would relabel history rather than repair it.
///
/// * `tasks.bg_intent` — the standing choice for the **next** dispatch.
///   `runner::finish_run` resets it to `work` after a successful run, so it is a
///   one-shot arming rather than a durable preference.
/// * `task_runs.intent` — what **this** run was. Mandatory, not merely tidy:
///   because the task's copy is reset, the run row is the only durable record
///   that a review ever happened, and `dispatch::history_block` could not label
///   a critique as one without it. (Sharper than `task_runs.executor`, which
///   exists because an executor can be *switched* later.)
///
/// No index — nothing filters on intent, and that absence **is** the skew
/// argument: see [`crate::intent`] for why one column is safe here where
/// `bg_executor` needed two. **The condition is load-bearing.** It holds only
/// while no Rust code derives a `ToolTier`, the repo gate, or the checkpoint
/// decision from `bg_intent`; review preselects read-only in the UI and nowhere
/// else. The moment intent decides a grant, a pre-v16 runner computes a
/// different tier than a v16 one and this becomes invariant 15's exact shape.
///
/// Not added to the FTS trigger's `OF` list — intent is not searchable text.
fn create_tables_v16(conn: &Connection) -> Result<()> {
    for stmt in [
        "ALTER TABLE tasks ADD COLUMN bg_intent TEXT NOT NULL DEFAULT 'work'",
        // Historical runs all ran work framing, so 'work' is not a guess here.
        "ALTER TABLE task_runs ADD COLUMN intent TEXT NOT NULL DEFAULT 'work'",
    ] {
        if let Err(e) = conn.execute(stmt, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
    }
    Ok(())
}

/// v17: lift the per-run TURN cap off a default nobody chose — gotcha #30, the
/// second instance, on the sibling setting.
///
/// Required rather than tidy, and for a reason that has already bitten once:
/// `budget_settings` reads `get_i64(conn, KEY, DEFAULT)`, so `DEFAULT_MAX_TURNS`
/// is a fallback for an **absent** key, not a value. `bg_max_turns` is present
/// in the real database, stored at `30`, so raising the Rust constant alone
/// changes nothing at all on the one machine that matters. That is verbatim
/// what happened to `bg_daily_token_cap` in v15 — same shape, same file, same
/// silent no-op upgrade.
///
/// Gated on the exact pre-v17 default, never on `value < new`: "still on the
/// default" and "deliberately set low" are different states, and only the first
/// is a migration's to overwrite. They are distinguishable here for exactly the
/// reason they were in v15 — there was no UI to edit this number until the same
/// change that raised it, so an exact match against `LEGACY_MAX_TURNS` can only
/// be the default.
///
/// One-shot, like v15 and unlike `backfill_short_ids`: the number is editable
/// now, so a heal outside the version gate would undo a deliberate lowering
/// every time the app started.
fn create_tables_v17(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE app_meta SET value = ?2 WHERE key = ?1 AND value = ?3",
        rusqlite::params![
            crate::db::meta::KEY_MAX_TURNS,
            crate::db::meta::DEFAULT_MAX_TURNS.to_string(),
            crate::db::meta::LEGACY_MAX_TURNS.to_string(),
        ],
    )?;
    Ok(())
}

/// v18: per-task run budget — a wall clock and a token ceiling a big task can
/// raise for itself, and the record of which clock a run was actually started
/// with.
///
/// All three nullable with no default, so the safe value and the absent value
/// coincide and there is nothing to backfill (v7's `bg_shell_allowed`
/// argument): NULL means "use the global setting", which is exactly what every
/// pre-v18 row was doing. No heal either — a task with no override is the
/// normal case, not damage.
///
/// **`task_runs.timeout_minutes` is the load-bearing one, and it is not
/// bookkeeping.** `db::runs::sweep_abandoned` reclaims a run whose process is
/// gone by comparing its age against the wall clock the runner enforces
/// (invariant 20). Once that clock is per-task, a single global cutoff would
/// sweep a healthy 60-minute run at 35 minutes — freeing a slot that is still
/// occupied, which is the guard's own failure mode inverted and silent, and
/// precisely what invariant 20 warns about for a hardcoded constant. So the
/// sweep reads the value the run was STARTED with, from the run row. On the
/// run rather than derived from the task for `task_runs.executor`'s reason: a
/// task's override can be changed mid-run, and deriving it would retroactively
/// re-judge a run that is already in flight.
fn create_tables_v18(conn: &Connection) -> Result<()> {
    for stmt in [
        "ALTER TABLE tasks ADD COLUMN bg_timeout_minutes INTEGER",
        "ALTER TABLE tasks ADD COLUMN bg_run_token_cap INTEGER",
        // NULL on every historical row = "whatever the global was", which is
        // the truth: until now there was nothing else it could have been.
        "ALTER TABLE task_runs ADD COLUMN timeout_minutes INTEGER",
    ] {
        if let Err(e) = conn.execute(stmt, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
    }
    Ok(())
}

/// v19 — PARA: a grouping node is an **area** or a **project**, and a project
/// ENDS.
///
/// Until now `projects` was one node type doing two jobs. `CXVentures` is a
/// permanent bucket that will never be finished; a wave of
/// government-contracting certifications filed into it is a finite piece of
/// work with a beginning and an end. Sharing one row type meant sharing one
/// board row, one badge and one activity strip — so the sprint's signal was
/// averaged into the bucket's, and `CXVentures` read as permanently `active`
/// because *something* in a 33-task grab bag always moved.
///
/// **`tasks.project` is untouched.** The name stays the join key (invariant
/// 17), so not one task row changes and every existing query still resolves.
/// What is added is what a name alone could never say: whether this thing ends.
///
/// Four columns:
///
/// * `kind` — `'area'` or `'project'`, **defaulting to `'area'` so every
///   existing row backfills to what it already behaves as.** Defaulting to
///   `'project'` would mark five permanent things as finite and start nagging
///   about closing each one, which is invariant 14's warning arriving from the
///   other direction: a signal nobody chose reads as an accusation.
/// * `parent_id` — the owning area's **row id, not its name**, and deliberately
///   so. Every other project join in this schema is by name, but adding a fifth
///   would put another line in `projects::rename`'s "every text join has to move
///   in the same breath" block, and the failure mode of missing it is silent.
///   Parenthood is the one field that is meaningless without a row — a phantom
///   project (work filed under a name with no `projects` row) cannot have been
///   given a parent by anyone — so the id costs nothing the name would have
///   bought. `ON DELETE SET NULL`: deleting an area must orphan its children to
///   the top level, never delete work (invariant 11b).
/// * `target_date` — full ISO-8601 UTC with a trailing `Z`, like `due_at` and
///   `sections.target_date`. A bare `YYYY-MM-DD` renders a day off once JS
///   parses it as local time.
/// * `closed_at` — the project is FINISHED, which is a different claim from
///   `archived` ("hide it from the sidebar"), the same way
///   `sections.completed_at` is different from deleting the section.
///
/// Depth is capped at one level (area → project → section) and enforced in
/// `projects.rs` rather than here: the rule is a cross-row check on the parent's
/// own `parent_id`, which a column CHECK cannot see.
fn create_tables_v19(conn: &Connection) -> Result<()> {
    for stmt in [
        // No CHECK constraint: SQLite cannot add one via ALTER TABLE, and a
        // table rebuild to gain it would put the FTS triggers and every
        // name-keyed join at risk for a value only `projects.rs` ever writes.
        "ALTER TABLE projects ADD COLUMN kind TEXT NOT NULL DEFAULT 'area'",
        "ALTER TABLE projects ADD COLUMN parent_id TEXT REFERENCES projects(id) ON DELETE SET NULL",
        "ALTER TABLE projects ADD COLUMN target_date TEXT",
        "ALTER TABLE projects ADD COLUMN closed_at TEXT",
    ] {
        if let Err(e) = conn.execute(stmt, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
    }
    // The child lookup runs once per area on every sidebar render and once per
    // row on the board.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_projects_parent ON projects(parent_id);",
    )?;
    Ok(())
}

/// v20 — which machine ran it, and is it still alive.
///
/// Both columns record a fact about a MOMENT, which is why they land before
/// there is a second machine rather than alongside one. A column describing
/// what happened during a run cannot be added afterwards and be true: nothing
/// can go back and observe which host executed a run that finished last week.
/// (A standing PREFERENCE — "where should this task prefer to run" — is the
/// opposite, and can arrive whenever the vocabulary for it is known.)
///
/// # Why legacy rows are left NULL rather than backfilled
///
/// The tempting move is `UPDATE task_runs SET host = <this machine>`, and it is
/// wrong in a way that only shows up later. Migrations run wherever the DB is
/// first opened by a v20 binary — which, after a `.backup` is restored or a
/// database is seeded onto a second host, may not be the machine that produced
/// those rows. That backfill would have the NAS stamp its own id onto runs the
/// MacBook executed, and every consumer downstream would believe it.
///
/// NULL is not a missing value here, it is a meaningful one: **this run predates
/// multi-host, so it came from the only machine there was.** Any host reading it
/// treats it exactly as today's code does — sweep it by age — which is safe
/// precisely because a live remote run is necessarily a v20 row WITH a host.
/// The unknown stays unknown, and nothing has to lie to fill it in.
fn create_tables_v20(conn: &Connection) -> Result<()> {
    for stmt in [
        // No DEFAULT, deliberately. A default would make every legacy row claim
        // a host, which is the backfill this migration exists to avoid.
        "ALTER TABLE task_runs ADD COLUMN host TEXT",
        // Liveness, not age. `sweep_abandoned` currently asks "has this run
        // exceeded its timeout", which is answerable about a local process and
        // meaningless about a remote one — a healthy run on another machine
        // looks identical to a dead one. A heartbeat is the difference.
        "ALTER TABLE task_runs ADD COLUMN heartbeat_at TEXT",
    ] {
        if let Err(e) = conn.execute(stmt, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
    }
    // The sweep and the concurrency gate both filter on (status, host) every
    // tick, and the gate runs before every single spawn.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_task_runs_host_status ON task_runs(host, status);",
    )?;
    Ok(())
}

/// v21 — the sync layer's own tables.
///
/// Nothing here is synced. The outbox is a record of what THIS host has not yet
/// sent, and the lease ledger is the canonical node's record of which T-number
/// ranges it has handed out; replicating either would be replicating a
/// machine's private bookkeeping, and in the ledger's case would let two nodes
/// believe they own the same range.
fn create_tables_v21(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        -- Intents this host has applied locally and not yet pushed.
        --
        -- A TABLE IN cxtasks.db, not a file beside it, and that is the whole
        -- design. The verb's effect and the record of it must commit together
        -- or a crash between them silently drops a change — and they can only
        -- be atomic if they are in the same database. It is also what lets
        -- `cxtasks-mcp` and `cxtasks-runner` append without a second lock
        -- domain: they already hold a connection to this file.
        --
        -- `local_seq` is INTEGER PRIMARY KEY AUTOINCREMENT rather than plain
        -- INTEGER PRIMARY KEY. Without AUTOINCREMENT, SQLite reuses the rowid
        -- of a deleted max row, so draining the outbox and appending again
        -- would re-issue a sequence number that has already been pushed — and
        -- ordering by it would then replay two different intents in the wrong
        -- order, with nothing to notice.
        CREATE TABLE IF NOT EXISTS sync_outbox (
            local_seq  INTEGER PRIMARY KEY AUTOINCREMENT,
            cmd        TEXT NOT NULL,
            args_json  TEXT NOT NULL,
            host       TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- T-number ranges the CANONICAL node has issued.
        --
        -- Lease blocks rather than `MAX(local, canonical)`: that races. Two
        -- nodes creating a task in the same window both claim 91, the merge
        -- takes 92, and there are two T91s — unrepairable under invariant 14,
        -- because T-numbers are already written into commit messages and notes.
        -- A range is handed out once, strictly above every range before it, so
        -- no coordination is needed on the hot path.
        --
        -- `hi` is INCLUSIVE. Half-open would be defensible; inclusive is what
        -- `next_short_id > sync_lease_hi` reads as "out of lease", and the two
        -- conventions must not both exist.
        CREATE TABLE IF NOT EXISTS short_id_leases (
            lo         INTEGER NOT NULL,
            hi         INTEGER NOT NULL,
            host       TEXT NOT NULL,
            claimed_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (lo)
        );

        -- The drain reads in `local_seq` order and deletes what it sent.
        CREATE INDEX IF NOT EXISTS idx_sync_outbox_seq ON sync_outbox(local_seq);
        "#,
    )?;
    Ok(())
}

/// v22 — an attachment's bytes get a content address, so the file can be a
/// CACHE rather than a second master copy.
///
/// Until now `stored_name` was the only thing a row said about its bytes, and a
/// name answers "which file" but never "is this the file". That gap is what
/// stops the local `attachments/` directory from being disposable: without a
/// digest there is no way to tell a complete download from a truncated one, so
/// the only available repair is "delete everything and re-fetch" — and nothing
/// can ever notice it is needed.
///
/// Nullable with **no default**, so the safe value and the absent value
/// coincide and there is nothing to backfill (v7's `bg_shell_allowed`
/// argument). NULL means "no content address on record", and a fetch for such a
/// row is accepted as served rather than refused — the graceful-degradation
/// shape of invariant 19, not invariant 15's executable divergence: an
/// unverified image is a worse guarantee, never a wrong one.
///
/// **Written once, at import, and never updated.** Filling it in later would be
/// the obvious kindness and is a trap: `task_attachments` is swept on
/// `created_at`, which never moves, so a late fill would not travel — and the
/// next pull would overwrite it back to NULL from the peer's post-image, count
/// as a change, redraw the UI and discard in-progress edits (gotcha #24),
/// forever. A column that cannot travel must not be written after the row that
/// carries it.
/// v23 — a second executor host becomes expressible.
///
/// Two things, and they are the two halves of failing closed.
///
/// **`tasks.bg_remote_allowed`** is the remote analogue of `bg_allowed`, in a
/// column no earlier binary reads, defaulting to 0. Deploying a remote host
/// therefore makes nothing newly claimable: every task that exists today stays
/// local-only until somebody says otherwise, one task at a time. It is the same
/// skew trick as `bg_codex_allowed` (see [`crate::executor`]) applied to a
/// different axis — where rather than what.
///
/// **`hosts`** is the registry [`crate::routing`] matches against. Note every
/// declared column's DEFAULT is the *least* capable value: read-only, mirror
/// only, no checkpoint, one at a time. A row inserted by a future binary that
/// forgets a column lands minimal rather than maximal, which is the only
/// direction it is safe to be wrong in.
///
/// Deliberately NOT added here: a notices table. The plan called for a durable
/// outbox so the app can say "4 runs finished on the NAS while you were away",
/// and `task_runs` already is one — it syncs whole-row, it carries `host` and
/// `finished_at`, and one `app_meta` cursor turns it into the same feature with
/// no second table to classify, sweep, purge and keep in step.
fn create_tables_v23(conn: &Connection) -> Result<()> {
    for stmt in [
        "ALTER TABLE tasks ADD COLUMN bg_remote_allowed INTEGER NOT NULL DEFAULT 0",
        // Cancellation has to CROSS hosts now. The file remains the interface —
        // `process.rs` is unchanged and still polls a path — but a file cannot
        // be seen from another machine, so the request needs a transport, and
        // the transport is the row that is already replicating.
        //
        // Note what this forces, which is gotcha #36 exactly: `task_runs` is
        // swept on MAX(started_at, heartbeat_at, finished_at), so a column
        // written on its own would never move the watermark and the request
        // would never travel. The sweep expression grows to include it — the
        // same fix `heartbeat_at` needed for the same reason.
        "ALTER TABLE task_runs ADD COLUMN cancel_requested_at TEXT",
    ] {
        if let Err(e) = conn.execute(stmt, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS hosts (
            id              TEXT PRIMARY KEY,
            label           TEXT NOT NULL,
            max_tier        TEXT NOT NULL DEFAULT 'read-only',
            executors       TEXT NOT NULL DEFAULT '',
            workspaces      TEXT NOT NULL DEFAULT 'mirror',
            can_checkpoint  INTEGER NOT NULL DEFAULT 0,
            max_concurrent  INTEGER NOT NULL DEFAULT 1,
            headroom_pct    INTEGER NOT NULL DEFAULT 0,
            priority        INTEGER NOT NULL DEFAULT 100,
            enabled         INTEGER NOT NULL DEFAULT 1,
            last_seen_at    TEXT,
            paused          INTEGER NOT NULL DEFAULT 0,
            gate            TEXT,
            executors_ready TEXT NOT NULL DEFAULT '',
            mirrors         TEXT NOT NULL DEFAULT '',
            mirror_remote   TEXT,
            created_at      TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );
         CREATE INDEX IF NOT EXISTS idx_hosts_enabled ON hosts(enabled, priority);",
    )?;
    Ok(())
}

/// v24 — a task can name the MCP servers a detached run may reach.
///
/// One column, and it holds NAMES validated against
/// [`crate::mcp_registry::REGISTRY`], never command lines. If it held commands
/// then "which servers may this task use" would be "what may this task
/// execute", and a task row would be a place to write arbitrary argv for an
/// unattended process — considerably worse than the repo gate it replaces.
///
/// Empty by default, which is the whole design: a detached run with no repo and
/// no servers can read nothing. That is a useless run and a safe one, and
/// making it useful is then a named, per-task decision.
///
/// Note there is deliberately NO "this task is detached" flag. A detached task
/// is one with no `repo_path`, and every drain query that predates this —
/// including every host that does not declare the `Detached` workspace —
/// already carries `repo_path IS NOT NULL`. **The existing guard is the skew
/// guard**, so a leftover runner cannot see a detached task with no new column
/// at all. Worth writing down, because the next reader will look for that flag.
fn create_tables_v24(conn: &Connection) -> Result<()> {
    if let Err(e) = conn.execute("ALTER TABLE tasks ADD COLUMN bg_mcp_servers TEXT", []) {
        let msg = e.to_string();
        if !msg.contains("duplicate column name") {
            return Err(e);
        }
    }
    Ok(())
}

/// v25 — tripwires: a task that waits for a CONDITION rather than a date.
///
/// Five stable columns on `tasks` and one volatile 1:1 table, and the split is
/// the design (invariant 26). Sync applies whole-row post-images, so every
/// "last checked at" written onto `tasks` would rewrite the task row a phone is
/// editing at that moment. The bookkeeping — requests, the lease, attempt
/// counters, the last note — therefore lives on `tripwire_runtime`, which
/// travels on its own and whose whole-row merge can clobber nothing but itself.
///
/// All five task columns are nullable with no default: NULL means "no
/// tripwire", which is what every existing row is, so nothing is backfilled
/// and a leftover pre-v25 writer simply never sets them. `tripwire_state` is
/// NULL exactly when `tripwire` is; `db::tasks::update` keeps both halves.
///
/// The runtime row is created lazily and RESET rather than deleted on clear —
/// so no verb joins `api::DELETES_ROWS` for it, and the row only ever goes by
/// cascade when its task does.
fn create_tables_v25(conn: &Connection) -> Result<()> {
    for stmt in [
        "ALTER TABLE tasks ADD COLUMN tripwire TEXT",
        "ALTER TABLE tasks ADD COLUMN tripwire_every_minutes INTEGER",
        "ALTER TABLE tasks ADD COLUMN tripwire_on_trip TEXT",
        "ALTER TABLE tasks ADD COLUMN tripwire_state TEXT",
        "ALTER TABLE tasks ADD COLUMN tripwire_tripped_at TEXT",
    ] {
        if let Err(e) = conn.execute(stmt, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_tasks_tripwire
             ON tasks(tripwire_state) WHERE tripwire_state IS NOT NULL;
         CREATE TABLE IF NOT EXISTS tripwire_runtime (
            task_id           TEXT PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
            requested_at      TEXT,
            lease_id          TEXT,
            lease_host        TEXT,
            lease_spec        TEXT,
            lease_request_at  TEXT,
            lease_expires_at  TEXT,
            checked_at        TEXT,
            last_outcome      TEXT,
            last_note         TEXT,
            failures          INTEGER NOT NULL DEFAULT 0,
            updated_at        TEXT NOT NULL DEFAULT (datetime('now'))
         );",
    )?;
    Ok(())
}

/// v26 — typed task relations for conclusions worth preserving.
///
/// `task_relations` stores only the two relationships whose absence was losing
/// information: a directional `supersedes` edge and a symmetric `relates_to`
/// edge. A superseded task stays a task — including its runs and comments —
/// but stops being handed out as work. Pure plan churn should still be deleted;
/// this table is for work that carries a conclusion worth preserving.
fn create_tables_v26(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS task_relations (
            from_task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            to_task_id   TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            kind         TEXT NOT NULL CHECK (kind IN ('supersedes','relates_to')),
            created_at   TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (from_task_id, to_task_id, kind),
            CHECK (from_task_id <> to_task_id)
         );
         CREATE INDEX IF NOT EXISTS idx_task_relations_to
             ON task_relations(to_task_id, kind);",
    )?;
    Ok(())
}

/// v27 — `blocked` leaves the status axis and becomes a HOLD.
///
/// v12 kept `blocked` as a status "because it is real" and had nowhere else to
/// put it. That left one hold on the progress axis while the other three
/// (a defer date, an armed tripwire, an open dependency) were derived — so the
/// Status facet and the Waiting facet each had a "Blocked" that meant a
/// different subset, and "Status is not Blocked" still showed every
/// dependency-blocked task. A hold is *why can't I act*, a status is *how far
/// along*, and a task has one of each: in progress AND waiting on a key is a
/// real state that a single field could not say.
///
/// So `blocked_reason` is now the hand-set hold on its own — non-NULL means
/// held, independent of status — and the status underneath goes back to what
/// it was, which for every existing row is `todo` (the only way in was the
/// picker or the runner parking a run; both start from open work). No column
/// change, so a pre-v27 binary keeps working: it can still write `'blocked'`,
/// and `heal_legacy_statuses` folds that back to `todo` on the next open,
/// reason intact, exactly as it does for `'next'`.
fn create_tables_v27(conn: &Connection) -> Result<()> {
    conn.execute("UPDATE tasks SET status = 'todo' WHERE status = 'blocked'", [])?;
    Ok(())
}

/// v28 — `task_comments.edited_at`, so an edited comment syncs.
///
/// Comments are edited in place, and the sync sweep found changed rows by
/// `created_at` alone — so an edit applied locally, was never pushed, and the
/// next pull put the old text back. Nullable and written only by
/// `comments::edit`, in the same UPDATE as the body (gotcha #36: a column on a
/// swept table must be written WITH the change it describes). Additive, so a
/// pre-v28 binary keeps working; its edits just don't stamp it.
fn create_tables_v28(conn: &Connection) -> Result<()> {
    if let Err(e) = conn.execute("ALTER TABLE task_comments ADD COLUMN edited_at TEXT", []) {
        let msg = e.to_string();
        if !msg.contains("duplicate column name") {
            return Err(e);
        }
    }
    Ok(())
}

/// v29 — `projects.icon` and `tags.icon`: the sidebar icon a person chose.
///
/// Either `lucide:<name>` (a built-in glyph, coloured by the row's existing
/// `color`) or a small PNG as a `data:` URI — an app's own icon, taken from its
/// repo. Inline rather than an attachment on purpose: it is a few KB, and in
/// the row it travels with the whole-row sync both tables already have, where
/// an attachment would need the blob path (invariant 23) for one icon.
/// Validated on write (`db::appearance`). Nullable = the default icon;
/// additive, so a pre-v29 binary keeps working and simply shows the default.
fn create_tables_v29(conn: &Connection) -> Result<()> {
    for table in ["projects", "tags"] {
        if let Err(e) = conn.execute(&format!("ALTER TABLE {table} ADD COLUMN icon TEXT"), []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
    }
    Ok(())
}

fn create_tables_v22(conn: &Connection) -> Result<()> {
    if let Err(e) = conn.execute("ALTER TABLE task_attachments ADD COLUMN sha256 TEXT", []) {
        let msg = e.to_string();
        if !msg.contains("duplicate column name") {
            return Err(e);
        }
    }
    Ok(())
}

/// FTS5 over `tasks(title, body, project)`, kept in sync by triggers exactly
/// like cxmail's `messages_fts` (gotcha #26). Three invariants carry over:
///   1. Never write `tasks_fts` directly — only [`crate::db::search::rebuild`].
///   2. Never enable `PRAGMA recursive_triggers`.
///   3. User keywords must go through `crate::db::search::build_match_expr`.
fn create_fts_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS tasks_fts USING fts5(
            title,
            body,
            project,
            tokenize = 'porter unicode61',
            prefix = '2 3'
        );

        CREATE TRIGGER IF NOT EXISTS tasks_fts_ai AFTER INSERT ON tasks BEGIN
            INSERT INTO tasks_fts(rowid, title, body, project)
            VALUES (new.rowid, new.title, COALESCE(new.body, ''), COALESCE(new.project, ''));
        END;

        -- Plain DELETE/UPDATE against the index, matching cxmail's
        -- `messages_fts_ad` / `_au`. The `INSERT INTO fts(fts,…) VALUES('delete',…)`
        -- form that FTS5 docs show is for **external-content** tables only; on
        -- an ordinary FTS5 table it fails with a bare "SQL logic error", which
        -- surfaces as every task DELETE failing rather than as a search bug.
        CREATE TRIGGER IF NOT EXISTS tasks_fts_ad AFTER DELETE ON tasks BEGIN
            DELETE FROM tasks_fts WHERE rowid = old.rowid;
        END;

        -- Scoped to the indexed columns so status/priority flips don't rewrite
        -- FTS rows (cxmail gotcha #26 invariant 5). Adding an indexed column
        -- means extending this OF list.
        CREATE TRIGGER IF NOT EXISTS tasks_fts_au
        AFTER UPDATE OF title, body, project ON tasks BEGIN
            UPDATE tasks_fts SET
                title   = new.title,
                body    = COALESCE(new.body, ''),
                project = COALESCE(new.project, '')
            WHERE rowid = new.rowid;
        END;
        "#,
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    initialize(&conn).expect("initialize schema");
    conn
}

#[cfg(test)]
mod tests {
    /// The whole schema, as a checked-in snapshot.
    ///
    /// # What this is for
    ///
    /// Two things, and the second is the reason it exists now rather than
    /// later.
    ///
    /// Today: a migration that alters a table it did not mean to, or an index
    /// quietly dropped by an `ALTER`, changes this file and has to be
    /// acknowledged. Nothing else in the suite notices a schema change that
    /// happens to leave every query working.
    ///
    /// Once there is a second host: this is the proof that two machines'
    /// databases are structurally identical. Whole-row sync assumes both ends
    /// agree on what a row IS, and the failure when they don't is not a crash
    /// — it is columns silently not travelling. A fingerprint both sides can
    /// compute turns that into an answerable question.
    ///
    /// # When it fails
    ///
    /// Read the diff. If the change was deliberate, regenerate:
    ///
    /// ```text
    /// sqlite3 <fresh.db> "PRAGMA trusted_schema=ON;
    ///   SELECT type || ' ' || name || ' :: ' ||
    ///          COALESCE(replace(replace(sql, char(10), ' '), char(9), ' '), '(implicit)')
    ///   FROM sqlite_master ORDER BY name, type;"
    /// ```
    ///
    /// then collapse runs of spaces. If it was not deliberate, you just caught
    /// the bug this test is for.
    /// Print the fingerprint in the exact format the test compares against.
    /// `cargo test -p cxtasks-core dump_schema_fingerprint -- --ignored --nocapture`
    #[test]
    #[ignore = "generator, not a check"]
    fn dump_schema_fingerprint() {
        let conn = test_conn();
        let mut stmt = conn
            .prepare(
                "SELECT type, name, COALESCE(sql, '(implicit)')
                   FROM sqlite_master ORDER BY name, type",
            )
            .unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |r| {
                let ty: String = r.get(0)?;
                let name: String = r.get(1)?;
                let sql: String = r.get(2)?;
                let sql = sql.replace(['\n', '\t'], " ");
                let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
                Ok(format!("{ty} {name} :: {sql}"))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        println!("--BEGIN--");
        for r in rows {
            println!("{r}");
        }
        println!("--END--");
    }

    #[test]
    fn the_schema_matches_its_checked_in_fingerprint() {
        let conn = test_conn();
        let mut stmt = conn
            .prepare(
                "SELECT type, name, COALESCE(sql, '(implicit)')
                   FROM sqlite_master ORDER BY name, type",
            )
            .unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |r| {
                let ty: String = r.get(0)?;
                let name: String = r.get(1)?;
                let sql: String = r.get(2)?;
                let sql = sql.replace(['\n', '\t'], " ");
                let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
                Ok(format!("{ty} {name} :: {sql}"))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        let expected: Vec<&str> = include_str!("schema_fingerprint.txt")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();

        // Report the DIFFERENCE, not "two long strings differ" — a 65-line
        // mismatch printed whole is unreadable and gets skimmed.
        let actual: Vec<&str> = rows.iter().map(|s| s.as_str()).collect();
        let added: Vec<_> = actual.iter().filter(|a| !expected.contains(a)).collect();
        let removed: Vec<_> = expected.iter().filter(|e| !actual.contains(e)).collect();
        assert!(
            added.is_empty() && removed.is_empty(),
            "schema drifted from its fingerprint.\n  ADDED:\n    {}\n  REMOVED:\n    {}",
            added
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join("\n    "),
            removed
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join("\n    "),
        );
    }

    /// The fingerprint must never be allowed to become trivially empty.
    ///
    /// Belt and braces: if `include_str!` ever picked up an empty or truncated
    /// file, the test above would pass with flying colours while checking
    /// nothing at all.
    #[test]
    fn the_fingerprint_is_not_vacuously_satisfied() {
        let n = include_str!("schema_fingerprint.txt")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        assert!(n > 50, "fingerprint has only {n} entries — is the file truncated?");
    }

    use super::*;

    /// The riskiest change in v3: flipping P0/P3 means every stored priority
    /// must be remapped, or a "drop everything" task silently becomes
    /// "whenever". Builds a genuine v2 database and migrates it.
    #[test]
    fn v3_inverts_existing_priorities_exactly_once() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        create_tables_v1(&conn).unwrap();
        create_tables_v2(&conn).unwrap();
        // `app_meta` is created lazily by get_schema_version, not by v1.
        get_schema_version(&conn).unwrap();
        set_schema_version(&conn, 2).unwrap();

        // Old semantics: 3 = highest … 0 = none.
        for (id, old) in [("a", 3), ("b", 2), ("c", 1), ("d", 0)] {
            conn.execute(
                "INSERT INTO tasks (id, title, priority) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, format!("task {id}"), old],
            )
            .unwrap();
        }

        initialize(&conn).expect("migrate v2 → v3");

        let p = |id: &str| -> i64 {
            conn.query_row(
                "SELECT priority FROM tasks WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap()
        };
        // old highest (3) → P0 highest; old none (0) → P3 lowest.
        assert_eq!(p("a"), 0, "old 3 (highest) must become P0");
        assert_eq!(p("b"), 1);
        assert_eq!(p("c"), 2);
        assert_eq!(p("d"), 3, "old 0 (none) must become P3 lowest");

        // Re-running must NOT flip them back — that would look like corruption.
        initialize(&conn).expect("idempotent re-init");
        assert_eq!(p("a"), 0, "the inversion ran twice");
        assert_eq!(p("d"), 3);
    }

    /// A genuine v11 database carrying the pre-v12 status vocabulary, so the
    /// v12 tests migrate real prior state rather than a fixture that already
    /// speaks the new one.
    fn v11_conn_with_legacy_statuses() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        // `app_meta` is created lazily by `get_schema_version`, not by v1, and
        // v6's short-id counter needs it — so it has to exist before the walk.
        get_schema_version(&conn).unwrap();
        for step in [
            create_tables_v1,
            create_tables_v2,
            create_tables_v3,
            create_tables_v4,
            create_tables_v5,
            create_tables_v6,
            create_tables_v7,
            create_tables_v8,
            create_tables_v9,
            create_tables_v10,
            create_tables_v11,
        ] {
            step(&conn).unwrap();
        }
        set_schema_version(&conn, 11).unwrap();

        for (id, status) in [
            ("a", "next"),
            ("b", "needs_review"),
            ("c", "cancelled"),
            ("d", "inbox"),
            ("e", "done"),
        ] {
            conn.execute(
                "INSERT INTO tasks (id, title, status) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, format!("task {id}"), status],
            )
            .unwrap();
        }
        conn
    }

    fn status_of(conn: &Connection, id: &str) -> String {
        conn.query_row(
            "SELECT status FROM tasks WHERE id = ?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// The rename half. Idempotent by construction — unlike v3's inversion,
    /// there is nothing left to convert on a second pass — but asserted anyway,
    /// because "ran twice" is the failure that reads as corruption.
    #[test]
    fn v12_renames_the_legacy_statuses_and_leaves_the_rest_alone() {
        let conn = v11_conn_with_legacy_statuses();
        initialize(&conn).expect("migrate v11 → v12");

        assert_eq!(status_of(&conn, "a"), "todo", "next must become todo");
        assert_eq!(status_of(&conn, "b"), "in_review");
        assert_eq!(status_of(&conn, "d"), "inbox", "untouched status changed");
        assert_eq!(status_of(&conn, "e"), "done");

        initialize(&conn).expect("idempotent re-init");
        assert_eq!(status_of(&conn, "a"), "todo");
        assert_eq!(status_of(&conn, "b"), "in_review");
    }

    /// v27: a `blocked` row keeps its reason and returns to `todo`. Twice —
    /// once through the gated migration, once through the ungated heal that
    /// catches a pre-v27 binary still writing the old status — because the
    /// row's fate must not depend on which binary opened the DB first.
    #[test]
    fn v27_turns_the_blocked_status_into_a_hold_and_keeps_healing_it() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO tasks (id, title, status, blocked_reason)
             VALUES ('h', 'held', 'blocked', 'waiting on Dana')",
            [],
        )
        .unwrap();
        set_schema_version(&conn, 26).unwrap();
        initialize(&conn).expect("migrate v26 → v27");
        assert_eq!(status_of(&conn, "h"), "todo");
        let reason: Option<String> = conn
            .query_row("SELECT blocked_reason FROM tasks WHERE id = 'h'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(reason.as_deref(), Some("waiting on Dana"), "the hold must survive");

        // The skew heal: an old binary writes `blocked` onto a v27 database.
        conn.execute("UPDATE tasks SET status = 'blocked' WHERE id = 'h'", []).unwrap();
        initialize(&conn).expect("re-open at v27");
        assert_eq!(status_of(&conn, "h"), "todo", "the heal must run outside the gate");
    }

    /// `cancelled` had to go *somewhere*, and both obvious targets are wrong:
    /// `done` claims work finished that wasn't, and a bare relabel to `todo`
    /// puts every abandoned task back on the board — because `View::All` is
    /// `status NOT IN (CLOSED_STATUSES)` and that set just shrank to `["done"]`.
    /// Archiving is what makes the drop survive the migration.
    #[test]
    fn v12_archives_cancelled_tasks_rather_than_calling_them_done() {
        let conn = v11_conn_with_legacy_statuses();
        initialize(&conn).expect("migrate v11 → v12");

        let (status, archived, batch, completed): (String, Option<String>, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT status, archived_at, archive_batch, completed_at FROM tasks WHERE id = 'c'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();

        assert_ne!(status, "done", "an abandoned task must not read as finished");
        assert_eq!(status, "todo");
        assert!(archived.is_some(), "a dropped task must leave the board");
        assert!(
            batch.is_some(),
            "without a batch id the restore path falls back to the timestamp (v9)"
        );
        assert!(completed.is_none());
    }

    /// The skew heal, which is the half that runs forever. A leftover pre-v12
    /// `cxtasks-mcp` keeps writing the old vocabulary after the DB is stamped
    /// 12, and `version < 12` never fires again — so an unhealed row would sit
    /// there permanently on a status nothing else speaks, rendering as a blank
    /// pill. Same reasoning as `backfill_short_ids`.
    #[test]
    fn a_stale_binary_writing_a_pre_v12_status_is_healed_on_the_next_open() {
        let conn = v11_conn_with_legacy_statuses();
        initialize(&conn).expect("migrate v11 → v12");

        // Exactly what an old binary does: a plain INSERT, long after the gate.
        conn.execute(
            "INSERT INTO tasks (id, title, status) VALUES
                ('late-1','filed by a stale mcp','next'),
                ('late-2','also stale','needs_review'),
                ('late-3','dropped by a stale mcp','cancelled')",
            [],
        )
        .unwrap();

        initialize(&conn).expect("re-open");

        assert_eq!(status_of(&conn, "late-1"), "todo");
        assert_eq!(status_of(&conn, "late-2"), "in_review");
        assert_eq!(status_of(&conn, "late-3"), "todo");
        let archived: Option<String> = conn
            .query_row(
                "SELECT archived_at FROM tasks WHERE id = 'late-3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            archived.is_some(),
            "a late 'cancelled' must be archived too, or it reappears on the board"
        );
    }

    /// Build a genuine v5 database — tables and stamp, no v6 columns — so the
    /// v6 tests migrate real prior state rather than a fixture that already has
    /// the columns under test.
    fn v5_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        create_tables_v1(&conn).unwrap();
        create_tables_v2(&conn).unwrap();
        create_tables_v3(&conn).unwrap();
        create_tables_v4(&conn).unwrap();
        create_tables_v5(&conn).unwrap();
        get_schema_version(&conn).unwrap(); // creates app_meta
        set_schema_version(&conn, 5).unwrap();
        conn
    }

    /// Insert a task the way a **pre-v6 binary** does: no `short_id`, because
    /// its INSERT statement doesn't know the column exists.
    fn raw_task(conn: &Connection, id: &str, created_at: &str) {
        conn.execute(
            "INSERT INTO tasks (id, title, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, format!("task {id}"), created_at],
        )
        .unwrap();
    }

    #[test]
    fn v6_tables_and_columns_exist() {
        let conn = test_conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='project_updates'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "project_updates missing after migration");

        for (table, column) in [
            ("tasks", "short_id"),
            ("projects", "health"),
            ("sections", "target_date"),
            ("sections", "completed_at"),
        ] {
            let found: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
                    rusqlite::params![column],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(found, 1, "{table}.{column} missing after migration");
        }

        // `short_id` must NOT be in the FTS update trigger's OF list — indexing
        // it would churn every FTS row on every backfill for no search value.
        let trigger: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='tasks_fts_au'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            !trigger.contains("short_id"),
            "short_id leaked into the FTS trigger: {trigger}"
        );
    }

    /// The v6 backfill numbers existing tasks in the order they were captured,
    /// and re-running `initialize` must never renumber them — a T-number that
    /// moves invalidates every reference already written down.
    #[test]
    fn v6_backfills_short_ids_in_created_order_exactly_once() {
        let conn = v5_conn();
        // Deliberately inserted out of chronological order.
        raw_task(&conn, "middle", "2026-02-01 10:00:00");
        raw_task(&conn, "oldest", "2026-01-01 10:00:00");
        raw_task(&conn, "newest", "2026-03-01 10:00:00");

        initialize(&conn).expect("migrate v5 → v6");

        let short = |id: &str| -> i64 {
            conn.query_row(
                "SELECT short_id FROM tasks WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            (short("oldest"), short("middle"), short("newest")),
            (1, 2, 3),
            "numbers must follow created_at, not storage order"
        );

        initialize(&conn).expect("idempotent re-init");
        assert_eq!(
            (short("oldest"), short("middle"), short("newest")),
            (1, 2, 3),
            "the backfill renumbered on a second run"
        );
    }

    /// The state the real database was found in: `bg_daily_token_cap` stored at
    /// the old default, so bumping the Rust constant alone would have raised
    /// nothing. And the other half — a cap somebody set on purpose is left
    /// exactly where they set it.
    #[test]
    fn v15_lifts_a_default_token_cap_and_leaves_a_chosen_one_alone() {
        use crate::db::meta::{
            budget_settings, DEFAULT_DAILY_TOKEN_CAP, KEY_DAILY_TOKEN_CAP, LEGACY_DAILY_TOKEN_CAP,
        };

        let conn = test_conn();
        crate::db::meta::set(&conn, KEY_DAILY_TOKEN_CAP, &LEGACY_DAILY_TOKEN_CAP.to_string())
            .unwrap();
        create_tables_v15(&conn).unwrap();
        assert_eq!(
            budget_settings(&conn).daily_token_cap,
            DEFAULT_DAILY_TOKEN_CAP,
            "a cap still on the pre-v15 default must be lifted — the stored value \
             wins over the constant, so leaving it is a silent no-op upgrade"
        );

        crate::db::meta::set(&conn, KEY_DAILY_TOKEN_CAP, "3000000").unwrap();
        create_tables_v15(&conn).unwrap();
        assert_eq!(
            budget_settings(&conn).daily_token_cap, 3_000_000,
            "a deliberately-set cap is not the migration's to raise"
        );
    }

    /// Gotcha #30's second instance, on the sibling setting. `bg_max_turns` was
    /// found stored at `30` in the real database, so raising `DEFAULT_MAX_TURNS`
    /// alone would have changed nothing on the one machine that matters — and
    /// the symptom would have been the same two runs dying at turn 31 with the
    /// constant, the doc comment and the tests all saying 150.
    #[test]
    fn v17_lifts_a_default_turn_cap_and_leaves_a_chosen_one_alone() {
        use crate::db::meta::{budget_settings, DEFAULT_MAX_TURNS, KEY_MAX_TURNS, LEGACY_MAX_TURNS};

        let conn = test_conn();
        crate::db::meta::set(&conn, KEY_MAX_TURNS, &LEGACY_MAX_TURNS.to_string()).unwrap();
        create_tables_v17(&conn).unwrap();
        assert_eq!(
            budget_settings(&conn).max_turns,
            DEFAULT_MAX_TURNS,
            "a turn cap still on the pre-v17 default must be lifted — the stored \
             value wins over the constant, so leaving it is a silent no-op upgrade"
        );

        crate::db::meta::set(&conn, KEY_MAX_TURNS, "45").unwrap();
        create_tables_v17(&conn).unwrap();
        assert_eq!(
            budget_settings(&conn).max_turns,
            45,
            "a deliberately-set cap is not the migration's to raise"
        );
    }

    /// The three-binary skew case, and the reason the backfill lives outside the
    /// version gate: after shipping, a leftover pre-v6 `cxtasks-mcp` keeps
    /// writing rows with a NULL `short_id` against an already-stamped v6 DB.
    #[test]
    fn a_pre_v6_writer_row_is_healed_on_next_initialize() {
        let conn = test_conn(); // already at CURRENT_VERSION
        raw_task(&conn, "written-by-an-old-binary", "2026-07-29 12:00:00");
        assert!(
            conn.query_row(
                "SELECT short_id IS NULL FROM tasks WHERE id = 'written-by-an-old-binary'",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap(),
            "fixture no longer reproduces the skew — a pre-v6 INSERT must leave NULL"
        );

        initialize(&conn).expect("re-init heals");
        let healed: Option<i64> = conn
            .query_row(
                "SELECT short_id FROM tasks WHERE id = 'written-by-an-old-binary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            healed.is_some(),
            "a row from a pre-v6 writer must be numbered on the next open, or it \
             stays permanently unaddressable as a T-number"
        );
    }

    /// v9 has to migrate tasks that are **already in the Trash**, and the
    /// batches they are sitting in have to survive it byte for byte — a
    /// migration that re-cut them would mean a parent and its children come back
    /// separately, or two unrelated deletions come back together, on the one
    /// upgrade where nobody would think to check.
    ///
    /// Starts from a **fully-stamped v8** database, Codex columns and all, which
    /// is not an arbitrary choice: that is exactly the state the real database
    /// was found in, and the state that makes folding these columns into v8
    /// silently useless (`version < 8` never fires again, so they never arrive).
    #[test]
    fn v9_backfills_batches_from_the_stamps_already_in_the_trash() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        create_tables_v1(&conn).unwrap();
        create_tables_v2(&conn).unwrap();
        create_tables_v3(&conn).unwrap();
        create_tables_v4(&conn).unwrap();
        create_tables_v5(&conn).unwrap();
        // Before v6: its T-number counter seed writes to `app_meta`, which is
        // created lazily by `get_schema_version` rather than by v1.
        get_schema_version(&conn).unwrap();
        create_tables_v6(&conn).unwrap();
        create_tables_v7(&conn).unwrap();
        create_tables_v8(&conn).unwrap();
        set_schema_version(&conn, 8).unwrap();

        // A v5-style batch: parent + child on one stamp, plus an unrelated
        // deletion on another.
        for (id, parent, stamp) in [
            ("parent", None, "2026-07-01 09:00:00.100"),
            ("child", Some("parent"), "2026-07-01 09:00:00.100"),
            ("other", None, "2026-07-02 09:00:00.200"),
        ] {
            conn.execute(
                "INSERT INTO tasks (id, title, parent_id, deleted_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, format!("task {id}"), parent, stamp],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO tasks (id, title, archived_at)
             VALUES ('shelved', 'set aside', '2026-06-01 08:00:00.000')",
            [],
        )
        .unwrap();

        initialize(&conn).expect("migrate v8 → v9");

        let batch = |id: &str| -> Option<String> {
            conn.query_row(
                "SELECT delete_batch FROM tasks WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            batch("parent"),
            batch("child"),
            "a batch that existed before the upgrade must survive it intact"
        );
        assert_ne!(
            batch("parent"),
            batch("other"),
            "two deletions that were separate must stay separate"
        );
        assert_eq!(
            conn.query_row(
                "SELECT archive_batch FROM tasks WHERE id = 'shelved'",
                [],
                |r| r.get::<_, Option<String>>(0)
            )
            .unwrap()
            .as_deref(),
            Some("2026-06-01 08:00:00.000"),
            "the Archive gets the same treatment as the Trash"
        );

        // Restoring through the real path still brings back exactly the batch.
        crate::db::tasks::restore(&conn, "parent").unwrap();
        for id in ["parent", "child"] {
            assert!(crate::db::tasks::get(&conn, id).unwrap().unwrap().deleted_at.is_none());
        }
        assert!(
            crate::db::tasks::get(&conn, "other").unwrap().unwrap().deleted_at.is_some(),
            "the unrelated deletion came back with it"
        );
    }

    /// A destroyed task's number must not be handed to the next task. Reuse
    /// would silently re-point a reference already written into a commit
    /// message or a comment.
    #[test]
    fn v6_short_ids_survive_hard_delete_without_reuse() {
        let conn = test_conn();
        let first = crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask {
                title: "doomed".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let taken = first.short_id.unwrap();
        crate::db::tasks::delete(&conn, &first.id).unwrap();

        let second = crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask {
                title: "next".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            second.short_id,
            Some(taken + 1),
            "a destroyed task's T-number must never be reissued"
        );

        // And the heal must not close the gap on a later open either.
        initialize(&conn).unwrap();
        assert_eq!(
            crate::db::tasks::get(&conn, &second.id).unwrap().unwrap().short_id,
            Some(taken + 1)
        );
    }

    /// A genuine v7 database — the state a pre-Codex install is actually in.
    fn v7_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        create_tables_v1(&conn).unwrap();
        create_tables_v2(&conn).unwrap();
        create_tables_v3(&conn).unwrap();
        create_tables_v4(&conn).unwrap();
        create_tables_v5(&conn).unwrap();
        // Before v6: that migration seeds the T-number counter into `app_meta`,
        // which `get_schema_version` is what actually creates.
        get_schema_version(&conn).unwrap();
        create_tables_v6(&conn).unwrap();
        create_tables_v7(&conn).unwrap();
        set_schema_version(&conn, 7).unwrap();
        conn
    }

    #[test]
    fn v8_columns_exist_after_migration() {
        let conn = test_conn();
        for (table, column) in [
            ("tasks", "bg_executor"),
            ("tasks", "bg_codex_allowed"),
            ("task_runs", "executor"),
        ] {
            let found: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
                    rusqlite::params![column],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(found, 1, "{table}.{column} missing after migration");
        }
    }

    /// Existing work must keep running exactly as it did. A v7 task that was
    /// opted into background runs stays opted in, under Claude — the migration
    /// is not allowed to quietly re-point it or switch it off.
    #[test]
    fn v8_leaves_existing_tasks_on_claude_and_still_enabled() {
        let conn = v7_conn();
        conn.execute(
            "INSERT INTO tasks (id, title, repo_path, bg_allowed, bg_writes_allowed)
             VALUES ('old', 'existing agent task', '/tmp', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_runs (id, task_id, mode, status)
             VALUES ('r-old', 'old', 'background', 'succeeded')",
            [],
        )
        .unwrap();

        initialize(&conn).expect("migrate v7 → v8");

        let t = crate::db::tasks::get(&conn, "old").unwrap().unwrap();
        assert_eq!(t.bg_executor, crate::executor::BackgroundExecutor::Claude);
        assert!(t.bg_allowed, "an existing background task must stay enabled");
        assert!(t.bg_writes_allowed);

        let run = crate::db::runs::get(&conn, "r-old").unwrap().unwrap();
        assert_eq!(
            run.executor,
            crate::executor::BackgroundExecutor::Claude,
            "a run that predates Codex is a Claude run — not an unknown one"
        );
    }

    /// v11's backfill seeds the repo mapping from **evidence** — pairs a task
    /// already carries — and refuses to guess where the evidence conflicts.
    ///
    /// Worth a test despite running once: a wrong row here is not a wrong
    /// *display*, it is a wrong answer from the table every future `file_task`
    /// resolves against, and it would land silently on the one database that
    /// matters. The ambiguous path is left unmapped on purpose — unmapped is a
    /// state the UI can show and a human can fix, where a confidently wrong
    /// owner is one nobody thinks to look at.
    #[test]
    fn v11_maps_only_the_repos_the_existing_tasks_agree_on() {
        let conn = test_conn();
        let file = |title: &str, project: &str, repo: &str| {
            crate::db::tasks::create(
                &conn,
                &crate::db::tasks::NewTask {
                    title: title.into(),
                    project: Some(project.into()),
                    repo_path: Some(repo.into()),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        // Two tasks agreeing — and note the folder name is NOT the project name,
        // which is the whole reason this table exists.
        file("a", "pocketbuddy", "/tmp/cx-v11/finance-app");
        file("b", "pocketbuddy", "/tmp/cx-v11/finance-app");
        file("c", "cxventures", "/tmp/cx-v11/cxventures");
        // One path, two different projects: genuinely ambiguous.
        file("d", "one", "/tmp/cx-v11/contested");
        file("e", "another", "/tmp/cx-v11/contested");

        // `test_conn` already ran v11 against an empty DB; re-run it over the
        // rows above, which is exactly what the real upgrade does.
        create_tables_v11(&conn).unwrap();

        assert_eq!(
            crate::db::projects::for_repo_path(&conn, "/tmp/cx-v11/finance-app")
                .unwrap()
                .as_deref(),
            Some("pocketbuddy")
        );
        assert_eq!(
            crate::db::projects::for_repo_path(&conn, "/tmp/cx-v11/cxventures")
                .unwrap()
                .as_deref(),
            Some("cxventures")
        );
        assert_eq!(
            crate::db::projects::for_repo_path(&conn, "/tmp/cx-v11/contested").unwrap(),
            None,
            "a path two projects disagree about must be left for a human, not guessed"
        );
        // And every project that got a repo got exactly one default.
        for p in ["pocketbuddy", "cxventures"] {
            let repos = crate::db::projects::list_repos(&conn, p).unwrap();
            assert_eq!(repos.iter().filter(|r| r.is_primary).count(), 1, "{p}");
        }
    }

    /// v13 hands the tasks filed before the propagation the repo their project
    /// already owned — once, and only into a blank.
    ///
    /// The blank-only half is the part worth pinning. A workspace that was
    /// deliberately cleared is a choice (plenty of a project's tasks are not
    /// code work), so this must never look like a heal that can run again and
    /// overwrite it — which is also why it sits inside the version gate while
    /// `backfill_short_ids` and `heal_legacy_statuses` sit outside.
    #[test]
    fn v13_fills_a_blank_workspace_from_the_projects_primary_repo() {
        let conn = test_conn();
        crate::db::projects::add_repo(&conn, "pocketbuddy", "/tmp/cx-v13/finance-app", true)
            .unwrap();
        crate::db::projects::add_repo(&conn, "pocketbuddy", "/tmp/cx-v13/xcode/pb", false).unwrap();

        // Written straight to SQL: `create` resolves this itself now, so the
        // only way to reproduce a pre-v13 row is to insert one.
        let blank = crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask {
                title: "filed before the propagation".into(),
                project: Some("pocketbuddy".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let unmapped = crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask {
                title: "a project with no repos".into(),
                project: Some("godly".into()),
                ..Default::default()
            },
        )
        .unwrap();
        conn.execute("UPDATE tasks SET repo_path = NULL", []).unwrap();
        let chosen = crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask {
                title: "the iOS half, chosen".into(),
                project: Some("pocketbuddy".into()),
                repo_path: Some("/tmp/cx-v13/xcode/pb".into()),
                ..Default::default()
            },
        )
        .unwrap();

        create_tables_v13(&conn).unwrap();

        let repo = |id: &str| {
            crate::db::tasks::get(&conn, id).unwrap().unwrap().repo_path
        };
        assert_eq!(repo(&blank.id).as_deref(), Some("/tmp/cx-v13/finance-app"));
        assert_eq!(
            repo(&chosen.id).as_deref(),
            Some("/tmp/cx-v13/xcode/pb"),
            "a workspace already set is never rewritten, even to the primary"
        );
        assert!(
            repo(&unmapped.id).is_none(),
            "a project with no repos resolves to nothing, not to a guess"
        );

        // Idempotent in the only sense that matters: re-running cannot undo a
        // subsequent clear, because there is nothing left for it to fill that a
        // human hasn't since emptied on purpose.
        crate::db::tasks::update(
            &conn,
            &blank.id,
            &crate::db::tasks::TaskPatch {
                repo_path: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
        create_tables_v13(&conn).unwrap();
        assert_eq!(
            repo(&blank.id).as_deref(),
            Some("/tmp/cx-v13/finance-app"),
            "a second run refills — which is exactly why this is version-gated \
             and NOT an ongoing heal"
        );
    }

    #[test]
    fn v14_adds_the_execution_trace_to_a_genuine_v13_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        get_schema_version(&conn).unwrap();
        for step in [
            create_tables_v1,
            create_tables_v2,
            create_tables_v3,
            create_tables_v4,
            create_tables_v5,
            create_tables_v6,
            create_tables_v7,
            create_tables_v8,
            create_tables_v9,
            create_tables_v10,
            create_tables_v11,
            create_tables_v12,
            create_tables_v13,
        ] {
            step(&conn).unwrap();
        }
        set_schema_version(&conn, 13).unwrap();

        initialize(&conn).expect("migrate v13 → v14");
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'table' AND name = 'run_events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);
        assert_eq!(get_schema_version(&conn).unwrap(), CURRENT_VERSION);
    }

    /// **The reason `bg_codex_allowed` exists.** Reproduces a leftover pre-v8
    /// runner: its drain query is `bg_allowed = 1` and it has never heard of
    /// `bg_executor`. A queued Codex task must be invisible to it, because the
    /// alternative is that runner claiming the task and executing a
    /// Codex-authored prompt with Claude's tool grants.
    #[test]
    fn a_legacy_runner_query_cannot_claim_a_codex_task() {
        let conn = test_conn();
        let codex = crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask {
                title: "codex work".into(),
                repo_path: Some("/tmp".into()),
                bg_allowed: Some(true),
                bg_executor: Some(crate::executor::BackgroundExecutor::Codex),
                status: Some("queued".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let claude = crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask {
                title: "claude work".into(),
                repo_path: Some("/tmp".into()),
                bg_allowed: Some(true),
                status: Some("queued".into()),
                ..Default::default()
            },
        )
        .unwrap();

        // Verbatim shape of the pre-v8 drain filter.
        let mut stmt = conn
            .prepare("SELECT id FROM tasks WHERE status = 'queued' AND bg_allowed = 1")
            .unwrap();
        let claimable: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert!(
            claimable.contains(&claude.id),
            "a Claude task must still be claimable by an old runner"
        );
        assert!(
            !claimable.contains(&codex.id),
            "a pre-v8 runner claimed a Codex task — it would run it with Claude"
        );
        // …and the new reader still reports it as enabled, so the UI is honest.
        assert!(crate::db::tasks::get(&conn, &codex.id).unwrap().unwrap().bg_allowed);
    }

    /// **The deliberate opposite of the test above, and that is the point.**
    ///
    /// This pins a DECISION rather than catching a bug: intent gets ONE column
    /// where `bg_executor` needed two, because a pre-v16 runner draining a
    /// review task degrades gracefully — same tier, same argv, same checkpoint,
    /// only ~400 characters of English differ, so the failure is "you got work
    /// when you asked for a critique" and the arming survives to try again.
    ///
    /// Read side by side with `a_legacy_runner_query_cannot_claim_a_codex_task`,
    /// the two are what make "one column here, two columns there" legible
    /// instead of looking like an inconsistency to tidy up. **It stops being
    /// true the moment any Rust code derives a `ToolTier`, the repo gate or the
    /// checkpoint decision from `bg_intent`** — see `crate::intent`.
    #[test]
    fn a_legacy_runner_still_claims_a_review_task_and_would_run_it_as_work() {
        let conn = test_conn();
        let review = crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask {
                title: "audit the plan".into(),
                repo_path: Some("/tmp".into()),
                bg_allowed: Some(true),
                status: Some("queued".into()),
                ..Default::default()
            },
        )
        .unwrap();
        crate::db::tasks::update(
            &conn,
            &review.id,
            &crate::db::tasks::TaskPatch {
                bg_intent: Some(crate::intent::BackgroundIntent::Review),
                ..Default::default()
            },
        )
        .unwrap();

        // Verbatim shape of the pre-v16 drain filter — it has never heard of
        // `bg_intent`, and nothing filters on it, which IS the safety argument.
        let mut stmt = conn
            .prepare("SELECT id FROM tasks WHERE status = 'queued' AND bg_allowed = 1")
            .unwrap();
        let claimable: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            claimable.contains(&review.id),
            "an armed review must stay claimable — hiding it from an old runner \
             would be the two-column encoding, which intent deliberately does not use"
        );

        // The tier a pre-v16 runner computes must be the tier a v16 one does.
        // Review preselects read-only in the UI and NOWHERE else; if that ever
        // moves into Rust, this assertion is the thing that fails first.
        let stored = crate::db::tasks::get(&conn, &review.id).unwrap().unwrap();
        let mut as_work = stored.clone();
        as_work.bg_intent = crate::intent::BackgroundIntent::Work;
        assert_eq!(
            crate::runner::ToolTier::for_task(&stored),
            crate::runner::ToolTier::for_task(&as_work),
            "intent must not decide a capability grant"
        );
    }

    /// v16's columns are additive with the safe value as the default, so every
    /// pre-existing task and run reads as `work` with nothing to backfill. A
    /// NULL or wrongly-defaulted column here means every historical run renders
    /// as a critique in the block the next agent trusts most.
    #[test]
    fn v16_leaves_every_existing_task_and_run_on_work() {
        let conn = test_conn();
        let t = crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask { title: "old".into(), ..Default::default() },
        )
        .unwrap();
        assert_eq!(t.bg_intent, crate::intent::BackgroundIntent::Work);

        // A row written the way a pre-v16 binary writes one — naming neither
        // column — must land on `work` rather than NULL.
        conn.execute(
            "INSERT INTO tasks (id, title) VALUES ('legacy16', 'written by an old mcp')",
            [],
        )
        .unwrap();
        let legacy = crate::db::tasks::get(&conn, "legacy16").unwrap().unwrap();
        assert_eq!(legacy.bg_intent, crate::intent::BackgroundIntent::Work);

        conn.execute(
            "INSERT INTO task_runs (id, task_id, mode, status) VALUES ('r16', 'legacy16', 'background', 'succeeded')",
            [],
        )
        .unwrap();
        let run = crate::db::runs::get(&conn, "r16").unwrap().unwrap();
        assert_eq!(
            run.intent,
            crate::intent::BackgroundIntent::Work,
            "a run an old runner recorded genuinely RAN work framing"
        );
    }

    /// The other half of the skew: a pre-v8 *writer* omits both new columns.
    /// Unlike `short_id` there is nothing to heal, because the column default
    /// and the safe value are the same — the row lands as an ordinary Claude
    /// task at the most restrictive Codex setting.
    #[test]
    fn a_pre_v8_writer_row_is_a_plain_claude_task() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO tasks (id, title, bg_allowed) VALUES ('legacy', 'written by an old mcp', 1)",
            [],
        )
        .unwrap();
        let t = crate::db::tasks::get(&conn, "legacy").unwrap().unwrap();
        assert_eq!(t.bg_executor, crate::executor::BackgroundExecutor::Claude);
        assert!(t.bg_allowed);
    }

    #[test]
    fn v8_is_idempotent() {
        let conn = v7_conn();
        initialize(&conn).expect("migrate v7 → v8");
        initialize(&conn).expect("re-init");
        assert_eq!(get_schema_version(&conn).unwrap(), CURRENT_VERSION);
    }

    #[test]
    fn v3_tables_exist_after_migration() {
        let conn = test_conn();
        for table in ["tags", "task_tags", "task_deps", "task_comments"] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    rusqlite::params![table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "{table} missing after migration");
        }
    }

    #[test]
    fn v10_attachment_table_exists_after_migration() {
        let conn = test_conn();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                  WHERE type='table' AND name='task_attachments'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "task_attachments missing after migration");
    }

    #[test]
    fn self_blocking_is_rejected_by_the_table_constraint() {
        let conn = test_conn();
        conn.execute("INSERT INTO tasks (id, title) VALUES ('a','a')", [])
            .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO task_deps (task_id, blocked_by_id) VALUES ('a','a')",
                []
            )
            .is_err(),
            "the CHECK should stop a task blocking itself even via raw SQL"
        );
    }

    /// The outbox sequence must never be REUSED.
    ///
    /// `INTEGER PRIMARY KEY` alone lets SQLite reuse the rowid of a deleted max
    /// row. The drain deletes what it sent, so without AUTOINCREMENT the very
    /// next append would re-issue a sequence number that has already been
    /// pushed — and since the drain orders by that number, two different
    /// intents would replay in the wrong order with nothing to notice.
    ///
    /// This is a one-word difference in the DDL and completely invisible until
    /// an outbox has been drained at least once, which no early test does.
    #[test]
    fn a_drained_outbox_never_reissues_a_sequence_number() {
        let conn = test_conn();
        let insert = "INSERT INTO sync_outbox (cmd, args_json, host) VALUES ('update_task', '{}', 'h')";

        conn.execute(insert, []).unwrap();
        conn.execute(insert, []).unwrap();
        let high: i64 = conn
            .query_row("SELECT MAX(local_seq) FROM sync_outbox", [], |r| r.get(0))
            .unwrap();

        // Drain: everything sent, everything deleted.
        conn.execute("DELETE FROM sync_outbox", []).unwrap();
        conn.execute(insert, []).unwrap();

        let next: i64 = conn
            .query_row("SELECT MAX(local_seq) FROM sync_outbox", [], |r| r.get(0))
            .unwrap();
        assert!(
            next > high,
            "local_seq {next} was reused after a drain (previous high {high}) — \
             the outbox needs AUTOINCREMENT"
        );
    }

    /// The lease ledger cannot record the same start twice.
    ///
    /// The stronger guarantee — that ranges never OVERLAP — belongs to the
    /// allocator, which always carves above `MAX(hi)`. This is the cheap
    /// structural half: two rows claiming the same `lo` is a bug that the
    /// database itself can refuse.
    #[test]
    fn two_leases_cannot_share_a_start() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO short_id_leases (lo, hi, host) VALUES (100, 599, 'mac')",
            [],
        )
        .unwrap();
        let clash = conn.execute(
            "INSERT INTO short_id_leases (lo, hi, host) VALUES (100, 199, 'nas')",
            [],
        );
        assert!(clash.is_err(), "a second lease starting at 100 must be refused");
    }

    /// A v20 database gains the sync tables and loses nothing.
    #[test]
    fn migrating_a_v20_database_to_v21_adds_the_tables_and_keeps_the_rows() {
        let conn = test_conn();
        crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask { title: "survivor".into(), ..Default::default() },
        )
        .unwrap();
        // Pretend this DB predates v21.
        set_schema_version(&conn, 20).unwrap();
        conn.execute_batch("DROP TABLE sync_outbox; DROP TABLE short_id_leases;").unwrap();

        initialize(&conn).expect("v20 -> v21");

        assert_eq!(get_schema_version(&conn).unwrap(), CURRENT_VERSION);
        for table in ["sync_outbox", "short_id_leases"] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "{table} missing after migration");
        }
        let kept: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE title = 'survivor'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 1, "the migration must not touch existing rows");
    }

    #[test]
    fn an_older_binary_cannot_stamp_the_version_backwards() {
        let conn = test_conn();
        // Simulate a future binary having migrated this DB.
        set_schema_version(&conn, CURRENT_VERSION + 5).unwrap();
        initialize(&conn).expect("initialize against a newer DB");
        assert_eq!(
            get_schema_version(&conn).unwrap(),
            CURRENT_VERSION + 5,
            "an older binary must not downgrade the schema stamp"
        );
    }

    #[test]
    fn initialize_is_idempotent() {
        let conn = test_conn();
        initialize(&conn).expect("second initialize");
        assert_eq!(get_schema_version(&conn).unwrap(), CURRENT_VERSION);
    }

    #[test]
    fn execution_columns_are_nullable() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO tasks (id, title) VALUES ('t1', 'plain todo')",
            [],
        )
        .expect("plain todo insert");
        let repo: Option<String> = conn
            .query_row("SELECT repo_path FROM tasks WHERE id='t1'", [], |r| r.get(0))
            .unwrap();
        assert!(repo.is_none(), "repo_path must default to NULL");
    }

    #[test]
    fn subtasks_cascade_on_parent_delete() {
        let conn = test_conn();
        conn.execute("INSERT INTO tasks (id, title) VALUES ('p', 'parent')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, title, parent_id) VALUES ('c', 'child', 'p')",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM tasks WHERE id='p'", []).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id='c'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "child task should cascade-delete with its parent");
    }

    #[test]
    fn runs_cascade_on_task_delete() {
        let conn = test_conn();
        conn.execute("INSERT INTO tasks (id, title) VALUES ('t', 'x')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO task_runs (id, task_id, mode, status) VALUES ('r','t','foreground','running')",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM tasks WHERE id='t'", []).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn fts_indexes_on_insert_and_evicts_on_delete() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO tasks (id, title, body, project) VALUES ('t','fix the webhook retry','backoff logic','cxventures')",
            [],
        )
        .unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks_fts WHERE tasks_fts MATCH 'webhook'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);

        conn.execute("DELETE FROM tasks WHERE id='t'", []).unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks_fts WHERE tasks_fts MATCH 'webhook'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 0, "delete must evict the FTS row immediately");
    }

    #[test]
    fn fts_update_trigger_is_scoped_to_indexed_columns() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO tasks (id, title) VALUES ('t','ship the runner')",
            [],
        )
        .unwrap();
        // A status flip must not churn the FTS row.
        conn.execute("UPDATE tasks SET status='done' WHERE id='t'", [])
            .unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks_fts WHERE tasks_fts MATCH 'runner'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);

        // A title edit must re-index exactly once (no duplicate rows).
        conn.execute("UPDATE tasks SET title='ship the dispatcher' WHERE id='t'", [])
            .unwrap();
        let old: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks_fts WHERE tasks_fts MATCH 'runner'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let new: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks_fts WHERE tasks_fts MATCH 'dispatcher'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!((old, new), (0, 1));
    }
}
