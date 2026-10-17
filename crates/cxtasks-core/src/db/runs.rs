//! Execution history — one `task_runs` row per dispatch, foreground or
//! background. The row is written at spawn time so a crashed run is still
//! visible (as `running`) rather than vanishing.
//!
//! **`running` means a background run this machine is currently executing, and
//! nothing else.** Two rules keep that true, and both exist because the state
//! used to be reachable with no way out of it:
//!
//! * A **foreground** dispatch is terminal the moment it is recorded
//!   ([`FOREGROUND_STATUS`]). The app opens a Ghostty session and then has zero
//!   visibility into it — it cannot report progress, cannot kill it and cannot
//!   learn the outcome — so claiming `running` was a claim it had no way to
//!   back up or ever retract. Nothing in the codebase called [`finish`] for a
//!   foreground run, which is not a bug that was introduced so much as one that
//!   could not not happen.
//! * A **background** run that outlives what the wall-clock guard permits is
//!   swept to [`STATUS_ABANDONED`] by [`sweep_abandoned`]. That one is not
//!   cosmetic: [`count_running_background`] feeds `runner::gate`, so a row
//!   stranded by a crash holds a concurrency slot **forever** and, at the
//!   default `max_concurrent = 2`, two of them stop the runner permanently
//!   with no recovery short of hand-editing SQLite.

use crate::error::AppError;
use crate::executor::BackgroundExecutor;
use crate::intent::BackgroundIntent;
use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};

/// Terminal + non-terminal run states.
///
/// The last three are all "one of OUR guards stopped a run that was working" —
/// `timeout` (wall clock), `budget_stopped` (the per-run token ceiling) and
/// `turns_exhausted` (the turn ceiling). They are deliberately not folded into
/// `failed`: a run we stopped on purpose is not a run that broke, and the two
/// need different advice on screen. `budget_stopped` sat here for a long time
/// with **no producer anywhere in Rust** — the per-run spend guard was designed
/// and never built — which is why a stopped run used to be recorded as `failed`
/// with nothing naming the guard.
pub const RUN_STATUSES: [&str; 12] = [
    "running",
    "ready_for_review",
    "needs_action",
    "unverified",
    "succeeded",
    "failed",
    "cancelled",
    "timeout",
    "budget_stopped",
    "turns_exhausted",
    "dispatched",
    "abandoned",
];

/// What a foreground dispatch records: a session was opened, full stop.
///
/// Terminal at spawn. See the module note — the alternative is a `running` row
/// that nothing on this machine is able to close.
pub const FOREGROUND_STATUS: &str = "dispatched";

/// A background run whose process is gone without ever reporting an outcome.
///
/// Deliberately **not** `failed`, and not `timeout` either. `failed` claims the
/// work broke, which we do not know — the usual cause is that the runner itself
/// died mid-batch (gotcha #4) and the agent may well have finished. `timeout`
/// claims *our* wall-clock guard fired, which is a specific event that writes
/// its own row and did not happen here. This is the honest third thing: nobody
/// is left to say what happened. Same argument as gotcha #31's — a run we
/// stopped on purpose is not a run that broke — one step further along.
pub const STATUS_ABANDONED: &str = "abandoned";

pub fn is_terminal(status: &str) -> bool {
    status != "running"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRun {
    pub id: String,
    pub task_id: String,
    /// `foreground` | `background`
    pub mode: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub session_id: Option<String>,
    pub exit_code: Option<i64>,
    pub result_text: Option<String>,
    pub output_tokens: Option<i64>,
    pub scratch_dir: Option<String>,
    pub input_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub model: Option<String>,
    /// Which agent produced this run.
    ///
    /// Recorded on the **run**, not read back off the task, because a task's
    /// executor can be switched afterwards — and if history took its answer
    /// from the task, every past run would retroactively claim to have been
    /// produced by whichever agent is selected now. Foreground runs are always
    /// Claude.
    pub executor: BackgroundExecutor,
    /// What this run was FOR — the work, or an audit of the previous handoff.
    ///
    /// Recorded on the run for a sharper reason than `executor`'s: the task's
    /// own `bg_intent` is **reset** by `runner::finish_run`, so without this
    /// column there would be no durable record that a review ever happened —
    /// and `dispatch::history_block` could not label a critique as one, which is
    /// the single distinction a continuation cannot afford to lose.
    pub intent: BackgroundIntent,
    /// The wall clock, in minutes, this run was actually STARTED with —
    /// `runner::timeout_for`'s answer at spawn time. `None` for foreground runs
    /// (they have no wall clock) and for any background row written before v18.
    ///
    /// Not a display field: [`sweep_abandoned`] reads it to tell a run that is
    /// merely long from one whose process is gone. It lives here rather than
    /// being read back off the task for `executor`'s reason, sharpened — a
    /// task's override can be edited *while the run is in flight*, and judging
    /// a running run against a number changed after it started is how the sweep
    /// reclaims a healthy run.
    pub timeout_minutes: Option<i64>,
    /// Which machine executed this run.
    ///
    /// `None` means the run predates v20 — i.e. predates there being more than
    /// one machine — so it came from the only host there was. That is a
    /// meaningful value, not a missing one, and it is why v20 backfills
    /// nothing: see `schema::create_tables_v20`.
    pub host: Option<String>,
    /// Last time the executing process said it was still alive.
    ///
    /// The sweep's question changes shape with a second host. "Has this run
    /// exceeded its timeout" is answerable about a local process and
    /// meaningless about a remote one — a healthy run on another machine is
    /// indistinguishable from a dead one by age alone. `None` on a foreign row
    /// therefore means "I cannot tell", and the sweep must leave it be.
    pub heartbeat_at: Option<String>,
}

impl TaskRun {
    /// Everything that counts against the rate limit. `output_tokens` alone is
    /// off by ~99%: a measured run emitting 4 output tokens touched 56,349.
    pub fn total_tokens(&self) -> i64 {
        self.input_tokens.unwrap_or(0)
            + self.cache_read_tokens.unwrap_or(0)
            + self.cache_write_tokens.unwrap_or(0)
            + self.output_tokens.unwrap_or(0)
    }
}

/// **Index-positional, paired with [`row_to_run`].** A new column goes on the
/// END of both lists — inserting one in the middle silently shifts every
/// `row.get(n)` after it onto the wrong field.
///
/// [`recent_with_tasks`] appends joined `tasks` columns AFTER this list, but
/// reads them **by alias**, not by absolute index — so growing `COLUMNS` no
/// longer shifts them. It used to, and the failure was wrong-column *data*
/// rather than a compile error: a run's title reading as its project. Keep the
/// `AS` aliases in that query and this stays safe.
const COLUMNS: &str = "id, task_id, mode, status, started_at, finished_at, session_id, \
     exit_code, result_text, output_tokens, scratch_dir, input_tokens, \
     cache_read_tokens, cache_write_tokens, model, executor, intent, timeout_minutes, \
     host, heartbeat_at";

fn row_to_run(row: &Row) -> rusqlite::Result<TaskRun> {
    Ok(TaskRun {
        id: row.get(0)?,
        task_id: row.get(1)?,
        mode: row.get(2)?,
        status: row.get(3)?,
        started_at: row.get(4)?,
        finished_at: row.get(5)?,
        session_id: row.get(6)?,
        exit_code: row.get(7)?,
        result_text: row.get(8)?,
        output_tokens: row.get(9)?,
        scratch_dir: row.get(10)?,
        input_tokens: row.get(11)?,
        cache_read_tokens: row.get(12)?,
        cache_write_tokens: row.get(13)?,
        model: row.get(14)?,
        executor: BackgroundExecutor::from_db(&row.get::<_, String>(15)?),
        intent: BackgroundIntent::from_db(&row.get::<_, String>(16)?),
        timeout_minutes: row.get(17)?,
        host: row.get(18)?,
        heartbeat_at: row.get(19)?,
    })
}

/// Record a dispatch. Returns the new run id.
pub fn start(
    conn: &Connection,
    task_id: &str,
    mode: &str,
    scratch_dir: Option<&str>,
    executor: BackgroundExecutor,
    intent: BackgroundIntent,
    timeout_minutes: Option<i64>,
) -> Result<TaskRun, AppError> {
    if mode != "foreground" && mode != "background" {
        return Err(AppError::Validation(format!(
            "mode must be foreground|background, got '{mode}'"
        )));
    }
    let id = uuid::Uuid::new_v4().to_string();
    // The heartbeat is seeded HERE rather than left for the first progress
    // write. A run whose process dies in its opening seconds — before it has
    // reported anything — would otherwise sit at NULL forever, and rule 3 of
    // the sweep (never reclaim a foreign row with no heartbeat) would strand
    // its slot permanently. Seeding at insert means every v20 row has a clock
    // from the instant it exists.
    conn.execute(
        "INSERT INTO task_runs (id, task_id, mode, status, scratch_dir, executor, intent,
                                timeout_minutes, host, heartbeat_at)
         VALUES (?1, ?2, ?3, 'running', ?4, ?5, ?6, ?7, ?8, datetime('now'))",
        params![
            id,
            task_id,
            mode,
            scratch_dir,
            executor,
            intent,
            timeout_minutes,
            crate::host::host_id()
        ],
    )?;
    get(conn, &id)?.ok_or_else(|| AppError::NotFound("run vanished after insert".into()))
}

#[derive(Debug, Clone, Default)]
pub struct RunOutcome {
    /// Which agent produced this. Not written back to the row — `executor` is
    /// set at [`start`] and a finished run must not be able to rewrite who ran
    /// it — but carried here so the completion notification can name the agent.
    pub executor: BackgroundExecutor,
    /// Why one of our guards stopped this run, as a clause that slots after
    /// "Background run" — e.g. `hit its 150-turn ceiling at turn 151 — a
    /// handoff was salvaged`.
    ///
    /// Not persisted, for the same reason `executor` is carried here: the run
    /// row already holds the full story in `result_text`, and this exists so
    /// `runner::after_run_patch` can put the *reason* in the task's
    /// `blocked_reason` instead of the generic "see the latest run for
    /// details" that sent Chris digging through a session transcript.
    pub stop_detail: Option<String>,
    pub status: String,
    pub exit_code: Option<i64>,
    pub result_text: Option<String>,
    pub output_tokens: Option<i64>,
    pub session_id: Option<String>,
    pub input_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub model: Option<String>,
}

impl RunOutcome {
    /// A run that never got off the ground, with the reason as its text.
    pub fn default_failed(executor: BackgroundExecutor, reason: String) -> Self {
        Self {
            status: "failed".into(),
            result_text: Some(reason),
            executor,
            ..Default::default()
        }
    }
}

/// Close out a run. Idempotent: finishing an already-finished run is a no-op so
/// a timeout racing a normal exit can't rewrite the recorded outcome.
pub fn finish(conn: &Connection, run_id: &str, outcome: &RunOutcome) -> Result<(), AppError> {
    if !RUN_STATUSES.contains(&outcome.status.as_str()) {
        return Err(AppError::Validation(format!(
            "unknown run status '{}'",
            outcome.status
        )));
    }
    conn.execute(
        "UPDATE task_runs
            SET status = ?1,
                exit_code = ?2,
                result_text = ?3,
                output_tokens = ?4,
                session_id = COALESCE(?5, session_id),
                input_tokens = ?7,
                cache_read_tokens = ?8,
                cache_write_tokens = ?9,
                model = COALESCE(?10, model),
                finished_at = datetime('now')
          WHERE id = ?6 AND status = 'running'",
        params![
            outcome.status,
            outcome.exit_code,
            outcome.result_text,
            outcome.output_tokens,
            outcome.session_id,
            run_id,
            outcome.input_tokens,
            outcome.cache_read_tokens,
            outcome.cache_write_tokens,
            outcome.model,
        ],
    )?;
    Ok(())
}

/// Retract an empty foreground dispatch receipt when its terminal handoff
/// fails. Foreground rows are terminal at spawn, so `finish` correctly refuses
/// them. This narrow exception never reopens a run or overwrites a handoff that
/// already arrived, and cannot rewrite any background outcome.
pub fn fail_foreground_dispatch(
    conn: &Connection,
    run_id: &str,
    reason: &str,
) -> Result<(), AppError> {
    conn.execute(
        "UPDATE task_runs SET status = 'failed', result_text = ?2
         WHERE id = ?1 AND mode = 'foreground' AND status = 'dispatched'
           AND COALESCE(result_text, '') = ''",
        params![run_id, reason],
    )?;
    Ok(())
}

/// Write text onto a run's `result_text` without reopening the row.
///
/// Its one caller is [`record_foreground_handoff`], and only on the branch where
/// the target row's handoff is empty — so in practice this SETS rather than
/// appends. It is written as an append anyway because [`finish`] refuses a
/// finished row (`WHERE … status = 'running'`), and a foreground row is finished
/// the instant it is recorded (invariant 20). Do not reach for it to stream
/// partial output into a live run: nothing does that, and appending onto a row
/// that already holds a handoff garbles both.
pub fn append_result(conn: &Connection, run_id: &str, chunk: &str) -> Result<(), AppError> {
    conn.execute(
        "UPDATE task_runs SET result_text = COALESCE(result_text, '') || ?1 WHERE id = ?2",
        params![chunk, run_id],
    )?;
    Ok(())
}

/// Put a FOREGROUND session's handoff on a run row — the same split the
/// background runner performs, through the channel a watched session has.
///
/// The background half was already right: `runner::finish_run` writes the full
/// handoff to `result_text` and a ~200-char summary to the comment log, and
/// `dispatch::history_block` replays the former in full. A foreground session
/// had no equivalent, so everything it wrote back — `complete_task`'s `result`,
/// a `comment_task` handoff — landed as an unbounded comment that
/// `HISTORY_COMMENT_CHARS` then truncated to 1200 chars mid-sentence. The bound
/// held; the content rotted.
///
/// **Attach iff the newest run is this session's own dispatch row.** A
/// foreground dispatch writes a row at spawn (`commands::dispatch`) with an
/// empty handoff, so the common case is a session filling in the row that
/// recorded it. Everything else CREATES:
///
/// * a row that already holds text is never appended to — that garbles the
///   artifact, breaks `handoff_summary`'s derivation and poisons
///   `history_block`'s dedup probe;
/// * a newer BACKGROUND run forces a create, because attaching to an older
///   foreground row would rank this handoff below the background one in
///   `list_for_task`'s newest-first order, so the replay would hand the next
///   agent the stale one and call it current.
///
/// No age bound is needed and none is wanted: a session left open all day is
/// legitimate (it is exactly why foreground runs are exempt from
/// [`sweep_abandoned`]), so "too old to attach to" is not a fact this layer can
/// know.
///
/// The created row is terminal at [`FOREGROUND_STATUS`] the moment it is
/// written, byte-parallel to `commands::dispatch` — a foreground row must never
/// sit at `running`, or it is a claim nothing on this machine can retract
/// (invariant 20).
///
/// `intent` is only consulted on the create path; an attach inherits whatever
/// the dispatch row recorded, which is what the session was actually framed as.
pub fn record_foreground_handoff(
    conn: &Connection,
    task_id: &str,
    intent: BackgroundIntent,
    text: &str,
) -> Result<TaskRun, AppError> {
    let attachable = list_for_task(conn, task_id)?.into_iter().next().filter(|r| {
        r.mode == "foreground"
            && r.finished_at.is_some()
            && r.result_text.as_deref().map(str::trim).unwrap_or("").is_empty()
    });

    if let Some(run) = attachable {
        append_result(conn, &run.id, text)?;
        return get(conn, &run.id)?
            .ok_or_else(|| AppError::NotFound("run vanished after its handoff was written".into()));
    }

    let run = start(
        conn,
        task_id,
        "foreground",
        None,
        // Foreground is always Claude — `PromptTarget::Foreground` carries no
        // executor at all, and a run row records what ran rather than what the
        // task is configured to delegate to.
        BackgroundExecutor::Claude,
        intent,
        // No wall clock: there is nothing to time out, and nothing sweeps it.
        None,
    )?;
    finish(
        conn,
        &run.id,
        &RunOutcome {
            status: FOREGROUND_STATUS.into(),
            result_text: Some(text.to_string()),
            ..Default::default()
        },
    )?;
    get(conn, &run.id)?
        .ok_or_else(|| AppError::NotFound("run vanished after its handoff was written".into()))
}

pub fn set_session_id(conn: &Connection, run_id: &str, session_id: &str) -> Result<(), AppError> {
    conn.execute(
        "UPDATE task_runs SET session_id = ?1 WHERE id = ?2",
        params![session_id, run_id],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<TaskRun>, AppError> {
    let sql = format!("SELECT {COLUMNS} FROM task_runs WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![id], row_to_run)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// A task's runs, **newest first**.
///
/// `rowid DESC` is not a nicety. `started_at` is written by `datetime('now')`,
/// which is SECOND-resolution, so two runs of one task started inside the same
/// second tie on it — and the sort then falls through to whatever SQLite
/// happens to return, which is insertion order, i.e. the exact inverse of what
/// this function claims. Same shape as gotcha #12's comment ordering, and it
/// matters more here: `dispatch::history_block` calls `.first()` "the most
/// recent run" and replays its handoff, so a tie silently hands the next agent
/// an older run's answer and labels it current. `rowid` is monotonic insertion
/// order, so it is the definitive tiebreaker.
pub fn list_for_task(conn: &Connection, task_id: &str) -> Result<Vec<TaskRun>, AppError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM task_runs WHERE task_id = ?1 \
         ORDER BY datetime(started_at) DESC, rowid DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![task_id], row_to_run)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// A run together with the task it belongs to.
///
/// Mission Control asks "what is running right now, across everything" — a
/// question `list_for_task` cannot answer, because it needs a task id you don't
/// have yet. Fetching every task and then a run list per task is N+1 queries to
/// render one panel, so the join happens here instead.
///
/// The run fields are flattened, so this is a superset of [`TaskRun`] on the
/// wire and the TypeScript side can extend the existing interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunWithTask {
    #[serde(flatten)]
    pub run: TaskRun,
    pub task_title: String,
    pub task_short_id: Option<i64>,
    pub task_project: Option<String>,
    pub task_status: String,
    /// Latest normalized trace event, for Mission Control's live one-liner.
    pub latest_event_summary: Option<String>,
    pub latest_event_phase: Option<String>,
    pub latest_event_seq: Option<i64>,
}

/// Recent runs, still-running ones first, newest first within each group.
///
/// **`rowid DESC` is the tiebreaker, for the reason spelled out on
/// [`list_for_task`].** `started_at` is `datetime('now')`, which is
/// SECOND-resolution, so runs started inside the same second tie on it and the
/// sort falls through to insertion order — the exact inverse of "newest first".
/// That is not a corner case here: the runner drains a batch, so several runs
/// starting in one second is the normal path, and Mission Control would show
/// them upside down with nothing to suggest the order was wrong.
///
/// **Filtered to `LIVE` tasks** (invariant 13): a run belonging to a trashed or
/// archived task must not surface on the board, or the panel hands you back work
/// you deliberately put away — complete with a "Review diff" button. This hides
/// no spend: `total_tokens_today` counts `task_runs` directly with no task join,
/// so a run trashed mid-flight still shows up in the budget meter, which is the
/// place that number actually has to be honest.
pub fn recent_with_tasks(conn: &Connection, limit: u32) -> Result<Vec<RunWithTask>, AppError> {
    let sql = with_task_sql(&format!(
        "WHERE {live}
          ORDER BY (r.status = 'running') DESC, datetime(r.started_at) DESC, r.rowid DESC
          LIMIT ?1",
        live = crate::db::tasks::LIVE,
    ));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![limit], row_to_run_with_task)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// ONE run, joined to its task — the deep link's lookup.
///
/// `cxtasks://run/<id>` had no verb that could answer it. `recent_with_tasks`
/// was the only thing returning runs, so a notification tapped after its run
/// aged out of that window landed on the Agents list instead of the run, and
/// the further back you scrolled the worse it got. Widening the recent window
/// to paper over that would have been the wrong fix twice: it makes a list
/// query answer a lookup, and it still fails at whatever the new limit is.
///
/// **Deliberately NOT filtered to `LIVE` tasks**, unlike [`recent_with_tasks`].
/// That function answers "what should the board show", where invariant 13 says
/// work you put away must not be handed back. This one answers "show me THIS
/// run", asked by someone who tapped a notification about it — and a link that
/// silently resolves to nothing because the task was trashed in the meantime is
/// a dead end with nothing on screen to explain it. The join stays INNER, so a
/// hard-deleted task takes its runs with it, which is the one case where
/// "not found" is the honest answer.
pub fn get_with_task(conn: &Connection, run_id: &str) -> Result<Option<RunWithTask>, AppError> {
    let sql = with_task_sql("WHERE r.id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![run_id], row_to_run_with_task)?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// The joined SELECT behind [`RunWithTask`], with the caller's tail appended.
///
/// Shared rather than copied per call site, for the reason stated on
/// [`COLUMNS`]: two hand-maintained column lists in one file drift, and the
/// failure here is not a compile error but wrong-column *data*. The run columns
/// are derived from `COLUMNS` so adding one cannot shift anything, and the task
/// columns carry `AS` aliases so [`row_to_run_with_task`] reads them by name.
fn with_task_sql(tail: &str) -> String {
    let prefixed = COLUMNS
        .split(", ")
        .map(|c| format!("r.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    // `LIVE`'s bare column names need no `t.` prefix: only `tasks` has
    // `deleted_at`/`archived_at`, so SQLite resolves them unambiguously — and
    // interpolating the constant verbatim is what keeps `grep LIVE` a complete
    // audit of the filter's call sites. `status` IS on both tables, hence the
    // explicit `r.`/`t.` qualifiers on every use of it.
    format!(
        "SELECT {prefixed},
                t.title   AS task_title,
                t.short_id AS task_short_id,
                t.project AS task_project,
                t.status  AS task_status,
                e.summary AS latest_event_summary,
                e.phase   AS latest_event_phase,
                e.seq     AS latest_event_seq
           FROM task_runs r
           JOIN tasks t ON t.id = r.task_id
           LEFT JOIN run_events e ON e.id = (
               SELECT re.id
                 FROM run_events re
                WHERE re.run_id = r.id
                ORDER BY re.seq DESC
                LIMIT 1
           )
          {tail}"
    )
}

fn row_to_run_with_task(row: &Row) -> rusqlite::Result<RunWithTask> {
    Ok(RunWithTask {
        run: row_to_run(row)?,
        // BY NAME, deliberately. These used to be absolute indexes starting
        // where `COLUMNS` ends, which meant adding a run column silently
        // shifted every one of them onto the wrong field — a run's title
        // reading as its project, with no compile error. The aliases in
        // `with_task_sql` are what make this safe; keep them and it cannot
        // drift again.
        task_title: row.get("task_title")?,
        task_short_id: row.get("task_short_id")?,
        task_project: row.get("task_project")?,
        task_status: row.get("task_status")?,
        latest_event_summary: row.get("latest_event_summary")?,
        latest_event_phase: row.get("latest_event_phase")?,
        latest_event_seq: row.get("latest_event_seq")?,
    })
}

/// Currently-running background runs. The runner's concurrency gate reads this.
///
/// **This is why [`sweep_abandoned`] exists.** The count comes from the DB, not
/// from the runner's in-process state, so a row left at `running` by a process
/// that died is indistinguishable from a live run and occupies a slot until
/// something writes to it. Nothing ever did.
/// Claim a queued task and record its run in ONE write transaction.
///
/// # Why the concurrency check moved in here
///
/// It used to sit in [`crate::runner::gate`], which reads the running count,
/// decides how many slots are free, and hands that number to a loop that then
/// claims tasks one at a time. Between the read and the claim there is no lock,
/// so two ticks — two processes, now that there can be two hosts — can both see
/// one free slot and both fill it. The window is small and the failure is not:
/// two unattended agents in one repo at once.
///
/// `BEGIN IMMEDIATE` takes the write lock up front, so the count is re-read
/// *inside* it and the claim happens before anybody else can look. The gate
/// stays — it is still what decides whether to bother, and it is still where
/// the budget lives — but it is now an optimisation rather than the guarantee.
///
/// # What this being one transaction forces, and why that is the point
///
/// The run row now exists before the checkpoint is taken, which reverses the
/// old order and fixes two bugs that were invisible while it was reversed:
///
/// * `checkpoint::capture` was called with **`task.id`**, because at that point
///   in the old sequence there was no run id to give it — the run row had not
///   been written yet. `ref_for_run(task.id)` means two runs of one task write
///   the same ref name, so the second silently overwrites the first's snapshot
///   and "restore" restores the wrong tree.
/// * A checkpoint failure parked the task with **no run row at all**, so the
///   history showed nothing happened. Now it is a recorded `failed` run with
///   the git error in `result_text`, which is where somebody would look.
///
/// Returns `Ok(None)` when the claim lost — another tick got there first, or
/// the host is already at its ceiling. Not an error: losing a race is the
/// normal outcome for all but one of the racers.
#[allow(clippy::too_many_arguments)]
pub fn claim_and_start(
    conn: &mut Connection,
    cap: &crate::routing::HostCapability,
    task_id: &str,
    max_concurrent: i64,
    executor: BackgroundExecutor,
    intent: BackgroundIntent,
    timeout_minutes: i64,
) -> Result<Option<TaskRun>, AppError> {
    // IMMEDIATE, not DEFERRED: a deferred transaction takes the write lock at
    // the first write, which is after the count has been read — exactly the
    // window this exists to close.
    conn.execute_batch("BEGIN IMMEDIATE")?;

    let result = (|| -> Result<Option<TaskRun>, AppError> {
        if count_running_background(conn)? >= max_concurrent {
            return Ok(None);
        }
        if !crate::db::tasks::claim_for_background(conn, cap, task_id)? {
            return Ok(None);
        }
        let run = start(
            conn,
            task_id,
            "background",
            None,
            executor,
            intent,
            Some(timeout_minutes),
        )?;
        Ok(Some(run))
    })();

    match result {
        Ok(v) => {
            conn.execute_batch("COMMIT")?;
            Ok(v)
        }
        Err(e) => {
            // Roll back rather than leaving a claimed task with no run: a task
            // stuck at `in_progress` with nothing running is invisible to the
            // drain query and to the sweep, so it never moves again.
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Start an agent tripwire CHECK: a run row plus the tripwire lease, in ONE
/// write transaction, with the run id as the lease id.
///
/// The shape mirrors [`claim_and_start`] and differs in exactly the ways a
/// check differs from work:
///
/// * **No task status change.** A check does not claim the task — the task
///   stays armed and hidden, and only `db::tripwires::record` moves it. What
///   is claimed is the `tripwire_runtime` lease (invariant 26), and taking it
///   HERE, inside the same `BEGIN IMMEDIATE` as the row insert, is what makes
///   "the run id IS the lease id" true rather than merely intended: there is
///   no window in which a run exists without its lease, or a lease names a run
///   that was never written.
/// * **One check in flight per host**, on top of `max_concurrent`. Checks are
///   the lane that can be quietly numerous, and a second one starting while
///   the first is still burning is how a sub-cap gets overshot by a whole
///   ceiling.
///
/// `Ok(None)` is the ordinary losing outcome — another process took the lease
/// between `list_due` and here, the host is at its limit, or a check is
/// already running — and the tick simply moves on.
pub fn start_check(
    conn: &mut Connection,
    task_id: &str,
    spec: &str,
    max_concurrent: i64,
    executor: BackgroundExecutor,
    timeout_minutes: i64,
) -> Result<Option<TaskRun>, AppError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<Option<TaskRun>, AppError> {
        if count_running_checks(conn)? > 0 {
            return Ok(None);
        }
        if count_running_background(conn)? >= max_concurrent {
            return Ok(None);
        }
        let run = start(
            conn,
            task_id,
            "background",
            None,
            executor,
            BackgroundIntent::Check,
            Some(timeout_minutes),
        )?;
        let lease = crate::tripwire::check_lease_minutes(timeout_minutes);
        if !crate::db::tripwires::claim(conn, task_id, &run.id, &crate::host::host_id(), spec, lease)? {
            // Somebody else holds the lease. Roll the row back rather than
            // finishing it as failed: nothing ran, and a `failed` check row
            // would count against the day for a check that never started.
            return Err(AppError::General("lease taken".into()));
        }
        Ok(Some(run))
    })();
    match result {
        Ok(v) => {
            conn.execute_batch("COMMIT")?;
            Ok(v)
        }
        Err(AppError::General(ref m)) if m == "lease taken" => {
            let _ = conn.execute_batch("ROLLBACK");
            Ok(None)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Start an INTERPRETATION run: a run row and nothing else.
///
/// The shape mirrors [`start_check`] and differs in exactly one way, which is
/// the interesting one: **there is no lease to take.** A check's lease is what
/// stops two hosts evaluating one condition at once; an interpretation is not a
/// check and records no attempt — the tripwire has already tripped, its lease is
/// already released, and what this run produces is a comment, not an outcome.
/// So the only things it has to win are the lane's one-at-a-time slot and the
/// host's concurrency ceiling, both of which are re-read inside the same
/// `BEGIN IMMEDIATE` that writes the row (`claim_and_start`'s lesson: a count
/// read outside the transaction is advice, not a guarantee).
///
/// `Ok(None)` is the ordinary losing outcome and the caller simply moves on —
/// the evidence is already on the task, which is what today's behaviour is, so
/// a missed interpretation costs a reading and never the report.
pub fn start_interpret(
    conn: &mut Connection,
    task_id: &str,
    max_concurrent: i64,
    timeout_minutes: i64,
) -> Result<Option<TaskRun>, AppError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<Option<TaskRun>, AppError> {
        if count_running_checks(conn)? > 0 {
            return Ok(None);
        }
        if count_running_background(conn)? >= max_concurrent {
            return Ok(None);
        }
        Ok(Some(start(
            conn,
            task_id,
            "background",
            None,
            BackgroundExecutor::Claude,
            BackgroundIntent::Interpret,
            Some(timeout_minutes),
        )?))
    })();
    match result {
        Ok(v) => {
            conn.execute_batch("COMMIT")?;
            Ok(v)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// In-flight runs the TRIPWIRE lane started on THIS host — checks and
/// interpretations together. Both `start_check` and `start_interpret` admit one
/// at a time, across the pair.
///
/// One counter rather than two because the thing being bounded is the lane, not
/// the kind: two of them burning at once is how the daily sub-cap gets
/// overshot by a whole ceiling, and it makes no difference to the budget which
/// one it was. Matches `tripwire_tokens_today`, deliberately — a counter and
/// the cap it defends must name the same rows.
pub fn count_running_checks(conn: &Connection) -> Result<i64, AppError> {
    let me = crate::host::host_id();
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task_runs
          WHERE status = 'running' AND mode = 'background'
            AND intent IN ('check', 'interpret')
            AND (host IS NULL OR host = ?1)",
        params![me],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Ask a run to stop, in a way that can reach another machine.
///
/// # The file stays the interface; the row is the transport
///
/// `process.rs` polls a path four times a second and is byte-identical to
/// before — a file is the right interface for "stop", because it works with the
/// database locked, with the runner wedged, and from a shell. What a file
/// cannot do is be seen from another host.
///
/// So the request is written to the run row, which is already replicating, and
/// each host's control thread translates the rows addressed to it into the
/// local file. The two halves are deliberately not merged: a local stop still
/// touches the file immediately, so it does not wait on a 3-second loop, and a
/// remote one arrives at whatever pace sync runs.
pub fn request_cancel(conn: &Connection, run_id: &str) -> Result<bool, AppError> {
    // Only a running run, and only once — re-stamping would move the sync
    // watermark on every click and re-push a row that has not changed.
    let n = conn.execute(
        "UPDATE task_runs
            SET cancel_requested_at = datetime('now')
          WHERE id = ?1 AND status = 'running' AND cancel_requested_at IS NULL",
        params![run_id],
    )?;
    Ok(n == 1)
}

/// Runs on THIS host that somebody has asked to stop.
///
/// Host-scoped, because the file this becomes is local: touching a cancel flag
/// for a run executing on another machine would do nothing here and clear
/// nothing there.
pub fn pending_cancels(conn: &Connection) -> Result<Vec<String>, AppError> {
    let me = crate::host::host_id();
    let mut stmt = conn.prepare(
        "SELECT id FROM task_runs
          WHERE status = 'running'
            AND cancel_requested_at IS NOT NULL
            AND (host IS NULL OR host = ?1)",
    )?;
    let rows = stmt
        .query_map(params![me], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Runs that finished on another host since `since`.
///
/// This is the "4 runs finished on the NAS while you were away" feed, and it
/// needs no outbox table: `task_runs` already replicates whole-row and already
/// carries `host` and `finished_at`. A second table would be one more thing to
/// classify, sweep, purge and keep in step, for a query.
pub fn finished_elsewhere_since(
    conn: &Connection,
    since: &str,
    limit: u32,
) -> Result<Vec<RunWithTask>, AppError> {
    let me = crate::host::host_id();
    // `rowid DESC` as the tiebreaker for the same reason `recent_with_tasks`
    // needs one: `finished_at` is second-resolution, so several runs can share
    // a stamp and "newest first" would otherwise be whatever order SQLite felt
    // like.
    let sql = with_task_sql(
        "WHERE r.mode = 'background'
            AND r.finished_at IS NOT NULL
            AND r.host IS NOT NULL AND r.host <> ?1
            AND datetime(r.finished_at) > datetime(?2)
          ORDER BY datetime(r.finished_at) DESC, r.rowid DESC
          LIMIT ?3",
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![me, since, limit], row_to_run_with_task)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Record the checkpoint a run was taken against.
///
/// Written after the run row rather than into it, because the ref name is
/// derived from the run id — see [`claim_and_start`] for why that ordering is
/// the fix and not an inconvenience.
pub fn set_checkpoint(conn: &Connection, run_id: &str, ref_name: &str) -> Result<(), AppError> {
    conn.execute(
        "UPDATE task_runs SET scratch_dir = ?2 WHERE id = ?1",
        params![run_id, ref_name],
    )?;
    Ok(())
}

pub fn count_running_background(conn: &Connection) -> Result<i64, AppError> {
    // THIS host's runs, not every host's.
    //
    // This feeds `runner::gate`, whose question is "do I have a free slot",
    // and a slot is a local process. Counting rows globally answers a
    // different question and answers it destructively: with `max_concurrent`
    // at 2, two runs on the MacBook would put every other host permanently at
    // its limit, so a second machine would sit there draining nothing and
    // showing no reason why.
    //
    // `host IS NULL` counts as mine for the same reason it does in
    // `sweep_abandoned`: a row with no host predates there being more than one,
    // so it came from the only machine there was. Dropping that clause would
    // silently stop counting every legacy in-flight run and let the gate
    // over-admit.
    let me = crate::host::host_id();
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task_runs
          WHERE status = 'running' AND mode = 'background'
            AND (host IS NULL OR host = ?1)",
        params![me],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Refuse anything that would resolve a foreign run against THIS machine's disk.
///
/// A checkpoint is a git ref in the repo the run executed against. Both
/// `run_diff` and `restore_checkpoint` take the run's ref name and hand it to
/// git in a locally-resolved repo — which is correct exactly while there is one
/// machine, and quietly wrong the moment there are two. For `run_diff` the
/// failure is a confusing "checkpoint no longer exists" about a checkpoint that
/// exists perfectly well somewhere else. For `restore_checkpoint` it is worse:
/// that command resets a working tree, and the tree it would reset is the one
/// in front of you, on the basis of a run you did not do here.
///
/// `None` passes, for the reason it does everywhere else in this file: a run
/// with no host predates there being more than one machine.
///
/// The message names the host on purpose. "This run happened on mini" is
/// actionable; "checkpoint no longer exists" sends you looking for a bug.
pub fn ensure_local(run: &TaskRun) -> Result<(), AppError> {
    match run.host.as_deref() {
        None => Ok(()),
        Some(h) if h == crate::host::host_id() => Ok(()),
        Some(h) => Err(AppError::Validation(format!(
            "this run executed on host {h}, not on this machine — its checkpoint \
             lives in that host's copy of the repo, so there is nothing here to \
             diff or restore"
        ))),
    }
}

/// Every host's in-flight background runs, for display rather than gating.
///
/// Mission Control asks "what is happening everywhere", which is the opposite
/// of what the gate asks. Kept as a separate function rather than a flag on the
/// one above, because the two are easy to confuse and confusing them in the
/// gate's direction is what locks a host out permanently.
pub fn count_running_background_all_hosts(conn: &Connection) -> Result<i64, AppError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task_runs WHERE status = 'running' AND mode = 'background'",
        [],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Close out foreground runs left `running` by a build that never closed them.
///
/// **An ongoing heal, not a one-shot migration**, and it needs no schema
/// version at all — the statuses are text in an existing column. The test
/// CLAUDE.md applies to that choice is whether the value being repaired is one
/// somebody could legitimately have *chosen*: a blank `repo_path` is a choice,
/// so v13's backfill is one-shot; a legacy status is not, so
/// `heal_legacy_statuses` runs forever. This is the second kind. Under the
/// current model a foreground run is terminal the instant it is recorded, so a
/// foreground row sitting at `running` is not a state anyone can be in — it is
/// only ever the fossil of a build where nothing called [`finish`].
///
/// Running forever also settles the binary-skew question rather than arguing
/// it: only the app writes foreground runs, so in principle one ship cycle
/// would be enough, but a heal that is always right costs one UPDATE at startup
/// and removes the need to be sure.
///
/// `finished_at` is set to `started_at`, not to now. The dispatch is what
/// happened, and it happened then; stamping the current time would date a
/// two-day-old session to this launch and put it at the top of every
/// newest-first list.
pub fn heal_foreground_runs(conn: &Connection) -> Result<usize, AppError> {
    let n = conn.execute(
        "UPDATE task_runs
            SET status = ?1, finished_at = COALESCE(finished_at, started_at)
          WHERE status = 'running' AND mode = 'foreground'",
        params![FOREGROUND_STATUS],
    )?;
    if n > 0 {
        log::info!("closed {n} foreground run(s) that were never finishable");
    }
    Ok(n)
}

/// How long past `timeout_minutes` a background run may sit before it is
/// treated as gone.
///
/// Generous on purpose. The cost of sweeping too early is killing the record of
/// a live run and freeing a slot that is still occupied — over-subscribing the
/// machine. The cost of sweeping late is a slot that stays busy a little
/// longer. Those are not symmetric, so the margin is wide.
pub const ABANDON_GRACE_MINUTES: i64 = 15;

/// How long a run may go without a heartbeat before it counts as dead.
///
/// The writer refreshes every 30s, so this is ten missed beats. Generous on
/// purpose, and the asymmetry is the whole point: reclaiming a slot late costs
/// one delayed run, while reclaiming it early destroys the record of a run that
/// is still working. Same argument as [`ABANDON_GRACE_MINUTES`], applied to
/// liveness instead of age.
pub const HEARTBEAT_STALE_MINUTES: i64 = 5;

/// "Still here." Written by the progress writer every 30s.
///
/// Deliberately touches nothing but the clock: a heartbeat must be safe to
/// write against a run whose outcome another process may be recording at the
/// same moment. The `status = 'running'` guard is what stops it resurrecting
/// the clock on a run that has just finished.
pub fn heartbeat(conn: &Connection, run_id: &str) -> Result<(), AppError> {
    conn.execute(
        "UPDATE task_runs SET heartbeat_at = datetime('now')
          WHERE id = ?1 AND status = 'running'",
        params![run_id],
    )?;
    Ok(())
}

/// Reclaim background runs whose process cannot still exist.
///
/// **The soundness argument is the wall clock, and it is the only reason an
/// age-based rule is legitimate here.** `runner::process` hard-kills a run at
/// `timeout_minutes` and writes `timeout`, so a background run still marked
/// `running` well past that bound is not a long run — it is a row whose writer
/// is gone. That holds no matter which of the three binaries calls this, which
/// is what makes it safe to run from `initialize` while the runner is mid-batch
/// in another process.
///
/// Consequently `timeout_minutes` MUST be read from settings at call time and
/// never hardcoded: raising the timeout to 120 with a constant baked in here
/// would start sweeping healthy runs at 35 minutes, which is the exact failure
/// this guard exists to prevent, inverted.
///
/// Foreground runs are untouched — they are terminal at spawn now
/// ([`FOREGROUND_STATUS`]) and have no wall clock to reason from anyway. A
/// pre-existing one is the migration's problem, not this function's.
///
/// Returns how many were reclaimed.
pub fn sweep_abandoned(conn: &Connection) -> Result<usize, AppError> {
    let global = crate::db::meta::budget_settings(conn).timeout_minutes;
    // PER ROW, not one cutoff for the batch. Since v18 a task can carry its own
    // wall clock, so "how long could this run legitimately still be going?" is
    // a property of the run, not of the settings — a 60-minute run judged
    // against a 20-minute global is reclaimed at 35 minutes while its process
    // is still working, which frees a slot that is genuinely occupied and
    // destroys the record of a live run. That is this guard's own failure mode,
    // inverted, and it is exactly what the doc comment above warns about for a
    // hardcoded constant.
    //
    // `COALESCE(timeout_minutes, ?)` is what makes it safe against rows written
    // before v18 and by any leftover pre-v18 binary: NULL there means "started
    // under the global clock", which was the only possibility at the time.
    //
    // Both operands wrapped in `datetime()` per invariant 1 — `started_at` is
    // SQLite's space-separated UTC form and would compare byte-wise against a
    // computed instant otherwise. The modifier is built by SQL string
    // concatenation rather than interpolated, so it follows each row's own
    // value; note `||` on a NULL yields NULL and a NULL modifier makes
    // `datetime()` NULL, which matches NOTHING — hence the COALESCE is not
    // optional politeness, it is the difference between sweeping and silently
    // never sweeping again.
    // THREE rules, and the third is an omission that has to be written down or
    // it reads as a missing case.
    //
    //   1. Mine, or legacy (host IS NULL — pre-v20, so from the only machine
    //      there was), judged past its own clock. Today's rule verbatim.
    //   2. Any row whose heartbeat has gone stale. Liveness, not age — a
    //      healthy 60-minute run on another host is indistinguishable from a
    //      dead one if you only look at `started_at`.
    //   3. A FOREIGN row with no heartbeat is never swept, which is what the
    //      `heartbeat_at IS NOT NULL` in branch 2 buys. Such a row is either
    //      pre-v20 or from a host that died before its first beat, and this
    //      process cannot tell which. Cost of leaving it: one stranded slot on
    //      a host that may be gone, visible in Mission Control and recoverable
    //      by hand. Cost of sweeping it: destroying a live run on a machine
    //      this one cannot see. That asymmetry decides it.
    let me = crate::host::host_id();
    let n = conn.execute(
        "UPDATE task_runs
            SET status = ?1,
                finished_at = datetime('now'),
                result_text = COALESCE(
                    result_text,
                    'This run was still marked running long after it could have been — the '
                    || 'process that started it is gone, so its outcome was never recorded. '
                    || 'The concurrency slot it held has been released.'
                )
          WHERE status = 'running'
            AND mode = 'background'
            AND (
                  ((host IS NULL OR host = ?4)
                   AND datetime(started_at) < datetime(
                           'now',
                           '-' || (COALESCE(timeout_minutes, ?2) + ?3) || ' minutes'
                       ))
               OR (heartbeat_at IS NOT NULL
                   AND datetime(heartbeat_at) < datetime(
                           'now', '-' || ?5 || ' minutes'
                       ))
            )",
        params![
            STATUS_ABANDONED,
            global,
            ABANDON_GRACE_MINUTES,
            me,
            HEARTBEAT_STALE_MINUTES
        ],
    )?;
    if n > 0 {
        // Says the DEFAULT cutoff, not the one that fired: with per-run clocks
        // there may be several. Phrased so it can't be read as a single number
        // every swept row shared.
        log::warn!(
            "reclaimed {n} abandoned background run(s), each past its own timeout \
             + {ABANDON_GRACE_MINUTES}m grace (global timeout {global}m)"
        );
    }
    Ok(n)
}

/// Background runs started inside the current budget window — the daily cap's
/// counter.
///
/// `started_at` is written by `datetime('now')` (UTC, space-separated), so it
/// is compared with the same `datetime()`-both-sides rule as everything else.
/// The window is local midnight or a manual reset, whichever is later.
///
/// **The boundary arrives as a bound parameter, not as interpolated SQL.** It
/// used to be `datetime(date('now','localtime'), 'utc')`, which asks the C
/// library, which asks `TZ` — so a UTC container and the Mac enforce one shared
/// daily cap against two different days, silently. Resolving it once in Rust
/// ([`crate::db::meta::window_start_utc`]) and binding the same string into all
/// three counters is what makes them agree with each other AND across hosts.
pub fn count_background_runs_today(conn: &Connection) -> Result<i64, AppError> {
    let (window, _) = crate::db::meta::window_start_utc(conn)?;
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task_runs
          WHERE mode = 'background'
            AND datetime(started_at) >= datetime(?1)",
        params![window],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Total tokens touched by background runs in the current window — input + both
/// cache buckets + output. THIS is the number that tracks rate-limit
/// consumption.
pub fn total_tokens_today(conn: &Connection) -> Result<i64, AppError> {
    let (window, _) = crate::db::meta::window_start_utc(conn)?;
    let n: i64 = conn.query_row(
        "SELECT COALESCE(SUM(COALESCE(input_tokens,0) + COALESCE(cache_read_tokens,0)
                            + COALESCE(cache_write_tokens,0) + COALESCE(output_tokens,0)), 0)
           FROM task_runs
          WHERE mode = 'background'
            AND datetime(started_at) >= datetime(?1)",
        params![window],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// The slice of [`total_tokens_today`] the tripwire lane spent — agent checks
/// and interpretations, `intent IN ('check','interpret')`. A BREAKDOWN of the
/// total, never a second budget: every row it sums is also summed by the daily
/// counter, so the lane can never spend past the day even when this sub-cap is
/// switched off.
///
/// Both intents, because the sub-cap answers "how much did arming tripwires
/// cost me today" and an interpretation is something a tripwire chose to spend.
/// Leaving it out would make a board full of `on_trip = interpret` tasks
/// invisible to the one number watching them.
pub fn tripwire_tokens_today(conn: &Connection) -> Result<i64, AppError> {
    let (window, _) = crate::db::meta::window_start_utc(conn)?;
    let n: i64 = conn.query_row(
        "SELECT COALESCE(SUM(COALESCE(input_tokens,0) + COALESCE(cache_read_tokens,0)
                            + COALESCE(cache_write_tokens,0) + COALESCE(output_tokens,0)), 0)
           FROM task_runs
          WHERE mode = 'background'
            AND intent IN ('check', 'interpret')
            AND datetime(started_at) >= datetime(?1)",
        params![window],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Output tokens only. Kept for the per-run display; do NOT use it as the
/// budget signal.
pub fn output_tokens_today(conn: &Connection) -> Result<i64, AppError> {
    let (window, _) = crate::db::meta::window_start_utc(conn)?;
    let n: i64 = conn.query_row(
        "SELECT COALESCE(SUM(output_tokens), 0) FROM task_runs
          WHERE mode = 'background'
            AND datetime(started_at) >= datetime(?1)",
        params![window],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Runs left `running` by a crashed or killed runner. Called at runner startup
/// so the concurrency gate isn't permanently occupied by ghosts.
// `reap_stale_running` used to live here, doing the same job as
// [`sweep_abandoned`] with three differences, each of which was the reason to
// merge them rather than keep both:
//
//   * it recorded `failed`, asserting the work broke when the usual cause is
//     that the RUNNER died and the agent was fine (see [`STATUS_ABANDONED`]);
//   * it took the cutoff as a PARAMETER, so the policy lived at the call site
//     as `timeout_minutes * 2` — a second place to get it right, and one that
//     scales badly at both ends (2 minutes for a 1-minute timeout, 4 hours for
//     a 2-hour one);
//   * it ran ONLY at runner startup, which recovers a runner that died — the
//     process exits, launchd restarts it, the reap fires — but not one that is
//     still alive having lost track of a run.
//
// Two functions with the same job in the same file is how the constant that
// matters gets fixed in one of them. There is one now.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{self, NewTask};

    fn seed_task(conn: &Connection) -> String {
        tasks::create(
            conn,
            &NewTask {
                title: "t".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .id
    }

    /// Runs started in the SAME SECOND still come back newest-first.
    ///
    /// `started_at` is second-resolution, so without `rowid DESC` the sort ties
    /// and falls through to insertion order — showing the batch upside down.
    /// The runner drains a batch at a time, so this is the normal path rather
    /// than a corner case. Same shape as gotcha #12 and as `list_for_task`.
    #[test]
    fn runs_started_in_the_same_second_come_back_newest_first() {
        let conn = test_conn();
        let ids: Vec<String> = (0..4)
            .map(|_| {
                let t = seed_task(&conn);
                start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None)
                    .unwrap()
                    .id
            })
            .collect();

        let got: Vec<String> = recent_with_tasks(&conn, 10).unwrap().into_iter().map(|r| r.run.id).collect();
        let expected: Vec<String> = ids.into_iter().rev().collect();
        assert_eq!(got, expected, "newest first, even on a second-resolution tie");
    }

    /// The bug `get_with_task` exists to fix, stated as the difference between
    /// the two functions.
    ///
    /// `recent_with_tasks` is a WINDOW. A notification tapped after its run has
    /// aged out of that window used to land on the Agents list with no way to
    /// reach the run it named. The assertion that matters is the first one —
    /// without it this test passes against a `get_run` implemented by scanning
    /// `recent_runs`, which is the workaround this replaces.
    #[test]
    fn a_run_outside_the_recent_window_is_still_reachable_by_id() {
        let conn = test_conn();
        let old = seed_task(&conn);
        let old_run = start(&conn, &old, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        let newer = seed_task(&conn);
        start(&conn, &newer, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();

        let window = recent_with_tasks(&conn, 1).unwrap();
        assert_eq!(window.len(), 1);
        assert_ne!(
            window[0].run.id, old_run.id,
            "the older run must be OUTSIDE the window, or this test proves nothing"
        );

        let found = get_with_task(&conn, &old_run.id).unwrap().expect("resolvable by id");
        assert_eq!(found.run.id, old_run.id);
        assert_eq!(found.task_title, "t", "and it carries its task, like the list does");
    }

    /// Unlike the board's list, a direct lookup still resolves a trashed task's
    /// run. Invariant 13 keeps put-away work off the BOARD; it does not make a
    /// link somebody deliberately followed dead-end silently.
    #[test]
    fn a_trashed_tasks_run_still_resolves_by_id_though_it_leaves_the_board() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        crate::db::tasks::trash(&conn, &t).unwrap();

        assert!(
            recent_with_tasks(&conn, 50).unwrap().iter().all(|r| r.run.id != run.id),
            "a trashed task's run must leave the board (invariant 13)"
        );
        assert_eq!(
            get_with_task(&conn, &run.id).unwrap().map(|r| r.run.id),
            Some(run.id.clone()),
            "but the deep link must still resolve it"
        );
    }

    #[test]
    fn an_unknown_run_id_is_none_not_an_error() {
        let conn = test_conn();
        assert!(get_with_task(&conn, "no-such-run").unwrap().is_none());
    }

    #[test]
    fn start_rejects_bad_mode() {
        let conn = test_conn();
        let t = seed_task(&conn);
        assert!(start(&conn, &t, "sideways", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).is_err());
    }

    #[test]
    fn start_then_finish_records_outcome() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "background", Some("/tmp/scratch"), BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        assert_eq!(run.status, "running");
        assert_eq!(count_running_background(&conn).unwrap(), 1);

        finish(
            &conn,
            &run.id,
            &RunOutcome {
                executor: BackgroundExecutor::Claude,
                status: "succeeded".into(),
                exit_code: Some(0),
                result_text: Some("done".into()),
                output_tokens: Some(1234),
                session_id: Some("sess-1".into()),
                input_tokens: Some(2),
                cache_read_tokens: Some(15_498),
                cache_write_tokens: Some(40_845),
                model: Some("claude-sonnet-5".into()),
                stop_detail: None,
            },
        )
        .unwrap();

        let run = get(&conn, &run.id).unwrap().unwrap();
        assert_eq!(run.status, "succeeded");
        assert_eq!(run.output_tokens, Some(1234));
        assert_eq!(run.model.as_deref(), Some("claude-sonnet-5"));
        // The number the budget guard must use: output alone would report 1234
        // for a run that actually touched 57,579.
        assert_eq!(run.total_tokens(), 2 + 15_498 + 40_845 + 1234);
        assert!(run.finished_at.is_some());
        assert_eq!(count_running_background(&conn).unwrap(), 0);
    }

    #[test]
    fn finish_is_idempotent_so_a_timeout_cannot_overwrite_a_real_outcome() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        finish(
            &conn,
            &run.id,
            &RunOutcome {
                status: "succeeded".into(),
                exit_code: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
        // A late timeout fires against the same run.
        finish(
            &conn,
            &run.id,
            &RunOutcome {
                status: "timeout".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(get(&conn, &run.id).unwrap().unwrap().status, "succeeded");
    }

    #[test]
    fn append_result_accumulates() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        append_result(&conn, &run.id, "first").unwrap();
        append_result(&conn, &run.id, " second").unwrap();
        assert_eq!(
            get(&conn, &run.id).unwrap().unwrap().result_text.as_deref(),
            Some("first second")
        );
    }

    // ── a foreground session's handoff ────────────────────────────────────

    /// The common case: a dispatched session writes back onto the row that
    /// recorded its own dispatch. `finished_at` must not move — the dispatch is
    /// what happened, and it happened then (`heal_foreground_runs`' argument).
    #[test]
    fn a_foreground_handoff_attaches_to_the_dispatch_row_it_belongs_to() {
        let conn = test_conn();
        let t = seed_task(&conn);
        // Exactly what `commands::dispatch` writes.
        let dispatched = start(&conn, &t, "foreground", Some("/tmp/scratch"), BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        finish(
            &conn,
            &dispatched.id,
            &RunOutcome { status: FOREGROUND_STATUS.into(), ..Default::default() },
        )
        .unwrap();
        let finished_at = get(&conn, &dispatched.id).unwrap().unwrap().finished_at;

        let run = record_foreground_handoff(&conn, &t, BackgroundIntent::Work, "Fixed the 429.")
            .unwrap();

        assert_eq!(run.id, dispatched.id, "a session must fill in its own dispatch row");
        assert_eq!(run.status, FOREGROUND_STATUS);
        assert_eq!(run.result_text.as_deref(), Some("Fixed the 429."));
        assert_eq!(run.finished_at, finished_at, "the dispatch happened when it happened");
        assert_eq!(list_for_task(&conn, &t).unwrap().len(), 1, "no second row");
    }

    /// A session started outside the app — `claude` in a terminal with the MCP
    /// registered — has no dispatch row to fill in, so one is created. It must
    /// be terminal at `dispatched` (invariant 20): a foreground row left at
    /// `running` holds a concurrency slot the gate counts and nothing can ever
    /// close.
    ///
    /// Asserts the GATE inputs, not merely the row, for the reason
    /// `a_stranded_run_stops_holding_its_concurrency_slot` does: the row's
    /// status is the mechanism, the wedged runner is the bug.
    #[test]
    fn a_session_with_no_dispatch_row_gets_a_run_terminal_at_dispatched() {
        let conn = test_conn();
        let t = seed_task(&conn);

        let run = record_foreground_handoff(
            &conn,
            &t,
            BackgroundIntent::Review,
            "Verdict: sound, except the line number.",
        )
        .unwrap();

        assert_eq!(run.mode, "foreground");
        assert_eq!(run.status, FOREGROUND_STATUS);
        assert!(run.finished_at.is_some(), "or nothing can ever close it");
        assert!(run.intent.is_review(), "the create path records what it was framed as");
        assert_eq!(run.executor, BackgroundExecutor::Claude);
        assert_eq!(count_running_background(&conn).unwrap(), 0, "it must hold no slot");
        assert_eq!(sweep_abandoned(&conn).unwrap(), 0, "and nothing must have to reclaim it");
    }

    /// Never append onto a row that already holds a handoff. Two handoffs
    /// concatenated into one `result_text` garble the artifact, break
    /// `runner::handoff_summary`'s opening-line derivation, and poison
    /// `dispatch::history_block`'s dedup probe — all silently.
    #[test]
    fn a_second_handoff_gets_its_own_row_rather_than_garbling_the_first() {
        let conn = test_conn();
        let t = seed_task(&conn);

        let first =
            record_foreground_handoff(&conn, &t, BackgroundIntent::Work, "First pass.").unwrap();
        let second =
            record_foreground_handoff(&conn, &t, BackgroundIntent::Work, "Second pass.").unwrap();

        assert_ne!(first.id, second.id);
        assert_eq!(
            get(&conn, &first.id).unwrap().unwrap().result_text.as_deref(),
            Some("First pass."),
            "the earlier handoff must survive intact"
        );
        assert_eq!(second.result_text.as_deref(), Some("Second pass."));
        assert_eq!(list_for_task(&conn, &t).unwrap().len(), 2);
    }

    /// A background run finished after the foreground dispatch means the
    /// foreground row is no longer newest. Attaching to it anyway would rank
    /// this handoff BELOW the background one in `list_for_task`'s newest-first
    /// order, so `history_block` would replay the stale background handoff and
    /// present it as the most recent thing that happened.
    #[test]
    fn a_handoff_never_attaches_to_a_background_run() {
        let conn = test_conn();
        let t = seed_task(&conn);

        let fg = start(&conn, &t, "foreground", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        finish(
            &conn,
            &fg.id,
            &RunOutcome { status: FOREGROUND_STATUS.into(), ..Default::default() },
        )
        .unwrap();
        // `started_at` is second-resolution, so force the ordering rather than
        // racing it (gotcha #12's lesson, applied to runs).
        conn.execute(
            "UPDATE task_runs SET started_at = datetime('now','-1 hour') WHERE id = ?1",
            params![fg.id],
        )
        .unwrap();

        let bg = start(&conn, &t, "background", None, BackgroundExecutor::Codex, BackgroundIntent::Work, None).unwrap();
        finish(
            &conn,
            &bg.id,
            &RunOutcome {
                status: "succeeded".into(),
                result_text: Some("the background handoff".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let run = record_foreground_handoff(&conn, &t, BackgroundIntent::Work, "the new handoff")
            .unwrap();

        assert_ne!(run.id, fg.id, "the empty foreground row is no longer the newest");
        assert_ne!(run.id, bg.id);
        assert_eq!(
            get(&conn, &bg.id).unwrap().unwrap().result_text.as_deref(),
            Some("the background handoff"),
            "a background run's artifact is never touched by a foreground write-back"
        );
        assert_eq!(
            list_for_task(&conn, &t).unwrap().first().map(|r| r.id.clone()),
            Some(run.id),
            "the new handoff must be the one a replay picks up"
        );
    }

    #[test]
    fn daily_counters_count_background_only() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let fg = start(&conn, &t, "foreground", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        finish(
            &conn,
            &fg.id,
            &RunOutcome {
                status: "succeeded".into(),
                output_tokens: Some(999),
                ..Default::default()
            },
        )
        .unwrap();

        let bg = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        finish(
            &conn,
            &bg.id,
            &RunOutcome {
                status: "succeeded".into(),
                output_tokens: Some(500),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(count_background_runs_today(&conn).unwrap(), 1);
        assert_eq!(output_tokens_today(&conn).unwrap(), 500);
    }

    /// The manual reset zeroes what the caps read **without** rewriting what
    /// anything cost — the run row keeps its tokens, and only the window moves.
    ///
    /// All three counters are asserted together because they are three separate
    /// SQL statements sharing one interpolated boundary; the failure this
    /// guards against is one of them keeping its own copy of the WHERE clause,
    /// which would leave the token cap forgiven and the run cap still tripped
    /// (or the reverse) with nothing on screen to explain the disagreement.
    #[test]
    fn a_manual_reset_zeroes_the_counters_without_touching_the_runs() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let bg = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        finish(
            &conn,
            &bg.id,
            &RunOutcome {
                status: "succeeded".into(),
                input_tokens: Some(7_884),
                cache_read_tokens: Some(13_310_598),
                cache_write_tokens: Some(1_235_726),
                output_tokens: Some(19_810),
                ..Default::default()
            },
        )
        .unwrap();
        // Backdated because both stamps are second-resolution: a run started in
        // the same second as the reset is still inside the window (`>=`), which
        // is correct for midnight and irrelevant in practice — you reset after
        // a run has spent the budget, not during the second it starts.
        //
        // ...but never backdated PAST local midnight, which is what made this
        // test fail for the first five minutes of every day. The window starts
        // at local midnight, so "five minutes ago" is outside today's budget
        // when the clock has just rolled over, and the run stopped counting
        // through no fault of the code under test. `MAX` of the two keeps it
        // both recent and inside the window on every run of the clock.
        conn.execute(
            "UPDATE task_runs
                SET started_at = MAX(
                      datetime('now','-5 minutes'),
                      datetime('now','localtime','start of day','utc','+1 second'))
              WHERE id = ?1",
            rusqlite::params![bg.id],
        )
        .unwrap();

        assert_eq!(total_tokens_today(&conn).unwrap(), 14_574_018);
        assert_eq!(count_background_runs_today(&conn).unwrap(), 1);

        crate::db::meta::reset_budget_window(&conn).unwrap();

        assert_eq!(total_tokens_today(&conn).unwrap(), 0);
        assert_eq!(count_background_runs_today(&conn).unwrap(), 0);
        assert_eq!(output_tokens_today(&conn).unwrap(), 0);
        assert_eq!(
            get(&conn, &bg.id).unwrap().unwrap().output_tokens,
            Some(19_810),
            "the reset must forgive the budget, not falsify the run"
        );
    }

    #[test]
    // `reap_stale_running_clears_ghost_runs` and `reap_leaves_fresh_runs_alone`
    // were here. Both are subsumed by the sweep tests further down —
    // `a_stranded_run_stops_holding_its_concurrency_slot` asserts the same
    // clearing AND the slot coming back, which is the part that was missing
    // and the reason any of this matters; `a_live_run_is_never_swept` covers
    // the sparing case at three ages instead of one.
    #[test]
    fn recent_with_tasks_puts_running_first_and_carries_the_task() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let old = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        finish(
            &conn,
            &old.id,
            &RunOutcome {
                status: "succeeded".into(),
                ..Default::default()
            },
        )
        .unwrap();
        // Newer by wall clock, but still running — it must outrank the finished
        // one regardless, because "what is happening now" is the panel's job.
        conn.execute(
            "UPDATE task_runs SET started_at = datetime('now','+1 hour') WHERE id = ?1",
            params![old.id],
        )
        .unwrap();
        let live = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();

        let rows = recent_with_tasks(&conn, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].run.id, live.id);
        assert_eq!(rows[0].task_title, "t");
        assert_eq!(rows[0].task_status, "inbox");
    }

    /// Every joined column, checked in one call — the guard on the alias.
    ///
    /// These used to be read by absolute index starting where `COLUMNS` ends,
    /// so adding a run column shifted all of them onto the wrong field with no
    /// compile error. The two nullable ones are why this test exists: a shift
    /// onto `task_short_id` or `task_project` comes back `None`, which is a
    /// perfectly plausible value, so the older tests asserting only
    /// `task_title` and `task_status` would have stayed green through it.
    #[test]
    fn recent_with_tasks_carries_every_joined_column_not_just_the_loud_ones() {
        let conn = test_conn();
        let t = tasks::create(
            &conn,
            &NewTask {
                title: "distinct title".into(),
                project: Some("distinct project".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let run = start(
            &conn,
            &t.id,
            "background",
            None,
            BackgroundExecutor::Claude,
            BackgroundIntent::Work,
            None,
        )
        .unwrap();

        let rows = recent_with_tasks(&conn, 10).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];

        assert_eq!(row.run.id, run.id, "run columns still read by index");
        assert_eq!(row.task_title, "distinct title");
        assert_eq!(row.task_project.as_deref(), Some("distinct project"));
        assert_eq!(row.task_status, "inbox");
        assert_eq!(
            row.task_short_id, t.short_id,
            "a shift onto short_id reads as None, which looks legitimate"
        );
        assert!(
            t.short_id.is_some(),
            "the assertion above is only meaningful if a short_id was actually claimed"
        );
    }

    #[test]
    fn recent_with_tasks_carries_only_the_latest_trace_event() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        for (call_id, phase, summary) in [
            ("call-1", "started", "Read src/main.rs"),
            ("call-1", "succeeded", "Read src/main.rs"),
            ("call-2", "started", "Grep privacy"),
        ] {
            crate::db::run_events::append(
                &conn,
                &run.id,
                &crate::db::run_events::NewRunEvent {
                    kind: "tool".into(),
                    call_id: Some(call_id.into()),
                    tool_name: Some("Tool".into()),
                    phase: phase.into(),
                    summary: summary.into(),
                    detail_json: None,
                },
            )
            .unwrap();
        }
        let row = recent_with_tasks(&conn, 1).unwrap().remove(0);
        assert_eq!(row.latest_event_summary.as_deref(), Some("Grep privacy"));
        assert_eq!(row.latest_event_phase.as_deref(), Some("started"));
        assert_eq!(row.latest_event_seq, Some(3));
    }

    /// Invariant 13: a run whose task is trashed or archived must not surface —
    /// Mission Control would otherwise hand back work that was deliberately put
    /// away, with a Review-diff button on it.
    #[test]
    fn recent_with_tasks_hides_runs_of_trashed_and_archived_tasks() {
        let conn = test_conn();

        let trashed = seed_task(&conn);
        start(&conn, &trashed, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        tasks::trash(&conn, &trashed).unwrap();
        assert!(recent_with_tasks(&conn, 10).unwrap().is_empty());

        let archived = seed_task(&conn);
        start(&conn, &archived, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        tasks::archive(&conn, &archived).unwrap();
        assert!(recent_with_tasks(&conn, 10).unwrap().is_empty());
    }

    #[test]
    fn runs_list_newest_first() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let a = start(&conn, &t, "foreground", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        conn.execute(
            "UPDATE task_runs SET started_at = datetime('now','-1 day') WHERE id = ?1",
            params![a.id],
        )
        .unwrap();
        let b = start(&conn, &t, "foreground", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        let runs = list_for_task(&conn, &t).unwrap();
        assert_eq!(runs[0].id, b.id);
        assert_eq!(runs[1].id, a.id);
    }

    /// History records who actually ran, and keeps saying so after the task is
    /// re-pointed at a different agent. Deriving the executor from the task
    /// instead would rewrite the past every time the setting changed.
    #[test]
    fn a_runs_executor_survives_the_task_being_switched() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let codex_run = start(&conn, &t, "background", None, BackgroundExecutor::Codex, BackgroundIntent::Work, None).unwrap();
        assert_eq!(codex_run.executor, BackgroundExecutor::Codex);

        tasks::update(
            &conn,
            &t,
            &tasks::TaskPatch {
                bg_executor: Some(BackgroundExecutor::Claude),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(
            get(&conn, &codex_run.id).unwrap().unwrap().executor,
            BackgroundExecutor::Codex,
            "switching the task rewrote what a past run says it ran under"
        );
    }

    /// `finish` deliberately does not touch `executor` — a completed run must
    /// not be able to change who produced it.
    #[test]
    fn finishing_a_run_cannot_rewrite_its_executor() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "background", None, BackgroundExecutor::Codex, BackgroundIntent::Work, None).unwrap();
        finish(
            &conn,
            &run.id,
            &RunOutcome {
                // A caller passing the wrong thing must not matter.
                executor: BackgroundExecutor::Claude,
                status: "succeeded".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            get(&conn, &run.id).unwrap().unwrap().executor,
            BackgroundExecutor::Codex
        );
    }

    // ---- self-healing ----

    /// Age a run's `started_at` by `minutes`, so the sweep's cutoff can be
    /// exercised without sleeping. Written through raw SQL because there is no
    /// legitimate API for backdating a run — which is the point.
    fn backdate(conn: &Connection, run_id: &str, minutes: i64) {
        conn.execute(
            "UPDATE task_runs SET started_at = datetime('now', ?2) WHERE id = ?1",
            params![run_id, format!("-{minutes} minutes")],
        )
        .unwrap();
    }

    /// **The bug this whole thing exists for.** `runner::gate` counts `running`
    /// background rows out of the DB, so a row stranded by a dead runner holds
    /// a concurrency slot forever — at the default `max_concurrent = 2`, two of
    /// them stop background execution permanently and the only symptom is that
    /// nothing ever runs again.
    #[test]
    fn a_stranded_run_stops_holding_its_concurrency_slot() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let timeout = crate::db::meta::budget_settings(&conn).timeout_minutes;

        for _ in 0..2 {
            let run = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None)
                .unwrap();
            backdate(&conn, &run.id, timeout + ABANDON_GRACE_MINUTES + 1);
        }
        assert_eq!(count_running_background(&conn).unwrap(), 2, "wedged");

        // The gate is the thing that was actually broken, so assert on the
        // gate — not merely on the row count it happens to be computed from.
        let settings = crate::db::meta::budget_settings(&conn);
        assert!(
            matches!(
                crate::runner::gate(
                    false,
                    &settings,
                    &crate::db::tasks::here(),
                    count_running_background(&conn).unwrap(),
                    0,
                    0,
                ),
                crate::runner::Gate::AtConcurrencyLimit { .. }
            ),
            "precondition: two stranded rows wedge the gate shut"
        );

        assert_eq!(sweep_abandoned(&conn).unwrap(), 2);
        assert_eq!(
            count_running_background(&conn).unwrap(),
            0,
            "the slots must come back, or the runner never works again"
        );
        assert!(
            matches!(
                crate::runner::gate(
                    false,
                    &settings,
                    &crate::db::tasks::here(),
                    count_running_background(&conn).unwrap(),
                    0,
                    0,
                ),
                crate::runner::Gate::Open { slots } if slots > 0
            ),
            "and the gate must reopen — that is the whole point"
        );
    }

    /// The other half, and the one that would do real damage if it were wrong:
    /// a LIVE run must survive. The sweep can be called from any of the three
    /// binaries — including from `initialize` while the runner is mid-batch in
    /// another process — so being wrong here means recording a live run as dead
    /// and over-subscribing the machine.
    // ── the sweep with more than one host ─────────────────────────────────

    /// Pretend a run belongs to another machine, with a heartbeat `beats_ago`
    /// minutes old (or none at all).
    fn make_foreign(conn: &Connection, run_id: &str, beats_ago: Option<i64>) {
        let hb = match beats_ago {
            Some(m) => format!("datetime('now','-{m} minutes')"),
            None => "NULL".to_string(),
        };
        conn.execute(
            &format!(
                "UPDATE task_runs SET host = 'some-other-machine', heartbeat_at = {hb}
                  WHERE id = ?1"
            ),
            params![run_id],
        )
        .unwrap();
    }

    /// The pair, in ONE call — which is the only version of this test that
    /// means anything.
    ///
    /// "The live one survived" passes just as well against a sweep that
    /// reclaims nothing at all, and "the stale one was reclaimed" passes
    /// against a sweep that reclaims everything. Only asserting both outcomes
    /// from a single call pins the actual behaviour: it discriminates.
    ///
    /// Both runs are old enough that the OLD age-only rule would have taken
    /// them both, so this also proves the age rule no longer reaches another
    /// host's rows.
    #[test]
    fn a_live_run_on_another_host_is_never_swept_and_a_stale_one_is_reclaimed() {
        let conn = test_conn();
        let t = seed_task(&conn);

        let alive = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        let dead = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();

        // Both started long ago — under the pre-v20 rule both were sweepable.
        let timeout = crate::db::meta::budget_settings(&conn).timeout_minutes;
        backdate(&conn, &alive.id, timeout + ABANDON_GRACE_MINUTES + 60);
        backdate(&conn, &dead.id, timeout + ABANDON_GRACE_MINUTES + 60);

        make_foreign(&conn, &alive.id, Some(0)); // beating right now
        make_foreign(&conn, &dead.id, Some(HEARTBEAT_STALE_MINUTES + 1));

        assert_eq!(
            sweep_abandoned(&conn).unwrap(),
            1,
            "exactly one of the two should have been reclaimed"
        );
        assert_eq!(
            get(&conn, &alive.id).unwrap().unwrap().status,
            "running",
            "a heartbeating run on another host is alive, however old it is"
        );
        assert_eq!(
            get(&conn, &dead.id).unwrap().unwrap().status,
            STATUS_ABANDONED,
            "a run that stopped beating is gone, whoever owned it"
        );
    }

    /// The gate counts MY slots, not the world's.
    ///
    /// Without the host filter, two runs on one machine would put every other
    /// host permanently at a `max_concurrent` of 2 — draining nothing, showing
    /// no reason. The all-hosts count is kept separately because Mission
    /// Control genuinely wants the other number.
    #[test]
    fn the_concurrency_gate_counts_only_this_hosts_runs() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let mine = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        let theirs = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();
        make_foreign(&conn, &theirs.id, Some(0));

        assert_eq!(count_running_background(&conn).unwrap(), 1, "only mine holds my slot");
        assert_eq!(
            count_running_background_all_hosts(&conn).unwrap(),
            2,
            "but both are genuinely running somewhere"
        );
        let _ = mine;
    }

    /// A destructive command must refuse a run it cannot reason about.
    ///
    /// `restore_checkpoint` resets a working tree. Pointed at a foreign run it
    /// would reset THIS machine's tree using a ref that was never here — and
    /// the pre-guard failure mode was a misleading "checkpoint no longer
    /// exists", which sends you hunting for a bug instead of telling you the
    /// run happened elsewhere.
    #[test]
    fn a_foreign_run_is_refused_by_the_checkpoint_guard_and_the_message_names_the_host() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();

        assert!(ensure_local(&run).is_ok(), "a run I started is mine");

        make_foreign(&conn, &run.id, Some(0));
        let foreign = get(&conn, &run.id).unwrap().unwrap();
        let err = ensure_local(&foreign).unwrap_err().to_string();
        assert!(
            err.contains("some-other-machine"),
            "the refusal has to name the host to be actionable: {err}"
        );

        // And a legacy row still works, or every pre-v20 diff button breaks.
        conn.execute("UPDATE task_runs SET host = NULL WHERE id = ?1", params![run.id]).unwrap();
        let legacy = get(&conn, &run.id).unwrap().unwrap();
        assert!(ensure_local(&legacy).is_ok());
    }

    /// Rule 3, which is an OMISSION and therefore easy to delete by accident.
    ///
    /// A foreign row with no heartbeat is unreadable: it is either pre-v20 or
    /// from a host that died before its first beat, and this process cannot
    /// tell which. Leaving it costs one stranded slot on a machine that may be
    /// gone. Sweeping it costs a live run on a machine we cannot see. The
    /// asymmetry is the whole argument, and this test is what keeps it.
    #[test]
    fn a_foreign_run_with_no_heartbeat_is_left_for_its_own_host() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();

        let timeout = crate::db::meta::budget_settings(&conn).timeout_minutes;
        backdate(&conn, &run.id, timeout + ABANDON_GRACE_MINUTES + 600);
        make_foreign(&conn, &run.id, None);

        assert_eq!(sweep_abandoned(&conn).unwrap(), 0);
        assert_eq!(get(&conn, &run.id).unwrap().unwrap().status, "running");
    }

    /// My own rows keep behaving exactly as they did before v20.
    ///
    /// The regression this guards is subtle: the new host predicate could have
    /// been written so that only rows with `host = me` match, which would
    /// silently stop sweeping every legacy row ever written — they all have
    /// `host IS NULL`, and nothing would ever reclaim their slots again.
    #[test]
    fn a_legacy_row_with_no_host_is_still_swept_by_age() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();

        // Exactly what a pre-v20 binary leaves behind: no host, no heartbeat.
        conn.execute(
            "UPDATE task_runs SET host = NULL, heartbeat_at = NULL WHERE id = ?1",
            params![run.id],
        )
        .unwrap();
        let timeout = crate::db::meta::budget_settings(&conn).timeout_minutes;
        backdate(&conn, &run.id, timeout + ABANDON_GRACE_MINUTES + 1);

        assert_eq!(sweep_abandoned(&conn).unwrap(), 1);
        assert_eq!(get(&conn, &run.id).unwrap().unwrap().status, STATUS_ABANDONED);
    }

    /// A run this host started records who ran it, from the moment it exists.
    #[test]
    fn a_new_run_records_its_host_and_starts_beating_immediately() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None).unwrap();

        assert_eq!(run.host.as_deref(), Some(crate::host::host_id().as_str()));
        assert!(
            run.heartbeat_at.is_some(),
            "seeded at insert, so a run that dies in its opening seconds is \
             still judged by liveness rather than stranding its slot forever"
        );
    }

    #[test]
    fn a_live_run_is_never_swept() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let fresh = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None)
            .unwrap();

        // Just started.
        assert_eq!(sweep_abandoned(&conn).unwrap(), 0);
        // Still inside the wall-clock guard the runner enforces, so it could
        // legitimately still be going.
        let timeout = crate::db::meta::budget_settings(&conn).timeout_minutes;
        backdate(&conn, &fresh.id, timeout - 1);
        assert_eq!(sweep_abandoned(&conn).unwrap(), 0);
        // Past the timeout but inside the grace: still spared, deliberately.
        // Sweeping early costs more than sweeping late.
        backdate(&conn, &fresh.id, timeout + ABANDON_GRACE_MINUTES - 1);
        assert_eq!(sweep_abandoned(&conn).unwrap(), 0);
        assert_eq!(get(&conn, &fresh.id).unwrap().unwrap().status, "running");
    }

    /// **The v18 half of the same rule, and the one a per-task clock breaks.**
    ///
    /// Once a task can carry its own wall clock, one cutoff for the whole batch
    /// is wrong in the dangerous direction: a 60-minute run judged against the
    /// 20-minute global is reclaimed at 35 minutes while its process is still
    /// working, which frees a slot that is genuinely occupied and destroys the
    /// record of a live run.
    ///
    /// Asserted as a PAIR in one sweep, deliberately. "The long run survived"
    /// alone also passes when the sweep is silently broken and reclaims
    /// nothing — which is exactly what a NULL date modifier produces — so the
    /// default-clock run beside it has to be swept in the same call for the
    /// assertion to mean anything.
    #[test]
    fn a_run_is_judged_against_its_own_clock_not_the_global_one() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let global = crate::db::meta::budget_settings(&conn).timeout_minutes;

        let long = start(
            &conn,
            &t,
            "background",
            None,
            BackgroundExecutor::Claude,
            BackgroundIntent::Work,
            Some(60),
        )
        .unwrap();
        let normal = start(
            &conn,
            &t,
            "background",
            None,
            BackgroundExecutor::Claude,
            BackgroundIntent::Work,
            Some(global),
        )
        .unwrap();
        // Old enough for the global clock, nowhere near the 60-minute one.
        let age = global + ABANDON_GRACE_MINUTES + 1;
        backdate(&conn, &long.id, age);
        backdate(&conn, &normal.id, age);

        assert_eq!(sweep_abandoned(&conn).unwrap(), 1, "exactly the default-clock run");
        assert_eq!(
            get(&conn, &long.id).unwrap().unwrap().status,
            "running",
            "a run given 60 minutes must not be reclaimed at {age}"
        );
        assert_eq!(get(&conn, &normal.id).unwrap().unwrap().status, STATUS_ABANDONED);

        // And it IS swept once past its own clock — otherwise a per-task
        // override would be a way to strand a slot forever.
        backdate(&conn, &long.id, 60 + ABANDON_GRACE_MINUTES + 1);
        assert_eq!(sweep_abandoned(&conn).unwrap(), 1);
    }

    /// A row written before v18 — or by any leftover pre-v18 binary — has a
    /// NULL `timeout_minutes`, which means "started under the global clock"
    /// because that was the only possibility then. The `COALESCE` is what makes
    /// that true; without it the date modifier is NULL, `datetime()` returns
    /// NULL, and the comparison matches NOTHING — so the sweep silently stops
    /// working and stranded runs wedge the gate shut again.
    #[test]
    fn a_pre_v18_row_with_no_recorded_clock_falls_back_to_the_global() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let legacy = start(
            &conn,
            &t,
            "background",
            None,
            BackgroundExecutor::Claude,
            BackgroundIntent::Work,
            None,
        )
        .unwrap();
        assert!(
            get(&conn, &legacy.id).unwrap().unwrap().timeout_minutes.is_none(),
            "the fixture must actually have a NULL clock or this proves nothing"
        );

        let global = crate::db::meta::budget_settings(&conn).timeout_minutes;
        backdate(&conn, &legacy.id, global + ABANDON_GRACE_MINUTES - 1);
        assert_eq!(sweep_abandoned(&conn).unwrap(), 0, "still inside the grace");
        backdate(&conn, &legacy.id, global + ABANDON_GRACE_MINUTES + 1);
        assert_eq!(sweep_abandoned(&conn).unwrap(), 1, "swept on the global clock");
    }

    /// The cutoff must be READ from settings, never baked in. Raising the
    /// timeout to 120 against a hardcoded 20 would start sweeping healthy runs
    /// half an hour in — the exact failure the guard exists to prevent,
    /// inverted, and silent.
    #[test]
    fn the_cutoff_follows_the_configured_timeout() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None)
            .unwrap();
        backdate(&conn, &run.id, 60);

        let mut settings = crate::db::meta::budget_settings(&conn);
        settings.timeout_minutes = 120;
        crate::db::meta::save_budget_settings(&conn, &settings).unwrap();
        assert_eq!(sweep_abandoned(&conn).unwrap(), 0, "a 120m timeout spares a 60m run");

        settings.timeout_minutes = 20;
        crate::db::meta::save_budget_settings(&conn, &settings).unwrap();
        assert_eq!(sweep_abandoned(&conn).unwrap(), 1, "a 20m timeout does not");
    }

    /// `abandoned` is its own status. Folding it into `failed` claims the work
    /// broke — which we do not know, and usually did not: the common cause is
    /// the RUNNER dying (gotcha #4) while the agent was fine. Folding it into
    /// `timeout` claims our wall-clock guard fired, which is an event that
    /// writes its own row and did not happen.
    #[test]
    fn an_abandoned_run_is_neither_failed_nor_timed_out() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None)
            .unwrap();
        backdate(&conn, &run.id, 999);
        sweep_abandoned(&conn).unwrap();

        let got = get(&conn, &run.id).unwrap().unwrap();
        assert_eq!(got.status, STATUS_ABANDONED);
        assert!(got.finished_at.is_some(), "a swept run must stop being open-ended");
        assert!(
            got.result_text.unwrap_or_default().contains("never recorded"),
            "it has to say what happened, or it reads as a mystery failure"
        );
    }

    /// A real outcome must never be overwritten. The sweep and a normal finish
    /// can race — a runner that reports at minute 36 against a 35-minute
    /// cutoff — and the recorded truth wins.
    #[test]
    fn the_sweep_cannot_rewrite_a_recorded_outcome() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None)
            .unwrap();
        finish(
            &conn,
            &run.id,
            &RunOutcome {
                status: "succeeded".into(),
                result_text: Some("the handoff".into()),
                ..Default::default()
            },
        )
        .unwrap();
        backdate(&conn, &run.id, 999);

        assert_eq!(sweep_abandoned(&conn).unwrap(), 0);
        let got = get(&conn, &run.id).unwrap().unwrap();
        assert_eq!(got.status, "succeeded");
        assert_eq!(got.result_text.as_deref(), Some("the handoff"));
    }

    /// Foreground runs have no wall clock to reason from — a session left open
    /// all day is legitimate — so the age-based sweep must not touch them. They
    /// are closed at spawn instead, and healed by `heal_foreground_runs`.
    #[test]
    fn the_sweep_leaves_foreground_runs_alone() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let fg = start(&conn, &t, "foreground", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None)
            .unwrap();
        backdate(&conn, &fg.id, 9999);
        assert_eq!(sweep_abandoned(&conn).unwrap(), 0);
        assert_eq!(get(&conn, &fg.id).unwrap().unwrap().status, "running");
    }

    /// The heal for the fossils: every foreground run the previous build
    /// created was left `running` with nothing anywhere able to close it.
    #[test]
    fn foreground_fossils_are_closed_and_dated_when_they_happened() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let fg = start(&conn, &t, "foreground", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None)
            .unwrap();
        backdate(&conn, &fg.id, 2880); // two days ago
        let started = get(&conn, &fg.id).unwrap().unwrap().started_at;

        assert_eq!(heal_foreground_runs(&conn).unwrap(), 1);
        let got = get(&conn, &fg.id).unwrap().unwrap();
        assert_eq!(got.status, FOREGROUND_STATUS);
        // Dated when it HAPPENED, not now — stamping the current time would
        // float a two-day-old session to the top of every newest-first list.
        assert_eq!(got.finished_at.as_deref(), Some(started.as_str()));

        // Idempotent: nothing left to heal on the next launch.
        assert_eq!(heal_foreground_runs(&conn).unwrap(), 0);
    }

    /// …and it must not reach across into background runs, which is where a
    /// mode filter is easy to drop. A live background run healed to
    /// "dispatched" would free a slot that is genuinely occupied.
    #[test]
    fn the_foreground_heal_never_touches_a_background_run() {
        let conn = test_conn();
        let t = seed_task(&conn);
        let bg = start(&conn, &t, "background", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None)
            .unwrap();
        assert_eq!(heal_foreground_runs(&conn).unwrap(), 0);
        assert_eq!(get(&conn, &bg.id).unwrap().unwrap().status, "running");
        assert_eq!(count_running_background(&conn).unwrap(), 1);
    }

    /// Both new statuses must be accepted by `finish`, which validates against
    /// `RUN_STATUSES`. A status the DB layer writes but the validator rejects
    /// is a runtime error on a recovery path — the worst place for one.
    #[test]
    fn the_new_statuses_are_valid_run_statuses() {
        assert!(RUN_STATUSES.contains(&FOREGROUND_STATUS));
        assert!(RUN_STATUSES.contains(&STATUS_ABANDONED));
        assert!(is_terminal(FOREGROUND_STATUS));
        assert!(is_terminal(STATUS_ABANDONED));

        let conn = test_conn();
        let t = seed_task(&conn);
        let run = start(&conn, &t, "foreground", None, BackgroundExecutor::Claude, BackgroundIntent::Work, None)
            .unwrap();
        finish(&conn, &run.id, &RunOutcome { status: FOREGROUND_STATUS.into(), ..Default::default() })
            .unwrap();
        assert_eq!(get(&conn, &run.id).unwrap().unwrap().status, FOREGROUND_STATUS);
    }
    // ── the atomic claim ───────────────────────────────────────────────────

    fn queued_task(conn: &Connection, title: &str, writes: bool) -> crate::db::tasks::Task {
        crate::db::tasks::create(
            conn,
            &crate::db::tasks::NewTask {
                title: title.into(),
                status: Some("queued".into()),
                bg_allowed: Some(true),
                bg_writes_allowed: Some(writes),
                repo_path: Some("/tmp/repo".into()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    /// The race the fold exists to close: two ticks each see one free slot,
    /// and the SECOND must lose inside the write lock rather than after it.
    #[test]
    fn the_concurrency_ceiling_is_re_checked_inside_the_claim() {
        let mut conn = crate::db::schema::test_conn();
        let a = queued_task(&conn, "a", false);
        let b = queued_task(&conn, "b", false);
        let here = crate::db::tasks::here();

        let first = claim_and_start(
            &mut conn, &here, &a.id, 1,
            BackgroundExecutor::Claude, BackgroundIntent::Work, 20,
        ).unwrap();
        assert!(first.is_some());

        // The gate said one slot; that slot is now taken. A second claim at the
        // same ceiling must come back empty rather than starting a second run.
        let second = claim_and_start(
            &mut conn, &here, &b.id, 1,
            BackgroundExecutor::Claude, BackgroundIntent::Work, 20,
        ).unwrap();
        assert!(
            second.is_none(),
            "two unattended agents started against a ceiling of one"
        );
        // ...and the loser is left QUEUED, not stranded in `in_progress` with no
        // run row — a task in that state is invisible to both the drain query
        // and the sweep, so it would never move again.
        assert_eq!(
            crate::db::tasks::get(&conn, &b.id).unwrap().unwrap().status,
            "queued"
        );
    }

    #[test]
    fn a_task_already_claimed_by_another_tick_yields_no_run() {
        let mut conn = crate::db::schema::test_conn();
        let t = queued_task(&conn, "t", false);
        let here = crate::db::tasks::here();
        assert!(claim_and_start(
            &mut conn, &here, &t.id, 4,
            BackgroundExecutor::Claude, BackgroundIntent::Work, 20,
        ).unwrap().is_some());
        assert!(
            claim_and_start(
                &mut conn, &here, &t.id, 4,
                BackgroundExecutor::Claude, BackgroundIntent::Work, 20,
            ).unwrap().is_none(),
            "the same task was dispatched twice"
        );
        assert_eq!(count_running_background(&conn).unwrap(), 1);
    }

    /// **The bug the reordering fixes.** The checkpoint ref is named for the
    /// RUN. Keyed on the task, two runs of one task write the same ref name, so
    /// the second silently overwrites the first's snapshot and a restore
    /// afterwards restores the wrong tree.
    #[test]
    fn two_runs_of_one_task_get_different_checkpoint_refs() {
        let mut conn = crate::db::schema::test_conn();
        let t = queued_task(&conn, "t", true);
        let here = crate::db::tasks::here();

        let mut refs = Vec::new();
        for _ in 0..2 {
            crate::db::tasks::update(
                &conn,
                &t.id,
                &crate::db::tasks::TaskPatch {
                    status: Some("queued".into()),
                    ..Default::default()
                },
            ).unwrap();
            let run = claim_and_start(
                &mut conn, &here, &t.id, 4,
                BackgroundExecutor::Claude, BackgroundIntent::Work, 20,
            ).unwrap().unwrap();
            let name = crate::checkpoint::ref_for_run(&run.id);
            set_checkpoint(&conn, &run.id, &name).unwrap();
            finish(
                &conn,
                &run.id,
                &RunOutcome { status: "succeeded".into(), ..Default::default() },
            )
            .unwrap();
            refs.push(name);
        }
        assert_ne!(
            refs[0], refs[1],
            "two runs of one task shared a checkpoint ref, so the second \
             overwrote the first's snapshot"
        );
        assert!(refs[0].starts_with(crate::checkpoint::REF_PREFIX));
    }

    /// A run row exists BEFORE the checkpoint is attempted, which is what lets
    /// a checkpoint failure be a recorded `failed` run instead of a task parked
    /// with no history at all.
    #[test]
    fn the_run_row_exists_before_any_checkpoint_is_attempted() {
        let mut conn = crate::db::schema::test_conn();
        let t = queued_task(&conn, "t", true);
        let run = claim_and_start(
            &mut conn, &crate::db::tasks::here(), &t.id, 4,
            BackgroundExecutor::Claude, BackgroundIntent::Work, 20,
        ).unwrap().unwrap();
        assert!(
            get(&conn, &run.id).unwrap().unwrap().scratch_dir.is_none(),
            "the run must start with no checkpoint — the ref is derived from its own id"
        );
        assert_eq!(list_for_task(&conn, &t.id).unwrap().len(), 1);
    }
}
