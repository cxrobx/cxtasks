//! Task CRUD + the view queries the sidebar is built from.
//!
//! **Timestamp discipline (cxmail gotcha #23).** Timestamps are stored as
//! ISO-8601 with a trailing `Z` (what `Date.toISOString()` emits). SQLite
//! compares TEXT columns byte-wise, and `T` (0x54) sorts *after* the space
//! (0x20) that `datetime('now')` uses — so a raw `due_at <= datetime('now')`
//! silently fires up to a day late. Every comparison in this file wraps **both**
//! operands in `datetime()`. Do not "optimize" one of them away to hit an index.

use crate::error::AppError;
use crate::executor::BackgroundExecutor;
use crate::intent::BackgroundIntent;
use crate::tripwire::{self, OnTrip, Tripwire, TripwireState};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde::{Deserialize, Deserializer, Serialize};

/// **P0 is the HIGHEST priority, P3 the lowest** — Meta's convention. This is
/// the opposite of the v1-v2 schema, which stored 0 = none … 3 = highest; the
/// stored values were remapped `new = 3 - old` by the v3 migration. The
/// practical consequence, and the easiest thing to get wrong: every
/// `ORDER BY priority` is **ASC**, and a new task defaults to `P3`, not `P0`.
pub const PRIORITY_HIGHEST: i64 = 0;
pub const PRIORITY_LOWEST: i64 = 3;

/// Every status a task may hold, in lifecycle order.
///
/// **One axis: how far along is this work.** See [`crate::db::schema`]'s v12
/// migration for why the old eight weren't mutually exclusive and what each of
/// these means.
pub const STATUSES: [&str; 6] = [
    "inbox",
    "todo",
    "queued",
    "in_progress",
    "in_review",
    "done",
];

/// The subset a human is offered — [`STATUSES`] minus the machine-owned ones.
///
/// `queued` is set by "Run in Background" and cleared by the runner claiming the
/// task; hand-setting it would put a task in the drain queue without any of the
/// checks [`crate::commands::dispatch`] runs first. It stays a real status
/// because it is the runner's atomic claim key (invariant 15) — it is just not a
/// thing you pick.
pub const USER_STATUSES: [&str; 5] = [
    "inbox",
    "todo",
    "in_progress",
    "in_review",
    "done",
];

/// Statuses that mean "no longer on the board".
///
/// One member since v12. Dropping a task is *archiving* it, not a second closed
/// status — see the v12 migration.
pub const CLOSED_STATUSES: [&str; 1] = ["done"];

/// The status a task should hold, or `None` if the string names nothing.
///
/// Accepts the pre-v12 spellings and maps them forward. That is not politeness:
/// three binaries share this database and don't upgrade in lockstep, so a
/// leftover `cxtasks-mcp` will go on passing `"next"` after the app speaks
/// `"todo"`. Rejecting it would make `/cxtasks:task` start failing on a
/// vocabulary that was correct when that binary was built.
///
/// `cancelled` is deliberately NOT mapped. There is no honest target — `done`
/// would claim the work finished and `todo` would silently put it back on the
/// board — so the caller is told to archive instead. Rows written by a stale
/// binary are cleaned up by [`heal_legacy_statuses`].
pub fn normalize_status(raw: &str) -> Option<&'static str> {
    let s = raw.trim();
    if let Some(found) = STATUSES.iter().find(|k| k.eq_ignore_ascii_case(s)) {
        return Some(found);
    }
    match s.to_ascii_lowercase().as_str() {
        "next" => Some("todo"),
        "needs_review" | "review" => Some("in_review"),
        // v27: `blocked` is a HOLD (`blocked_reason`), not a status. A pre-v27
        // client — the iOS app, an old plugin copy — still sends it alongside
        // a reason; the reason is what it meant, and the status underneath is
        // open work.
        "blocked" => Some("todo"),
        _ => None,
    }
}

pub fn is_valid_status(s: &str) -> bool {
    normalize_status(s).is_some()
}

/// The error a bad status produces, in one place so every caller says the same
/// thing — including the bit users actually need, which is where `cancelled`
/// went.
fn invalid_status(status: &str) -> AppError {
    if status.trim().eq_ignore_ascii_case("cancelled")
        || status.trim().eq_ignore_ascii_case("canceled")
    {
        return AppError::Validation(
            "'cancelled' is no longer a status — archive the task instead".into(),
        );
    }
    AppError::Validation(format!(
        "unknown status '{status}' (expected one of {})",
        STATUSES.join(", ")
    ))
}

/// Convert any pre-v12 status left behind by a stale binary.
///
/// Runs on **every** `initialize`, outside the version gates, for the same
/// reason [`backfill_short_ids`] does: the v12 remap and this heal are one code
/// path, and a row a leftover `cxtasks-mcp` writes tomorrow needs it as much as
/// a row that predates the migration. Converges even while a stale writer is
/// still running — it writes `'next'`, the next `initialize` moves it to
/// `'todo'`, and nothing loops because the heal never writes a legacy value.
pub fn heal_legacy_statuses(conn: &Connection) -> Result<(), AppError> {
    // Same order and effect as the v12 migration: archive the cancelled, then
    // rename. Kept in step with it deliberately — a divergence here would mean a
    // row's fate depended on which binary happened to open the DB first.
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
        -- v27: a pre-v27 writer's `blocked` keeps its reason (that binary
        -- required one), so the hold survives and only the axis is corrected.
        UPDATE tasks SET status = 'todo'      WHERE status = 'blocked';
        "#,
    )?;
    Ok(())
}

/// Days a trashed task survives before [`purge_expired`] destroys it for good.
pub const RETENTION_DAYS: i64 = 30;

/// **The board.** Not trashed, not archived — every query that hands a task to
/// a human or to an agent must carry this.
///
/// It exists as one shared constant rather than fifteen hand-written clauses
/// because that is precisely the failure mode this feature dies of: miss a
/// single call site and a "deleted" task still fires a reminder, still gets
/// counted in a sidebar badge, or — worst — still gets dispatched to an
/// unattended background run. Grep for `LIVE` to audit the set.
pub const LIVE: &str = "deleted_at IS NULL AND archived_at IS NULL";

/// **Everything the user still considers real**, archived included.
///
/// Used only by search: archiving is supposed to keep a task retrievable while
/// getting it off the board, and an archive you can't search is a black hole you
/// can only browse. Trashed tasks are excluded here as everywhere else.
pub const NOT_DELETED: &str = "deleted_at IS NULL";

/// One of the two hidden states. Both work identically and differ only in the
/// pair of columns they touch, so the batch mechanics live here once rather
/// than being written twice and drifting apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hidden {
    Trashed,
    Archived,
}

impl Hidden {
    /// The human-facing timestamp: **when** it went. Compared by
    /// [`purge_expired`] and [`days_until_purge`], ordered on by the Trash view.
    fn stamp_col(self) -> &'static str {
        match self {
            Self::Trashed => "deleted_at",
            Self::Archived => "archived_at",
        }
    }

    /// The batch identity: **which** deletion it went in. A UUID, so two
    /// deletions can never share one however close together they land.
    fn batch_col(self) -> &'static str {
        match self {
            Self::Trashed => "delete_batch",
            Self::Archived => "archive_batch",
        }
    }

    /// The expression two rows are compared by to decide whether they went
    /// together, qualified with `prefix` (`"t."`, `"p."`, or `""`).
    ///
    /// The `COALESCE` is the binary-skew fallback, and it is why v8 needs no
    /// ongoing backfill: three binaries share this DB and don't upgrade in
    /// lockstep, so a leftover pre-v8 writer goes on stamping `deleted_at` with
    /// a NULL batch. Keying on the batch column alone would put every one of
    /// those rows in the same NULL batch — restoring any one of them would
    /// empty the Trash. Falling back to the timestamp gives them exactly v5's
    /// behaviour instead: correct except for a same-millisecond collision.
    fn batch_key(self, prefix: &str) -> String {
        format!(
            "COALESCE({prefix}{}, {prefix}{})",
            self.batch_col(),
            self.stamp_col()
        )
    }
}

/// The root rows of a trash/archive batch — a task whose parent did **not** go
/// into the same batch. [`trash`] and [`archive`] stamp a whole subtree with one
/// batch id, so without this the Trash view lists a deleted parent *and* each
/// of its children as separate, individually-restorable entries, which reads as
/// "deleting one task deleted five things".
pub(crate) fn batch_roots(hidden: Hidden) -> String {
    let (parent, own) = (hidden.batch_key("p."), hidden.batch_key("tasks."));
    format!(
        "NOT EXISTS (SELECT 1 FROM tasks p
                      WHERE p.id = tasks.parent_id AND {parent} = {own})"
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub body: Option<String>,
    pub status: String,
    pub priority: i64,
    pub project: Option<String>,
    pub due_at: Option<String>,
    pub defer_until: Option<String>,
    pub recurrence: Option<String>,
    pub parent_id: Option<String>,
    pub sort_order: i64,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub repo_path: Option<String>,
    pub prompt: Option<String>,
    /// **Computed, not a column.** True when this task is opted into unattended
    /// runs *under the executor it names*.
    ///
    /// The flag is stored in a per-executor column (`bg_allowed` for Claude,
    /// `bg_codex_allowed` for Codex) as a guard against a leftover pre-v8
    /// runner claiming a Codex task and running it with Claude — see
    /// [`crate::executor`]. Everything downstream of this struct sees one
    /// boolean and never has to know that.
    pub bg_allowed: bool,
    /// Which background agent this task is dispatched to. Foreground dispatch
    /// is always Claude and ignores this.
    pub bg_executor: BackgroundExecutor,
    pub bg_writes_allowed: bool,
    pub reminded_at: Option<String>,
    /// Overrides the global background model for this task. NULL = use it.
    pub model: Option<String>,
    /// Grouping within a project or view. NULL = ungrouped.
    pub section_id: Option<String>,
    /// In the Trash since this instant; NULL = not trashed. Doubles as the
    /// batch key tying a subtree's deletion together — see [`trash`].
    pub deleted_at: Option<String>,
    /// Archived at this instant; NULL = not archived. Never auto-purged.
    pub archived_at: Option<String>,
    /// The speakable reference — rendered `T42`. `None` only for a row written
    /// by a pre-v6 binary and not yet healed by [`backfill_short_ids`].
    pub short_id: Option<i64>,
    /// Grant a background run a shell, without granting `Edit`/`Write`.
    ///
    /// Ignored when `bg_writes_allowed` is set, which already implies a shell.
    /// Like writes, this makes the run mutation-capable: it requires a git repo
    /// and is checkpointed first ([`crate::runner::ToolTier`]).
    pub bg_shell_allowed: bool,
    /// May this task run on a host that is not the machine holding its repo?
    ///
    /// Default false, and in a column no pre-v23 binary reads. That is the
    /// point: standing up a remote host makes nothing newly claimable, because
    /// every task that already existed says no. See [`crate::routing`] for the
    /// other half — the host's own capability-generated drain SQL.
    pub bg_remote_allowed: bool,
    /// Comma-separated MCP server NAMES a detached run of this task may reach,
    /// validated against [`crate::mcp_registry::REGISTRY`].
    ///
    /// Empty or `None` grants nothing beyond the `cxtasks` write-back router,
    /// and that is the design rather than an omission — see the registry's
    /// module docs. Meaningless for a repo-bound run, which is bounded by its
    /// repo.
    pub bg_mcp_servers: Option<String>,
    /// The hand-set HOLD: what this task is waiting on, outside CXTasks.
    /// Non-NULL means held, and that is independent of `status` (v27) — a
    /// task can be `in_progress` and waiting on a key at the same time. A held
    /// task leaves every actionable view and lands in Waiting, exactly like a
    /// defer date or an armed tripwire ([`not_held_sql`]).
    ///
    /// This is the manual/runner reason only. A task blocked by a *dependency*
    /// is derived from `task_deps` (`deps::blocked_task_ids`), so the two never
    /// fight over one column; [`blocked_sql`] unions them for the badge.
    pub blocked_reason: Option<String>,
    /// What the **next** background dispatch of this task is for — do the work,
    /// or audit the last run's handoff.
    ///
    /// A one-shot arming, not a durable preference: [`crate::runner`]'s
    /// `finish_run` resets it to `Work` after a successful run, so nothing may
    /// treat it as user state that persists across runs. Deliberately absent
    /// from [`NewTask`] — a task that has never run has nothing to review.
    pub bg_intent: BackgroundIntent,
    /// Wall clock for a background run of THIS task, in minutes. NULL = use the
    /// global `timeout_minutes`.
    ///
    /// Read through [`crate::runner::timeout_for`], never directly — the
    /// resolved value is also written onto the run row, because
    /// [`crate::db::runs::sweep_abandoned`] has to know which clock a run in
    /// flight was started with (invariant 20).
    pub bg_timeout_minutes: Option<i64>,
    /// Token ceiling for ONE background run of this task. NULL = use the global
    /// `run_token_cap`; `0` disables the ceiling.
    ///
    /// Raised together with the clock in practice: a 60-minute Claude run under
    /// the 25M default dies on tokens around minute 15, so a clock raised alone
    /// buys almost nothing. Bounds the run, never the day — `daily_token_cap`
    /// is untouched by this and stays the outer guard (invariant 6).
    pub bg_run_token_cap: Option<i64>,
    /// The tripwire spec — `script:<name> [arg]` — or NULL for none.
    ///
    /// A CONDITION the task waits for, where `defer_until` is a date. While
    /// `tripwire_state` is `armed` the task is hidden exactly where a deferred
    /// one is (invariant 26). Stored normalized (`Tripwire::to_string`), so the
    /// runner's lease can compare it byte-wise.
    pub tripwire: Option<String>,
    /// Re-check cadence in minutes; NULL = manual (only on "check now").
    pub tripwire_every_minutes: Option<i64>,
    /// `complete` = auto-complete on trip so `blocked_by` releases dependents.
    /// NULL means surface only, which is the default.
    pub tripwire_on_trip: Option<String>,
    /// `armed` | `tripped` | `error`; NULL exactly when `tripwire` is. Only
    /// `armed` hides — a broken check must never keep a task hidden.
    pub tripwire_state: Option<String>,
    /// When the check last came back true. Bookkeeping, one-time.
    pub tripwire_tripped_at: Option<String>,
}

impl Task {
    /// True when the task carries an explicit, non-empty `repo_path`.
    ///
    /// NOT "can this be dispatched" — foreground dispatch works either way,
    /// falling back to the user's home directory when this is false
    /// (`dispatch::resolve_repo_or_home`). This gates project-scoped things
    /// only: the "⎇ has repo" list marker, "Reveal Repo in Finder" (there's
    /// nothing to reveal at a fallback root), and the background Queue action.
    /// Queueing then persists `bg_allowed` as the runner's durable permission
    /// gate; the button itself is the user's opt-in.
    pub fn is_dispatchable(&self) -> bool {
        self.repo_path
            .as_deref()
            .map(|p| !p.trim().is_empty())
            .unwrap_or(false)
    }

    /// Waiting on a condition: the tripwire is set and still `armed`. This is
    /// the one state that hides the task (invariant 26); `tripped` and `error`
    /// both surface it.
    pub fn tripwire_armed(&self) -> bool {
        self.tripwire.is_some()
            && self.tripwire_state.as_deref().map(TripwireState::from_db)
                == Some(TripwireState::Armed)
    }

    /// A STANDING tripwire (`tripwire_on_trip = 'monitor'`): completing the
    /// task means "handled" and re-arms it, and the runner keeps checking it
    /// while it is done. See [`OnTrip::Monitor`].
    pub fn is_monitor(&self) -> bool {
        self.tripwire.is_some()
            && OnTrip::from_db(self.tripwire_on_trip.as_deref()) == OnTrip::Monitor
    }

    /// The prompt actually sent to Claude: explicit `prompt`, else title + body.
    pub fn effective_prompt(&self) -> String {
        if let Some(p) = self.prompt.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            return p.to_string();
        }
        match self.body.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
            Some(body) => format!("{}\n\n{}", self.title, body),
            None => self.title.clone(),
        }
    }
}

/// **Index-positional, paired with [`row_to_task`].** A new column goes on the
/// END of both lists — inserting one in the middle silently shifts every
/// `row.get(n)` after it onto the wrong field.
pub(crate) const COLUMNS: &str = "id, title, body, status, priority, project, due_at, defer_until, \
     recurrence, parent_id, sort_order, created_at, updated_at, completed_at, \
     repo_path, prompt, bg_allowed, bg_writes_allowed, reminded_at, model, section_id, \
     deleted_at, archived_at, short_id, bg_shell_allowed, bg_executor, bg_codex_allowed, \
     blocked_reason, bg_intent, bg_timeout_minutes, bg_run_token_cap, bg_remote_allowed, \
     bg_mcp_servers, tripwire, tripwire_every_minutes, tripwire_on_trip, tripwire_state, \
     tripwire_tripped_at";

pub(crate) fn row_to_task(row: &Row) -> rusqlite::Result<Task> {
    // Index 16 is the RAW `bg_allowed` column — Claude's gate alone — and index
    // 26 is Codex's. `Task.bg_allowed` is the one that matters to callers, so it
    // is folded here rather than at every read site.
    let bg_executor = BackgroundExecutor::from_db(&row.get::<_, String>(25)?);
    let bg_allowed = match bg_executor {
        BackgroundExecutor::Claude => row.get::<_, i64>(16)? != 0,
        BackgroundExecutor::Codex => row.get::<_, i64>(26)? != 0,
    };
    Ok(Task {
        id: row.get(0)?,
        title: row.get(1)?,
        body: row.get(2)?,
        status: row.get(3)?,
        priority: row.get(4)?,
        project: row.get(5)?,
        due_at: row.get(6)?,
        defer_until: row.get(7)?,
        recurrence: row.get(8)?,
        parent_id: row.get(9)?,
        sort_order: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        completed_at: row.get(13)?,
        repo_path: row.get(14)?,
        prompt: row.get(15)?,
        bg_allowed,
        bg_executor,
        bg_writes_allowed: row.get::<_, i64>(17)? != 0,
        reminded_at: row.get(18)?,
        model: row.get(19)?,
        section_id: row.get(20)?,
        deleted_at: row.get(21)?,
        archived_at: row.get(22)?,
        short_id: row.get(23)?,
        bg_shell_allowed: row.get::<_, i64>(24)? != 0,
        blocked_reason: row.get(27)?,
        bg_intent: BackgroundIntent::from_db(&row.get::<_, String>(28)?),
        bg_timeout_minutes: row.get(29)?,
        bg_run_token_cap: row.get(30)?,
        bg_remote_allowed: row.get::<_, i64>(31)? != 0,
        bg_mcp_servers: row.get(32)?,
        tripwire: row.get(33)?,
        tripwire_every_minutes: row.get(34)?,
        tripwire_on_trip: row.get(35)?,
        tripwire_state: row.get(36)?,
        tripwire_tripped_at: row.get(37)?,
    })
}

// ── T-numbers ─────────────────────────────────────────────────────────────

/// `T42` or `t42` → `Some(42)`.
///
/// Everything else is `None` so a caller can fall through to a UUID lookup: a
/// UUID, a bare `42` (ambiguous — it could be a search term), a lone `T`, or
/// `T4x`. Hand-rolled rather than a regex because this is the whole grammar and
/// it isn't worth a dependency.
pub fn parse_short_ref(raw: &str) -> Option<i64> {
    let s = raw.trim();
    let digits = s.strip_prefix('T').or_else(|| s.strip_prefix('t'))?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

pub fn get_by_short_id(conn: &Connection, short_id: i64) -> Result<Option<Task>, AppError> {
    let sql = format!("SELECT {COLUMNS} FROM tasks WHERE short_id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![short_id], row_to_task)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Look a task up by **either** form of reference: `T42` or a raw UUID.
///
/// Unfiltered, exactly like [`get`] — restore has to be able to fetch a trashed
/// task, and "T42" printed in a comment last month should still resolve to
/// something after the task is archived.
pub fn resolve_ref(conn: &Connection, raw: &str) -> Result<Option<Task>, AppError> {
    match parse_short_ref(raw) {
        Some(n) => get_by_short_id(conn, n),
        None => get(conn, raw.trim()),
    }
}

/// Give a T-number to every task that lacks one, oldest first.
///
/// Called from `schema::initialize` on **every** open, not from inside the v6
/// gate — see the comment there. Two properties are load-bearing:
///
/// * the order is `datetime(created_at), rowid`, so the numbers a backfill hands
///   out read in the order the tasks were actually captured rather than in
///   SQLite's storage order;
/// * the UPDATE is scoped to `short_id IS NULL`, so this can never renumber a
///   row that already has one. A T-number that changes is worse than no
///   T-number: every reference written down before the change now points
///   somewhere else.
pub fn backfill_short_ids(conn: &Connection) -> Result<usize, AppError> {
    let pending: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM tasks WHERE short_id IS NULL
              ORDER BY datetime(created_at) ASC, rowid ASC",
        )?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut assigned = 0usize;
    for id in &pending {
        // Out of lease: stop, rather than asking again for every remaining row.
        // This runs on every `initialize`, so a host that has exhausted its
        // lease would otherwise re-ask once per unnumbered task on every single
        // process start.
        let Some(short) = crate::db::meta::claim_short_id(conn)? else { break };
        assigned += conn.execute(
            "UPDATE tasks SET short_id = ?1 WHERE id = ?2 AND short_id IS NULL",
            params![short, id],
        )?;
    }
    if assigned > 0 {
        log::info!("assigned T-numbers to {assigned} task(s)");
    }
    Ok(assigned)
}

// ── create ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct NewTask {
    /// A client-generated id, for a task captured while offline.
    ///
    /// **This is what makes an offline capture idempotent**, and it has to come
    /// from the client. A phone that queues a create and replays it after a
    /// timeout has no way to know whether the first attempt landed; if the
    /// server mints the id, the retry makes a duplicate and nothing can tell
    /// the two apart afterwards. With the id supplied, a repeat create finds
    /// the existing row and returns it.
    ///
    /// `None` means "you pick" — every existing caller.
    #[serde(default)]
    pub id: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub status: Option<String>,
    pub priority: Option<i64>,
    pub project: Option<String>,
    pub due_at: Option<String>,
    pub defer_until: Option<String>,
    pub recurrence: Option<String>,
    pub parent_id: Option<String>,
    pub repo_path: Option<String>,
    pub prompt: Option<String>,
    pub bg_allowed: Option<bool>,
    /// Defaults to Claude, so every existing caller keeps its behaviour.
    pub bg_executor: Option<BackgroundExecutor>,
    pub bg_writes_allowed: Option<bool>,
    pub bg_shell_allowed: Option<bool>,
    pub bg_remote_allowed: Option<bool>,
    pub bg_mcp_servers: Option<String>,
    pub model: Option<String>,
    pub section_id: Option<String>,
    /// Required when `status` is `blocked`, rejected otherwise.
    pub blocked_reason: Option<String>,
    /// Per-task wall clock in minutes; None = use the global setting.
    pub bg_timeout_minutes: Option<i64>,
    /// Per-task ceiling on ONE run's tokens; None = use the global setting.
    pub bg_run_token_cap: Option<i64>,
    /// `script:<name> [arg]`. Filed ARMED — the task is hidden until the check
    /// trips. Validated by grammar here; whether the script exists on this
    /// host is the MCP's question, because a synced row may name a script only
    /// one machine has.
    pub tripwire: Option<String>,
    /// Cadence in minutes; None = manual. Requires `tripwire`.
    pub tripwire_every_minutes: Option<i64>,
    /// `surface` (default) or `complete`. Requires `tripwire`.
    pub tripwire_on_trip: Option<String>,
}

/// The tripwire fields of a create or an update, resolved TOGETHER.
///
/// Returned as the values to store, so `create` and `update` write the same
/// normalized spec and the same refusals — a floor enforced here is enforced
/// for the app, the MCP and the server at once.
struct TripwireColumns {
    spec: String,
    every: Option<i64>,
    /// `Some("complete")`, `Some("monitor")` or `None` — surface is the
    /// absence, so "no policy" has ONE representation.
    on_trip: Option<String>,
}

fn resolve_tripwire(
    spec: Option<&str>,
    every: Option<i64>,
    on_trip: Option<&str>,
) -> Result<Option<TripwireColumns>, AppError> {
    let spec = spec.map(str::trim).filter(|s| !s.is_empty());
    let on_trip = on_trip.map(str::trim).filter(|s| !s.is_empty());
    let Some(raw) = spec else {
        if every.is_some() || on_trip.is_some() {
            return Err(AppError::Validation(
                "this task has no tripwire — set `tripwire` (e.g. script:<name>) before a \
                 cadence or an on-trip policy"
                    .into(),
            ));
        }
        return Ok(None);
    };
    let parsed = Tripwire::parse(raw).map_err(AppError::Validation)?;
    tripwire::validate_cadence(parsed.kind(), every).map_err(AppError::Validation)?;
    let policy = OnTrip::parse(on_trip.unwrap_or("")).map_err(AppError::Validation)?;
    Ok(Some(TripwireColumns {
        spec: parsed.to_string(),
        every,
        on_trip: match policy {
            OnTrip::Surface => None,
            other => Some(other.as_str().to_string()),
        },
    }))
}

/// A monitor re-arms itself when it is completed, so it can't also repeat:
/// completing a recurring task materializes a NEW instance, and that instance
/// carries no tripwire (one-shot across recurrence, invariant 26). The check
/// would end on the first completion — the exact failure a monitor exists to
/// prevent. Refused at write time, on create and update alike.
fn refuse_recurring_monitor(on_trip: Option<&str>, recurrence: Option<&str>) -> Result<(), AppError> {
    let monitor = on_trip.is_some() && OnTrip::from_db(on_trip) == OnTrip::Monitor;
    let repeats = recurrence.map(str::trim).is_some_and(|r| !r.is_empty());
    if monitor && repeats {
        return Err(AppError::Validation(
            "a monitor tripwire can't also repeat — completing a monitor already re-arms it; \
             drop the recurrence, or use tripwire_on_trip 'surface'"
                .into(),
        ));
    }
    Ok(())
}

/// Clamp a per-task run budget to the same bounds the global settings use, or
/// reject it as a named error.
///
/// Silently clamping is right for a *stored setting* the user can see and
/// re-edit; it is wrong here, because the caller is usually an agent that will
/// never look again. "60000 minutes was quietly stored as 120" reads exactly
/// like it worked. So an out-of-range value is refused and says the range.
fn validate_run_budget(
    timeout_minutes: Option<i64>,
    run_token_cap: Option<i64>,
) -> Result<(), AppError> {
    use crate::db::meta;
    if let Some(t) = timeout_minutes {
        if !(meta::MIN_TIMEOUT_MINUTES..=meta::MAX_TIMEOUT_MINUTES).contains(&t) {
            return Err(AppError::Validation(format!(
                "bg_timeout_minutes must be between {} and {} (got {t})",
                meta::MIN_TIMEOUT_MINUTES,
                meta::MAX_TIMEOUT_MINUTES
            )));
        }
    }
    if let Some(c) = run_token_cap {
        if !(meta::MIN_RUN_TOKEN_CAP..=meta::MAX_RUN_TOKEN_CAP).contains(&c) {
            return Err(AppError::Validation(format!(
                "bg_run_token_cap must be between {} and {} (got {c}); 0 disables the ceiling",
                meta::MIN_RUN_TOKEN_CAP,
                meta::MAX_RUN_TOKEN_CAP
            )));
        }
    }
    Ok(())
}

pub fn create(conn: &Connection, new: &NewTask) -> Result<Task, AppError> {
    let title = new.title.trim();
    if title.is_empty() {
        return Err(AppError::Validation("title must not be empty".into()));
    }
    let raw_status = new.status.as_deref().unwrap_or("inbox");
    let status = normalize_status(raw_status).ok_or_else(|| invalid_status(raw_status))?;
    validate_run_budget(new.bg_timeout_minutes, new.bg_run_token_cap)?;
    let blocked_reason = new
        .blocked_reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // A hold is written or it isn't; a closed task holds nothing.
    let blocked_reason = if CLOSED_STATUSES.contains(&status) { None } else { blocked_reason };
    // Same joint resolution as `update`, so the two cannot disagree about a
    // floor or a spelling. A task filed WITH a tripwire is filed armed.
    let tripwire_cols = resolve_tripwire(
        new.tripwire.as_deref(),
        new.tripwire_every_minutes,
        new.tripwire_on_trip.as_deref(),
    )?;
    refuse_recurring_monitor(
        tripwire_cols.as_ref().and_then(|t| t.on_trip.as_deref()),
        new.recurrence.as_deref(),
    )?;
    // Default to the LOWEST priority. With P0 = highest, defaulting to 0 would
    // silently mark every captured thought as drop-everything urgent.
    let priority = new
        .priority
        .unwrap_or(PRIORITY_LOWEST)
        .clamp(PRIORITY_HIGHEST, PRIORITY_LOWEST);
    // A client-supplied id makes this idempotent: if that row already exists,
    // the caller is retrying a create whose response it never saw, and the
    // right answer is the row it already made — not a second one. Returning
    // early also skips the project/repo side effects, which have already run.
    if let Some(supplied) = new.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(existing) = get(conn, supplied)? {
            log::info!("create_task replayed for {supplied}; returning the existing row");
            return Ok(existing);
        }
    }
    let id = new
        .id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let project = new.project.as_deref().map(str::trim).filter(|s| !s.is_empty());

    // Give the project independent existence the moment a task references it.
    // The v4 migration backfilled `projects` once and nothing maintains it
    // since, so without this a project created by the MCP — every `/task` and
    // every `/breakdown` — has no sidebar entry and no row on the GSD board,
    // while its tasks sit there filed under a name the app doesn't know about.
    if let Some(p) = project {
        crate::db::projects::ensure_exists(conn, p)?;
    }

    // A task filed into a project INHERITS that project's primary repo.
    //
    // Resolution runs both directions (invariant 17), and until now only one of
    // them was wired anywhere durable: path → project happens inside `file_task`,
    // while project → path lived exclusively in the detail pane's project field.
    // So a task that got its project any other way — the quick-add bar's
    // `#pocketbuddy`, an explicit `project` on `file_task`, a subtask, a
    // `/breakdown` phase — was created with a blank workspace even though the
    // mapping to answer it was sitting in `project_repos`. The launch console
    // then reads "Needs repo" for a project whose repo the app knows.
    //
    // Only ever fills a BLANK: an explicit `repo_path` is a deliberate choice
    // (the iOS half of a project is picked on purpose) and outranks the default.
    let explicit_repo = new.repo_path.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let repo_path = match (explicit_repo, project) {
        (None, Some(p)) => crate::db::projects::primary_repo(conn, p)?,
        _ => explicit_repo.map(str::to_string),
    };

    // Claimed BEFORE the insert and outside any transaction. If the insert then
    // fails the number is simply skipped — see `meta::claim_short_id` on why a
    // gap is acceptable and a reuse is not.
    //
    // `None` means this host has burned its whole T-number lease while offline.
    // It binds as NULL, the task is created anyway, and `backfill_short_ids`
    // numbers it once a lease arrives. Refusing to create the task instead
    // would let a lapsed lease block capture, which is a far worse failure than
    // a task that is briefly unaddressable as "T42".
    let short_id = crate::db::meta::claim_short_id(conn)?;

    // New tasks sort to the top of their bucket.
    let sort_order: i64 = conn
        .query_row("SELECT COALESCE(MIN(sort_order), 0) - 1 FROM tasks", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);

    // The enabled flag goes into the column belonging to the chosen executor and
    // 0 into the other, so a pre-v8 runner (which only knows `bg_allowed`) can
    // never see a Codex task as work — see `crate::executor`.
    let bg_executor = new.bg_executor.unwrap_or_default();
    let bg_enabled = new.bg_allowed.unwrap_or(false);
    let bg_claude_gate = bg_enabled && bg_executor == BackgroundExecutor::Claude;
    let bg_codex_gate = bg_enabled && bg_executor == BackgroundExecutor::Codex;

    conn.execute(
        "INSERT INTO tasks (id, title, body, status, priority, project, due_at, defer_until,
                            recurrence, parent_id, sort_order, repo_path, prompt,
                            bg_allowed, bg_writes_allowed, bg_shell_allowed, model, section_id,
                            short_id, bg_executor, bg_codex_allowed, blocked_reason,
                            bg_timeout_minutes, bg_run_token_cap, bg_remote_allowed,
                            bg_mcp_servers, tripwire, tripwire_every_minutes, tripwire_on_trip,
                            tripwire_state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                 ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30)",
        params![
            id,
            title,
            new.body.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            status,
            priority,
            project,
            new.due_at,
            new.defer_until,
            new.recurrence,
            new.parent_id,
            sort_order,
            repo_path,
            new.prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            i64::from(bg_claude_gate),
            i64::from(new.bg_writes_allowed.unwrap_or(false)),
            i64::from(new.bg_shell_allowed.unwrap_or(false)),
            new.model.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            new.section_id.as_deref().filter(|s| !s.is_empty()),
            short_id,
            bg_executor,
            i64::from(bg_codex_gate),
            blocked_reason,
            new.bg_timeout_minutes,
            new.bg_run_token_cap,
            i64::from(new.bg_remote_allowed.unwrap_or(false)),
            new.bg_mcp_servers.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            tripwire_cols.as_ref().map(|t| t.spec.clone()),
            tripwire_cols.as_ref().and_then(|t| t.every),
            tripwire_cols.as_ref().and_then(|t| t.on_trip.clone()),
            tripwire_cols.as_ref().map(|_| TripwireState::Armed.as_str()),
        ],
    )?;
    if tripwire_cols.is_some() {
        crate::db::tripwires::ensure_runtime(conn, &id)?;
    }

    get(conn, &id)?.ok_or_else(|| AppError::NotFound("task vanished after insert".into()))
}

// ── read ──────────────────────────────────────────────────────────────────

pub fn get(conn: &Connection, id: &str) -> Result<Option<Task>, AppError> {
    let sql = format!("SELECT {COLUMNS} FROM tasks WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![id], row_to_task)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Subtasks of `parent_id`, **matching the parent's own trash/archive state**.
///
/// A live parent shows only live children. A parent sitting in the Trash shows
/// the children that went in with it — which is what makes the detail pane an
/// honest preview of what restoring will bring back. Filtering these to `LIVE`
/// instead would render every trashed parent as childless and make a restore
/// look like it resurrected work from nowhere.
///
/// `IS` rather than `=` because both sides are usually NULL, and `NULL = NULL`
/// is NULL, not true — the null-safe comparison is the whole trick here.
pub fn list_children(conn: &Connection, parent_id: &str) -> Result<Vec<Task>, AppError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM tasks
          WHERE parent_id = ?1
            AND deleted_at  IS (SELECT deleted_at  FROM tasks WHERE id = ?1)
            AND archived_at IS (SELECT archived_at FROM tasks WHERE id = ?1)
          ORDER BY sort_order ASC, created_at ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![parent_id], row_to_task)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The sidebar views. Each is a WHERE fragment over `tasks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    /// **What is left to file**: open work with no home and no date — no
    /// project, no view section, no due date — that is not held, not in
    /// flight and not a decision. Deliberately NOT `status = 'inbox'`.
    ///
    /// Triage is *"give it a home or a date"*, and either one takes a task out.
    /// A home is a project or one of the view-level sections (Personal,
    /// Backlog); a date is a due date. Filed-but-undated work is not lost by
    /// leaving: it is actionable, so Open lists it, and so does its project.
    ///
    /// This used to be "every unscheduled task", with the badge counting only
    /// the project-less subset — two definitions of "triaged" (a date, for the
    /// list; a project, for the badge), so the header read 7 over a badge of 4
    /// and three filed tasks sat in the Inbox forever. One predicate now, and
    /// the badge is its length (see [`view_counts`]).
    ///
    /// `inbox` → `todo` still buys nothing: no view and no query that hands out
    /// work distinguishes the two — see [`next_task`].
    Inbox,
    /// Due (or overdue) by end of the local day, not deferred, not closed.
    Today,
    /// Held back by something that has to HAPPEN first: a start date, an
    /// armed tripwire, an open blocker, or a hand-set `blocked` status. The
    /// question this view answers is "what am I waiting on, and since when" —
    /// which no other view asks, and since Upcoming was retired it is the ONE
    /// "not yet" list. Gate: [`waiting_sql`]. Ordered oldest-first, because
    /// age is the thing to read here.
    ///
    /// The `upcoming` alias is wire compatibility, not a second name: three
    /// binaries, a server and a phone don't upgrade in lockstep, so an
    /// un-updated client asking for the retired view gets these rows instead
    /// of a deserialize error.
    #[serde(alias = "upcoming")]
    Waiting,
    /// Open and actionable RIGHT NOW: not closed and not HELD — nothing has to
    /// happen before it can be started. The sidebar's "Open" view. A future
    /// due date does not disqualify a task: a deadline says when work is due,
    /// not when it may begin, and hiding deadline work until the day it lands
    /// is how a due date turns into a surprise. The whole difference from
    /// [`View::All`] is the hold, which is what [`View::Waiting`] lists.
    Open,
    /// Everything open, held rows included. No sidebar row since the Open
    /// split — this is what a project drill-down and the GSD board query,
    /// because "show me this project" means all of it.
    All,
    /// Work that has been produced and is waiting on a human to accept it —
    /// usually a finished background run, but a human can park a task here too.
    InReview,
    /// Closed. One status since v12: dropping a task archives it.
    Done,
    /// Soft-deleted, still recoverable. Purged after [`RETENTION_DAYS`].
    Trash,
    /// Deliberately set aside. Never purged, always restorable.
    Archive,
}

impl View {
    /// True for the two views that deliberately look *past* the `LIVE` filter.
    /// Every other view is on the board and must never show a hidden task.
    pub(crate) fn is_hidden_view(self) -> bool {
        matches!(self, View::Trash | View::Archive)
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ListFilters {
    pub view: Option<View>,
    pub project: Option<String>,
    pub status: Option<String>,
    /// Narrow to tasks carrying ALL of these tags.
    pub tags: Option<Vec<String>>,
    /// Hide tasks with an open blocker. Off by default — you still want to SEE
    /// blocked work on the board, just not be handed it by `next_task`.
    pub hide_blocked: Option<bool>,
    /// Include subtasks as top-level rows. Default false — they render nested
    /// under their parent in the detail pane.
    pub include_subtasks: Option<bool>,
    pub limit: Option<u32>,
}

/// Local-day upper bound for "today": tomorrow's local midnight, expressed as
/// naive local wall-clock so it compares against `datetime(col,'localtime')`.
pub(crate) const TOMORROW_LOCAL: &str = "datetime(date('now','localtime'), '+1 day')";

/// The DATE half of the waiting predicate: the start date has passed (or was
/// never set). Both operands wrapped per invariant 1. The runner's `list_due`
/// uses this half alone, because a tripwire is not evaluated until its date
/// passes — the date gate comes first.
pub const DEFER_PASSED: &str = "(defer_until IS NULL OR datetime(defer_until) <= datetime('now'))";

/// The CONDITION half: no tripwire, or one that has already tripped or broken.
/// Only `armed` hides (invariant 26) — `tripped` and `error` both surface the
/// task, because a check that cannot run must never keep work out of sight.
pub const TRIPWIRE_NOT_ARMED: &str = "(tripwire_state IS NULL OR tripwire_state <> 'armed')";

/// The tag that marks a task as a DECISION — a question only Chris can
/// answer, not work an agent can do. A convention on `tags`, not a column
/// (invariant 29): it needs no migration, it syncs with the row, an agent can
/// set it with `tag_task`, and removing the tag is the whole undo. What the
/// tag CHANGES is behaviour: Today shows it, Inbox does not, `next_task` and
/// both runner claim paths skip it, and the background queue refuses it.
pub const DECISION_TAG: &str = "decision";

/// Row-side predicate: the task carries [`DECISION_TAG`].
pub fn is_decision_sql() -> String {
    format!(
        "EXISTS(SELECT 1 FROM task_tags cxd_tt JOIN tags cxd_g ON cxd_g.id = cxd_tt.tag_id
                 WHERE cxd_tt.task_id = tasks.id AND cxd_g.name = '{DECISION_TAG}' COLLATE NOCASE)"
    )
}

/// Row-side predicate: the task is NOT a decision. Shared by `next_task` and
/// both runner claim paths, the way `NOT_SUPERSEDED` is, so the three cannot
/// drift on what "agent work" means.
pub fn not_decision_sql() -> String {
    format!("NOT {}", is_decision_sql())
}

/// **Waiting**: held back by something other than a lack of time — a start
/// date, an armed tripwire, an open blocker, or a hand-set `blocked` status.
/// [`View::Waiting`]'s gate and `FilterSpec.waiting`'s, shared so the sidebar
/// row and the MCP facet cannot disagree about what "waiting" means.
///
/// Distinct from [`planned_sql`]: a task due next week is planned, not
/// waiting — nothing has to happen before it can start. The blocker half
/// follows `deps`' rule (invariant 10): a trashed or archived blocker no
/// longer holds anything.
///
/// `COALESCE` on the tripwire state is load-bearing: this predicate is also
/// used NEGATED (`FilterSpec.waiting = false`), and `NULL = 'armed'` is NULL,
/// not false — so `NOT (… OR NULL …)` is NULL and a task with no tripwire
/// would vanish from "not waiting". The test for the false direction found it.
pub fn waiting_sql() -> String {
    format!(
        "({} OR {TRIPWIRE_ARMED} OR {})",
        DEFERRED_SQL,
        blocked_sql()
    )
}

/// **Unfiled**: no project and no view-level section — the half of the
/// Inbox predicate that says "has no home". A project counts whatever its
/// state; a section counts only if it exists and groups a VIEW, because that
/// is the only kind of section a project-less row can be seen under.
pub const UNFILED_SQL: &str = "(COALESCE(TRIM(project), '') = '' \
     AND NOT EXISTS (SELECT 1 FROM sections cxu_s \
                      WHERE cxu_s.id = tasks.section_id AND cxu_s.project IS NULL))";

/// The DATE half of [`waiting_sql`]: a start date that has not arrived. The
/// exact complement of [`DEFER_PASSED`], written out rather than negated so a
/// filter naming this kind reads the same way the view does.
pub const DEFERRED_SQL: &str =
    "(defer_until IS NOT NULL AND datetime(defer_until) > datetime('now'))";

/// The CONDITION half: a tripwire still waiting on its condition. The
/// `COALESCE` is load-bearing for the same reason it always was — this is used
/// NEGATED (`waiting: false`, and now `exclude_waiting`), and `NULL = 'armed'`
/// is NULL, so a task with no tripwire would vanish from "not waiting".
pub const TRIPWIRE_ARMED: &str = "COALESCE(tripwire_state, '') = 'armed'";

/// The BLOCKER half: held by a written reason or by an open dependency. Both,
/// because both draw the row's blocked badge — a "Blocked" filter that matched
/// only one of them would disagree with what is on screen. Follows deps' rule
/// (invariant 10): a trashed or archived blocker holds nothing.
pub fn blocked_sql() -> String {
    format!("(blocked_reason IS NOT NULL OR {})", dep_blocked_sql())
}

/// An open dependency on THIS row, as a correlated subquery. Shared by
/// [`blocked_sql`] and [`not_held_sql`] so the badge and the actionable views
/// cannot disagree about what "another task is in the way" means.
fn dep_blocked_sql() -> String {
    let closed = closed_list();
    format!(
        "EXISTS(SELECT 1 FROM task_deps cxw_d JOIN tasks cxw_b ON cxw_b.id = cxw_d.blocked_by_id
                 WHERE cxw_d.task_id = tasks.id
                   AND cxw_b.status NOT IN ({closed})
                   AND cxw_b.deleted_at IS NULL AND cxw_b.archived_at IS NULL)"
    )
}

/// **Not deferred**: the date has passed AND no tripwire is armed. The literal
/// is the conjunction of [`DEFER_PASSED`] and [`TRIPWIRE_NOT_ARMED`] (a `const`
/// cannot `format!`); `not_deferred_is_the_two_halves` pins that.
///
/// Since v27 this is only HALF of "allowed to be worked" — [`not_held_sql`] is
/// the whole of it. Reach for this one only where a blocker genuinely does not
/// apply (the tripwire lane's own gate).
pub const NOT_DEFERRED: &str = "((defer_until IS NULL OR datetime(defer_until) <= datetime('now')) \
     AND (tripwire_state IS NULL OR tripwire_state <> 'armed'))";

/// **Not held**: the exact complement of [`waiting_sql`], and the one predicate
/// every actionable surface carries — Inbox, Today, Open, `next_task`, the
/// decision strip, the board. No hold of any kind: the date has passed, no
/// tripwire is armed, no reason is written, and nothing open is in the way.
///
/// That last clause is what v27 added. Dependency-blocking used to be subtracted
/// in Rust by `next_task` and the runner alone, so Open and Today still listed
/// a task whose blocker was open and merely painted a badge on it. A hold is a
/// hold: a task you cannot start does not belong in the list of things to start.
pub fn not_held_sql() -> String {
    format!(
        "({NOT_DEFERRED} AND blocked_reason IS NULL AND NOT {})",
        dep_blocked_sql()
    )
}

/// **Actionable right now**: not held. [`View::Open`]'s gate, exported so
/// `projects::board` counts the same thing the sidebar shows — two
/// hand-written copies would let the board and the view disagree about one
/// project, which is exactly the kind of drift that reads as a broken count
/// rather than as two queries.
///
/// It used to carry a date gate too (`not_due_later_sql`, now deleted), which
/// hid every future-dated task from the one list that answers "what could I
/// pick up". A due date is a DEADLINE, not a start date — the thing that says
/// "not yet" is `defer_until`, and `next_task` never honoured the date gate in
/// the first place. Open now agrees with it.
pub fn actionable_sql() -> String {
    not_held_sql()
}

/// **Planned**: deferred into the future, or waiting on an armed tripwire —
/// the board's "planned" count, and the only surviving caller of this
/// predicate now that the `Upcoming` view is gone.
///
/// It used to include "due on a later day", which is what made the retired
/// view overlap Waiting almost exactly while hiding deadline work from Open.
/// What remains is [`waiting_sql`] minus its blocker half — a blocker is
/// something to chase, not something planned. It kept its own name because the
/// board's `planned_count` field is decoded non-optionally by the iOS client.
pub fn planned_sql() -> String {
    format!("({DEFERRED_SQL} OR {TRIPWIRE_ARMED})")
}

pub(crate) fn closed_list() -> String {
    CLOSED_STATUSES
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The WHERE fragments that ARE this view — its identity, and nothing a chip
/// could sensibly remove.
///
/// Deliberately does **not** include the `status NOT IN ('done')` that six of
/// these views used to carry inline. That was never part of what "Today" means;
/// it is a default the caller applies and the filter strip renders as a
/// removable chip (invariant 21). `Done` and `InReview` keep their status
/// predicate, because there the status IS the view.
pub(crate) fn view_where(view: View) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    // Trashed and archived tasks are invisible to every view but their own.
    // This one line is what makes "deleted" mean deleted everywhere else —
    // including `view_counts`, which is implemented on top of `list`.
    if !view.is_hidden_view() {
        parts.push(LIVE.into());
    }
    match view {
        View::Inbox => {
            // The three in-flight statuses are excluded because Today already
            // surfaces them regardless of date (see the hatch below). A task
            // sitting in two views reads as two tasks.
            parts.push("status NOT IN ('in_progress', 'queued', 'in_review')".into());
            // A decision is on Today for the same reason in-flight work is —
            // it needs a human now — so it leaves here for the same reason.
            parts.push(not_decision_sql());
            // Held work — deferred, on a tripwire, blocked — is deliberately out
            // of sight until released; same rule as Today, same `datetime()` on
            // both operands (invariant 1).
            parts.push(not_held_sql());
            // A dated task has been triaged — that is what a date IS — so it
            // belongs to Today or Open, not to the list of things still to
            // think about. A null test, not a comparison, so invariant 1
            // doesn't apply.
            parts.push("due_at IS NULL".into());
            // …and so has a task with a HOME: a project, or one of the
            // view-level sections (`sections.project IS NULL` — Personal,
            // Backlog). Dragging a row into one of those is filing it, so it
            // leaves here the way giving it a project does.
            //
            // The section test goes through `sections` rather than trusting
            // `section_id IS NOT NULL`: a project-less task pointing at a
            // PROJECT's section, or at one sync has not delivered yet, has no
            // home anyone can see, and hiding it from the Inbox would lose it.
            parts.push(UNFILED_SQL.into());
        }
        View::Today => {
            // Not held — deferred, armed, or blocked…
            parts.push(not_held_sql());
            // …and due before tomorrow's local midnight (overdue included), or
            // already actively in flight.
            // …or a decision awaiting Chris (`DECISION_TAG`): undated, but
            // "today" in the sense that matters — nothing else can move it.
            parts.push(format!(
                "((due_at IS NOT NULL AND datetime(due_at,'localtime') < {TOMORROW_LOCAL})
                  OR status IN ('in_progress','queued','in_review')
                  OR {})",
                is_decision_sql()
            ));
        }
        View::Waiting => parts.push(waiting_sql()),
        // Not held. Same shared predicate the board counts with — and
        // deliberately no date gate, so a deadline is visible before the day
        // it falls due.
        View::Open => parts.push(actionable_sql()),
        View::All => {}
        View::InReview => parts.push("status = 'in_review'".into()),
        View::Done => parts.push(format!("status IN ({})", closed_list())),
        // Ordered newest-first below, and listing batch roots only so a deleted
        // parent doesn't appear alongside each of the children it took with it.
        View::Trash => {
            parts.push("deleted_at IS NOT NULL".into());
            parts.push(batch_roots(Hidden::Trashed));
        }
        // Trash wins over Archive for display: something both archived and
        // trashed is *in the Trash*, on the purge clock, and needs to be
        // findable there. Restoring it returns it to the Archive.
        View::Archive => {
            parts.push("archived_at IS NOT NULL AND deleted_at IS NULL".into());
            parts.push(batch_roots(Hidden::Archived));
        }
    }
    parts
}

/// True for the views whose own status predicate IS the view, so a caller must
/// not add the "leave finished work out" default on top — `status IN ('done')`
/// AND `status NOT IN ('done')` is an empty list with nothing on screen to
/// explain it.
pub(crate) fn view_owns_status(view: View) -> bool {
    matches!(
        view,
        View::Done | View::InReview | View::Trash | View::Archive
    )
}

/// This view's sort keys, minus the section grouping (which the caller
/// prepends, and only under the default sort).
///
/// Trash and Archive sort by when they were set aside, newest first — the
/// question there is "what did I just delete", never "what's most urgent".
/// Both operands of a datetime comparison get wrapped per the module rule.
pub(crate) fn view_default_order(view: Option<View>) -> &'static str {
    match view {
        Some(View::Trash) => "datetime(deleted_at) DESC, datetime(created_at) DESC",
        Some(View::Archive) => "datetime(archived_at) DESC, datetime(created_at) DESC",
        // Oldest first: the point of Waiting is to notice what has waited
        // longest, and priority order would bury a P3 that has sat for a
        // month under a P1 filed this morning.
        Some(View::Waiting) => "datetime(created_at) ASC, rowid ASC",
        _ => "priority ASC, \
              CASE WHEN due_at IS NULL THEN 1 ELSE 0 END ASC, \
              datetime(due_at) ASC",
    }
}

/// The section keys the board order opens with. Only under the default sort,
/// and never in a hidden view — see `db::search::filtered`.
pub(crate) const SECTION_ORDER: &str = "(section_id IS NULL) ASC, \
     COALESCE((SELECT sort_order FROM sections s WHERE s.id = tasks.section_id), 0) ASC";

/// A view's task list.
///
/// **An adapter over [`db::search::filtered`], which is the one query builder**
/// (invariant 21). It exists because the view/project/tag shape is what the
/// MCP, `view_counts` and the older callers already speak; everything it does
/// is expressed as a `FilterSpec`, so a facet can never mean one thing here and
/// another in the filter strip.
pub fn list(conn: &Connection, filters: &ListFilters) -> Result<Vec<Task>, AppError> {
    let view = filters.view.unwrap_or(View::All);
    let spec = crate::db::search::FilterSpec {
        view: Some(view),
        project: filters.project.clone(),
        statuses: filters
            .status
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| vec![s.to_string()]),
        // The default every open view used to carry inline. Explicit now, so
        // the app can render it as a chip and take it off.
        exclude_statuses: (!view_owns_status(view)).then(|| vec![CLOSED_STATUSES[0].to_string()]),
        // …which means this caller has already said everything it wants about
        // closure; the search-side default must not add a second rule.
        exclude_closed: Some(false),
        tags: filters.tags.clone(),
        hide_blocked: filters.hide_blocked,
        include_subtasks: filters.include_subtasks,
        limit: filters.limit,
        ..Default::default()
    };
    crate::db::search::filtered(conn, &spec)
}

/// The single most pressing open task — what `/next` answers with.
///
/// Ranking: overdue first, then priority, then nearest due date, then manual
/// order. Deferred, blocked, closed and review-pending tasks are excluded.
pub fn next_task(conn: &Connection, project: Option<&str>) -> Result<Option<Task>, AppError> {
    let closed = closed_list();
    // A held task — blocked by a dependency, by a written reason, by a date or
    // a tripwire — is not actionable, so handing it back as "what to work on"
    // is actively wrong: you'd start it and immediately discover you can't.
    // All four live in `not_held_sql`, the same predicate Open carries.
    //
    // `in_review` is the same wrongness from the other end: the work is DONE
    // and waiting on a human, so a picker handed one either redoes finished
    // work or flips it to in_progress and destroys the review state. It stays
    // OUT of CLOSED_STATUSES, which is deliberately one member (see the v12
    // migration) and feeds every count and view — this is a statement about
    // what is actionable, which is this query's business alone.
    let mut sql = format!(
        "SELECT {COLUMNS} FROM tasks
         WHERE {LIVE}
           AND status NOT IN ({closed})
           AND status <> 'in_review'
           AND parent_id IS NULL
           AND {}
           AND {}
           AND {}",
        not_held_sql(),
        crate::db::relations::NOT_SUPERSEDED,
        not_decision_sql()
    );
    let mut binds: Vec<String> = Vec::new();
    if let Some(p) = project.map(str::trim).filter(|p| !p.is_empty()) {
        // Scoped, so `/next CXVentures` can hand back work sitting in a project
        // nested under it — otherwise nesting a wave of tasks under an area
        // would quietly remove them from what that area hands you.
        let scope = crate::db::projects::expand_scope(conn, p)?;
        let placeholders = crate::db::projects::scope_placeholders(&scope);
        sql.push_str(&format!(" AND project COLLATE NOCASE IN ({placeholders})"));
        binds.extend(scope);
    }
    sql.push_str(
        " ORDER BY (due_at IS NOT NULL AND datetime(due_at) <= datetime('now')) DESC,
                   priority ASC,
                   CASE WHEN due_at IS NULL THEN 1 ELSE 0 END ASC,
                   datetime(due_at) ASC,
                   sort_order ASC
         LIMIT 1",
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params_from_iter(binds.iter()), row_to_task)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Every open decision awaiting a human: live, not closed, not deferred, and
/// carrying [`DECISION_TAG`]. What the MCP's `next_task` appends when it hands
/// an agent something else, so a session that asks "what next" also learns
/// what only Chris can move.
pub fn open_decisions(conn: &Connection) -> Result<Vec<Task>, AppError> {
    let closed = closed_list();
    let sql = format!(
        "SELECT {COLUMNS} FROM tasks
         WHERE {LIVE}
           AND status NOT IN ({closed})
           AND {}
           AND {}
         ORDER BY priority ASC, datetime(created_at) ASC, rowid ASC",
        not_held_sql(),
        is_decision_sql()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_task)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Distinct project names with open/closed counts, for the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub project: String,
    pub open_count: i64,
}

pub fn list_projects(conn: &Connection) -> Result<Vec<ProjectSummary>, AppError> {
    let closed = closed_list();
    let sql = format!(
        "SELECT project, COUNT(*) FROM tasks
         WHERE {LIVE} AND project IS NOT NULL AND TRIM(project) <> ''
           AND status NOT IN ({closed})
         GROUP BY project ORDER BY COUNT(*) DESC, project ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(ProjectSummary {
                project: r.get(0)?,
                open_count: r.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Per-view counts for the sidebar badges.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ViewCounts {
    pub inbox: i64,
    pub today: i64,
    /// **Legacy, and kept on the wire on purpose.** The Upcoming view is gone;
    /// this field is now a copy of `waiting`. The iOS client decodes it
    /// non-optionally, so dropping the key would make every `view_counts`
    /// response fail to parse there — freezing its Today screen and making
    /// Settings report the Mac unreachable, with nothing erroring on this
    /// side. Remove it once the phone has shipped without it.
    pub upcoming: i64,
    pub waiting: i64,
    pub in_review: i64,
}

pub fn view_counts(conn: &Connection) -> Result<ViewCounts, AppError> {
    let rows = |view: View| -> Result<Vec<Task>, AppError> {
        list(
            conn,
            &ListFilters {
                view: Some(view),
                limit: Some(2000),
                ..Default::default()
            },
        )
    };
    let count = |view: View| -> Result<i64, AppError> { Ok(rows(view)?.len() as i64) };
    let waiting = count(View::Waiting)?;
    Ok(ViewCounts {
        // The Inbox view's length, like every other badge. It used to filter
        // the view's rows down to the project-less ones, because the view held
        // every UNDATED task and a badge counting that never reached zero. The
        // view now holds only what is left to file (see `View::Inbox`), so the
        // badge and the header are one number and draining the Inbox means
        // exactly what the badge says: give each row a home or a date.
        inbox: count(View::Inbox)?,
        today: count(View::Today)?,
        // Legacy key, same number as `waiting` — see the field's comment.
        upcoming: waiting,
        waiting,
        in_review: count(View::InReview)?,
    })
}

// ── update ────────────────────────────────────────────────────────────────

/// `None` = leave alone, `Some(None)` = clear the column, `Some(Some(v))` = set.
/// JSON `null` clears; an absent key leaves the column untouched.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

/// **Absent = leave alone, explicit `null` = clear.** That distinction is the
/// entire contract of this type, and it survives serialization only because
/// every field carries `skip_serializing_if`.
///
/// Without it a derived `Serialize` emits `null` for `None`, which the
/// deserializer reads back as `Some(None)` — "clear this column". A patch
/// round-tripped through JSON would then wipe every field the caller did not
/// set. `api::dispatch` round-trips patches by construction, so this is not
/// hypothetical; `a_patch_survives_a_json_round_trip_without_clearing_anything`
/// is what keeps it honest.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TaskPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub body: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub project: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub due_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub defer_until: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_order: Option<i64>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub prompt: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg_allowed: Option<bool>,
    /// Switch which agent runs this task in the background.
    ///
    /// Not independent of `bg_allowed` or `model`: see [`update`], where all
    /// three are resolved together because the enabled flag lives in an
    /// executor-specific column and a model name is only meaningful to the
    /// executor it was chosen for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg_executor: Option<BackgroundExecutor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg_writes_allowed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg_shell_allowed: Option<bool>,
    /// May this task leave the machine that holds its repo?
    ///
    /// A plain `Option<bool>` and not `Option<Option<bool>>`: the column is
    /// `NOT NULL DEFAULT 0`, so there is no "clear it" state distinct from
    /// setting it false.
    pub bg_remote_allowed: Option<bool>,
    /// `Some("")` clears the list; absent leaves it alone.
    pub bg_mcp_servers: Option<String>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub model: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub section_id: Option<Option<String>>,
    /// The hand-set hold: what the task is waiting on. `Some(Some(reason))`
    /// holds it, `Some(None)` (or an empty string) releases it, absent leaves
    /// it alone. Independent of `status` since v27, with two exceptions
    /// [`update`] applies for the caller: entering `done` releases it (a closed
    /// task holds nothing), and entering `queued` releases it, because the
    /// runner is what writes most of these ("Background run refused: …") and
    /// re-queueing IS the un-park gesture — a reason left behind would keep the
    /// task out of the drain forever.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<Option<String>>,
    /// Arm the next background dispatch as work or as a review.
    ///
    /// A plain `Option`, not a `double_option`: the column is `NOT NULL`, so
    /// there is no "clear it" state to express — same as `bg_executor`.
    /// Resolved on its own in [`update`], deliberately NOT swept into the joint
    /// executor/flag/model block; see the comment there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg_intent: Option<BackgroundIntent>,
    /// Per-task wall clock in minutes. Explicit `null` clears the override and
    /// returns the task to the global setting, so this is a `double_option`
    /// like `model` — "leave it alone" and "put it back on the default" are
    /// different requests and a plain `Option` cannot say the second.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub bg_timeout_minutes: Option<Option<i64>>,
    /// Per-task ceiling on ONE run's tokens. `double_option` for the reason
    /// above; `Some(Some(0))` disables the ceiling and `Some(None)` restores
    /// the global one — two different things that must stay distinguishable.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub bg_run_token_cap: Option<Option<i64>>,
    /// The tripwire spec. `Some(None)` clears the tripwire and every column
    /// beside it; a new or changed spec re-arms. Resolved together with the
    /// four fields below in [`update`] — see the block there.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub tripwire: Option<Option<String>>,
    /// Cadence in minutes; `Some(None)` = manual. Floor-checked against the
    /// spec's kind.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub tripwire_every_minutes: Option<Option<i64>>,
    /// `surface` | `complete`; `Some(None)` = surface.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub tripwire_on_trip: Option<Option<String>>,
    /// The ONLY state a caller may write is `armed`, and only from `tripped`
    /// or `error` — a re-arm. The runner owns the other two.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tripwire_state: Option<String>,
    /// "Check now": stamps a request on the runtime row for the runner's next
    /// tick, whatever the cadence. Refused while the task is deferred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tripwire_check_requested: Option<bool>,
}

impl TaskPatch {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.body.is_none()
            && self.status.is_none()
            && self.priority.is_none()
            && self.project.is_none()
            && self.due_at.is_none()
            && self.defer_until.is_none()
            && self.recurrence.is_none()
            && self.sort_order.is_none()
            && self.repo_path.is_none()
            && self.prompt.is_none()
            && self.bg_allowed.is_none()
            && self.bg_executor.is_none()
            && self.bg_writes_allowed.is_none()
            && self.bg_shell_allowed.is_none()
            && self.bg_remote_allowed.is_none()
            && self.bg_mcp_servers.is_none()
            && self.model.is_none()
            && self.section_id.is_none()
            && self.blocked_reason.is_none()
            && self.bg_intent.is_none()
            && self.bg_timeout_minutes.is_none()
            && self.bg_run_token_cap.is_none()
            && self.tripwire.is_none()
            && self.tripwire_every_minutes.is_none()
            && self.tripwire_on_trip.is_none()
            && self.tripwire_state.is_none()
            && self.tripwire_check_requested.is_none()
    }

    fn touches_tripwire(&self) -> bool {
        self.tripwire.is_some()
            || self.tripwire_every_minutes.is_some()
            || self.tripwire_on_trip.is_some()
            || self.tripwire_state.is_some()
            || self.tripwire_check_requested.is_some()
    }
}

/// Two project names that mean the same project.
///
/// Matches how the database compares them — `project_repos` and `sections` both
/// join `COLLATE NOCASE`, and `projects::normalize` collapses inner whitespace —
/// so "Artist  Advisory" and "artist advisory" can't be read as a move.
fn eq_name(a: &str, b: &str) -> bool {
    crate::db::projects::normalize(a).to_lowercase()
        == crate::db::projects::normalize(b).to_lowercase()
}

pub fn update(conn: &Connection, id: &str, patch: &TaskPatch) -> Result<Task, AppError> {
    let Some(current) = get(conn, id)? else {
        return Err(AppError::NotFound(format!("task {id} not found")));
    };
    if patch.is_empty() {
        return get(conn, id)?
            .ok_or_else(|| AppError::NotFound(format!("task {id} not found")));
    }
    // `flatten()` so only a real value is checked — clearing an override back
    // to the global (`Some(None)`) has nothing to validate.
    validate_run_budget(
        patch.bg_timeout_minutes.flatten(),
        patch.bg_run_token_cap.flatten(),
    )?;

    let mut sets: Vec<&str> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    // Nullable text columns share one shape: Some(None) → NULL, Some(Some) → value.
    let push_opt = |sql: &'static str,
                        value: &Option<Option<String>>,
                        sets: &mut Vec<&str>,
                        binds: &mut Vec<Box<dyn rusqlite::ToSql>>| {
        if let Some(v) = value {
            sets.push(sql);
            let cleaned = v
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            binds.push(Box::new(cleaned));
        }
    };

    // The same shape for nullable INTEGER columns. A separate closure rather
    // than a generic one because the text version's "empty string → NULL"
    // cleaning is exactly wrong here: `0` is a meaningful value for
    // `bg_run_token_cap` (it disables the ceiling) and must never be treated as
    // absent.
    let push_opt_i64 = |sql: &'static str,
                        value: &Option<Option<i64>>,
                        sets: &mut Vec<&str>,
                        binds: &mut Vec<Box<dyn rusqlite::ToSql>>| {
        if let Some(v) = value {
            sets.push(sql);
            binds.push(Box::new(*v));
        }
    };

    if let Some(title) = patch.title.as_deref().map(str::trim) {
        if title.is_empty() {
            return Err(AppError::Validation("title must not be empty".into()));
        }
        sets.push("title = ?");
        binds.push(Box::new(title.to_string()));
    }
    push_opt("body = ?", &patch.body, &mut sets, &mut binds);
    // ── status, and the hold that rides beside it ──────────────────────────
    //
    // Since v27 `blocked_reason` is a HOLD, not the written half of a status:
    // it is set and cleared on its own, and `status` says how far along the
    // work is regardless. Two transitions still release it for the caller,
    // because leaving it behind produces a state nobody meant: `done` (a
    // closed task waits on nothing — and a recurrence's next instance must
    // not inherit the hold) and `queued` (most reasons are the runner's own
    // "Background run refused: …"; re-queueing is the un-park gesture, and a
    // reason left in place would keep the task out of the drain forever).
    let mut closing = false;
    if let Some(raw) = patch.status.as_deref() {
        let status = normalize_status(raw).ok_or_else(|| invalid_status(raw))?;
        closing = CLOSED_STATUSES.contains(&status);
        sets.push("status = ?");
        binds.push(Box::new(status.to_string()));
        // Keep completed_at consistent with the lifecycle in one place.
        if CLOSED_STATUSES.contains(&status) {
            sets.push("completed_at = COALESCE(completed_at, datetime('now'))");
        } else {
            sets.push("completed_at = NULL");
        }
        if CLOSED_STATUSES.contains(&status) || status == "queued" {
            sets.push("blocked_reason = NULL");
        }
    }
    if let Some(reason) = &patch.blocked_reason {
        // Explicit in this patch wins over the release above — a caller that
        // queues AND writes a reason in one patch has said something odd, and
        // the reason is the part it typed.
        match reason.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(r) => {
                sets.push("blocked_reason = ?");
                binds.push(Box::new(r.to_string()));
            }
            None => sets.push("blocked_reason = NULL"),
        }
    }
    if let Some(priority) = patch.priority {
        sets.push("priority = ?");
        binds.push(Box::new(priority.clamp(PRIORITY_HIGHEST, PRIORITY_LOWEST)));
    }
    push_opt("project = ?", &patch.project, &mut sets, &mut binds);
    // ── project → workspace, the other half of invariant 17 ──────────────────
    //
    // Setting a project inherits its primary repo, exactly as `create` does, so
    // every route that files a task into a project agrees: the quick-add bar, a
    // row dragged onto a project group in `TaskList`, `update_task`, and the
    // detail pane's project field (which no longer resolves this itself).
    //
    // Skipped entirely when the same patch carries `repo_path` — an explicit
    // workspace is the caller's decision, and it would also mean two
    // `repo_path = ?` fragments in one SET list.
    if patch.repo_path.is_none() {
        if let Some(Some(raw)) = &patch.project {
            let new_project = raw.trim();
            let old_project = current.project.as_deref().unwrap_or("").trim();
            let current_repo = current.repo_path.as_deref().unwrap_or("").trim();
            let moved = !eq_name(old_project, new_project);
            let inherit = if current_repo.is_empty() {
                // Nothing to lose. Fills whether or not the project changed, so
                // re-committing the project on a repo-less task resolves it.
                !new_project.is_empty()
            } else if moved && !new_project.is_empty() && !old_project.is_empty() {
                // The workspace is only REPLACED when it belongs to the project
                // being left — moving a task to cxventures while it goes on
                // pointing at `finance-app` is a live dispatch footgun, and a
                // stale repo is the one case where leaving it alone is worse
                // than changing it. A repo owned by neither project (deliberate
                // cross-project work) survives untouched, and so does the iOS
                // half of a project you are NOT leaving, which is why this is
                // gated on `moved` rather than running on every project write.
                crate::db::projects::for_repo_path(conn, current_repo)?
                    .is_some_and(|owner| eq_name(&owner, old_project))
            } else {
                false
            };
            if inherit {
                if let Some(primary) = crate::db::projects::primary_repo(conn, new_project)? {
                    sets.push("repo_path = ?");
                    binds.push(Box::new(primary));
                }
            }
        }
    }
    push_opt("due_at = ?", &patch.due_at, &mut sets, &mut binds);
    push_opt("defer_until = ?", &patch.defer_until, &mut sets, &mut binds);
    push_opt("recurrence = ?", &patch.recurrence, &mut sets, &mut binds);
    push_opt("repo_path = ?", &patch.repo_path, &mut sets, &mut binds);
    push_opt("prompt = ?", &patch.prompt, &mut sets, &mut binds);
    push_opt("model = ?", &patch.model, &mut sets, &mut binds);
    push_opt("section_id = ?", &patch.section_id, &mut sets, &mut binds);
    push_opt_i64(
        "bg_timeout_minutes = ?",
        &patch.bg_timeout_minutes,
        &mut sets,
        &mut binds,
    );
    push_opt_i64(
        "bg_run_token_cap = ?",
        &patch.bg_run_token_cap,
        &mut sets,
        &mut binds,
    );
    // ── tripwire: spec, cadence, policy, state, request — resolved TOGETHER ──
    //
    // Same shape as `status + blocked_reason` above, and for the same reason:
    // these are five columns describing ONE thing. The effective `(spec,
    // every, on_trip)` is patch ∪ current, so the floor is checked against the
    // kind the task will actually have, not against whichever half the caller
    // happened to send. A new or changed spec re-arms and resets the runtime
    // row; clearing the spec clears every sibling column. `tripwire_state` is
    // writable only as `armed`, and only from `tripped`/`error` — the runner
    // owns the other transitions (invariant 26).
    //
    // The runtime row is touched AFTER the task UPDATE below, through the
    // three flags — `db::tripwires` is a separate table on purpose, because a
    // whole-row post-image of `tasks` must never carry attempt bookkeeping.
    let mut runtime_reset = false;
    let mut runtime_ensure = false;
    let mut request_check = false;
    // Whether the block below already wrote `tripwire_state`, so the monitor
    // rule after it never adds a second `tripwire_state = …` to the SET list.
    let mut state_written = false;
    if patch.touches_tripwire() {
        let spec_raw: Option<String> = match &patch.tripwire {
            Some(v) => v.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
            None => current.tripwire.clone(),
        };
        let clearing = patch.tripwire.is_some() && spec_raw.is_none();
        let every = match &patch.tripwire_every_minutes {
            Some(v) => *v,
            None => current.tripwire_every_minutes,
        };
        let on_trip_raw: Option<String> = match &patch.tripwire_on_trip {
            Some(v) => v.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
            None => current.tripwire_on_trip.clone(),
        };
        // An explicit clear takes everything with it — including a cadence
        // the row still carries, which the resolver would otherwise refuse
        // as "a cadence with no spec". Clearing is the caller's answer to
        // that, not a contradiction.
        let resolved = if clearing {
            None
        } else {
            resolve_tripwire(spec_raw.as_deref(), every, on_trip_raw.as_deref())?
        };
        match resolved {
            None => {
                // `resolve_tripwire` already refused a cadence or policy with
                // no spec to hang it on, so reaching here with no spec means
                // either an explicit clear or a state/request write on a task
                // that has nothing to arm.
                if !clearing {
                    return Err(AppError::Validation(
                        "this task has no tripwire — set `tripwire` (e.g. script:<name>) \
                         before re-arming it or requesting a check"
                            .into(),
                    ));
                }
                sets.push("tripwire = NULL");
                sets.push("tripwire_every_minutes = NULL");
                sets.push("tripwire_on_trip = NULL");
                sets.push("tripwire_state = NULL");
                sets.push("tripwire_tripped_at = NULL");
                // Reset, never deleted: the row goes only by cascade, so no
                // verb here has to join `api::DELETES_ROWS`.
                runtime_reset = current.tripwire.is_some();
            }
            Some(cols) => {
                let changed = current.tripwire.as_deref() != Some(cols.spec.as_str());
                sets.push("tripwire = ?");
                binds.push(Box::new(cols.spec));
                sets.push("tripwire_every_minutes = ?");
                binds.push(Box::new(cols.every));
                sets.push("tripwire_on_trip = ?");
                binds.push(Box::new(cols.on_trip));

                let mut state = current
                    .tripwire_state
                    .as_deref()
                    .map(TripwireState::from_db);
                if changed {
                    // A different condition is a new tripwire: armed, fresh
                    // runtime, whatever the old one had recorded.
                    state = Some(TripwireState::Armed);
                    sets.push("tripwire_state = 'armed'");
                    sets.push("tripwire_tripped_at = NULL");
                    runtime_reset = true;
                    state_written = true;
                } else if let Some(requested) = patch.tripwire_state.as_deref() {
                    let requested = TripwireState::parse(requested).map_err(AppError::Validation)?;
                    if requested != TripwireState::Armed {
                        return Err(AppError::Validation(
                            "only 'armed' may be written to tripwire_state — 'tripped' and \
                             'error' are recorded by the runner"
                                .into(),
                        ));
                    }
                    if state == Some(TripwireState::Armed) {
                        return Err(AppError::Validation(
                            "the tripwire is already armed; re-arm applies to a tripped or \
                             errored one"
                                .into(),
                        ));
                    }
                    state = Some(TripwireState::Armed);
                    sets.push("tripwire_state = 'armed'");
                    sets.push("tripwire_tripped_at = NULL");
                    runtime_reset = true;
                    state_written = true;
                }
                if state.is_none() {
                    // A spec with no state is a row this build has never
                    // touched (written by hand, or by a client that knew the
                    // column and not the rule). Arm it: hidden-with-a-reason is
                    // what the spec asked for.
                    state = Some(TripwireState::Armed);
                    sets.push("tripwire_state = 'armed'");
                    runtime_ensure = true;
                    state_written = true;
                }
                if patch.tripwire_check_requested == Some(true) {
                    if state != Some(TripwireState::Armed) {
                        return Err(AppError::Validation(format!(
                            "the tripwire is {}, not armed — re-arm it first (tripwire_state: 'armed')",
                            state.map(|s| s.as_str()).unwrap_or("unset")
                        )));
                    }
                    // The date gate comes first (invariant 26): a check on a
                    // deferred task would be refused by the runner anyway, and
                    // an accepted request that nothing honours is the worse
                    // outcome. The effective date is patch ∪ current, like
                    // everything else in this block.
                    let defer = match &patch.defer_until {
                        Some(v) => v.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
                        None => current.defer_until.clone(),
                    };
                    if let Some(d) = defer.as_deref() {
                        let in_future: bool = conn.query_row(
                            "SELECT datetime(?1) > datetime('now')",
                            params![d],
                            |r| r.get(0),
                        )?;
                        if in_future {
                            return Err(AppError::Validation(format!(
                                "this task is deferred until {d}; a tripwire is not evaluated \
                                 until its date passes, so a check now would do nothing"
                            )));
                        }
                    }
                    request_check = true;
                    runtime_ensure = true;
                }
            }
        }
    }
    // ── a monitor: Complete means "handled" ─────────────────────────────────
    //
    // Closing a tripped or errored monitor re-arms it — the same write the
    // Re-arm button makes — and resets the runtime row, so its `checked_at` is
    // gone and the runner checks again on its next tick. A fix that missed
    // something reopens the task within seconds rather than a cadence later.
    // Everything is patch ∪ current, like the tripwire block above: the policy
    // or the recurrence may arrive in this very patch.
    //
    // This is the fast path, not the guarantee. A done row can also arrive by
    // sync from a build that doesn't know the rule; the runner re-arms those
    // (`db::tripwires::rearm_handled_monitors`) and keeps checking a done
    // monitor either way.
    let eff_spec_present = match &patch.tripwire {
        Some(v) => v.as_deref().is_some_and(|v| !v.trim().is_empty()),
        None => current.tripwire.is_some(),
    };
    let eff_on_trip: Option<String> = if !eff_spec_present {
        None
    } else {
        match &patch.tripwire_on_trip {
            Some(v) => v.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
            None => current.tripwire_on_trip.clone(),
        }
    };
    let eff_recurrence: Option<String> = match &patch.recurrence {
        Some(v) => v.clone(),
        None => current.recurrence.clone(),
    };
    refuse_recurring_monitor(eff_on_trip.as_deref(), eff_recurrence.as_deref())?;
    let monitor = OnTrip::from_db(eff_on_trip.as_deref()) == OnTrip::Monitor;
    if closing && monitor && !state_written {
        let fired = current
            .tripwire_state
            .as_deref()
            .map(TripwireState::from_db)
            .is_some_and(|s| s != TripwireState::Armed);
        if fired {
            sets.push("tripwire_state = 'armed'");
            sets.push("tripwire_tripped_at = NULL");
            runtime_reset = true;
        }
    }
    if let Some(sort_order) = patch.sort_order {
        sets.push("sort_order = ?");
        binds.push(Box::new(sort_order));
    }
    // ── executor + enabled flag: written TOGETHER, never separately ──────
    //
    // `bg_allowed` is not a column on its own any more; it is whichever of
    // `bg_allowed` / `bg_codex_allowed` belongs to the task's executor (see
    // `crate::executor`). So flipping the executor has to *move* the flag, and
    // setting the flag has to know the executor. Handling them independently
    // produces the two failure modes this replaces: switching Claude → Codex on
    // an enabled task would leave the flag in the Claude column, where the task
    // reads as disabled in the UI while a pre-v8 runner still sees it as work;
    // and enabling a Codex task would write the Claude column, which is exactly
    // the mis-dispatch the split exists to prevent.
    if patch.bg_allowed.is_some() || patch.bg_executor.is_some() {
        let executor = patch.bg_executor.unwrap_or(current.bg_executor);
        let enabled = patch.bg_allowed.unwrap_or(current.bg_allowed);
        sets.push("bg_executor = ?");
        binds.push(Box::new(executor));
        sets.push("bg_allowed = ?");
        binds.push(Box::new(i64::from(
            enabled && executor == BackgroundExecutor::Claude,
        )));
        sets.push("bg_codex_allowed = ?");
        binds.push(Box::new(i64::from(
            enabled && executor == BackgroundExecutor::Codex,
        )));

        // A model name belongs to one vendor. Carrying `claude-sonnet-5` across
        // a switch to Codex hands the Codex CLI a model it doesn't have, and the
        // run fails at spawn for a reason that has nothing to do with the task.
        // Clearing falls back to the new executor's own default, which is the
        // only value guaranteed to be valid. An explicit `model` in the same
        // patch wins — that's the caller replacing it deliberately.
        if executor != current.bg_executor && patch.model.is_none() {
            sets.push("model = NULL");
        }
    }
    // **Standalone, and that is deliberate** — note it sits OUTSIDE the joint
    // executor/flag/model block above, unlike the three fields it neighbours.
    // Switching the executor while a review is armed is the *intended* gesture:
    // arming a review preselects the OTHER agent, so sweeping intent into that
    // resolution would disarm the review at the exact moment the user set it up.
    // Nothing about intent constrains, or is constrained by, which agent runs it.
    if let Some(v) = patch.bg_intent {
        // `check` and `interpret` live on `task_runs.intent` only. A task armed
        // as either would be a queued run of a kind the drain has no framing
        // for; the tripwire lane records the intent itself, on the run row, at
        // the moment it starts one (see `crate::intent`).
        if v.is_tripwire_run() {
            return Err(AppError::Validation(format!(
                "bg_intent '{v}' is machine-only — the runner records it on a tripwire run's \
                 row; arm a tripwire instead (`agent:<condition>`, or tripwire_on_trip \
                 'interpret')"
            )));
        }
        sets.push("bg_intent = ?");
        binds.push(Box::new(v));
    }
    if let Some(v) = patch.bg_writes_allowed {
        sets.push("bg_writes_allowed = ?");
        binds.push(Box::new(i64::from(v)));
    }
    if let Some(v) = patch.bg_shell_allowed {
        sets.push("bg_shell_allowed = ?");
        binds.push(Box::new(i64::from(v)));
    }
    if let Some(v) = patch.bg_remote_allowed {
        sets.push("bg_remote_allowed = ?");
        binds.push(Box::new(i64::from(v)));
    }
    if let Some(v) = &patch.bg_mcp_servers {
        sets.push("bg_mcp_servers = ?");
        // An empty string stores NULL, so "no servers" has ONE representation
        // rather than two that every reader would have to handle.
        binds.push(Box::new(
            Some(v.trim().to_string()).filter(|s| !s.is_empty()),
        ));
    }

    // A due-date change re-arms the reminder.
    if patch.due_at.is_some() {
        sets.push("reminded_at = NULL");
    }
    sets.push("updated_at = datetime('now')");

    let sql = format!("UPDATE tasks SET {} WHERE id = ?", sets.join(", "));
    binds.push(Box::new(id.to_string()));
    conn.execute(&sql, params_from_iter(binds.iter().map(|b| b.as_ref())))?;

    // The volatile half, on its own row. Reset implies ensure.
    if runtime_reset {
        crate::db::tripwires::reset_runtime(conn, id)?;
    } else if runtime_ensure {
        crate::db::tripwires::ensure_runtime(conn, id)?;
    }
    if request_check {
        crate::db::tripwires::request_check(conn, id)?;
    }

    get(conn, id)?.ok_or_else(|| AppError::NotFound(format!("task {id} not found")))
}

/// Mark a task done. Returns the completed task; recurrence materialization is
/// layered on top of this by [`crate::db::recurrence`] in the app/MCP paths.
pub fn complete(conn: &Connection, id: &str) -> Result<Task, AppError> {
    update(
        conn,
        id,
        &TaskPatch {
            status: Some("done".into()),
            ..Default::default()
        },
    )
}

/// **Permanent, irreversible destruction.** Cascades to subtasks, runs, tags,
/// dependency edges, comments and the FTS row.
///
/// Reachable from exactly two places by design: [`purge_expired`] once the
/// retention window has run out, and an explicit "Delete Permanently" inside the
/// Trash. Everything a user or an agent reaches for goes through [`trash`]
/// instead. If you are adding a third caller, that is the thing to reconsider.
pub fn delete(conn: &Connection, id: &str) -> Result<(), AppError> {
    let n = conn.execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
    if n == 0 {
        return Err(AppError::NotFound(format!("task {id} not found")));
    }
    // The `task_tags` cascade above can have just taken a tag's last task with
    // it. Tags are emergent, so an unreferenced one is collected rather than
    // left in the sidebar — see `db::tags::gc`.
    crate::db::tags::gc(conn)?;
    Ok(())
}

// ── trash / archive ───────────────────────────────────────────────────────
//
// Both work the same way and differ only in which column they stamp and whether
// a purge clock is attached, so the subtree mechanics live in one pair of
// helpers rather than being written twice and drifting.

/// One millisecond-precision instant, produced by SQLite so the format matches
/// what `datetime()` and the purge comparison expect.
///
/// This is a **timestamp only**. It used to double as the batch key, which is
/// the bug v8 fixed: two `trash()` calls in the same millisecond produced the
/// same string and merged into one restore batch. Identity now comes from
/// [`new_batch`], and nothing about correctness depends on this value being
/// distinct from any other.
fn now_ms(conn: &Connection) -> Result<String, AppError> {
    Ok(conn.query_row("SELECT strftime('%Y-%m-%d %H:%M:%f','now')", [], |r| r.get(0))?)
}

/// A fresh batch id. Unique without coordinating with anything — which is the
/// whole point, since the three binaries sharing this DB each trash on their
/// own connection and a clock reading can't tell them apart.
fn new_batch() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Stamp `id` and every descendant that isn't already hidden with one timestamp
/// and one batch id.
///
/// `UNION` (not `UNION ALL`) dedupes the walk, so a hand-edited `parent_id`
/// cycle terminates instead of hanging — the same defence `deps::reaches` needs
/// for its `seen` set.
///
/// The `IS NULL` guard is what keeps a separately-trashed descendant in its own
/// batch: it was stamped earlier, so it is skipped here and keeps the batch id
/// it went in with.
fn stamp_subtree(
    conn: &Connection,
    id: &str,
    hidden: Hidden,
    stamp: &str,
    batch: &str,
) -> Result<usize, AppError> {
    let (stamp_col, batch_col) = (hidden.stamp_col(), hidden.batch_col());
    let sql = format!(
        "WITH RECURSIVE subtree(id) AS (
             SELECT id FROM tasks WHERE id = ?1
             UNION
             SELECT t.id FROM tasks t JOIN subtree s ON t.parent_id = s.id
         )
         UPDATE tasks SET {stamp_col} = ?2, {batch_col} = ?3, updated_at = datetime('now')
          WHERE id IN (SELECT id FROM subtree) AND {stamp_col} IS NULL"
    );
    Ok(conn.execute(&sql, params![id, stamp, batch])?)
}

/// Clear both columns on every row of the batch `id` belongs to.
///
/// Restores from the **top** of the batch: it walks up to the highest ancestor
/// sharing the batch before walking back down. Restoring a child otherwise
/// resurrects it under a parent that is still in the Trash — visible nowhere,
/// since a live view lists batch roots and this task's root isn't live.
fn unstamp_batch(conn: &Connection, id: &str, hidden: Hidden) -> Result<usize, AppError> {
    let (stamp_col, batch_col) = (hidden.stamp_col(), hidden.batch_col());
    let (own, t, p) = (
        hidden.batch_key(""),
        hidden.batch_key("t."),
        hidden.batch_key("p."),
    );
    let batch: Option<String> = conn
        .query_row(
            &format!("SELECT {own} FROM tasks WHERE id = ?1"),
            params![id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(batch) = batch else {
        return Ok(0); // not trashed/archived — restoring is a no-op, not an error
    };

    let sql = format!(
        "WITH RECURSIVE
         -- up to the batch root…
         ancestors(id, parent_id) AS (
             SELECT id, parent_id FROM tasks WHERE id = ?1
             UNION
             SELECT t.id, t.parent_id FROM tasks t
               JOIN ancestors a ON t.id = a.parent_id
              WHERE {t} = ?2
         ),
         root(id) AS (
             SELECT id FROM ancestors
              WHERE parent_id IS NULL
                 OR NOT EXISTS (SELECT 1 FROM tasks p
                                 WHERE p.id = ancestors.parent_id AND {p} = ?2)
         ),
         -- …then back down over everything in the same batch.
         subtree(id) AS (
             SELECT id FROM root
             UNION
             SELECT t.id FROM tasks t JOIN subtree s ON t.parent_id = s.id
              WHERE {t} = ?2
         )
         UPDATE tasks SET {stamp_col} = NULL, {batch_col} = NULL, updated_at = datetime('now')
          WHERE id IN (SELECT id FROM subtree) AND {own} = ?2"
    );
    Ok(conn.execute(&sql, params![id, batch])?)
}

/// Move a task and its subtree to the Trash. Recoverable for
/// [`RETENTION_DAYS`]; `status`, tags, comments, runs and dependency edges are
/// all left untouched so restore is exact rather than approximate.
///
/// Returns the timestamp written, purely as information — the batch's identity
/// is the id generated here, not the instant.
pub fn trash(conn: &Connection, id: &str) -> Result<String, AppError> {
    if get(conn, id)?.is_none() {
        return Err(AppError::NotFound(format!("task {id} not found")));
    }
    let stamp = now_ms(conn)?;
    stamp_subtree(conn, id, Hidden::Trashed, &stamp, &new_batch())?;
    Ok(stamp)
}

/// Pull a task back out of the Trash, along with everything trashed with it.
pub fn restore(conn: &Connection, id: &str) -> Result<Task, AppError> {
    unstamp_batch(conn, id, Hidden::Trashed)?;
    get(conn, id)?.ok_or_else(|| AppError::NotFound(format!("task {id} not found")))
}

/// Set a task and its subtree aside. Unlike the Trash there is no clock on it —
/// archived work is kept deliberately and stays searchable.
pub fn archive(conn: &Connection, id: &str) -> Result<String, AppError> {
    if get(conn, id)?.is_none() {
        return Err(AppError::NotFound(format!("task {id} not found")));
    }
    let stamp = now_ms(conn)?;
    stamp_subtree(conn, id, Hidden::Archived, &stamp, &new_batch())?;
    Ok(stamp)
}

pub fn unarchive(conn: &Connection, id: &str) -> Result<Task, AppError> {
    unstamp_batch(conn, id, Hidden::Archived)?;
    get(conn, id)?.ok_or_else(|| AppError::NotFound(format!("task {id} not found")))
}

/// Destroy trashed tasks whose retention window has closed. Returns how many
/// went. **The only automatic caller of [`delete`] in the codebase.**
///
/// Both operands wrapped in `datetime()` per the module rule — a byte-wise
/// comparison here would be the same class of bug as the reminder scan, except
/// the failure mode is destroying data early rather than notifying late.
pub fn purge_expired(conn: &Connection) -> Result<usize, AppError> {
    // **Canonical role only, once sync is on.**
    //
    // This is a hard delete that nothing records: it is driven by the clock
    // rather than by a person, so no intent describes it and the sweep cannot
    // see it — a swept table reports rows that EXIST, and the whole point of a
    // purge is that they stop existing. Run on a local host, its deletions
    // would apply here, never push, and be undone by the next snapshot: the
    // trash would refill itself, on a timer, with no explanation.
    //
    // The canonical node has no such problem, because everyone else converges
    // onto it. Refusing here rather than silently skipping, so a caller that
    // starts purging on the wrong node learns immediately.
    if crate::sync::state::enabled(conn) && !crate::sync::state::is_canonical(conn) {
        return Err(AppError::Validation(
            "purging expired trash is the canonical node's job — a hard delete here \
             would be undone by the next pull, and the trash would refill itself"
                .into(),
        ));
    }
    let n = conn.execute(
        &format!(
            "DELETE FROM tasks
              WHERE deleted_at IS NOT NULL
                AND datetime(deleted_at) <= datetime('now', '-{RETENTION_DAYS} days')"
        ),
        [],
    )?;
    if n > 0 {
        log::info!("purged {n} task(s) past the {RETENTION_DAYS}-day trash window");
        crate::db::tags::gc(conn)?;
    }
    Ok(n)
}

/// Whole-numbered days left before [`purge_expired`] takes this task, for the
/// countdown the Trash renders. `None` when the task isn't trashed.
pub fn days_until_purge(conn: &Connection, id: &str) -> Result<Option<i64>, AppError> {
    let days: Option<f64> = conn
        .query_row(
            &format!(
                "SELECT julianday(deleted_at, '+{RETENTION_DAYS} days') - julianday('now')
                   FROM tasks WHERE id = ?1 AND deleted_at IS NOT NULL"
            ),
            params![id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(days.map(|d| d.ceil().max(0.0) as i64))
}

// ── due / reminder scans (gotcha #23 territory) ───────────────────────────

/// Tasks whose `due_at` has passed and that haven't been reminded about yet.
///
/// Both operands are wrapped in `datetime()`. A raw comparison here is the
/// exact shape of cxmail gotcha #23 and defers every same-day reminder to UTC
/// midnight. The regression test below pins it.
pub fn list_due_for_reminder(conn: &Connection) -> Result<Vec<Task>, AppError> {
    let closed = closed_list();
    let sql = format!(
        "SELECT {COLUMNS} FROM tasks
         WHERE {LIVE}
           AND due_at IS NOT NULL
           AND reminded_at IS NULL
           AND status NOT IN ({closed})
           AND datetime(due_at) <= datetime('now')
         ORDER BY datetime(due_at) ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], row_to_task)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn mark_reminded(conn: &Connection, id: &str) -> Result<(), AppError> {
    conn.execute(
        "UPDATE tasks SET reminded_at = datetime('now') WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// Tasks whose `defer_until` has passed — they graduate back into Today.
/// Same `datetime()`-both-sides rule.
pub fn list_undeferred(conn: &Connection) -> Result<Vec<Task>, AppError> {
    let closed = closed_list();
    let sql = format!(
        "SELECT {COLUMNS} FROM tasks
         WHERE {LIVE}
           AND defer_until IS NOT NULL
           AND status NOT IN ({closed})
           AND datetime(defer_until) <= datetime('now')"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], row_to_task)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ── background queue ──────────────────────────────────────────────────────

/// The capability the local Mac has always had, for tests about the QUEUE
/// rather than about routing.
///
/// Named for what it means at the call site — "this host, here" — because the
/// alternative reading of a bare `cap()` in a drain test is "some capability",
/// and the whole point of these tests is that they assert the unchanged
/// single-host behaviour.
#[cfg(test)]
pub(crate) fn here() -> crate::routing::HostCapability {
    crate::routing::HostCapability::local_default("test-host".into(), "test".into())
}


/// The runner's drain query: queued, dispatchable, and within THIS host's
/// declared capability.
///
/// The capability half is generated by [`crate::routing::queue_sql`] rather
/// than written here, so a host can never claim beyond what it declares — see
/// that function for why generated beats hand-written. What stays local to this
/// query is everything that is about the *task list* rather than the host:
/// liveness, status, blocked dependencies and ordering.
///
/// This query and [`claim_for_background`] must use the same capability clause.
/// One listing a task the other cannot claim is a queue that spins forever,
/// with nothing on screen to explain it — which is why both take the same
/// `cap` and neither builds its own.
pub fn list_queued_for_background(
    conn: &Connection,
    cap: &crate::routing::HostCapability,
    limit: u32,
) -> Result<Vec<Task>, AppError> {
    // Same reasoning as next_task: dispatching a blocked task burns a run to
    // discover it can't proceed.
    let blocked = crate::db::deps::blocked_task_ids(conn)?;
    let blocked_clause = if blocked.is_empty() {
        String::new()
    } else {
        let quoted: Vec<String> = blocked
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect();
        format!(" AND id NOT IN ({})", quoted.join(", "))
    };
    let capable = crate::routing::queue_sql(cap);
    let sql = format!(
        "SELECT {COLUMNS} FROM tasks
         WHERE {LIVE}
           AND status = 'queued'
           AND {TRIPWIRE_NOT_ARMED}
           AND blocked_reason IS NULL
           AND {capable}
           AND {}
           AND {}
           {blocked_clause}
         ORDER BY priority ASC, datetime(created_at) ASC
         LIMIT ?1",
        crate::db::relations::NOT_SUPERSEDED,
        not_decision_sql()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![limit], row_to_task)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Atomically claim a queued task for a background run. Returns false if
/// another runner tick — on this host or any other — already moved it.
///
/// Prefer [`crate::db::runs::claim_and_start`], which does this inside the same
/// write transaction as the run row. This is the bare claim, kept public
/// because the dry-run path claims and rolls back without ever writing a run.
pub fn claim_for_background(
    conn: &Connection,
    cap: &crate::routing::HostCapability,
    id: &str,
) -> Result<bool, AppError> {
    // `LIVE` and the capability clause are repeated here rather than trusted
    // from the drain query: this is the last gate before an unattended agent
    // actually starts, and a task can be trashed — or have its remote opt-in
    // revoked — in the window between being listed and being claimed.
    let capable = crate::routing::queue_sql(cap);
    let sql = format!(
        "UPDATE tasks SET status = 'in_progress', updated_at = datetime('now')
         WHERE id = ?1 AND status = 'queued' AND {capable} AND {LIVE}
           AND {TRIPWIRE_NOT_ARMED} AND blocked_reason IS NULL AND {} AND {}",
        crate::db::relations::NOT_SUPERSEDED,
        not_decision_sql()
    );
    let n = conn.execute(&sql, params![id])?;
    Ok(n == 1)
}

#[cfg(test)]
mod remote_skew_tests {
    //! What a runner that predates v23 can and cannot see.
    //!
    //! These two tests are a PAIR and neither means anything alone. The first
    //! says the guard works; the second says it is not simply blocking
    //! everything, which is the way a fail-closed guard passes its own test
    //! while being useless.

    use super::*;
    use crate::db::schema::test_conn;
    use crate::routing::{HostCapability, WorkspaceKind};

    /// The exact drain predicate a pre-v23 binary compiles. Written out as a
    /// literal rather than generated, because the whole question is what SQL
    /// that OTHER binary contains — generating it from today's code would test
    /// this build against itself.
    fn legacy_drain(conn: &Connection) -> Vec<String> {
        let sql = "SELECT id FROM tasks
                    WHERE deleted_at IS NULL AND archived_at IS NULL
                      AND status = 'queued'
                      AND ((bg_executor = 'claude' AND bg_allowed = 1)
                        OR (bg_executor = 'codex' AND bg_codex_allowed = 1))
                      AND repo_path IS NOT NULL AND TRIM(repo_path) <> ''";
        let mut stmt = conn.prepare(sql).unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    fn nas() -> HostCapability {
        let mut c = HostCapability::local_default("nas".into(), "the NAS".into());
        c.workspaces = vec![WorkspaceKind::Mirror];
        c.max_tier = crate::runner::ToolTier::ReadOnly;
        c.can_checkpoint = false;
        c
    }

    fn task(conn: &Connection, local: bool, remote: bool) -> Task {
        let t = create(
            conn,
            &NewTask {
                title: "work".into(),
                status: Some("queued".into()),
                bg_allowed: Some(local),
                bg_remote_allowed: Some(remote),
                repo_path: Some("/repo".into()),
                ..Default::default()
            },
        )
        .unwrap();
        t
    }

    /// A task meant only for the NAS is INVISIBLE to a leftover runner on the
    /// Mac. That runner would execute it against the live tree, under the
    /// user's own `~/.claude/settings.json` — which is exactly the reach the
    /// remote tier ceiling exists to withhold.
    #[test]
    fn a_legacy_runner_query_cannot_claim_a_remote_only_task() {
        let conn = test_conn();
        let t = task(&conn, false, true);

        assert!(
            !legacy_drain(&conn).contains(&t.id),
            "a pre-v23 runner could see a task that was never meant to run locally"
        );
        // Nor can a CURRENT local runner, which reads the same column.
        assert!(list_queued_for_background(&conn, &here(), 10)
            .unwrap()
            .is_empty());
        // The NAS, reading its own column, can.
        let seen = list_queued_for_background(&conn, &nas(), 10).unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].id, t.id);
    }

    /// The deliberate opposite, and the reason the guard is not simply "deny
    /// everything". A task marked eligible BOTH ways is claimable by the old
    /// runner too — it runs against the live tree, which is what a current
    /// local runner does anyway, since Live beats Mirror.
    #[test]
    fn a_legacy_runner_still_claims_a_dual_eligible_task() {
        let conn = test_conn();
        let t = task(&conn, true, true);

        assert!(
            legacy_drain(&conn).contains(&t.id),
            "the guard is blocking work it was never meant to block"
        );
        assert_eq!(
            list_queued_for_background(&conn, &here(), 10).unwrap().len(),
            1
        );
        assert_eq!(
            list_queued_for_background(&conn, &nas(), 10).unwrap().len(),
            1
        );
    }

    /// The generated half of failing closed, end to end against real rows: a
    /// read-only host's own SQL cannot surface a task that asks to write, even
    /// when that task is marked remote-eligible.
    #[test]
    fn a_read_only_host_cannot_drain_a_write_task_however_it_is_marked() {
        let conn = test_conn();
        create(
            &conn,
            &NewTask {
                title: "writes".into(),
                status: Some("queued".into()),
                bg_allowed: Some(true),
                bg_remote_allowed: Some(true),
                bg_writes_allowed: Some(true),
                repo_path: Some("/repo".into()),
                ..Default::default()
            },
        )
        .unwrap();

        assert!(
            list_queued_for_background(&conn, &nas(), 10).unwrap().is_empty(),
            "a read-only host drained a task that may write to the tree"
        );
        // The Mac, which declares Write, takes it.
        assert_eq!(
            list_queued_for_background(&conn, &here(), 10).unwrap().len(),
            1
        );
    }

    /// **The front door.** The routing machinery was complete for a while with
    /// no way to reach it: every task said "this Mac only" and nothing could
    /// say otherwise, so a second host could be running and correctly claim
    /// nothing at all. A patch has to actually land in the column.
    #[test]
    fn a_patch_can_mark_a_task_remote_eligible_and_take_it_back() {
        let conn = test_conn();
        let t = task(&conn, true, false);
        assert!(!t.bg_remote_allowed);

        update(
            &conn,
            &t.id,
            &TaskPatch {
                bg_remote_allowed: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(get(&conn, &t.id).unwrap().unwrap().bg_remote_allowed);
        assert!(list_queued_for_background(&conn, &nas(), 10).unwrap().len() == 1);

        update(
            &conn,
            &t.id,
            &TaskPatch {
                bg_remote_allowed: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!get(&conn, &t.id).unwrap().unwrap().bg_remote_allowed);
        assert!(
            list_queued_for_background(&conn, &nas(), 10).unwrap().is_empty(),
            "revoking must take it back out of the remote host's reach"
        );
    }

    /// An empty grant stores NULL, so "no servers" has ONE representation
    /// rather than two that every reader would have to handle.
    #[test]
    fn an_mcp_grant_round_trips_and_clears_to_null() {
        let conn = test_conn();
        let t = task(&conn, true, false);
        assert!(t.bg_mcp_servers.is_none());

        update(&conn, &t.id, &TaskPatch { bg_mcp_servers: Some("vault".into()), ..Default::default() })
            .unwrap();
        assert_eq!(
            get(&conn, &t.id).unwrap().unwrap().bg_mcp_servers.as_deref(),
            Some("vault")
        );

        update(&conn, &t.id, &TaskPatch { bg_mcp_servers: Some("  ".into()), ..Default::default() })
            .unwrap();
        assert!(
            get(&conn, &t.id).unwrap().unwrap().bg_mcp_servers.is_none(),
            "an empty grant must store NULL, not an empty string"
        );
    }

    /// The claim uses the SAME clause as the drain. One listing a task the
    /// other cannot claim is a queue that spins forever with nothing on screen
    /// to explain it.
    #[test]
    fn the_claim_refuses_exactly_what_the_drain_refuses() {
        let conn = test_conn();
        let remote_only = task(&conn, false, true);
        assert!(
            !claim_for_background(&conn, &here(), &remote_only.id).unwrap(),
            "the local host claimed a task its own drain query cannot see"
        );
        assert!(claim_for_background(&conn, &nas(), &remote_only.id).unwrap());
    }
}

#[cfg(test)]
mod executor_tests {
    use super::*;
    use crate::db::schema::test_conn;

    fn enabled(t: &Task) -> bool {
        t.bg_allowed
    }

    /// The raw columns behind the computed flag. Read directly, because the
    /// whole point of the encoding is what a *different binary's* SQL sees.
    fn raw_gates(conn: &Connection, id: &str) -> (bool, bool) {
        conn.query_row(
            "SELECT bg_allowed, bg_codex_allowed FROM tasks WHERE id = ?1",
            params![id],
            |r| Ok((r.get::<_, i64>(0)? != 0, r.get::<_, i64>(1)? != 0)),
        )
        .unwrap()
    }

    fn new_task(conn: &Connection, executor: BackgroundExecutor, enabled: bool) -> Task {
        create(
            conn,
            &NewTask {
                title: format!("{executor} task"),
                repo_path: Some("/tmp".into()),
                bg_allowed: Some(enabled),
                bg_executor: Some(executor),
                ..Default::default()
            },
        )
        .unwrap()
    }

    /// Running out of lease must never block capture.
    ///
    /// The task is created with `short_id = NULL` — unaddressable as "T42" and
    /// otherwise completely normal — and `backfill_short_ids` numbers it the
    /// moment a lease arrives. Refusing the create instead would let a lapsed
    /// lease stop a laptop from taking notes, which is a far worse failure.
    #[test]
    fn a_task_is_still_created_when_the_lease_is_exhausted() {
        let conn = test_conn();
        crate::sync::leases::adopt(&conn, crate::sync::leases::Lease { lo: 1, hi: 1 }).unwrap();

        let numbered = create(&conn, &NewTask { title: "has one".into(), ..Default::default() }).unwrap();
        assert_eq!(numbered.short_id, Some(1));

        let unnumbered = create(&conn, &NewTask { title: "no number yet".into(), ..Default::default() }).unwrap();
        assert_eq!(unnumbered.short_id, None, "out of lease, but still created");
        assert_eq!(get(&conn, &unnumbered.id).unwrap().unwrap().title, "no number yet");

        // A renewed lease heals it, through the path that has existed since v6.
        crate::sync::leases::adopt(&conn, crate::sync::leases::Lease { lo: 50, hi: 99 }).unwrap();
        assert_eq!(backfill_short_ids(&conn).unwrap(), 1);
        assert_eq!(get(&conn, &unnumbered.id).unwrap().unwrap().short_id, Some(50));
    }

    /// And the backfill STOPS when the lease is gone, rather than asking once
    /// per unnumbered row on every `initialize`.
    #[test]
    fn the_backfill_stops_instead_of_spinning_when_out_of_lease() {
        let conn = test_conn();
        crate::sync::leases::adopt(&conn, crate::sync::leases::Lease { lo: 1, hi: 1 }).unwrap();
        for i in 0..5 {
            create(&conn, &NewTask { title: format!("t{i}"), ..Default::default() }).unwrap();
        }
        // One number existed; four tasks have none and no lease remains.
        assert_eq!(backfill_short_ids(&conn).unwrap(), 0);
        let unnumbered: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE short_id IS NULL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(unnumbered, 4);
    }

    #[test]
    fn a_new_task_defaults_to_claude() {
        let conn = test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "plain".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(t.bg_executor, BackgroundExecutor::Claude);
        assert!(!t.bg_allowed);
    }

    /// The encoding, asserted at the column level: an enabled Codex task must
    /// leave the legacy `bg_allowed` column at 0, because that column is what a
    /// pre-v8 runner queries.
    #[test]
    fn the_enabled_flag_lands_in_the_column_belonging_to_the_executor() {
        let conn = test_conn();

        let claude = new_task(&conn, BackgroundExecutor::Claude, true);
        assert!(enabled(&claude));
        assert_eq!(raw_gates(&conn, &claude.id), (true, false));

        let codex = new_task(&conn, BackgroundExecutor::Codex, true);
        assert!(enabled(&codex));
        assert_eq!(
            raw_gates(&conn, &codex.id),
            (false, true),
            "an enabled Codex task must be invisible to a `bg_allowed = 1` query"
        );

        let off = new_task(&conn, BackgroundExecutor::Codex, false);
        assert!(!enabled(&off));
        assert_eq!(raw_gates(&conn, &off.id), (false, false));
    }

    /// Switching executor must MOVE the flag, not leave it behind. Leaving it
    /// gives the worst of both: the app shows the task as disabled while an old
    /// runner still sees it as claimable Claude work.
    #[test]
    fn switching_executor_carries_the_enabled_flag_across() {
        let conn = test_conn();
        let t = new_task(&conn, BackgroundExecutor::Claude, true);

        let t = update(
            &conn,
            &t.id,
            &TaskPatch {
                bg_executor: Some(BackgroundExecutor::Codex),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(t.bg_executor, BackgroundExecutor::Codex);
        assert!(enabled(&t), "the task was enabled before the switch");
        assert_eq!(raw_gates(&conn, &t.id), (false, true));

        // …and back again.
        let t = update(
            &conn,
            &t.id,
            &TaskPatch {
                bg_executor: Some(BackgroundExecutor::Claude),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(enabled(&t));
        assert_eq!(raw_gates(&conn, &t.id), (true, false));
    }

    #[test]
    fn switching_executor_on_a_disabled_task_leaves_it_disabled() {
        let conn = test_conn();
        let t = new_task(&conn, BackgroundExecutor::Claude, false);
        let t = update(
            &conn,
            &t.id,
            &TaskPatch {
                bg_executor: Some(BackgroundExecutor::Codex),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!enabled(&t));
        assert_eq!(raw_gates(&conn, &t.id), (false, false));
    }

    /// Toggling the flag alone must respect the executor already set — writing
    /// `bg_allowed` unconditionally is the mis-dispatch the split prevents.
    #[test]
    fn enabling_a_codex_task_never_writes_the_legacy_column() {
        let conn = test_conn();
        let t = new_task(&conn, BackgroundExecutor::Codex, false);
        let t = update(
            &conn,
            &t.id,
            &TaskPatch {
                bg_allowed: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(enabled(&t));
        assert_eq!(raw_gates(&conn, &t.id), (false, true));
    }

    /// **Intent must NOT be swept into the joint executor/flag/model
    /// resolution**, however much it looks like it belongs beside three fields
    /// that are resolved together.
    ///
    /// Arming a review preselects the OTHER agent, so the executor switch and
    /// the intent write arrive in the same gesture — and folding intent into
    /// that block would disarm the review at the exact moment the user set it
    /// up, silently, with the control still reading "Review".
    #[test]
    fn switching_the_executor_leaves_the_queued_intent_alone() {
        let conn = test_conn();
        let t = create(&conn, &NewTask { title: "x".into(), ..Default::default() }).unwrap();
        let t = update(
            &conn,
            &t.id,
            &TaskPatch {
                bg_intent: Some(crate::intent::BackgroundIntent::Review),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(t.bg_intent.is_review());

        // The switch a review preselects — both in one patch, as the UI sends
        // it, and then again on its own.
        let t = update(
            &conn,
            &t.id,
            &TaskPatch {
                bg_executor: Some(BackgroundExecutor::Codex),
                bg_writes_allowed: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(t.bg_intent.is_review(), "the review must survive its own preselection");
        assert_eq!(t.bg_executor, BackgroundExecutor::Codex);

        let t = update(
            &conn,
            &t.id,
            &TaskPatch { bg_allowed: Some(true), ..Default::default() },
        )
        .unwrap();
        assert!(t.bg_intent.is_review(), "queueing must not disarm it either");
    }

    /// A model name belongs to one vendor. Carrying `claude-sonnet-5` into a
    /// Codex run means a spawn failure with nothing to do with the task.
    #[test]
    fn switching_executor_clears_an_incompatible_model() {
        let conn = test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "x".into(),
                model: Some("claude-opus-5".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(t.model.as_deref(), Some("claude-opus-5"));

        let t = update(
            &conn,
            &t.id,
            &TaskPatch {
                bg_executor: Some(BackgroundExecutor::Codex),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(t.model, None, "a Claude model must not survive into a Codex run");
    }

    /// …unless the caller replaces it in the same call, which is them saying
    /// deliberately which model the new executor should use.
    #[test]
    fn a_model_supplied_with_the_switch_wins_over_the_clear() {
        let conn = test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "x".into(),
                model: Some("claude-opus-5".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let t = update(
            &conn,
            &t.id,
            &TaskPatch {
                bg_executor: Some(BackgroundExecutor::Codex),
                model: Some(Some("gpt-5.2-codex".into())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(t.model.as_deref(), Some("gpt-5.2-codex"));
    }

    /// Re-affirming the SAME executor is not a switch, so it must not wipe a
    /// model the user deliberately set. (The UI writes the executor on every
    /// radio change, including a no-op one.)
    #[test]
    fn re_setting_the_same_executor_keeps_the_model() {
        let conn = test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "x".into(),
                model: Some("claude-opus-5".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let t = update(
            &conn,
            &t.id,
            &TaskPatch {
                bg_executor: Some(BackgroundExecutor::Claude),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(t.model.as_deref(), Some("claude-opus-5"));
    }

    /// Both executors' queued work has to be handed out, and the two queries
    /// that decide it must agree — a task one lists and the other can't claim
    /// is a queue that spins with nothing on screen to explain it.
    #[test]
    fn the_drain_query_and_the_claim_agree_for_both_executors() {
        let conn = test_conn();
        let mut ids = Vec::new();
        for executor in BackgroundExecutor::ALL {
            let t = create(
                &conn,
                &NewTask {
                    title: format!("{executor} queued"),
                    repo_path: Some("/tmp".into()),
                    bg_allowed: Some(true),
                    bg_executor: Some(executor),
                    status: Some("queued".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            ids.push((executor, t.id));
        }

        let listed = list_queued_for_background(&conn, &here(), 10).unwrap();
        for (executor, id) in &ids {
            assert!(
                listed.iter().any(|t| &t.id == id),
                "{executor} task was not listed for the runner"
            );
            assert!(
                claim_for_background(&conn, &here(), id).unwrap(),
                "{executor} task was listed but could not be claimed"
            );
        }
    }

    /// The same disabled-task rule for both: an opted-out Codex task is not
    /// work, exactly as an opted-out Claude one isn't.
    #[test]
    fn a_disabled_task_of_either_executor_is_never_handed_out() {
        let conn = test_conn();
        for executor in BackgroundExecutor::ALL {
            let t = create(
                &conn,
                &NewTask {
                    title: format!("{executor} off"),
                    repo_path: Some("/tmp".into()),
                    bg_allowed: Some(false),
                    bg_executor: Some(executor),
                    status: Some("queued".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(list_queued_for_background(&conn, &here(), 10)
                .unwrap()
                .iter()
                .all(|q| q.id != t.id));
            assert!(!claim_for_background(&conn, &here(), &t.id).unwrap());
        }
    }

    /// Invariant 13 extends to Codex: a trashed task must not reach an
    /// unattended agent, whichever agent that is.
    #[test]
    fn a_trashed_codex_task_is_not_handed_to_the_runner() {
        let conn = test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "codex".into(),
                repo_path: Some("/tmp".into()),
                bg_allowed: Some(true),
                bg_executor: Some(BackgroundExecutor::Codex),
                status: Some("queued".into()),
                ..Default::default()
            },
        )
        .unwrap();
        trash(&conn, &t.id).unwrap();
        assert!(list_queued_for_background(&conn, &here(), 10).unwrap().is_empty());
        assert!(!claim_for_background(&conn, &here(), &t.id).unwrap());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    static TRACED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// The exact `UPDATE tasks SET …` a patch produces, pinned.
    ///
    /// # Why the STRING and not just the resulting row
    ///
    /// `update` writes fields the caller never asked for — `status` and
    /// `blocked_reason` resolve together, a `project` change can prefill
    /// `repo_path`, a `bg_executor` change writes three columns and clears
    /// `model`, a `due_at` change nulls `reminded_at`. All of those are pushed
    /// onto one `Vec` and joined, so the ORDER of the pushes decides what a
    /// duplicated assignment ends up as.
    ///
    /// Tests that assert on the resulting row only catch reorderings in the
    /// combinations somebody thought to write a test for. This catches any
    /// reordering at all, including in combinations that do not exist yet —
    /// which is the point, because the ones that do not exist yet are exactly
    /// the ones nobody will think to check.
    ///
    /// If this fails and the change was deliberate, read the new string and
    /// make sure a later assignment did not just silently win over an earlier
    /// one, then update the expectation.
    #[test]
    fn the_update_statement_keeps_its_assignment_order() {
        let mut conn = test_conn();
        let t = create(&conn, &NewTask { title: "t".into(), ..Default::default() }).unwrap();

        TRACED.lock().unwrap().clear();
        conn.trace(Some(|sql: &str| {
            if sql.starts_with("UPDATE tasks SET") {
                TRACED.lock().unwrap().push(sql.to_string());
            }
        }));

        // A patch touching the ordering-sensitive groups at once: the joint
        // status/blocked pair, the executor trio, and the due/reminder pair.
        update(
            &conn,
            &t.id,
            &TaskPatch {
                title: Some("renamed".into()),
                priority: Some(0),
                bg_executor: Some(crate::executor::BackgroundExecutor::Codex),
                bg_allowed: Some(true),
                due_at: Some(Some("2026-01-01T00:00:00.000Z".into())),
                ..Default::default()
            },
        )
        .unwrap();
        conn.trace(None);

        let traced = TRACED.lock().unwrap().clone();
        assert_eq!(traced.len(), 1, "expected exactly one UPDATE, got {traced:?}");
        let sql = &traced[0];

        // The assigned COLUMNS, in order. `trace` expands bound values, so
        // asserting the whole clause would also pin every literal in the patch
        // and fail for reasons that have nothing to do with ordering.
        let sets = sql
            .trim_start_matches("UPDATE tasks SET ")
            .split(" WHERE ")
            .next()
            .unwrap();
        let columns: Vec<&str> = sets
            .split(", ")
            .map(|a| a.split(" = ").next().unwrap())
            .collect();

        assert_eq!(
            columns,
            [
                "title",
                "priority",
                "due_at",
                "bg_executor",
                "bg_allowed",
                "bg_codex_allowed",
                "model",
                "reminded_at",
                "updated_at",
            ],
            "the SET clause changed shape — see this test's doc comment"
        );

        // The three writes the caller never asked for, spelled out so the
        // reason each is in the list survives a future reordering.
        assert!(sets.contains("model = NULL"), "switching executor must clear the model");
        assert!(sets.contains("reminded_at = NULL"), "a new due date must re-arm the reminder");
        assert!(
            sets.contains("bg_allowed = 0") && sets.contains("bg_codex_allowed = 1"),
            "the opt-in is PER EXECUTOR (invariant 15): asking for `bg_allowed` while \
             switching to Codex must set the Codex flag, not Claude's — got: {sets}"
        );
    }

    use super::*;
    use crate::db::schema::test_conn;

    fn seed(conn: &Connection, title: &str) -> Task {
        create(
            conn,
            &NewTask {
                title: title.into(),
                ..Default::default()
            },
        )
        .unwrap()
    }

    // ── project → workspace (invariant 17) ────────────────────────────────

    /// The real shape: a project whose name is nothing like its directory, and
    /// a second repo it also owns, so "the primary" is a real choice.
    fn seed_portfolio(conn: &Connection) {
        use crate::db::projects::add_repo;
        add_repo(conn, "pocketbuddy", "/tmp/cx-repo/finance-app", true).unwrap();
        add_repo(conn, "pocketbuddy", "/tmp/cx-repo/xcode/pocketbuddy", false).unwrap();
        add_repo(conn, "cxventures", "/tmp/cx-repo/cxventures", true).unwrap();
    }

    fn filed(conn: &Connection, title: &str, project: &str, repo: Option<&str>) -> Task {
        create(
            conn,
            &NewTask {
                title: title.into(),
                project: Some(project.into()),
                repo_path: repo.map(str::to_string),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn filing_into_a_project_inherits_its_primary_repo() {
        let conn = test_conn();
        seed_portfolio(&conn);

        // The quick-add bar's `#pocketbuddy` — a project and nothing else. This
        // is the case that shipped blank: the mapping existed and nothing read
        // it, so the launch console said "Needs repo" for a project whose repo
        // the app knew.
        let t = filed(&conn, "explore creatify for UGC", "pocketbuddy", None);
        assert_eq!(t.repo_path.as_deref(), Some("/tmp/cx-repo/finance-app"));

        // Matching is by NAME, not by directory — the whole reason the mapping
        // is stored rather than derived.
        let t = filed(&conn, "typed with odd case", "PocketBuddy", None);
        assert_eq!(t.repo_path.as_deref(), Some("/tmp/cx-repo/finance-app"));
    }

    #[test]
    fn an_explicit_repo_outranks_the_project_default() {
        let conn = test_conn();
        seed_portfolio(&conn);

        // The iOS half is chosen on purpose. Snapping it back to the web repo
        // would make the mapping actively destructive rather than helpful.
        let t = filed(
            &conn,
            "fix the widget",
            "pocketbuddy",
            Some("/tmp/cx-repo/xcode/pocketbuddy"),
        );
        assert_eq!(t.repo_path.as_deref(), Some("/tmp/cx-repo/xcode/pocketbuddy"));

        // Cross-project work: filed against another project's repo deliberately.
        let t = filed(
            &conn,
            "wire the webhook pocketbuddy calls",
            "pocketbuddy",
            Some("/tmp/cx-repo/cxventures"),
        );
        assert_eq!(t.repo_path.as_deref(), Some("/tmp/cx-repo/cxventures"));
    }

    #[test]
    fn an_unmapped_project_leaves_the_workspace_blank() {
        let conn = test_conn();
        seed_portfolio(&conn);
        // Unmapped resolves to NOTHING, never to a guess — the same rule
        // `for_repo_path` follows in the other direction. Blank is visible and
        // fixable; a confidently wrong workspace is neither.
        let t = filed(&conn, "read the contract", "godly", None);
        assert!(t.repo_path.is_none());
    }

    #[test]
    fn setting_a_project_fills_a_blank_workspace() {
        let conn = test_conn();
        seed_portfolio(&conn);
        let t = seed(&conn, "unfiled thought");
        assert!(t.repo_path.is_none());

        // The route a row dragged onto a project group takes.
        let after = update(
            &conn,
            &t.id,
            &TaskPatch {
                project: Some(Some("pocketbuddy".into())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(after.repo_path.as_deref(), Some("/tmp/cx-repo/finance-app"));
    }

    #[test]
    fn an_explicit_repo_in_the_same_patch_still_wins() {
        let conn = test_conn();
        seed_portfolio(&conn);
        let t = seed(&conn, "unfiled thought");

        let after = update(
            &conn,
            &t.id,
            &TaskPatch {
                project: Some(Some("pocketbuddy".into())),
                repo_path: Some(Some("/tmp/cx-repo/xcode/pocketbuddy".into())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            after.repo_path.as_deref(),
            Some("/tmp/cx-repo/xcode/pocketbuddy"),
            "a workspace named in the same patch is the caller's decision"
        );
    }

    /// The same rule as `an_explicit_repo_in_the_same_patch_still_wins` above,
    /// on the other branch of `inherit` — that one starts from an UNFILED task,
    /// so it exercises the fill-a-blank path. This is the **replace** path, and
    /// it is the one ⌘Z traverses.
    ///
    /// `taskStore::inversePatch` sends the old `project` AND the old
    /// `repo_path` together, because the move rewrote the workspace as a side
    /// effect. Both land in one `UPDATE`: the prefill pushes `repo_path = ?`
    /// from inside the project branch, and `push_opt` pushes the caller's own
    /// afterwards. SQLite takes the LAST assignment, so the explicit value
    /// wins — which is what lets undo restore a SECONDARY repo the prefill
    /// would never have chosen. Reorder those two pushes and undo silently
    /// starts snapping tasks onto whichever repo the project calls primary.
    #[test]
    fn an_explicit_repo_in_the_same_patch_beats_the_project_prefill() {
        let conn = test_conn();
        seed_portfolio(&conn);

        // The iOS half — deliberately NOT pocketbuddy's primary repo, so a
        // prefill and an honest restore give different answers.
        let t = filed(
            &conn,
            "widget work",
            "pocketbuddy",
            Some("/tmp/cx-repo/xcode/pocketbuddy"),
        );
        let moved = update(
            &conn,
            &t.id,
            &TaskPatch {
                project: Some(Some("cxventures".into())),
                repo_path: Some(Some("/tmp/cx-repo/cxventures".into())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(moved.repo_path.as_deref(), Some("/tmp/cx-repo/cxventures"));

        // Now the inverse, exactly as `taskStore::inversePatch` builds it.
        let undone = update(
            &conn,
            &t.id,
            &TaskPatch {
                project: Some(Some("pocketbuddy".into())),
                repo_path: Some(Some("/tmp/cx-repo/xcode/pocketbuddy".into())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(undone.project.as_deref(), Some("pocketbuddy"));
        assert_eq!(
            undone.repo_path.as_deref(),
            Some("/tmp/cx-repo/xcode/pocketbuddy"),
            "the prefill overwrote the explicit repo — undo would snap the task \
             onto pocketbuddy's PRIMARY repo instead of the one it was on"
        );
    }

    #[test]
    fn moving_a_task_repoints_only_the_old_projects_own_repo() {
        let conn = test_conn();
        seed_portfolio(&conn);

        // (a) A repo owned by the project being LEFT is stale the moment the
        // task moves — keeping it would dispatch cxventures work into
        // finance-app, which is the footgun this branch exists for.
        let t = filed(&conn, "moved work", "pocketbuddy", None);
        assert_eq!(t.repo_path.as_deref(), Some("/tmp/cx-repo/finance-app"));
        let after = update(
            &conn,
            &t.id,
            &TaskPatch {
                project: Some(Some("cxventures".into())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(after.repo_path.as_deref(), Some("/tmp/cx-repo/cxventures"));

        // (b) A repo owned by NEITHER project is a deliberate cross-project
        // choice and survives the move untouched.
        let t = filed(&conn, "vault work", "pocketbuddy", Some("/tmp/cx-repo/unmapped"));
        let after = update(
            &conn,
            &t.id,
            &TaskPatch {
                project: Some(Some("cxventures".into())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(after.repo_path.as_deref(), Some("/tmp/cx-repo/unmapped"));

        // (c) Re-committing the SAME project must not re-point anything — this
        // is the whole reason the replace is gated on the project changing. The
        // detail pane's project field writes on blur, so without the gate the
        // iOS half would snap back to the web repo on every glance at it.
        let t = filed(
            &conn,
            "widget work",
            "pocketbuddy",
            Some("/tmp/cx-repo/xcode/pocketbuddy"),
        );
        let after = update(
            &conn,
            &t.id,
            &TaskPatch {
                project: Some(Some("PocketBuddy".into())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            after.repo_path.as_deref(),
            Some("/tmp/cx-repo/xcode/pocketbuddy"),
            "case is not a move"
        );
    }

    #[test]
    fn clearing_the_project_leaves_the_workspace_alone() {
        let conn = test_conn();
        seed_portfolio(&conn);
        let t = filed(&conn, "unfiled but still code work", "pocketbuddy", None);

        let after = update(
            &conn,
            &t.id,
            &TaskPatch {
                project: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(after.project.is_none());
        assert_eq!(
            after.repo_path.as_deref(),
            Some("/tmp/cx-repo/finance-app"),
            "unfiling a task must not also strip the workspace it can still run in"
        );
    }

    // ── T-numbers ─────────────────────────────────────────────────────────

    #[test]
    fn resolve_ref_accepts_t_numbers_and_uuids() {
        let conn = test_conn();
        let t = seed(&conn, "the task");
        let n = t.short_id.expect("create must assign a T-number");

        for form in [format!("T{n}"), format!("t{n}"), format!(" T{n} "), t.id.clone()] {
            assert_eq!(
                resolve_ref(&conn, &form).unwrap().map(|r| r.id).as_deref(),
                Some(t.id.as_str()),
                "'{form}' should resolve to the task"
            );
        }

        // Misses resolve to None rather than erroring, so a caller can report
        // "not found" for a typo the same way it does for a stale UUID.
        for miss in ["T999999", "nope", "", "T", "T4x", "42"] {
            assert!(
                resolve_ref(&conn, miss).unwrap().is_none(),
                "'{miss}' must not resolve to anything"
            );
        }

        // A bare number is NOT a reference — it's ambiguous with a search term.
        assert_eq!(parse_short_ref("42"), None);
        assert_eq!(parse_short_ref("T42"), Some(42));
        assert_eq!(parse_short_ref("t7"), Some(7));

        // Unfiltered, like `get` — a reference written down last month has to
        // keep meaning something after the task is trashed.
        trash(&conn, &t.id).unwrap();
        assert!(resolve_ref(&conn, &format!("T{n}")).unwrap().is_some());
    }

    /// The v4 backfill ran once and nothing maintains `projects` since, so a
    /// project arriving via the MCP would otherwise have tasks but no sidebar
    /// entry and no row on the GSD board.
    #[test]
    fn create_ensures_a_projects_row() {
        let conn = test_conn();
        create(
            &conn,
            &NewTask {
                title: "filed from a session".into(),
                project: Some("  brand-new-project  ".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let all = crate::db::projects::list(&conn, true).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "brand-new-project", "the name must be trimmed to match tasks.project");
        assert_eq!(all[0].open_count, 1, "the text join must line up with the row");

        // A second task in the same project must not create a duplicate, and a
        // case variant must reuse the existing row rather than erroring.
        create(
            &conn,
            &NewTask {
                title: "another".into(),
                project: Some("Brand-New-Project".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(crate::db::projects::list(&conn, true).unwrap().len(), 1);

        // A task with no project must not conjure one.
        seed(&conn, "unfiled");
        assert_eq!(crate::db::projects::list(&conn, true).unwrap().len(), 1);
    }

    // ── status vocabulary (v12) ───────────────────────────────────────────

    /// The pre-v12 spellings must keep resolving. Three binaries share this DB
    /// and don't upgrade together, so a leftover `cxtasks-mcp` goes on sending
    /// `"next"` after the app speaks `"todo"` — rejecting it would make
    /// `/cxtasks:task` start failing on a vocabulary that was correct when that
    /// binary was built.
    #[test]
    fn the_pre_v12_status_spellings_map_forward() {
        assert_eq!(normalize_status("next"), Some("todo"));
        assert_eq!(normalize_status("needs_review"), Some("in_review"));
        assert_eq!(normalize_status("review"), Some("in_review"));
        assert_eq!(normalize_status(" Todo "), Some("todo"));
        assert_eq!(normalize_status("IN_REVIEW"), Some("in_review"));
        assert_eq!(normalize_status("nonsense"), None);
    }

    /// `cancelled` maps to nothing on purpose. `done` would claim the work
    /// finished and `todo` would silently put it back on the board, so the
    /// caller is told where it went instead of being quietly given one of them.
    #[test]
    fn cancelled_is_refused_and_says_where_it_went() {
        assert_eq!(normalize_status("cancelled"), None);
        let conn = test_conn();
        let t = seed(&conn, "drop me").id;
        let err = update(
            &conn,
            &t,
            &TaskPatch {
                status: Some("cancelled".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("archive"),
            "the error must point at the replacement, got: {msg}"
        );
    }

    /// v27: a hold is written or it isn't, and the status underneath is
    /// untouched. The old `blocked` status is accepted from a pre-v27 client
    /// and folded to `todo`, reason intact — nothing sent by the iOS app or an
    /// old plugin copy may bounce.
    #[test]
    fn a_hold_is_independent_of_status() {
        let conn = test_conn();
        let t = seed(&conn, "waiting on someone").id;

        // Hold an in-progress task without moving it.
        update(&conn, &t, &TaskPatch { status: Some("in_progress".into()), ..Default::default() })
            .unwrap();
        let held = update(
            &conn,
            &t,
            &TaskPatch {
                blocked_reason: Some(Some("waiting on Dana's sign-off".into())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(held.status, "in_progress", "a hold must not rewrite progress");
        assert_eq!(held.blocked_reason.as_deref(), Some("waiting on Dana's sign-off"));

        // Whitespace is a release, not a reason.
        let released = update(
            &conn,
            &t,
            &TaskPatch { blocked_reason: Some(Some("   ".into())), ..Default::default() },
        )
        .unwrap();
        assert!(released.blocked_reason.is_none());
        assert_eq!(released.status, "in_progress");

        // The legacy spelling: status "blocked" + reason → todo + hold.
        let legacy = update(
            &conn,
            &t,
            &TaskPatch {
                status: Some("blocked".into()),
                blocked_reason: Some(Some("waiting on an API key".into())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(legacy.status, "todo");
        assert_eq!(legacy.blocked_reason.as_deref(), Some("waiting on an API key"));
        assert!(!STATUSES.contains(&"blocked"), "blocked is not a status any more");
    }

    /// The two transitions that release a hold for the caller: closing (a done
    /// task waits on nothing, and a recurrence's next instance must not inherit
    /// it) and queueing (re-queueing IS the un-park gesture for the runner's
    /// own "Background run refused" holds).
    #[test]
    fn done_and_queued_release_the_hold_but_progress_does_not() {
        let conn = test_conn();
        let t = seed(&conn, "waiting on someone").id;
        let hold = || TaskPatch {
            blocked_reason: Some(Some("waiting on an API key".into())),
            ..Default::default()
        };

        update(&conn, &t, &hold()).unwrap();
        let moved = update(
            &conn,
            &t,
            &TaskPatch { status: Some("in_progress".into()), ..Default::default() },
        )
        .unwrap();
        assert_eq!(
            moved.blocked_reason.as_deref(),
            Some("waiting on an API key"),
            "starting work on a held task does not silently drop what it waits on"
        );

        update(&conn, &t, &hold()).unwrap();
        let queued = update(
            &conn,
            &t,
            &TaskPatch { status: Some("queued".into()), ..Default::default() },
        )
        .unwrap();
        assert!(queued.blocked_reason.is_none(), "queued must be drainable");

        // And via the completion path, which doesn't go through the UI at all.
        update(&conn, &t, &hold()).unwrap();
        assert!(complete(&conn, &t).unwrap().blocked_reason.is_none());
    }

    /// Editing an unrelated field on a held task must not touch the hold —
    /// otherwise every patch path in the app has to know about this column.
    #[test]
    fn patching_another_field_keeps_an_existing_blocked_reason() {
        let conn = test_conn();
        let t = seed(&conn, "waiting on someone").id;
        update(
            &conn,
            &t,
            &TaskPatch {
                blocked_reason: Some(Some("waiting on legal".into())),
                ..Default::default()
            },
        )
        .unwrap();

        let renamed = update(
            &conn,
            &t,
            &TaskPatch {
                title: Some("waiting on someone (renamed)".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(renamed.blocked_reason.as_deref(), Some("waiting on legal"));
    }

    /// `queued` stays a real status — it is the runner's atomic claim key, and
    /// invariant 15 turns on old and new binaries agreeing about that query — so
    /// the *only* thing separating it from the rest is that it is not offered.
    /// If it ever appears in `USER_STATUSES`, a human can drop a task into the
    /// drain queue past every check `queue_background` runs first.
    #[test]
    fn queued_is_a_real_status_but_never_a_human_choice() {
        assert!(STATUSES.contains(&"queued"));
        assert!(!USER_STATUSES.contains(&"queued"));
        for s in USER_STATUSES {
            assert!(STATUSES.contains(&s), "{s} is offered but is not a status");
        }
    }

    // ── trash / archive ───────────────────────────────────────────────────

    /// **The test this whole feature lives or dies by.**
    ///
    /// Soft delete fails one call site at a time: forget the filter in the
    /// reminder scan and a deleted task still notifies you; forget it in the
    /// background drain and a deleted task gets handed to an unattended agent.
    /// So this asserts absence from *every* function that hands a task to a
    /// human or an agent, in one place. A new listing query belongs in this
    /// list — if it isn't here, nothing stops it leaking trashed work.
    #[test]
    fn a_trashed_task_disappears_from_every_path_that_hands_out_work() {
        let conn = test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "leaky".into(),
                status: Some("queued".into()),
                project: Some("cxtasks".into()),
                due_at: Some("2020-01-01T00:00:00.000Z".into()),
                defer_until: Some("2020-01-01T00:00:00.000Z".into()),
                repo_path: Some("/tmp/repo".into()),
                bg_allowed: Some(true),
                ..Default::default()
            },
        )
        .unwrap();

        // Present everywhere first, or the assertions below prove nothing.
        assert!(!list(&conn, &ListFilters::default()).unwrap().is_empty());
        assert!(next_task(&conn, None).unwrap().is_some());
        assert!(!list_due_for_reminder(&conn).unwrap().is_empty());
        assert!(!list_undeferred(&conn).unwrap().is_empty());
        assert!(!list_queued_for_background(&conn, &here(), 10).unwrap().is_empty());
        assert!(!list_projects(&conn).unwrap().is_empty());
        // The GSD board is a hand-out surface too — it names the next task to
        // work on — so it belongs in this list, not in a test of its own.
        let board_row = |conn: &Connection| {
            crate::db::projects::board(conn)
                .unwrap()
                .into_iter()
                .find(|r| r.project == "cxtasks")
                .expect("the project should have a board row either way")
        };
        assert_eq!(board_row(&conn).open_count, 1);
        assert!(board_row(&conn).next_task_title.is_some());
        // Mission Control's feed is a hand-out surface as well: it renders a
        // trashed task's run as a live card with a review affordance on it.
        crate::db::runs::start(&conn, &t.id, "background", None, crate::executor::BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        assert!(!crate::db::runs::recent_with_tasks(&conn, 10).unwrap().is_empty());

        trash(&conn, &t.id).unwrap();

        assert!(list(&conn, &ListFilters::default()).unwrap().is_empty(), "list");
        assert!(next_task(&conn, None).unwrap().is_none(), "next_task");
        assert!(list_due_for_reminder(&conn).unwrap().is_empty(), "reminders");
        assert!(list_undeferred(&conn).unwrap().is_empty(), "undeferred");
        assert!(
            list_queued_for_background(&conn, &here(), 10).unwrap().is_empty(),
            "background queue — a trashed task must never reach an agent"
        );
        assert!(list_projects(&conn).unwrap().is_empty(), "project counts");
        assert_eq!(view_counts(&conn).unwrap().inbox, 0, "sidebar badges");
        assert_eq!(board_row(&conn).open_count, 0, "GSD board counts");
        assert!(
            board_row(&conn).next_task_title.is_none(),
            "the board must not hand out a trashed task as what to work on next"
        );
        assert!(
            !claim_for_background(&conn, &here(), &t.id).unwrap(),
            "claim is the last gate before an agent starts and must re-check"
        );

        // …but it is still fetchable by id, which is what restore needs.
        assert!(get(&conn, &t.id).unwrap().is_some());
        assert_eq!(list_view(&conn, View::Trash).len(), 1);
    }

    fn list_view(conn: &Connection, view: View) -> Vec<Task> {
        list(
            conn,
            &ListFilters {
                view: Some(view),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn restore_puts_a_task_back_exactly_as_it_was() {
        let conn = test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "mid-flight".into(),
                status: Some("in_progress".into()),
                ..Default::default()
            },
        )
        .unwrap();
        trash(&conn, &t.id).unwrap();
        let back = restore(&conn, &t.id).unwrap();
        assert!(back.deleted_at.is_none());
        assert_eq!(
            back.status, "in_progress",
            "status must survive the round trip — that's why trash isn't a status"
        );
        assert_eq!(list_view(&conn, View::Trash).len(), 0);
        assert_eq!(list(&conn, &ListFilters::default()).unwrap().len(), 1);
    }

    #[test]
    fn trashing_a_parent_takes_its_subtree_and_restore_brings_it_back() {
        let conn = test_conn();
        let parent = seed(&conn, "parent");
        let child = create(
            &conn,
            &NewTask {
                title: "child".into(),
                parent_id: Some(parent.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let grandchild = create(
            &conn,
            &NewTask {
                title: "grandchild".into(),
                parent_id: Some(child.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();

        trash(&conn, &parent.id).unwrap();
        for id in [&parent.id, &child.id, &grandchild.id] {
            assert!(
                get(&conn, id).unwrap().unwrap().deleted_at.is_some(),
                "the whole subtree goes, not just the row you clicked"
            );
        }
        // One entry in the Trash, not three — the children went as a batch.
        assert_eq!(list_view(&conn, View::Trash).len(), 1);

        restore(&conn, &parent.id).unwrap();
        for id in [&parent.id, &child.id, &grandchild.id] {
            assert!(get(&conn, id).unwrap().unwrap().deleted_at.is_none());
        }
    }

    /// Restore always works from the **top** of the batch, which is the only
    /// reason `unstamp_batch` walks upward before walking down. Restoring just
    /// the row you clicked would put a grandchild back underneath a parent
    /// still in the Trash — visible in no view at all, since the board lists
    /// batch roots and this one's root isn't on it.
    #[test]
    fn restoring_from_inside_a_batch_brings_back_the_whole_batch() {
        let conn = test_conn();
        let parent = seed(&conn, "parent");
        let child = create(
            &conn,
            &NewTask {
                title: "child".into(),
                parent_id: Some(parent.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let grandchild = create(
            &conn,
            &NewTask {
                title: "grandchild".into(),
                parent_id: Some(child.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        trash(&conn, &parent.id).unwrap();

        // Restore the deepest node, not the root that was trashed.
        restore(&conn, &grandchild.id).unwrap();

        for (label, id) in [
            ("parent", &parent.id),
            ("child", &child.id),
            ("grandchild", &grandchild.id),
        ] {
            assert!(
                get(&conn, id).unwrap().unwrap().deleted_at.is_none(),
                "{label} should have come back with the batch"
            );
        }
        assert_eq!(list_view(&conn, View::Trash).len(), 0);
        assert_eq!(list(&conn, &ListFilters::default()).unwrap().len(), 1);
    }

    /// The reason a batch has an id of its own rather than being keyed by when
    /// it happened.
    ///
    /// This one trashes back to back and depends on the two calls actually
    /// landing in the same instant to exercise the bug — which they did, ~4 runs
    /// in 5, back when the key was the timestamp. That makes it a *sampling* of
    /// the failure, so
    /// `two_deletions_at_the_same_instant_are_still_two_batches` pins the same
    /// property deterministically. Both are kept: this is the shape a user hits,
    /// that one is the shape that can't rot.
    #[test]
    fn restoring_a_parent_does_not_resurrect_a_separately_trashed_child() {
        let conn = test_conn();
        let parent = seed(&conn, "parent");
        let keep = create(
            &conn,
            &NewTask {
                title: "still wanted".into(),
                parent_id: Some(parent.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let gone = create(
            &conn,
            &NewTask {
                title: "deliberately deleted earlier".into(),
                parent_id: Some(parent.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();

        trash(&conn, &gone.id).unwrap(); // deleted on its own, first…
        trash(&conn, &parent.id).unwrap(); // …then the parent, separately
        restore(&conn, &parent.id).unwrap();

        assert!(get(&conn, &parent.id).unwrap().unwrap().deleted_at.is_none());
        assert!(get(&conn, &keep.id).unwrap().unwrap().deleted_at.is_none());
        assert!(
            get(&conn, &gone.id).unwrap().unwrap().deleted_at.is_some(),
            "a child trashed in its own batch must stay trashed"
        );
    }

    /// Build the shape the collision is actually reachable in: a parent with a
    /// child that was already trashed on its own. Returns `(parent, keep, gone)`.
    ///
    /// Two *unrelated* deletions can never merge however identical their stamps
    /// — the batch walk descends by `parent_id`, so a second root is never
    /// reached. The merge needs a batch whose downward walk *passes through* a
    /// row from an earlier one, which is what this sets up. Getting that wrong
    /// is how you write a same-instant test that passes against the broken
    /// mechanism.
    fn parent_over_a_separately_hidden_child(conn: &Connection) -> (Task, Task, Task) {
        let parent = seed(conn, "parent");
        let mut child = |title: &str| {
            create(
                conn,
                &NewTask {
                    title: title.into(),
                    parent_id: Some(parent.id.clone()),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        (parent.clone(), child("still wanted"), child("deleted earlier"))
    }

    /// Two deletions sharing one timestamp are still two batches.
    ///
    /// Forces the collision instead of racing for it: every row is rewritten to
    /// one pinned instant, which is exactly what the clock produced on its own
    /// whenever two `trash()` calls landed in the same millisecond — a loop over
    /// the MCP, or a fast multi-delete. Under the v5 mechanism "same instant"
    /// *was* "same batch", so restoring the parent swept the earlier deletion
    /// back up with it.
    #[test]
    fn two_deletions_at_the_same_instant_are_still_two_batches() {
        let conn = test_conn();
        let (parent, keep, gone) = parent_over_a_separately_hidden_child(&conn);

        trash(&conn, &gone.id).unwrap(); // deleted on its own, first…
        trash(&conn, &parent.id).unwrap(); // …then the parent, separately
        conn.execute(
            "UPDATE tasks SET deleted_at = '2020-01-01 00:00:00.001'
              WHERE deleted_at IS NOT NULL",
            [],
        )
        .unwrap();

        restore(&conn, &parent.id).unwrap();
        assert!(get(&conn, &parent.id).unwrap().unwrap().deleted_at.is_none());
        assert!(
            get(&conn, &keep.id).unwrap().unwrap().deleted_at.is_none(),
            "the child that went with the parent must come back with it"
        );
        assert!(
            get(&conn, &gone.id).unwrap().unwrap().deleted_at.is_some(),
            "a child trashed in its own batch must stay trashed, however close \
             the two deletions landed"
        );
    }

    /// Same property for the Archive, which shares the mechanism and shared the
    /// bug.
    #[test]
    fn two_archives_at_the_same_instant_are_still_two_batches() {
        let conn = test_conn();
        let (parent, keep, gone) = parent_over_a_separately_hidden_child(&conn);

        archive(&conn, &gone.id).unwrap();
        archive(&conn, &parent.id).unwrap();
        conn.execute(
            "UPDATE tasks SET archived_at = '2020-01-01 00:00:00.001'
              WHERE archived_at IS NOT NULL",
            [],
        )
        .unwrap();

        unarchive(&conn, &parent.id).unwrap();
        assert!(get(&conn, &parent.id).unwrap().unwrap().archived_at.is_none());
        assert!(get(&conn, &keep.id).unwrap().unwrap().archived_at.is_none());
        assert!(
            get(&conn, &gone.id).unwrap().unwrap().archived_at.is_some(),
            "a child archived in its own batch must stay archived"
        );
    }

    /// Rows written by a **pre-v8 binary** — timestamp set, batch id NULL — must
    /// keep behaving the way they did before, not collapse into one NULL batch
    /// where restoring anything empties the Trash. This is what the `COALESCE`
    /// in `Hidden::batch_key` is for, and the DB is guaranteed to hold such rows
    /// whenever a leftover `cxtasks-mcp` or `cxtasks-runner` is still installed.
    ///
    /// The bar here is v5's behaviour, not v8's: distinct stamps still separate
    /// batches, and the same-millisecond case stays broken for these rows. It is
    /// not repairable after the fact — nothing on disk records which deletion a
    /// row belonged to — so the goal is that old rows are no *worse* than they
    /// were, and that they don't drag the new mechanism down with them.
    #[test]
    fn rows_from_a_pre_v8_writer_still_restore_as_their_own_batch() {
        let conn = test_conn();
        let (parent, keep, gone) = parent_over_a_separately_hidden_child(&conn);

        trash(&conn, &gone.id).unwrap();
        trash(&conn, &parent.id).unwrap();
        // Rewind to what a pre-v8 writer leaves behind: a stamp per deletion,
        // no batch ids at all.
        conn.execute(
            "UPDATE tasks SET delete_batch = NULL,
                              deleted_at = CASE WHEN id = ?1 THEN '2020-01-01 00:00:00.001'
                                                ELSE '2020-01-01 00:00:00.002' END
              WHERE deleted_at IS NOT NULL",
            params![gone.id],
        )
        .unwrap();

        assert_eq!(
            list_view(&conn, View::Trash).len(),
            2,
            "the parent's batch and the earlier deletion, not one merged entry"
        );

        restore(&conn, &parent.id).unwrap();
        assert!(get(&conn, &parent.id).unwrap().unwrap().deleted_at.is_none());
        assert!(
            get(&conn, &keep.id).unwrap().unwrap().deleted_at.is_none(),
            "the child shared the old stamp, so it comes back with its parent"
        );
        assert!(
            get(&conn, &gone.id).unwrap().unwrap().deleted_at.is_some(),
            "a different stamp is still a different batch"
        );
    }

    /// A subtask trashed while its parent stays live has to be reachable in the
    /// Trash on its own, or it is unrecoverable through the UI.
    #[test]
    fn a_subtask_trashed_alone_is_its_own_trash_entry() {
        let conn = test_conn();
        let parent = seed(&conn, "parent");
        let child = create(
            &conn,
            &NewTask {
                title: "child".into(),
                parent_id: Some(parent.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        trash(&conn, &child.id).unwrap();

        let trashed = list_view(&conn, View::Trash);
        assert_eq!(trashed.len(), 1);
        assert_eq!(trashed[0].id, child.id);
        // The parent is untouched and still on the board.
        assert_eq!(list(&conn, &ListFilters::default()).unwrap().len(), 1);
    }

    #[test]
    fn archive_hides_from_the_board_without_a_purge_clock() {
        let conn = test_conn();
        let t = seed(&conn, "someday");
        archive(&conn, &t.id).unwrap();

        assert!(list(&conn, &ListFilters::default()).unwrap().is_empty());
        assert_eq!(list_view(&conn, View::Archive).len(), 1);
        assert!(
            days_until_purge(&conn, &t.id).unwrap().is_none(),
            "archived work is never on a clock"
        );

        unarchive(&conn, &t.id).unwrap();
        assert_eq!(list(&conn, &ListFilters::default()).unwrap().len(), 1);
    }

    /// The four-state corner: archived, then trashed. Restoring must not dump it
    /// back onto the board it was deliberately taken off.
    #[test]
    fn restoring_an_archived_task_returns_it_to_the_archive() {
        let conn = test_conn();
        let t = seed(&conn, "set aside, then deleted");
        archive(&conn, &t.id).unwrap();
        trash(&conn, &t.id).unwrap();

        assert_eq!(list_view(&conn, View::Trash).len(), 1);
        assert_eq!(
            list_view(&conn, View::Archive).len(),
            0,
            "while trashed it belongs to the Trash, on the purge clock"
        );

        restore(&conn, &t.id).unwrap();
        assert_eq!(list_view(&conn, View::Archive).len(), 1);
        assert!(
            list(&conn, &ListFilters::default()).unwrap().is_empty(),
            "restore must not silently un-archive"
        );
    }

    /// A local host must not purge: the delete would never travel, and the next
    /// pull would put every task straight back.
    #[test]
    fn a_local_host_refuses_to_purge_once_sync_is_on() {
        let conn = test_conn();
        crate::sync::state::set_enabled(&conn, true).unwrap();

        let err = purge_expired(&conn).unwrap_err().to_string();
        assert!(err.contains("canonical node's job"), "{err}");

        // The canonical node does it.
        crate::sync::state::set_canonical(&conn, true).unwrap();
        assert!(purge_expired(&conn).is_ok());
    }

    /// And with sync off — the standalone Mac — nothing changes.
    #[test]
    fn purging_still_works_with_sync_off() {
        let conn = test_conn();
        assert!(purge_expired(&conn).is_ok());
    }

    #[test]
    fn purge_destroys_only_tasks_past_the_retention_window() {
        let conn = test_conn();
        let fresh = seed(&conn, "deleted just now");
        let old = seed(&conn, "deleted long ago");
        let boundary = seed(&conn, "deleted one day short");
        trash(&conn, &fresh.id).unwrap();
        trash(&conn, &old.id).unwrap();
        trash(&conn, &boundary.id).unwrap();

        // Backdate without going through `trash`, which always stamps "now".
        conn.execute(
            &format!(
                "UPDATE tasks SET deleted_at =
                    strftime('%Y-%m-%d %H:%M:%f','now','-{} days') WHERE id = ?1",
                RETENTION_DAYS + 1
            ),
            params![old.id],
        )
        .unwrap();
        conn.execute(
            &format!(
                "UPDATE tasks SET deleted_at =
                    strftime('%Y-%m-%d %H:%M:%f','now','-{} days') WHERE id = ?1",
                RETENTION_DAYS - 1
            ),
            params![boundary.id],
        )
        .unwrap();

        assert_eq!(purge_expired(&conn).unwrap(), 1);
        assert!(get(&conn, &old.id).unwrap().is_none(), "expired task survived");
        assert!(get(&conn, &fresh.id).unwrap().is_some(), "fresh task destroyed");
        assert!(
            get(&conn, &boundary.id).unwrap().is_some(),
            "a task one day inside the window must not be purged"
        );
    }

    #[test]
    fn purge_never_touches_live_or_archived_tasks() {
        let conn = test_conn();
        let live = seed(&conn, "live");
        let archived = seed(&conn, "archived");
        archive(&conn, &archived.id).unwrap();
        // Wind the clock far past the window; neither is trashed, so neither goes.
        assert_eq!(purge_expired(&conn).unwrap(), 0);
        assert!(get(&conn, &live.id).unwrap().is_some());
        assert!(get(&conn, &archived.id).unwrap().is_some());
    }

    #[test]
    fn days_until_purge_counts_down_from_the_retention_window() {
        let conn = test_conn();
        let t = seed(&conn, "x");
        trash(&conn, &t.id).unwrap();
        assert_eq!(days_until_purge(&conn, &t.id).unwrap(), Some(RETENTION_DAYS));

        conn.execute(
            &format!(
                "UPDATE tasks SET deleted_at =
                    strftime('%Y-%m-%d %H:%M:%f','now','-{} days') WHERE id = ?1",
                RETENTION_DAYS - 3
            ),
            params![t.id],
        )
        .unwrap();
        assert_eq!(days_until_purge(&conn, &t.id).unwrap(), Some(3));
    }

    #[test]
    fn subtasks_of_a_trashed_parent_stay_visible_in_its_detail_pane() {
        let conn = test_conn();
        let parent = seed(&conn, "parent");
        create(
            &conn,
            &NewTask {
                title: "child".into(),
                parent_id: Some(parent.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(list_children(&conn, &parent.id).unwrap().len(), 1);
        trash(&conn, &parent.id).unwrap();
        assert_eq!(
            list_children(&conn, &parent.id).unwrap().len(),
            1,
            "a trashed parent must still show what restoring it will bring back"
        );
    }

    #[test]
    fn a_live_parent_never_shows_a_trashed_child() {
        let conn = test_conn();
        let parent = seed(&conn, "parent");
        let child = create(
            &conn,
            &NewTask {
                title: "child".into(),
                parent_id: Some(parent.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        trash(&conn, &child.id).unwrap();
        assert!(list_children(&conn, &parent.id).unwrap().is_empty());
    }

    #[test]
    fn trash_and_restore_are_idempotent() {
        let conn = test_conn();
        let t = seed(&conn, "x");
        trash(&conn, &t.id).unwrap();

        // Rewind the stamp to an instant the clock cannot produce mid-test, so
        // "did the second trash re-stamp it?" is answered without depending on
        // wall-clock resolution at all.
        //
        // This used to trash twice back to back and assert the two returned
        // stamps DIFFERED. `deleted_at` is millisecond-resolution, so both calls
        // land in the same millisecond on any quick machine — it failed ~11 runs
        // in 12 in isolation, and only passed under the full suite's scheduling
        // jitter. It was also the weaker check twice over: no caller uses
        // `trash`'s return value, and when the two stamps DID tie, the
        // re-stamp assertion below became `first == first` and proved nothing.
        let pinned = "2020-01-01 00:00:00.001";
        conn.execute(
            "UPDATE tasks SET deleted_at = ?1 WHERE id = ?2",
            params![pinned, t.id],
        )
        .unwrap();

        trash(&conn, &t.id).unwrap();
        assert_eq!(
            get(&conn, &t.id).unwrap().unwrap().deleted_at.as_deref(),
            Some(pinned),
            "a second trash must not re-stamp an already-trashed row, or the batch splits"
        );
        restore(&conn, &t.id).unwrap();
        restore(&conn, &t.id).expect("restoring a live task is a no-op, not an error");
    }

    #[test]
    fn trashing_an_unknown_task_is_an_error() {
        let conn = test_conn();
        assert!(matches!(
            trash(&conn, "nope").unwrap_err(),
            AppError::NotFound(_)
        ));
    }

    #[test]
    fn create_rejects_blank_title() {
        let conn = test_conn();
        let err = create(
            &conn,
            &NewTask {
                title: "   ".into(),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    #[test]
    fn create_defaults_to_inbox_and_no_execution_context() {
        let conn = test_conn();
        let t = seed(&conn, "buy strings");
        assert_eq!(t.status, "inbox");
        assert_eq!(
            t.priority, PRIORITY_LOWEST,
            "a captured task must default to the LOWEST priority — with P0 = highest, \
             defaulting to 0 would mark every stray thought drop-everything urgent"
        );
        assert!(!t.is_dispatchable(), "plain todo must not be dispatchable");
        assert!(!t.bg_allowed);
        assert!(!t.bg_writes_allowed);
    }

    #[test]
    fn effective_prompt_falls_back_to_title_and_body() {
        let conn = test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "fix the intake webhook retry".into(),
                body: Some("it drops on 429".into()),
                repo_path: Some("/Users/x/Projects/cxventures".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(t.is_dispatchable());
        assert_eq!(
            t.effective_prompt(),
            "fix the intake webhook retry\n\nit drops on 429"
        );

        let t2 = update(
            &conn,
            &t.id,
            &TaskPatch {
                prompt: Some(Some("explicit prompt wins".into())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(t2.effective_prompt(), "explicit prompt wins");
    }

    #[test]
    fn update_null_clears_and_absent_preserves() {
        let conn = test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "with context".into(),
                body: Some("notes".into()),
                project: Some("cxventures".into()),
                ..Default::default()
            },
        )
        .unwrap();

        // Absent key: `{"title": "renamed"}` must not touch body/project.
        let patch: TaskPatch = serde_json::from_str(r#"{"title":"renamed"}"#).unwrap();
        let t = update(&conn, &t.id, &patch).unwrap();
        assert_eq!(t.title, "renamed");
        assert_eq!(t.body.as_deref(), Some("notes"));
        assert_eq!(t.project.as_deref(), Some("cxventures"));

        // Explicit null clears.
        let patch: TaskPatch = serde_json::from_str(r#"{"body":null}"#).unwrap();
        let t = update(&conn, &t.id, &patch).unwrap();
        assert_eq!(t.body, None);
        assert_eq!(t.project.as_deref(), Some("cxventures"));
    }

    #[test]
    fn update_rejects_unknown_status() {
        let conn = test_conn();
        let t = seed(&conn, "x");
        let err = update(
            &conn,
            &t.id,
            &TaskPatch {
                status: Some("wat".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    #[test]
    fn complete_sets_completed_at_and_reopen_clears_it() {
        let conn = test_conn();
        let t = seed(&conn, "ship it");
        let done = complete(&conn, &t.id).unwrap();
        assert_eq!(done.status, "done");
        assert!(done.completed_at.is_some());

        let reopened = update(
            &conn,
            &t.id,
            &TaskPatch {
                status: Some("next".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(reopened.completed_at, None);
    }

    /// **Gotcha #23 regression.** A naive 2020/2999 fixture does NOT catch the
    /// lexicographic bug — `'2020…' < '2026…'` compares correctly even when
    /// broken. The fixture must share today's UTC date but be in the past.
    #[test]
    fn due_scan_fires_for_a_time_earlier_today_utc() {
        let conn = test_conn();
        let t = seed(&conn, "due earlier today");
        conn.execute(
            "UPDATE tasks SET due_at = date('now') || 'T00:00:00.001Z' WHERE id = ?1",
            params![t.id],
        )
        .unwrap();

        let due = list_due_for_reminder(&conn).unwrap();
        assert_eq!(
            due.len(),
            1,
            "a task due earlier today (UTC) must be due NOW, not at UTC midnight — \
             if this fails, a datetime() wrapper was dropped from the due scan"
        );

        // Prove the fixture actually exercises the bug: the raw byte-wise
        // comparison the wrapper protects against still reports "not due".
        let broken: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE due_at <= datetime('now')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            broken, 0,
            "fixture no longer reproduces the lexicographic bug — rewrite it"
        );
    }

    #[test]
    fn defer_scan_uses_the_same_wrapper() {
        let conn = test_conn();
        let t = seed(&conn, "deferred earlier today");
        conn.execute(
            "UPDATE tasks SET defer_until = date('now') || 'T00:00:00.001Z' WHERE id = ?1",
            params![t.id],
        )
        .unwrap();
        assert_eq!(list_undeferred(&conn).unwrap().len(), 1);
    }

    #[test]
    fn reminder_scan_skips_already_reminded_and_closed() {
        let conn = test_conn();
        let a = seed(&conn, "a");
        let b = seed(&conn, "b");
        for id in [&a.id, &b.id] {
            conn.execute(
                "UPDATE tasks SET due_at = date('now') || 'T00:00:00.001Z' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        }
        mark_reminded(&conn, &a.id).unwrap();
        let due = list_due_for_reminder(&conn).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, b.id);

        complete(&conn, &b.id).unwrap();
        assert!(list_due_for_reminder(&conn).unwrap().is_empty());
    }

    #[test]
    fn changing_due_date_rearms_the_reminder() {
        let conn = test_conn();
        let t = seed(&conn, "t");
        mark_reminded(&conn, &t.id).unwrap();
        let t = update(
            &conn,
            &t.id,
            &TaskPatch {
                due_at: Some(Some("2030-01-01T00:00:00.000Z".into())),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(t.reminded_at, None);
    }

    #[test]
    fn today_view_includes_overdue_and_excludes_deferred() {
        let conn = test_conn();
        let overdue = seed(&conn, "overdue");
        conn.execute(
            "UPDATE tasks SET due_at = date('now') || 'T00:00:00.001Z' WHERE id = ?1",
            params![overdue.id],
        )
        .unwrap();

        let deferred = seed(&conn, "deferred");
        conn.execute(
            "UPDATE tasks SET due_at = date('now') || 'T00:00:00.001Z',
                              defer_until = '2099-01-01T00:00:00.000Z' WHERE id = ?1",
            params![deferred.id],
        )
        .unwrap();

        let far = seed(&conn, "far future");
        conn.execute(
            "UPDATE tasks SET due_at = '2099-06-01T00:00:00.000Z' WHERE id = ?1",
            params![far.id],
        )
        .unwrap();

        let today = list(
            &conn,
            &ListFilters {
                view: Some(View::Today),
                ..Default::default()
            },
        )
        .unwrap();
        let titles: Vec<_> = today.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["overdue"]);

        // "deferred" is due TODAY but deferred to 2099: hidden from Today by
        // the defer, and the ONE list that has it is Waiting. "far future" is
        // a deadline with no hold on it, so it is not waiting on anything —
        // it is in Open, which is the whole point of retiring Upcoming.
        let waiting = list(
            &conn,
            &ListFilters {
                view: Some(View::Waiting),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            waiting.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            vec!["deferred"]
        );

        let open = list(
            &conn,
            &ListFilters {
                view: Some(View::Open),
                ..Default::default()
            },
        )
        .unwrap();
        let open: Vec<_> = open.iter().map(|t| t.title.as_str()).collect();
        assert!(
            open.contains(&"far future"),
            "a deadline is visible before the day it falls due: {open:?}"
        );
        assert!(!open.contains(&"deferred"), "a held row is not in Open: {open:?}");
    }

    /// Open is "not held". A future due date is a DEADLINE and does not hold
    /// anything back, so it stays in Open; a defer does, so it doesn't. All
    /// keeps showing everything open, because a project drill-down queries it.
    #[test]
    fn open_view_excludes_deferred_but_keeps_future_due() {
        let conn = test_conn();
        seed(&conn, "undated");

        let overdue = seed(&conn, "overdue");
        conn.execute(
            "UPDATE tasks SET due_at = date('now') || 'T00:00:00.001Z' WHERE id = ?1",
            params![overdue.id],
        )
        .unwrap();

        let deferred = seed(&conn, "deferred");
        conn.execute(
            "UPDATE tasks SET defer_until = '2099-01-01T00:00:00.000Z' WHERE id = ?1",
            params![deferred.id],
        )
        .unwrap();

        // Deferred earlier today = the defer has PASSED — actionable again.
        // The same-day fixture is what catches a missing datetime() wrapper
        // (invariant 1); a 2020 date would pass against the broken comparison.
        let undeferred = seed(&conn, "undeferred");
        conn.execute(
            "UPDATE tasks SET defer_until = date('now') || 'T00:00:00.001Z' WHERE id = ?1",
            params![undeferred.id],
        )
        .unwrap();

        let future_due = seed(&conn, "future due");
        conn.execute(
            "UPDATE tasks SET due_at = '2099-06-01T00:00:00.000Z' WHERE id = ?1",
            params![future_due.id],
        )
        .unwrap();

        let names = |view: View| -> Vec<String> {
            list(
                &conn,
                &ListFilters {
                    view: Some(view),
                    ..Default::default()
                },
            )
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect()
        };

        let open = names(View::Open);
        assert!(open.contains(&"undated".to_string()));
        assert!(open.contains(&"overdue".to_string()));
        assert!(open.contains(&"undeferred".to_string()), "a passed defer is actionable");
        assert!(!open.contains(&"deferred".to_string()), "deferred is held, not actionable");
        assert!(
            open.contains(&"future due".to_string()),
            "a due date is a deadline, not a hold — it must not hide the task"
        );

        let all = names(View::All);
        assert_eq!(all.len(), 5, "All keeps everything open: {all:?}");
    }

    /// Wire compatibility for the retired view. Three binaries, a server and a
    /// phone do not upgrade in lockstep, so `"upcoming"` has to keep
    /// deserializing — and has to land on Waiting, the list that inherited its
    /// rows, rather than on an error a caller would read as "the Mac is down".
    #[test]
    fn the_retired_upcoming_spelling_still_deserializes_to_waiting() {
        assert_eq!(
            serde_json::from_str::<View>("\"upcoming\"").unwrap(),
            View::Waiting
        );
        // And Waiting still round-trips under its own name, which the alias
        // must not steal: serde serializes the variant, never the alias.
        assert_eq!(serde_json::to_string(&View::Waiting).unwrap(), "\"waiting\"");
    }

    /// The legacy `upcoming` count key is a copy of `waiting`, not a second
    /// number. iOS decodes it non-optionally, so it must be present AND must
    /// not report something the phone's Upcoming page (now Waiting's rows)
    /// would contradict.
    #[test]
    fn the_legacy_upcoming_count_mirrors_waiting() {
        let conn = test_conn();
        let deferred = seed(&conn, "deferred");
        conn.execute(
            "UPDATE tasks SET defer_until = '2099-01-01T00:00:00.000Z' WHERE id = ?1",
            params![deferred.id],
        )
        .unwrap();
        let future_due = seed(&conn, "future due");
        conn.execute(
            "UPDATE tasks SET due_at = '2099-06-01T00:00:00.000Z' WHERE id = ?1",
            params![future_due.id],
        )
        .unwrap();

        let counts = view_counts(&conn).unwrap();
        assert_eq!(counts.waiting, 1, "only the deferred row is held");
        assert_eq!(counts.upcoming, counts.waiting);

        let json = serde_json::to_value(&counts).unwrap();
        assert!(
            json.get("upcoming").is_some(),
            "the legacy key must stay on the wire: {json}"
        );
    }

    #[test]
    fn list_hides_subtasks_unless_asked() {
        let conn = test_conn();
        let parent = seed(&conn, "parent");
        create(
            &conn,
            &NewTask {
                title: "child".into(),
                parent_id: Some(parent.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();

        let top = list(&conn, &ListFilters::default()).unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!(list_children(&conn, &parent.id).unwrap().len(), 1);

        let all = list(
            &conn,
            &ListFilters {
                include_subtasks: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn p0_sorts_above_p3_in_the_list() {
        let conn = test_conn();
        for (title, p) in [("lowest", 3), ("highest", 0), ("middle", 1)] {
            create(
                &conn,
                &NewTask {
                    title: title.into(),
                    priority: Some(p),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let titles: Vec<_> = list(&conn, &ListFilters::default())
            .unwrap()
            .iter()
            .map(|t| t.title.clone())
            .collect();
        assert_eq!(
            titles,
            vec!["highest", "middle", "lowest"],
            "P0 must come first — ORDER BY priority is ASC under the Meta convention"
        );
    }

    #[test]
    fn next_task_prefers_overdue_then_priority() {
        let conn = test_conn();
        create(
            &conn,
            &NewTask {
                title: "high priority, no due".into(),
                priority: Some(0), // P0
                ..Default::default()
            },
        )
        .unwrap();
        let overdue = create(
            &conn,
            &NewTask {
                title: "low priority but overdue".into(),
                priority: Some(2), // P2
                ..Default::default()
            },
        )
        .unwrap();
        conn.execute(
            "UPDATE tasks SET due_at = date('now') || 'T00:00:00.001Z' WHERE id = ?1",
            params![overdue.id],
        )
        .unwrap();

        let next = next_task(&conn, None).unwrap().unwrap();
        assert_eq!(next.title, "low priority but overdue");
    }

    #[test]
    fn next_task_skips_blocked_work() {
        let conn = test_conn();
        let blocked = create(
            &conn,
            &NewTask {
                title: "deploy".into(),
                priority: Some(0), // P0 — would otherwise win outright
                ..Default::default()
            },
        )
        .unwrap();
        let blocker = create(
            &conn,
            &NewTask {
                title: "fix the webhook".into(),
                priority: Some(3),
                ..Default::default()
            },
        )
        .unwrap();
        crate::db::deps::add(&conn, &blocked.id, &blocker.id).unwrap();

        let next = next_task(&conn, None).unwrap().unwrap();
        assert_eq!(
            next.title, "fix the webhook",
            "a blocked P0 must not be offered — you'd start it and immediately stall"
        );

        // Once the blocker closes, the P0 becomes actionable again.
        complete(&conn, &blocker.id).unwrap();
        assert_eq!(next_task(&conn, None).unwrap().unwrap().title, "deploy");
    }

    #[test]
    fn superseded_work_is_never_handed_out_as_next_or_background_work() {
        let conn = test_conn();
        let old = create(
            &conn,
            &NewTask {
                title: "old plan".into(),
                priority: Some(0),
                status: Some("queued".into()),
                bg_allowed: Some(true),
                repo_path: Some("/tmp".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let replacement = create(
            &conn,
            &NewTask {
                title: "replacement".into(),
                priority: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        crate::db::comments::add(
            &conn,
            &old.id,
            crate::db::comments::AUTHOR_ME,
            "Ruled out: the old API cannot preserve ordering",
        )
        .unwrap();
        crate::db::relations::add(
            &conn,
            &replacement.id,
            &old.id,
            crate::db::relations::RelationKind::Supersedes,
        )
        .unwrap();

        assert_eq!(
            next_task(&conn, None).unwrap().unwrap().id,
            replacement.id,
            "the P0 older task must not outrank its replacement"
        );
        assert!(
            list_queued_for_background(&conn, &here(), 10).unwrap().is_empty(),
            "the runner list must exclude superseded work"
        );
        assert!(
            !claim_for_background(&conn, &here(), &old.id).unwrap(),
            "a stale runner must also lose the final claim"
        );
    }

    #[test]
    fn background_queue_skips_blocked_work() {
        let conn = test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "runnable".into(),
                status: Some("queued".into()),
                bg_allowed: Some(true),
                repo_path: Some("/tmp".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let blocker = create(
            &conn,
            &NewTask {
                title: "blocker".into(),
                ..Default::default()
            },
        )
        .unwrap();
        crate::db::deps::add(&conn, &t.id, &blocker.id).unwrap();
        assert!(
            list_queued_for_background(&conn, &here(), 10).unwrap().is_empty(),
            "dispatching a blocked task burns a run to discover it can't proceed"
        );
        complete(&conn, &blocker.id).unwrap();
        assert_eq!(list_queued_for_background(&conn, &here(), 10).unwrap().len(), 1);
    }

    #[test]
    fn list_can_filter_by_tags_and_hide_blocked() {
        let conn = test_conn();
        let a = create(&conn, &NewTask { title: "a".into(), ..Default::default() }).unwrap();
        let b = create(&conn, &NewTask { title: "b".into(), ..Default::default() }).unwrap();
        crate::db::tags::attach(&conn, &a.id, "billable").unwrap();

        let tagged = list(
            &conn,
            &ListFilters {
                tags: Some(vec!["billable".into()]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(tagged.len(), 1);
        assert_eq!(tagged[0].id, a.id);

        // A tag nothing carries yields nothing, not everything.
        let none = list(
            &conn,
            &ListFilters {
                tags: Some(vec!["nonexistent".into()]),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(none.is_empty());

        crate::db::deps::add(&conn, &a.id, &b.id).unwrap();
        let visible = list(
            &conn,
            &ListFilters {
                hide_blocked: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, b.id, "the blocked task should be hidden");

        // Default keeps blocked tasks visible — you still want to see them.
        assert_eq!(list(&conn, &ListFilters::default()).unwrap().len(), 2);
    }

    #[test]
    fn next_task_skips_deferred_and_closed() {
        let conn = test_conn();
        let deferred = seed(&conn, "deferred");
        conn.execute(
            "UPDATE tasks SET defer_until = '2099-01-01T00:00:00.000Z' WHERE id = ?1",
            params![deferred.id],
        )
        .unwrap();
        assert!(next_task(&conn, None).unwrap().is_none());

        let open = seed(&conn, "open");
        assert_eq!(next_task(&conn, None).unwrap().unwrap().id, open.id);
        complete(&conn, &open.id).unwrap();
        assert!(next_task(&conn, None).unwrap().is_none());
    }

    /// The mirror of the blocked case: `in_review` is "done, awaiting a human",
    /// so it is not work to hand back — and skipping it must not hide the task
    /// underneath, which is the half a bare `is_none()` assertion would miss.
    #[test]
    fn next_task_skips_a_task_that_is_only_awaiting_review() {
        let conn = test_conn();
        let reviewing = seed(&conn, "awaiting review");
        conn.execute(
            "UPDATE tasks SET status = 'in_review' WHERE id = ?1",
            params![reviewing.id],
        )
        .unwrap();
        assert!(
            next_task(&conn, None).unwrap().is_none(),
            "a review-pending task is not work to pick up"
        );

        let open = seed(&conn, "open");
        assert_eq!(
            next_task(&conn, None).unwrap().unwrap().id,
            open.id,
            "skipping the review-pending task must surface the one behind it"
        );
    }

    #[test]
    fn next_task_can_scope_to_a_project() {
        let conn = test_conn();
        create(
            &conn,
            &NewTask {
                title: "cxv work".into(),
                project: Some("cxventures".into()),
                priority: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        create(
            &conn,
            &NewTask {
                title: "music".into(),
                project: Some("chrisx".into()),
                priority: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        let next = next_task(&conn, Some("cxventures")).unwrap().unwrap();
        assert_eq!(next.title, "cxv work");
    }

    #[test]
    fn background_queue_requires_opt_in_and_repo_path() {
        let conn = test_conn();
        // queued but not bg_allowed
        let a = create(
            &conn,
            &NewTask {
                title: "no opt-in".into(),
                status: Some("queued".into()),
                repo_path: Some("/tmp/repo".into()),
                ..Default::default()
            },
        )
        .unwrap();
        // queued + bg_allowed but no repo_path
        create(
            &conn,
            &NewTask {
                title: "no repo".into(),
                status: Some("queued".into()),
                bg_allowed: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        // the real one
        let c = create(
            &conn,
            &NewTask {
                title: "runnable".into(),
                status: Some("queued".into()),
                bg_allowed: Some(true),
                repo_path: Some("/tmp/repo".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let queued = list_queued_for_background(&conn, &here(), 10).unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].id, c.id);
        assert_ne!(queued[0].id, a.id);
    }

    #[test]
    fn claim_for_background_is_single_winner() {
        let conn = test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "runnable".into(),
                status: Some("queued".into()),
                bg_allowed: Some(true),
                repo_path: Some("/tmp/repo".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(claim_for_background(&conn, &here(), &t.id).unwrap());
        assert!(
            !claim_for_background(&conn, &here(), &t.id).unwrap(),
            "a second claim must lose — this is the double-dispatch guard"
        );
    }

    #[test]
    fn view_counts_and_projects() {
        let conn = test_conn();
        create(
            &conn,
            &NewTask {
                title: "a".into(),
                project: Some("cxventures".into()),
                ..Default::default()
            },
        )
        .unwrap();
        create(
            &conn,
            &NewTask {
                title: "b".into(),
                project: Some("cxventures".into()),
                status: Some("in_review".into()),
                ..Default::default()
            },
        )
        .unwrap();
        create(
            &conn,
            &NewTask {
                title: "c".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let counts = view_counts(&conn).unwrap();
        assert_eq!(
            counts.inbox, 1,
            "'a' carries a project, so it is filed and off the Inbox; 'b' is in \
             review; only 'c' is left to file"
        );
        assert_eq!(counts.in_review, 1);

        let projects = list_projects(&conn).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].project, "cxventures");
        assert_eq!(projects[0].open_count, 2);
    }

    /// The Inbox view is every UNSCHEDULED open task, not `status = 'inbox'`.
    ///
    /// `inbox` and `todo` must sit in it identically. Nothing else in the app
    /// tells them apart — not [`next_task`], not any other view — so a view
    /// that did would put back the meaningless status move this shape exists to
    /// remove, and triage would go back to being a hop between two labels
    /// instead of "give it a project".
    #[test]
    fn the_inbox_view_holds_unscheduled_work_whatever_its_status() {
        let conn = test_conn();
        let mk = |t: NewTask| {
            create(&conn, &t).unwrap();
        };

        mk(NewTask {
            title: "captured".into(), // defaults to `inbox`
            ..Default::default()
        });
        mk(NewTask {
            title: "triaged".into(),
            status: Some("todo".into()),
            ..Default::default()
        });

        // Scheduled, held, in flight, and closed all belong elsewhere. A
        // written hold (v27) leaves Inbox exactly as a defer date does — both
        // are "not yet", and Waiting is where "not yet" lives.
        mk(NewTask {
            title: "waiting".into(),
            status: Some("todo".into()),
            blocked_reason: Some("client hasn't sent the key".into()),
            ..Default::default()
        });
        mk(NewTask {
            title: "dated".into(),
            status: Some("todo".into()),
            due_at: Some("2030-01-01T00:00:00.000Z".into()),
            ..Default::default()
        });
        mk(NewTask {
            title: "deferred".into(),
            status: Some("todo".into()),
            defer_until: Some("2030-01-01T00:00:00.000Z".into()),
            ..Default::default()
        });
        mk(NewTask {
            title: "running".into(),
            status: Some("in_progress".into()),
            ..Default::default()
        });
        mk(NewTask {
            title: "finished".into(),
            status: Some("done".into()),
            ..Default::default()
        });

        let mut titles: Vec<String> = list(
            &conn,
            &ListFilters {
                view: Some(View::Inbox),
                ..Default::default()
            },
        )
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
        titles.sort();
        assert_eq!(titles, vec!["captured", "triaged"]);
    }

    /// **The Inbox is what is left to file, and its badge is its length.**
    ///
    /// The real DB had the Inbox header reading 7 over a badge of 4: the list
    /// held every undated task while the badge counted only the project-less
    /// ones — two definitions of "triaged". A home (a project, or a view-level
    /// section) or a date now takes a task out, the badge counts the list, and
    /// the filed rows are still reachable from Open.
    #[test]
    fn the_inbox_is_what_is_left_to_file_and_its_badge_is_its_length() {
        let conn = test_conn();
        let mk = |title: &str, project: Option<&str>| {
            create(
                &conn,
                &NewTask {
                    title: title.into(),
                    project: project.map(Into::into),
                    status: Some("todo".into()),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        mk("filed in a project", Some("cxtasks"));
        let sectioned = mk("filed in a view section", None);
        let stray = mk("pointing at a project's section", None);
        mk("loose", None);

        let personal = crate::db::projects::create_section(&conn, None, "Personal").unwrap();
        crate::db::projects::move_task_to_section(&conn, &sectioned.id, Some(&personal.id)).unwrap();
        // A project-less row whose section belongs to a PROJECT has no home it
        // can be seen under, so it must stay in the Inbox rather than vanish.
        let phase = crate::db::projects::create_section(&conn, Some("cxtasks"), "phase 1").unwrap();
        crate::db::projects::move_task_to_section(&conn, &stray.id, Some(&phase.id)).unwrap();

        let titles_in = |view: View| -> Vec<String> {
            let mut t: Vec<String> = list(
                &conn,
                &ListFilters { view: Some(view), ..Default::default() },
            )
            .unwrap()
            .into_iter()
            .map(|t| t.title)
            .collect();
            t.sort();
            t
        };

        let inbox = titles_in(View::Inbox);
        assert_eq!(inbox, vec!["loose", "pointing at a project's section"]);
        assert_eq!(view_counts(&conn).unwrap().inbox, inbox.len() as i64, "badge = list");

        // Leaving the Inbox is not leaving the app: both filed rows are
        // actionable, so Open still lists them.
        let open = titles_in(View::Open);
        assert!(open.contains(&"filed in a project".to_string()));
        assert!(open.contains(&"filed in a view section".to_string()));
    }
}

#[cfg(test)]
mod waiting_and_decision_tests {
    //! The Waiting view, the `decision` tag, and the people facet — three
    //! conventions that live on existing tables (invariant 29). What is pinned
    //! here is the BEHAVIOUR each one changes, because a convention with no
    //! behaviour is just a naming habit.

    use super::*;
    use crate::db::schema::test_conn;

    fn titled(conn: &Connection, title: &str) -> Task {
        create(conn, &NewTask { title: title.into(), ..Default::default() }).unwrap()
    }

    fn titles(tasks: &[Task]) -> Vec<&str> {
        tasks.iter().map(|t| t.title.as_str()).collect()
    }

    fn view(conn: &Connection, v: View) -> Vec<Task> {
        list(conn, &ListFilters { view: Some(v), ..Default::default() }).unwrap()
    }

    #[test]
    fn waiting_view_holds_every_kind_of_held_back_task_and_nothing_else() {
        let conn = test_conn();
        // Created in this order so the oldest-first ordering is checkable.
        let deferred = titled(&conn, "deferred");
        let armed = titled(&conn, "armed");
        let blocked_status = titled(&conn, "blocked status");
        let behind = titled(&conn, "behind another task");
        let blocker = titled(&conn, "the blocker");
        let _plain = titled(&conn, "plain");
        let _due_later = create(
            &conn,
            &NewTask {
                title: "due later".into(),
                due_at: Some("2099-01-01T12:00:00.000Z".into()),
                ..Default::default()
            },
        )
        .unwrap();

        conn.execute(
            "UPDATE tasks SET defer_until = '2099-01-01T00:00:00.000Z' WHERE id = ?1",
            [&deferred.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE tasks SET tripwire = 'script:x', tripwire_state = 'armed' WHERE id = ?1",
            [&armed.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE tasks SET blocked_reason = 'paperwork' WHERE id = ?1",
            [&blocked_status.id],
        )
        .unwrap();
        crate::db::deps::add(&conn, &behind.id, &blocker.id).unwrap();

        let waiting = view(&conn, View::Waiting);
        assert_eq!(
            titles(&waiting),
            vec!["deferred", "armed", "blocked status", "behind another task"],
            "the four held-back kinds, oldest first; planned and plain work stay out"
        );

        // Closing the blocker releases the task it held — deps' rule, not a
        // second one.
        update(&conn, &blocker.id, &TaskPatch { status: Some("done".into()), ..Default::default() })
            .unwrap();
        assert_eq!(
            titles(&view(&conn, View::Waiting)),
            vec!["deferred", "armed", "blocked status"]
        );
        assert_eq!(view_counts(&conn).unwrap().waiting, 3);
    }

    #[test]
    fn waiting_facet_shares_the_view_predicate_in_both_directions() {
        let conn = test_conn();
        let held = titled(&conn, "held");
        let _free = titled(&conn, "free");
        conn.execute(
            "UPDATE tasks SET defer_until = '2099-01-01T00:00:00.000Z' WHERE id = ?1",
            [&held.id],
        )
        .unwrap();
        // `*` is the whole predicate — the MCP's `waiting: true`, and what the
        // Waiting view is. The FALSE direction is the half that catches the
        // three-valued-logic trap `TRIPWIRE_ARMED`'s COALESCE exists for: both
        // rows here have a NULL `tripwire_state`, so a negation that isn't
        // NULL-safe returns nothing at all. Keep it.
        let spec = |waiting: bool| crate::db::search::FilterSpec {
            waiting: waiting.then(|| vec!["*".to_string()]),
            exclude_waiting: (!waiting).then(|| vec!["*".to_string()]),
            ..Default::default()
        };
        assert_eq!(titles(&crate::db::search::filtered(&conn, &spec(true)).unwrap()), vec!["held"]);
        assert_eq!(titles(&crate::db::search::filtered(&conn, &spec(false)).unwrap()), vec!["free"]);

        // And each KIND names exactly the badge it is spelled after.
        let kind = |k: &str| crate::db::search::FilterSpec {
            waiting: Some(vec![k.to_string()]),
            ..Default::default()
        };
        assert_eq!(titles(&crate::db::search::filtered(&conn, &kind("deferred")).unwrap()), vec!["held"]);
        assert!(crate::db::search::filtered(&conn, &kind("tripwire")).unwrap().is_empty());
        assert!(crate::db::search::filtered(&conn, &kind("nonsense")).is_err(), "an unknown kind is loud");
    }

    #[test]
    fn a_decision_is_on_today_and_off_inbox_and_never_handed_to_an_agent() {
        let conn = test_conn();
        let work = create(
            &conn,
            &NewTask { title: "work".into(), priority: Some(2), ..Default::default() },
        )
        .unwrap();
        let decision = create(
            &conn,
            &NewTask { title: "decide".into(), priority: Some(0), ..Default::default() },
        )
        .unwrap();
        crate::db::tags::attach(&conn, &decision.id, DECISION_TAG).unwrap();

        assert_eq!(titles(&view(&conn, View::Today)), vec!["decide"], "undated, but on Today");
        assert_eq!(titles(&view(&conn, View::Inbox)), vec!["work"], "and off Inbox, so it is not two rows");
        assert_eq!(titles(&view(&conn, View::Open)), vec!["decide", "work"], "still actionable — by Chris");

        // P0, and still not what an agent gets.
        assert_eq!(next_task(&conn, None).unwrap().unwrap().id, work.id);
        assert_eq!(titles(&open_decisions(&conn).unwrap()), vec!["decide"]);

        // Deferring it takes it off Today and out of open_decisions, like anything else.
        conn.execute(
            "UPDATE tasks SET defer_until = '2099-01-01T00:00:00.000Z' WHERE id = ?1",
            [&decision.id],
        )
        .unwrap();
        assert!(view(&conn, View::Today).is_empty());
        assert!(open_decisions(&conn).unwrap().is_empty());

        // A decision filed as a SUBTASK (an agent's follow-up) reaches Today
        // too — and only Today; Open keeps a subtask under its parent.
        let nested = create(
            &conn,
            &NewTask {
                title: "nested decision".into(),
                parent_id: Some(work.id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        crate::db::tags::attach(&conn, &nested.id, DECISION_TAG).unwrap();
        assert_eq!(titles(&view(&conn, View::Today)), vec!["nested decision"]);
        assert_eq!(titles(&view(&conn, View::Open)), vec!["work"]);
        assert_eq!(titles(&open_decisions(&conn).unwrap()), vec!["nested decision"]);
    }

    #[test]
    fn the_runner_drain_skips_a_decision_even_when_it_is_queued() {
        let conn = test_conn();
        let cap = crate::routing::HostCapability::declared_for_this_host();
        let repo = std::env::temp_dir().to_string_lossy().to_string();
        let mut queued = |title: &str| {
            let t = create(
                &conn,
                &NewTask {
                    title: title.into(),
                    repo_path: Some(repo.clone()),
                    bg_allowed: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
            conn.execute("UPDATE tasks SET status = 'queued' WHERE id = ?1", [&t.id]).unwrap();
            t
        };
        let work = queued("work");
        let decision = queued("decide");
        crate::db::tags::attach(&conn, &decision.id, DECISION_TAG).unwrap();

        let listed = list_queued_for_background(&conn, &cap, 10).unwrap();
        assert_eq!(titles(&listed), vec!["work"]);
        assert!(!claim_for_background(&conn, &cap, &decision.id).unwrap());
        assert!(claim_for_background(&conn, &cap, &work.id).unwrap());
    }

    #[test]
    fn people_are_at_tags_and_the_facet_ors_them() {
        let conn = test_conn();
        let sam = titled(&conn, "follow up with Sam");
        let dana = titled(&conn, "ask Dana");
        let _nobody = titled(&conn, "refactor");
        crate::db::tags::attach(&conn, &sam.id, "@Sam").unwrap();
        crate::db::tags::attach(&conn, &dana.id, "@dana").unwrap();
        assert!(crate::db::tags::has(&conn, &sam.id, "@sam").unwrap(), "normalized to lowercase");
        assert!(crate::db::tags::upsert(&conn, "@", None).is_err(), "a bare @ is not a person");

        let people = |names: &[&str]| crate::db::search::FilterSpec {
            people: Some(names.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        };
        let got = |spec| {
            let mut t = titles(&crate::db::search::filtered(&conn, &spec).unwrap())
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            t.sort();
            t
        };
        assert_eq!(got(people(&["sam"])), vec!["follow up with Sam"], "with or without the @");
        assert_eq!(got(people(&["@sam", "dana"])), vec!["ask Dana", "follow up with Sam"], "OR, not AND");
        assert_eq!(got(people(&["*"])), vec!["ask Dana", "follow up with Sam"], "any person at all");
        assert!(got(people(&["nobody"])).is_empty());
    }
}
