//! The two tripwire lanes of the runner's tick: scripts (FREE) and agent
//! checks (a bounded background run).
//!
//! # The free lane
//!
//! A script costs neither tokens nor a concurrency slot, so this pass runs
//! BEFORE the token/concurrency gate's early return — a day whose budget is
//! spent must still notice that PocketBuddy got its first user (gotcha #42).
//! The three switches (`STOP_BACKGROUND`, host paused, host disabled) stop
//! this lane too: one switch, one doctrine.
//!
//! ```text
//! claim_due     — list_due → claim (CAS) each SCRIPT spec, up to SCRIPTS_PER_TICK
//! run_claimed   — ONE worker thread, sequential: resolve the script, spawn,
//!                 drain both pipes concurrently, poll try_wait, kill at the
//!                 timeout, map the exit code → db::tripwires::record →
//!                 announce a transition (bridge envelope, else a banner)
//! ```
//!
//! No in-flight set: the lease IS the claim. A crash between claim and
//! record leaves a lease to expire, and the next tick reclaims it.
//!
//! # The agent lane (Release 2)
//!
//! An `agent:<condition>` tripwire is evaluated by a real `claude -p` — so it
//! sits BELOW the budget gate's return, takes ONE concurrency slot at most,
//! and runs through the same `run_one_inner` every background run does, as a
//! [`RunKind::Check`]. What makes it a check rather than a run:
//!
//! * **Admission** ([`agent_pass`]): one check in flight per host, and the
//!   check's whole token ceiling is RESERVED against the daily sub-cap
//!   (`tripwire_daily_token_cap`) before it starts, so the sub-cap can never
//!   be overshot by a check already burning. A refused admission is not
//!   recorded — the tripwire simply stays due for a later tick.
//! * **The run row IS the lease** (`db::runs::start_check`): inserted and
//!   claimed in one transaction, no task status change, one slot counted.
//! * **Bounds** ([`check_limits`]): 12 turns · 3M tokens · 10 min, each the
//!   smaller of the task's/global setting and the constant, where a task's
//!   `bg_run_token_cap = 0` ("no ceiling") yields the constant rather than no
//!   ceiling (`tripwire::min_nonzero`).
//! * **Pinned read-only**, one MCP tool (`get_task`), no checkpoint, no
//!   salvage turn, Claude only.
//! * **The verdict** ([`verdict_outcome`]): the last `VERDICT:` line of the
//!   final message; UNSURE, no verdict, or any non-success is an ERROR, which
//!   backs off and surfaces the task after the same five tries a script gets.
//!
//! [`RunKind::Check`]: crate::runner::RunKind::Check
//!
//! # The script's contract
//!
//! `<tripwires dir>/<name> [arg]`, cwd = the task's repo if it resolves, else
//! the data dir; env `CXTASKS_TASK_ID`, `CXTASKS_TASK_REF`, `CXTASKS_TASK_TITLE`,
//! `CXTASKS_TRIPWIRE=1`. Exit 0 = tripped (stdout is the evidence), 1 = not
//! yet, anything else / a timeout / a spawn failure = error (stderr is the
//! note). stdout and stderr are drained on their own threads, bounded to
//! [`CAPTURE_BYTES`] each with the rest discarded, so a noisy script cannot
//! fill a pipe and hang — the same lesson `process.rs` learned for agents.

use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::db::{self, meta::BudgetSettings, runs::RunOutcome, tasks::Task, tripwires::Transition};
use crate::error::AppError;
use crate::executor::BackgroundExecutor;
use crate::mcp::bridge::Envelope;
use crate::runner::{RunKind, RunLimits};
use crate::tripwire::{
    self, Outcome, Tripwire, Verdict, CAPTURE_BYTES, CHECK_MAX_TURNS, CHECK_TIMEOUT_MINUTES,
    CHECK_TOKEN_CEILING, SCRIPTS_PER_TICK, SCRIPT_LEASE_MINUTES, SCRIPT_TIMEOUT_SECS,
};

/// What a claimed attempt will run.
#[derive(Debug, Clone)]
pub enum Check {
    Script { name: String, arg: Option<String> },
    Jev { condition: String },
    /// A row hand-edited into nonsense, or a grammar a later client speaks
    /// that this build does not. Recorded as an error, so the task SURFACES
    /// rather than sitting armed forever.
    Unsupported(String),
}

/// How many due tripwires the agent lane looks through to find ONE agent
/// spec. Scripts and agents share `list_due`, so a board with many script
/// tripwires must not hide the one agent check behind the script limit.
const AGENT_SCAN: usize = 200;

/// One leased attempt, carrying everything needed to run it without the DB.
#[derive(Debug, Clone)]
pub struct Claimed {
    pub task: Task,
    pub lease_id: String,
    pub check: Check,
}

/// Take a lease on every due SCRIPT tripwire, up to `limit`.
///
/// An `agent:` spec is left alone here — not claimed, not marked — because
/// it belongs to the other lane, which has a budget to consult first. A
/// script lane that claimed it would have to record something, and "not
/// mine" is not an outcome a tripwire has.
pub fn claim_due(conn: &Connection, limit: usize) -> Result<Vec<Claimed>, AppError> {
    let host = crate::host::host_id();
    let mut out = Vec::new();
    for (task, _runtime) in db::tripwires::list_due(conn, limit)? {
        let Some(spec) = task.tripwire.clone() else { continue };
        let check = match Tripwire::parse(&spec) {
            Ok(Tripwire::Script { name, arg }) => Check::Script { name, arg },
            Ok(Tripwire::Agent { .. }) => continue,
            Ok(Tripwire::Jev { condition }) => {
                let status = crate::jev::status(conn);
                if !status.settings.enabled || !status.configured {
                    continue;
                }
                Check::Jev { condition }
            }
            Err(e) => Check::Unsupported(format!("unreadable tripwire spec '{spec}': {e}")),
        };
        let lease_id = uuid::Uuid::new_v4().to_string();
        if !db::tripwires::claim(conn, &task.id, &lease_id, &host, &spec, SCRIPT_LEASE_MINUTES)? {
            // Another process got there between list and claim. Its problem now.
            continue;
        }
        out.push(Claimed { task, lease_id, check });
    }
    Ok(out)
}

/// Claim what is due and evaluate it on one worker thread. Returns how many
/// attempts were started; the thread records each one as it finishes.
///
/// `in_flight` is bumped once for the batch so `--once` waits for it, the
/// same way it waits for agent runs.
pub fn script_pass(conn: &Connection, in_flight: &Arc<AtomicUsize>) -> Result<usize, AppError> {
    let claimed = claim_due(conn, SCRIPTS_PER_TICK)?;
    if claimed.is_empty() {
        return Ok(0);
    }
    let n = claimed.len();
    in_flight.fetch_add(1, Ordering::SeqCst);
    let in_flight = in_flight.clone();
    std::thread::spawn(move || {
        struct SlotGuard(Arc<AtomicUsize>);
        impl Drop for SlotGuard {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let _slot = SlotGuard(in_flight);
        let conn = match super::open_db() {
            Ok(c) => c,
            Err(e) => {
                // The leases expire and the next tick retries. Nothing is lost
                // but a few minutes.
                log::error!("tripwire: worker could not open the database ({e}); leases will expire");
                return;
            }
        };
        let (_transitions, pending) =
            run_claimed(&conn, &crate::paths::tripwires_dir(), claimed);
        if pending.is_empty() {
            return;
        }
        // Sequential, on this thread, below everything else the batch did. The
        // scripts have all recorded by now, so a ten-minute reading cannot make
        // one of them miss its lease.
        let mut conn = conn;
        let settings = db::meta::budget_settings(&conn);
        for p in &pending {
            if crate::paths::background_paused() {
                log::info!("tripwire: kill switch tripped; not interpreting the rest");
                break;
            }
            interpret_one(&mut conn, p, &settings);
        }
    });
    Ok(n)
}

// ── the agent lane ──────────────────────────────────────────────────────

/// The bounds one check runs under: each the smaller of what the task or
/// the global settings would give its WORK and the check constant.
///
/// `min_nonzero` is the load-bearing part — a task whose work runs with no
/// per-run ceiling (`bg_run_token_cap = 0`) must not hand that "no ceiling"
/// to a run nobody asked for today.
pub fn check_limits(task: &Task, settings: &BudgetSettings) -> RunLimits {
    RunLimits {
        mirror: None,
        extra_mcp: Vec::new(),
        timeout: Duration::from_secs(check_timeout_minutes(task, settings) as u64 * 60),
        model: check_model_for(task, settings),
        max_turns: tripwire::min_nonzero(settings.max_turns, CHECK_MAX_TURNS),
        token_ceiling: tripwire::min_nonzero(
            crate::runner::token_cap_for(task, settings),
            CHECK_TOKEN_CEILING,
        ),
    }
}

/// The wall clock for a check, in minutes. Resolved on its own because the
/// run row records it and the lease is derived from it.
pub fn check_timeout_minutes(task: &Task, settings: &BudgetSettings) -> i64 {
    tripwire::min_nonzero(crate::runner::timeout_for(task, settings), CHECK_TIMEOUT_MINUTES)
}

/// A check always runs on Claude, so a Codex task's own model name (which
/// belongs to the other vendor) is never handed to it; the global Claude
/// default is used instead. A Claude task's override IS honoured — that is
/// how "haiku for this tripwire" is expressed.
pub fn check_model_for(task: &Task, settings: &BudgetSettings) -> Option<String> {
    match task.bg_executor {
        BackgroundExecutor::Claude => crate::runner::model_for(task, settings),
        BackgroundExecutor::Codex => Some(settings.model.clone()),
    }
}

/// The ONE due agent tripwire the lane would run next, if any.
fn next_due_agent(conn: &Connection) -> Result<Option<(Task, String, String)>, AppError> {
    for (task, _rt) in db::tripwires::list_due(conn, AGENT_SCAN)? {
        let Some(spec) = task.tripwire.clone() else { continue };
        if let Ok(Tripwire::Agent { condition }) = Tripwire::parse(&spec) {
            return Ok(Some((task, spec, condition)));
        }
    }
    Ok(None)
}

/// Why the lane declined to start a check this tick — logged, never recorded,
/// because none of these is a fact about the CONDITION.
#[derive(Debug, PartialEq, Eq)]
pub enum CheckAdmission {
    /// Reserving this check's whole ceiling would cross the sub-cap.
    SubCapReached { spent: i64, ceiling: i64, cap: i64 },
    Admitted,
}

/// The sub-cap reservation: the check's full ceiling counts as spent from the
/// moment it is admitted, so a cap can never be overshot by a check already
/// in flight. `cap = 0` switches the sub-cap off (the daily cap still holds).
pub fn admit_check(spent: i64, ceiling: i64, cap: i64) -> CheckAdmission {
    if cap > 0 && spent.saturating_add(ceiling) > cap {
        CheckAdmission::SubCapReached { spent, ceiling, cap }
    } else {
        CheckAdmission::Admitted
    }
}

/// The agent-check lane: start at most `slots` checks (in practice one).
///
/// Returns how many started — each one holds a concurrency slot the caller
/// must subtract before draining queued work. Everything that can refuse
/// does so BEFORE a run row exists, except the workspace, which needs the
/// run id; that refusal finishes the row as `failed` and records an error
/// attempt, so the tripwire backs off and surfaces after the threshold.
pub fn agent_pass(
    conn: &mut Connection,
    in_flight: &Arc<AtomicUsize>,
    settings: &BudgetSettings,
    cap: &crate::routing::HostCapability,
    slots: i64,
) -> Result<usize, AppError> {
    if slots < 1 {
        return Ok(0);
    }
    let Some((task, spec, _condition)) = next_due_agent(conn)? else {
        return Ok(0);
    };
    let label = task.short_id.map(|n| format!("T{n}")).unwrap_or_else(|| task.id.clone());

    // Budget first, and a refusal here is silent on the task: a sub-cap that
    // is spent is a fact about the day, not about the condition.
    let limits = check_limits(&task, settings);
    let spent = db::runs::tripwire_tokens_today(conn)?;
    if let CheckAdmission::SubCapReached { spent, ceiling, cap } =
        admit_check(spent, limits.token_ceiling, settings.tripwire_daily_token_cap)
    {
        log::info!(
            "tripwire: {label} not checked — agent checks have spent {spent} of the {cap} daily \
             sub-cap and this one reserves {ceiling}"
        );
        return Ok(0);
    }

    // The CLI, before anything is written. A missing Claude is recorded as a
    // failed attempt (it IS a fact about whether the condition can be
    // evaluated here), through the lease path with no run row, exactly as a
    // missing script is.
    let Some(cli) = crate::runner::Provider::Claude.find_cli() else {
        record_without_run(
            conn,
            &task,
            &spec,
            "the Claude CLI wasn't found on this host, so the agent check could not run",
        );
        return Ok(0);
    };

    let timeout_minutes = check_timeout_minutes(&task, settings);
    let host_max = crate::runner::host_max_concurrent(settings, cap);
    let Some(run) = db::runs::start_check(
        conn,
        &task.id,
        &spec,
        host_max,
        BackgroundExecutor::Claude,
        timeout_minutes,
    )?
    else {
        // Another check is in flight, the host is at its ceiling, or the
        // lease was taken between `list_due` and here. All ordinary.
        return Ok(0);
    };

    let workspace = match crate::runner::ready_for(conn, cap, &task, &run.id, RunKind::Check) {
        Ok(w) => w,
        Err(reason) => {
            log::warn!("tripwire: {label} check refused: {reason}");
            let outcome = crate::runner::RunOutcome::default_failed(
                BackgroundExecutor::Claude,
                format!("Tripwire check refused: {reason}"),
            );
            let _ = db::runs::finish(conn, &run.id, &outcome);
            record_and_announce(conn, &task, &run.id, &Outcome::Error(reason));
            return Ok(0);
        }
    };
    let limits = RunLimits {
        extra_mcp: crate::mcp_registry::resolve_all(task.bg_mcp_servers.as_deref().unwrap_or_default())
            .map(|(ready, _)| ready)
            .unwrap_or_default(),
        mirror: workspace.snapshot.clone(),
        ..limits
    };

    in_flight.fetch_add(1, Ordering::SeqCst);
    let in_flight = in_flight.clone();
    let run_id = run.id.clone();
    let repo = workspace.path.clone();
    log::info!(
        "tripwire: {label} agent check starting as run {run_id} ({} turns · {} tokens · {} min)",
        limits.max_turns,
        limits.token_ceiling,
        timeout_minutes
    );
    std::thread::spawn(move || {
        struct SlotGuard(Arc<AtomicUsize>);
        impl Drop for SlotGuard {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let _slot = SlotGuard(in_flight);
        let outcome = crate::runner::run_one_recorded(
            &task,
            &repo,
            crate::runner::Provider::Claude,
            &cli,
            &limits,
            &run_id,
            RunKind::Check,
        );
        match super::open_db() {
            Ok(conn) => finish_check(&conn, &task, &run_id, &outcome),
            // The lease and the run row both expire on their own clocks; the
            // next tick reclaims the tripwire and the sweep the row.
            Err(e) => log::error!("tripwire: could not open the database to record check {run_id}: {e}"),
        }
        crate::runner::release(&workspace);
    });
    Ok(1)
}

/// Close a check: finish the run row, translate the final message into a
/// tripwire outcome, record it against the run-id lease, announce.
///
/// No comment and no task-status patch — `db::tripwires::record` writes the
/// comment on a TRANSITION, and a not-yet is deliberately silent (a check
/// that commented "not yet" every hour would bury the log). The run row
/// carries the full message for anyone who wants to read the reasoning.
pub fn finish_check(conn: &Connection, task: &Task, run_id: &str, outcome: &RunOutcome) {
    if let Err(e) = db::runs::finish(conn, run_id, outcome) {
        log::error!("tripwire: could not record check run {run_id}: {e}");
    }
    let tw = verdict_outcome(outcome);
    log::info!(
        "tripwire: {} agent check → {} ({} tokens)",
        task.short_id.map(|n| format!("T{n}")).unwrap_or_else(|| task.id.clone()),
        tw.kind().as_str(),
        outcome.input_tokens.unwrap_or(0)
            + outcome.cache_read_tokens.unwrap_or(0)
            + outcome.cache_write_tokens.unwrap_or(0)
            + outcome.output_tokens.unwrap_or(0)
    );
    record_and_announce(conn, task, run_id, &tw);
    // The app live-refreshes on a run ending; a check is a run. Envelope
    // only — no banner from here, because a not-yet is not news, and a trip
    // has already been announced by `record_and_announce`.
    let env = Envelope::new("run-finished", &task.id)
        .with_tool("tripwire")
        .with_run_id(run_id)
        .with_summary(&format!("tripwire check: {}", tw.kind().as_str()));
    let _ = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(crate::mcp::bridge::notify_delivered(&env)));
}

/// A run's outcome, read as a tripwire attempt.
///
/// Only a `succeeded` run is even asked for its verdict; a timeout, a
/// ceiling, a cancel or a spawn failure is an error naming the status. Then
/// the last `VERDICT:` line decides, and UNSURE is an error on purpose — a
/// task must not stay hidden on the strength of a shrug, and five shrugs in
/// a row surface it.
pub fn verdict_outcome(outcome: &RunOutcome) -> Outcome {
    let text = outcome.result_text.as_deref().unwrap_or("").trim();
    if outcome.status != "succeeded" {
        let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        return Outcome::Error(match outcome.stop_detail.as_deref() {
            Some(d) => format!("check {}: {d}", outcome.status),
            None if first.is_empty() => format!("check {}", outcome.status),
            None => format!("check {}: {first}", outcome.status),
        });
    }
    let (verdict, evidence) = tripwire::parse_verdict(text);
    match verdict {
        Some(Verdict::Tripped) => Outcome::Tripped(evidence),
        Some(Verdict::NotYet) => Outcome::NotYet(evidence),
        Some(Verdict::Unsure) => Outcome::Error(format!("the check was UNSURE: {evidence}")),
        None => Outcome::Error(format!(
            "the check's final message had no VERDICT line: {}",
            text.chars().take(200).collect::<String>()
        )),
    }
}

/// Record an attempt that never got a run row — the lease is a fresh id.
fn record_without_run(conn: &Connection, task: &Task, spec: &str, reason: &str) {
    let lease = uuid::Uuid::new_v4().to_string();
    match db::tripwires::claim(conn, &task.id, &lease, &crate::host::host_id(), spec, SCRIPT_LEASE_MINUTES) {
        Ok(true) => record_and_announce(conn, task, &lease, &Outcome::Error(reason.to_string())),
        Ok(false) => {}
        Err(e) => log::error!("tripwire: could not claim {} to record a refusal: {e}", task.id),
    }
}

fn record_and_announce(conn: &Connection, task: &Task, lease_id: &str, outcome: &Outcome) {
    match db::tripwires::record(conn, &task.id, lease_id, outcome) {
        Ok(recorded) => {
            if recorded.discarded {
                log::info!("tripwire: check on {} discarded (task changed or lease lost)", task.id);
            }
            if let Some(t) = recorded.transition {
                announce(task, &t);
            }
        }
        Err(e) => log::error!("tripwire: could not record the check on {}: {e}", task.id),
    }
}

/// A trip that asked to be INTERPRETED, with its evidence in full.
///
/// The evidence here is what the script actually wrote (bounded by
/// [`CAPTURE_BYTES`]), not the `last_note` the attempt stored — `bound_note`
/// keeps [`tripwire::NOTE_CHARS`] and the report that motivated this feature is
/// ten times that. Carrying it out of `run_claimed` in memory is the only way
/// the interpretation sees the same bytes a human running the script by hand
/// would.
#[derive(Debug, Clone)]
pub struct Pending {
    pub task: Task,
    pub evidence: String,
}

/// Evaluate each claimed attempt in turn and record it. Announces every
/// transition AFTER its transaction commits.
///
/// Returns the transitions **and** the trips whose `on_trip` asked for an
/// interpretation. It returns them rather than launching them so this function
/// stays free of `claude -p`: it is the one the tests drive directly, and a
/// spawn hidden in here would make every tripwire test either spend tokens or
/// need a fake CLI.
pub fn run_claimed(
    conn: &Connection,
    dir: &Path,
    claimed: Vec<Claimed>,
) -> (Vec<Transition>, Vec<Pending>) {
    let mut announced = Vec::new();
    let mut pending = Vec::new();
    for c in claimed {
        let outcome = match &c.check {
            Check::Unsupported(reason) => Outcome::Error(reason.clone()),
            Check::Script { name, arg } => match tripwire::script_path_in(dir, name) {
                Err(e) => Outcome::Error(e),
                Ok(path) => run_script(
                    &path,
                    arg.as_deref(),
                    &cwd_for(&c.task, dir),
                    &env_for(&c.task),
                    Duration::from_secs(SCRIPT_TIMEOUT_SECS),
                ),
            },
            Check::Jev { condition } => match crate::jev::condition_holds(conn, &c.task, condition) {
                Ok(p) if p >= 0.75 => Outcome::Tripped(format!(
                    "Jev judged the condition satisfied (probability {p:.2})"
                )),
                Ok(p) => Outcome::NotYet(format!(
                    "Jev judged the condition not yet satisfied (probability {p:.2})"
                )),
                Err(e) => Outcome::Error(format!("Jev check failed: {e}")),
            },
        };
        log::info!(
            "tripwire: {} {} → {}",
            c.task.short_id.map(|n| format!("T{n}")).unwrap_or_else(|| c.task.id.clone()),
            c.task.tripwire.as_deref().unwrap_or("?"),
            outcome.kind().as_str()
        );
        match db::tripwires::record(conn, &c.task.id, &c.lease_id, &outcome) {
            Ok(recorded) => {
                if recorded.discarded {
                    log::info!("tripwire: attempt on {} discarded (task changed or lease lost)", c.task.id);
                }
                if let Some(t) = recorded.transition {
                    // Only a committed TRIP earns an interpretation, and only
                    // one that was not discarded — reading the output of a
                    // check nobody is waiting for any more would comment on a
                    // task whose spec has since changed.
                    if matches!(t, Transition::Tripped { .. })
                        && crate::tripwire::OnTrip::from_db(c.task.tripwire_on_trip.as_deref())
                            .wants_interpretation()
                    {
                        let evidence = outcome.note().trim().to_string();
                        if evidence.is_empty() {
                            log::info!(
                                "tripwire: {} asked for an interpretation and the check produced \
                                 no output, so there is nothing to read",
                                c.task.id
                            );
                        } else {
                            pending.push(Pending { task: c.task.clone(), evidence });
                        }
                    }
                    announce(&c.task, &t);
                    announced.push(t);
                }
            }
            Err(e) => log::error!("tripwire: could not record attempt on {}: {e}", c.task.id),
        }
    }
    (announced, pending)
}

// ── the interpretation lane ─────────────────────────────────────────────

/// Why the lane declined to interpret — logged, never recorded. None of these
/// is a fact about the evidence, and the evidence is already on the task.
#[derive(Debug, PartialEq, Eq)]
pub enum InterpretAdmission {
    /// The day's whole token budget would be crossed by this run's ceiling.
    DailyCapReached { spent: i64, ceiling: i64, cap: i64 },
    /// The tripwire lane's own sub-cap would be crossed.
    SubCapReached { spent: i64, ceiling: i64, cap: i64 },
    Admitted,
}

/// The budget admission for an interpretation, as pure arithmetic.
///
/// **It has to be asked here, by this lane, because of where the lane sits.**
/// The script pass runs ABOVE the tick's budget return on purpose — a spent
/// budget is no reason not to run a zero-token script (gotcha #42) — and an
/// interpretation is launched from inside that pass. So it cannot inherit the
/// gate's answer and must re-ask the two questions the gate would have asked:
/// the global daily cap first and the tripwire sub-cap second, in that order,
/// for `runner::gate`'s own reason (a host asked to explain itself should not
/// name its share of a budget that is already gone).
///
/// The whole ceiling is reserved, not the expected spend, so a cap cannot be
/// overshot by a run already burning — `admit_check`'s rule, unchanged.
pub fn admit_interpret(
    tokens_today: i64,
    daily_cap: i64,
    tripwire_today: i64,
    sub_cap: i64,
    ceiling: i64,
) -> InterpretAdmission {
    if daily_cap > 0 && tokens_today.saturating_add(ceiling) > daily_cap {
        return InterpretAdmission::DailyCapReached {
            spent: tokens_today,
            ceiling,
            cap: daily_cap,
        };
    }
    if sub_cap > 0 && tripwire_today.saturating_add(ceiling) > sub_cap {
        return InterpretAdmission::SubCapReached {
            spent: tripwire_today,
            ceiling,
            cap: sub_cap,
        };
    }
    InterpretAdmission::Admitted
}

/// The bounds one interpretation runs under.
///
/// No `mirror` and no `extra_mcp`, and neither is an omission: there is no
/// workspace to describe and no server to name (`claude::command` returns no
/// `--mcp-config` for this kind, so under `--strict-mcp-config` an entry here
/// would be dead weight at best). `min_nonzero` for the same reason a check
/// uses it — a task whose work runs uncapped must not hand "no ceiling" to a
/// run nobody asked for today.
pub fn interpret_limits(task: &Task, settings: &BudgetSettings) -> RunLimits {
    RunLimits {
        mirror: None,
        extra_mcp: Vec::new(),
        timeout: Duration::from_secs(interpret_timeout_minutes(task, settings) as u64 * 60),
        model: check_model_for(task, settings),
        max_turns: tripwire::min_nonzero(settings.max_turns, tripwire::INTERPRET_MAX_TURNS),
        token_ceiling: tripwire::min_nonzero(
            crate::runner::token_cap_for(task, settings),
            tripwire::INTERPRET_TOKEN_CEILING,
        ),
    }
}

/// The wall clock for an interpretation, in minutes. Resolved on its own
/// because the run row records it and the sweep judges against it.
pub fn interpret_timeout_minutes(task: &Task, settings: &BudgetSettings) -> i64 {
    tripwire::min_nonzero(
        crate::runner::timeout_for(task, settings),
        tripwire::INTERPRET_TIMEOUT_MINUTES,
    )
}

/// The directory an interpretation runs in.
///
/// **Never `$HOME` and never the task's repo** — an app-owned scratch dir under
/// the data dir, created empty. With zero tools this is cosmetic as a boundary
/// and is set anyway, for the reason `WorkspaceKind::Detached` gives: the cwd of
/// an unattended process is not the place to be relaxed, and `claude` resolves
/// its own project-level config against it. A repo-less task has no repo to use
/// and a repo-bound one must not have its tree touched by a run that cannot see
/// it.
fn interpret_cwd(run_id: &str) -> Result<PathBuf, String> {
    let path = crate::paths::run_workspace(run_id);
    std::fs::create_dir_all(&path)
        .map_err(|e| format!("could not create {}: {e}", path.display()))?;
    Ok(path)
}

/// Read one trip's evidence and write what it means onto the task.
///
/// Runs SEQUENTIALLY on the script lane's worker thread, which already holds an
/// `in_flight` slot for its batch — so `--once` waits for it without any extra
/// bookkeeping, and two interpretations can never overlap on one host even
/// before `start_interpret`'s own check.
///
/// Every refusal is a log line and nothing else. The evidence is already on the
/// task as a comment, which is exactly today's behaviour, so a missed
/// interpretation costs a reading and never the report.
pub fn interpret_one(conn: &mut Connection, p: &Pending, settings: &BudgetSettings) -> bool {
    let task = &p.task;
    let label = task.short_id.map(|n| format!("T{n}")).unwrap_or_else(|| task.id.clone());

    let limits = interpret_limits(task, settings);
    let tokens_today = db::runs::total_tokens_today(conn).unwrap_or(0);
    let tripwire_today = db::runs::tripwire_tokens_today(conn).unwrap_or(0);
    match admit_interpret(
        tokens_today,
        settings.daily_token_cap,
        tripwire_today,
        settings.tripwire_daily_token_cap,
        limits.token_ceiling,
    ) {
        InterpretAdmission::Admitted => {}
        other => {
            log::info!("tripwire: {label} tripped but was not interpreted — {other:?}");
            return false;
        }
    }

    let Some(cli) = crate::runner::Provider::Claude.find_cli() else {
        log::warn!(
            "tripwire: {label} tripped and asked for an interpretation, but the Claude CLI \
             wasn't found on this host"
        );
        return false;
    };

    let timeout_minutes = interpret_timeout_minutes(task, settings);
    // The host's declared narrowing is not available here — the script lane has
    // no `HostCapability` and must not grow a reason to register one — so this
    // is the GLOBAL ceiling. Narrower than it would be is the safe direction,
    // and `start_interpret` re-reads the count inside its own transaction.
    let max_concurrent = settings.max_concurrent;
    let run = match db::runs::start_interpret(conn, &task.id, max_concurrent, timeout_minutes) {
        Ok(Some(r)) => r,
        // At the host's ceiling, or the lane is already busy. Ordinary.
        Ok(None) => return false,
        Err(e) => {
            log::warn!("tripwire: could not start an interpretation for {label}: {e}");
            return false;
        }
    };

    // The evidence is staged BEFORE the prompt is built, because the prompt
    // builder reads it back off disk (see `dispatch::write_interpret_evidence`).
    if let Err(e) = crate::dispatch::write_interpret_evidence(task, &p.evidence) {
        let outcome = crate::runner::RunOutcome::default_failed(
            BackgroundExecutor::Claude,
            format!("could not stage the evidence to interpret: {e}"),
        );
        let _ = db::runs::finish(conn, &run.id, &outcome);
        return false;
    }
    let repo = match interpret_cwd(&run.id) {
        Ok(p) => p,
        Err(reason) => {
            let outcome = crate::runner::RunOutcome::default_failed(
                BackgroundExecutor::Claude,
                format!("Interpretation refused: {reason}"),
            );
            let _ = db::runs::finish(conn, &run.id, &outcome);
            return false;
        }
    };

    log::info!(
        "tripwire: {label} interpreting {} chars of evidence as run {} ({} turns · {} tokens · \
         {timeout_minutes} min)",
        p.evidence.chars().count(),
        run.id,
        limits.max_turns,
        limits.token_ceiling
    );
    let outcome = crate::runner::run_one_recorded(
        task,
        &repo,
        crate::runner::Provider::Claude,
        &cli,
        &limits,
        &run.id,
        RunKind::Interpret,
    );
    finish_interpret(conn, task, &run.id, &outcome);
    // The scratch workspace is ours and holds nothing anybody reads — unlike
    // the per-task scratch dir, which keeps the prompt and the evidence.
    let _ = std::fs::remove_dir_all(&repo);
    true
}

/// Close an interpretation: finish the run row, write the reading onto the task.
///
/// The comment is the product, so a run that produced no usable text writes
/// nothing rather than a comment saying it failed: the evidence is already on
/// the task under the same author, and a second entry announcing that the
/// reading did not happen buries it. The run row carries the status for anyone
/// asking why.
pub fn finish_interpret(conn: &Connection, task: &Task, run_id: &str, outcome: &RunOutcome) {
    if let Err(e) = db::runs::finish(conn, run_id, outcome) {
        log::error!("tripwire: could not record interpretation run {run_id}: {e}");
    }
    let label = task.short_id.map(|n| format!("T{n}")).unwrap_or_else(|| task.id.clone());
    let text = outcome.result_text.as_deref().unwrap_or("").trim();
    if outcome.status != "succeeded" || text.is_empty() {
        log::warn!(
            "tripwire: {label} interpretation ended {} with {} chars — nothing written to the \
             task; the evidence comment stands on its own",
            outcome.status,
            text.chars().count()
        );
        return;
    }
    let spec = task.tripwire.as_deref().unwrap_or("the tripwire");
    if let Err(e) = crate::db::comments::add(
        conn,
        &task.id,
        crate::db::comments::AUTHOR_TRIPWIRE,
        &format!("What it means · {spec}\n\n{text}"),
    ) {
        log::error!("tripwire: could not write the interpretation onto {label}: {e}");
        return;
    }
    let env = Envelope::new("run-finished", &task.id)
        .with_tool("tripwire")
        .with_run_id(run_id)
        .with_summary("tripwire interpretation");
    let _ = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(crate::mcp::bridge::notify_delivered(&env)));
}

/// The task's repo when it is a real directory, else the data dir — which is
/// the tripwires directory's parent, so a test's temp root serves the same
/// role.
fn cwd_for(task: &Task, dir: &Path) -> PathBuf {
    if let Some(repo) = task.repo_path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let p = PathBuf::from(repo);
        if p.is_dir() {
            return p;
        }
    }
    dir.parent().map(Path::to_path_buf).unwrap_or_else(|| dir.to_path_buf())
}

fn env_for(task: &Task) -> Vec<(&'static str, String)> {
    vec![
        ("CXTASKS_TASK_ID", task.id.clone()),
        (
            "CXTASKS_TASK_REF",
            task.short_id.map(|n| format!("T{n}")).unwrap_or_default(),
        ),
        ("CXTASKS_TASK_TITLE", task.title.clone()),
        ("CXTASKS_TRIPWIRE", "1".into()),
    ]
}

/// What a reader thread has captured so far — shared, because the thread may
/// outlive the attempt (see [`Capture::finish`]).
struct Capture {
    buf: Arc<Mutex<(Vec<u8>, bool)>>,
    handle: JoinHandle<()>,
}

impl Capture {
    /// Collect the text. Waits for the reader to finish for at most `grace`,
    /// then takes whatever it has.
    ///
    /// The bounded wait is the point. A pipe stays open as long as ANY
    /// process holds its write end, and a script that backgrounds something
    /// (`some-daemon &`) — or one killed while its own child lives on — leaves
    /// a writer behind. Joining unconditionally would wait on that stranger
    /// for as long as it likes; the runner tick would be stuck behind a
    /// process it did not start. So the thread is left to die when the pipe
    /// finally closes, and the attempt moves on with what it saw.
    fn finish(self, grace: Duration) -> (String, bool) {
        let deadline = Instant::now() + grace;
        while !self.handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if self.handle.is_finished() {
            let _ = self.handle.join();
        }
        let (bytes, dropped) = self.buf.lock().map(|g| g.clone()).unwrap_or_default();
        (String::from_utf8_lossy(&bytes).into_owned(), dropped)
    }
}

/// Read a pipe to exhaustion on its own thread, keeping the first
/// [`CAPTURE_BYTES`].
///
/// Keeps READING after the cap — the point is that the child can never block
/// on a full pipe, and stopping at the cap would be exactly that.
fn drain<R: Read + Send + 'static>(reader: Option<R>) -> Capture {
    let buf: Arc<Mutex<(Vec<u8>, bool)>> = Arc::new(Mutex::new((Vec::new(), false)));
    let shared = buf.clone();
    let handle = std::thread::spawn(move || {
        let Some(mut r) = reader else { return };
        let mut scratch = [0u8; 8192];
        loop {
            match r.read(&mut scratch) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut g) = shared.lock() {
                        let room = CAPTURE_BYTES.saturating_sub(g.0.len());
                        if room >= n {
                            g.0.extend_from_slice(&scratch[..n]);
                        } else {
                            g.0.extend_from_slice(&scratch[..room]);
                            g.1 = true;
                        }
                    }
                }
            }
        }
    });
    Capture { buf, handle }
}

/// Kill the script AND everything it started. The child is put in its own
/// process group at spawn, so one signal to the group reaches a `sleep` the
/// script is waiting on — killing `sh` alone would leave that grandchild
/// running for its full duration with the stdout pipe still open.
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as libc::pid_t;
        // Negative pid = the whole group. Best effort; the plain kill follows.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Spawn one script and map how it ended onto an [`Outcome`].
pub fn run_script(
    path: &Path,
    arg: Option<&str>,
    cwd: &Path,
    env: &[(&str, String)],
    timeout: Duration,
) -> Outcome {
    let mut cmd = Command::new(path);
    if let Some(a) = arg {
        // Through `arg`, never a shell: the value reaches the script as one
        // argv entry whatever characters it holds.
        cmd.arg(a);
    }
    cmd.current_dir(cwd)
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so `kill_tree` can reach what it spawns.
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome::Error(format!("could not start {}: {e}", path.display())),
    };
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());

    let started = Instant::now();
    let ended = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    kill_tree(&mut child);
                    break Err(format!("timed out after {} s and was killed", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => {
                kill_tree(&mut child);
                break Err(format!("could not wait for the script: {e}"));
            }
        }
    };
    // The child has exited; its pipes close unless something it started is
    // still holding them. Two seconds is generous for a closed pipe and short
    // enough that a stranger holding it open cannot stall the tick.
    let (stdout, out_dropped) = out.finish(Duration::from_secs(2));
    let (stderr, err_dropped) = err.finish(Duration::from_secs(2));
    let mark = |s: String, dropped: bool| {
        let s = s.trim().to_string();
        if dropped {
            format!("{s} …[output truncated at {CAPTURE_BYTES} bytes]")
        } else {
            s
        }
    };
    let stdout = mark(stdout, out_dropped);
    let stderr = mark(stderr, err_dropped);
    let detail = |primary: &str, secondary: &str| -> String {
        if !primary.is_empty() {
            primary.to_string()
        } else {
            secondary.to_string()
        }
    };

    match ended {
        Err(why) => Outcome::Error(match stderr.is_empty() {
            true => why,
            false => format!("{why}: {stderr}"),
        }),
        Ok(status) => match status.code() {
            Some(0) => Outcome::Tripped(stdout),
            Some(1) => Outcome::NotYet(detail(&stdout, &stderr)),
            Some(code) => Outcome::Error(format!("exit {code}: {}", detail(&stderr, &stdout))),
            None => Outcome::Error(format!("killed by a signal: {}", detail(&stderr, &stdout))),
        },
    }
}

/// Hand the transition to whoever is in front of the user. The app, if it is
/// listening, posts the banner around what is on screen; otherwise this
/// process posts through the platform hook (silent on a host with nobody at
/// it — see `notify`).
fn announce(task: &Task, t: &Transition) {
    let (kind, title, note) = match t {
        Transition::Tripped { note, completed, .. } => (
            "tripwire-tripped",
            if *completed {
                "CXTasks — tripwire tripped, task completed"
            } else {
                "CXTasks — tripwire tripped"
            },
            note,
        ),
        Transition::Failed { note, .. } => ("tripwire-failed", "CXTasks — tripwire check failed", note),
    };
    let first_line = note
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(120)
        .collect::<String>();
    let summary = if first_line.is_empty() {
        task.title.clone()
    } else {
        format!("{} — {first_line}", task.title)
    };
    let env = Envelope::new(kind, &task.id)
        .with_tool("tripwire")
        .with_summary(&summary);
    let delivered = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(crate::mcp::bridge::notify_delivered(&env)))
        .unwrap_or(false);
    if !delivered {
        crate::notify::post(&task.id, title, &summary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{create, get, NewTask, TaskPatch};
    use crate::runner::{lanes, Gate};
    use std::os::unix::fs::PermissionsExt;

    /// A throwaway data root with a `tripwires/` inside it, so `cwd_for`'s
    /// fallback (the dir's parent) is a real directory.
    fn root(tag: &str) -> PathBuf {
        let r = std::env::temp_dir().join(format!("cxtasks-tw-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(r.join("tripwires")).unwrap();
        r
    }

    fn script(root: &Path, name: &str, body: &str) {
        let p = root.join("tripwires").join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn armed(conn: &Connection, spec: &str, every: Option<i64>, on_trip: Option<&str>) -> Task {
        create(
            conn,
            &NewTask {
                title: "gate".into(),
                tripwire: Some(spec.into()),
                tripwire_every_minutes: every,
                tripwire_on_trip: on_trip.map(str::to_string),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn pass(conn: &Connection, root: &Path) -> Vec<Transition> {
        pass_full(conn, root).0
    }

    /// `pass` plus whatever asked to be interpreted. Separate so the dozens of
    /// existing callers stay a one-liner and the interpretation tests can see
    /// the half they care about.
    fn pass_full(conn: &Connection, root: &Path) -> (Vec<Transition>, Vec<Pending>) {
        let claimed = claim_due(conn, SCRIPTS_PER_TICK).unwrap();
        run_claimed(conn, &root.join("tripwires"), claimed)
    }

    // ── on_trip = interpret ─────────────────────────────────────────────

    /// **The T226 shape, and the measurement that decided the design.** A trip
    /// under `interpret` carries its evidence out IN FULL, while the comment the
    /// task gets is bounded to `NOTE_CHARS` — so an interpretation reading the
    /// comment would be reading a ninth of its own report.
    #[test]
    fn a_trip_asking_to_be_interpreted_carries_more_evidence_than_the_comment_can_hold() {
        let conn = test_conn();
        let r = root("interpret");
        // 60 lines of ~30 chars: comfortably past NOTE_CHARS (500), comfortably
        // inside CAPTURE_BYTES (4096).
        script(
            &r,
            "io-report",
            "i=0; while [ $i -lt 60 ]; do echo \"2026-09-30 host 426.39 GB line $i\"; i=$((i+1)); done; exit 0",
        );
        let t = armed(&conn, "script:io-report", Some(60), Some("interpret"));

        let (transitions, pending) = pass_full(&conn, &r);
        assert!(matches!(&transitions[..], [Transition::Tripped { completed: false, .. }]), "{transitions:?}");
        assert_eq!(pending.len(), 1, "the trip asked to be interpreted: {pending:?}");
        assert_eq!(pending[0].task.id, t.id);

        let evidence = &pending[0].evidence;
        assert!(evidence.contains("line 0") && evidence.contains("line 59"), "the whole output");
        assert!(
            evidence.chars().count() > tripwire::NOTE_CHARS,
            "the fixture has to be bigger than the note, or it proves nothing: {}",
            evidence.chars().count()
        );

        // And the comment really is the truncated half — this is the gap.
        let log = crate::db::comments::for_task(&conn, &t.id).unwrap();
        assert!(log[0].body.contains("line 0"), "{}", log[0].body);
        assert!(
            !log[0].body.contains("line 59"),
            "if the comment held the whole report there would be nothing to fix: {}",
            log[0].body
        );

        // An interpretation does NOT complete the task, and does not re-arm it.
        let after = get(&conn, &t.id).unwrap().unwrap();
        assert_ne!(after.status, "done", "interpret surfaces, it does not close");
        assert_eq!(after.tripwire_state.as_deref(), Some("tripped"));
        assert!(!after.is_monitor(), "interpret is not monitor");
        let _ = std::fs::remove_dir_all(&r);
    }

    /// Every other policy asks for nothing, and a not-yet never does — which is
    /// what keeps a cadenced tripwire from paying for a reading every hour.
    #[test]
    fn no_other_policy_asks_for_an_interpretation() {
        for (policy, body) in [
            (None, "echo yes; exit 0"),
            (Some("complete"), "echo yes; exit 0"),
            (Some("monitor"), "echo yes; exit 0"),
            (Some("interpret"), "echo no; exit 1"),
            (Some("interpret"), "echo boom >&2; exit 7"),
            // Tripped with NO output: there is nothing to read.
            (Some("interpret"), "exit 0"),
        ] {
            let conn = test_conn();
            let r = root("nointerp");
            script(&r, "s", body);
            armed(&conn, "script:s", Some(60), policy);
            let (_, pending) = pass_full(&conn, &r);
            assert!(pending.is_empty(), "{policy:?} / {body} asked for an interpretation");
            let _ = std::fs::remove_dir_all(&r);
        }
    }

    /// A spec whose task is no longer ours is DISCARDED, and a discarded
    /// attempt must not produce a reading either — commenting on a task whose
    /// tripwire has since changed is a confident answer to a stale question.
    #[test]
    fn a_discarded_attempt_asks_for_no_interpretation() {
        let conn = test_conn();
        let r = root("discard-interp");
        script(&r, "slow-true", "echo 'the report'; exit 0");
        let t = armed(&conn, "script:slow-true", Some(60), Some("interpret"));
        let claimed = claim_due(&conn, SCRIPTS_PER_TICK).unwrap();
        assert_eq!(claimed.len(), 1);
        // The spec changes under the claim.
        crate::db::tasks::update(
            &conn,
            &t.id,
            &TaskPatch { tripwire: Some(Some("script:something-else".into())), ..Default::default() },
        )
        .unwrap();
        let (transitions, pending) = run_claimed(&conn, &r.join("tripwires"), claimed);
        assert!(transitions.is_empty(), "{transitions:?}");
        assert!(pending.is_empty(), "a discarded attempt produced a reading: {pending:?}");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn exit_zero_trips_and_with_complete_finishes_the_task() {
        let conn = test_conn();
        let r = root("trip");
        script(&r, "always-true", "echo 'user 7 linked a bank'; exit 0");
        let t = armed(&conn, "script:always-true", Some(5), Some("complete"));
        let dependent = create(&conn, &NewTask { title: "legal".into(), ..Default::default() }).unwrap();
        crate::db::deps::add(&conn, &dependent.id, &t.id).unwrap();

        let transitions = pass(&conn, &r);
        assert!(matches!(&transitions[..], [Transition::Tripped { completed: true, .. }]), "{transitions:?}");
        let after = get(&conn, &t.id).unwrap().unwrap();
        assert_eq!(after.status, "done");
        assert_eq!(after.tripwire_state.as_deref(), Some("tripped"));
        let log = crate::db::comments::for_task(&conn, &t.id).unwrap();
        assert!(log[0].body.contains("user 7 linked a bank"), "{}", log[0].body);
        assert!(crate::db::deps::blocked_task_ids(&conn).unwrap().is_empty(), "dependent released");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn exit_one_is_not_yet_and_the_task_stays_hidden() {
        let conn = test_conn();
        let r = root("notyet");
        script(&r, "not-yet", "echo '0 users'; exit 1");
        let t = armed(&conn, "script:not-yet", Some(5), None);
        let transitions = pass(&conn, &r);
        assert!(transitions.is_empty());
        let after = get(&conn, &t.id).unwrap().unwrap();
        assert!(after.tripwire_armed());
        let rt = crate::db::tripwires::for_task(&conn, &t.id).unwrap().unwrap();
        assert_eq!(rt.last_outcome.as_deref(), Some("not_yet"));
        assert_eq!(rt.last_note.as_deref(), Some("0 users"));
        assert!(rt.lease_id.is_none());
        // Hidden from the actionable views, present in Waiting — the one
        // "not yet" list (invariant 26).
        use crate::db::tasks::{list, ListFilters, View};
        let open = list(&conn, &ListFilters { view: Some(View::Open), ..Default::default() }).unwrap();
        assert!(open.is_empty());
        let waiting = list(&conn, &ListFilters { view: Some(View::Waiting), ..Default::default() }).unwrap();
        assert_eq!(waiting.len(), 1);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_manual_tripwire_that_errors_surfaces_at_once_with_stderr_as_the_note() {
        let conn = test_conn();
        let r = root("err");
        script(&r, "broken", "echo 'no such table: users' >&2; exit 2");
        let t = armed(&conn, "script:broken", None, None);
        assert!(pass(&conn, &r).is_empty(), "manual and unrequested: nothing runs");
        crate::db::tasks::update(&conn, &t.id, &TaskPatch { tripwire_check_requested: Some(true), ..Default::default() }).unwrap();
        let transitions = pass(&conn, &r);
        assert!(matches!(&transitions[..], [Transition::Failed { failures: 1, .. }]), "{transitions:?}");
        let after = get(&conn, &t.id).unwrap().unwrap();
        assert_eq!(after.tripwire_state.as_deref(), Some("error"));
        let rt = crate::db::tripwires::for_task(&conn, &t.id).unwrap().unwrap();
        assert_eq!(rt.last_note.as_deref(), Some("exit 2: no such table: users"));
        assert!(rt.requested_at.is_none(), "the request was served");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_missing_script_is_an_error_not_a_hang_and_a_cadenced_one_backs_off() {
        let conn = test_conn();
        let r = root("missing");
        let t = armed(&conn, "script:does-not-exist", Some(5), None);
        let transitions = pass(&conn, &r);
        assert!(transitions.is_empty(), "first failure of a cadenced tripwire is tolerated");
        let rt = crate::db::tripwires::for_task(&conn, &t.id).unwrap().unwrap();
        assert_eq!(rt.failures, 1);
        assert!(rt.last_note.as_deref().unwrap().contains("does-not-exist"));
        assert!(get(&conn, &t.id).unwrap().unwrap().tripwire_armed());
        // Backed off: not due again yet.
        assert!(claim_due(&conn, 10).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_script_that_outlives_the_timeout_is_killed_and_recorded_as_an_error() {
        let r = root("hang");
        script(&r, "sleeper", "sleep 30; exit 0");
        let started = Instant::now();
        let out = run_script(
            &r.join("tripwires/sleeper"),
            None,
            &r,
            &[],
            Duration::from_secs(1),
        );
        assert!(started.elapsed() < Duration::from_secs(10), "must not wait for the sleep");
        match out {
            Outcome::Error(e) => assert!(e.contains("timed out"), "{e}"),
            other => panic!("expected an error, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&r);
    }

    /// The pipe-drain lesson: a script that writes far more than the pipe
    /// buffer must neither hang the runner nor bloat the note.
    #[test]
    fn a_noisy_script_is_drained_and_its_output_truncated() {
        let r = root("noisy");
        script(&r, "noisy", "head -c 1048576 /dev/zero | tr '\\0' 'x'; echo; echo 'tail' >&2; exit 0");
        let started = Instant::now();
        let out = run_script(&r.join("tripwires/noisy"), None, &r, &[], Duration::from_secs(20));
        assert!(started.elapsed() < Duration::from_secs(15), "a full pipe must not stall the child");
        match out {
            Outcome::Tripped(note) => {
                assert!(note.contains("truncated"), "{}", &note[note.len().saturating_sub(80)..]);
                assert!(note.len() < CAPTURE_BYTES + 100, "note is bounded: {}", note.len());
            }
            other => panic!("expected tripped, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn the_argument_arrives_as_dollar_one_and_the_task_is_in_the_environment() {
        let conn = test_conn();
        let r = root("env");
        script(&r, "echo-arg", r#"echo "arg=[$1] ref=$CXTASKS_TASK_REF tw=$CXTASKS_TRIPWIRE title=$CXTASKS_TASK_TITLE"; exit 0"#);
        // A hostile-looking argument: quotes, a semicolon, a `$` — all of it
        // must land in $1 verbatim, because nothing here goes through a shell.
        let t = armed(&conn, "script:echo-arg it's; $HOME `x`", None, None);
        crate::db::tasks::update(&conn, &t.id, &TaskPatch { tripwire_check_requested: Some(true), ..Default::default() }).unwrap();
        pass(&conn, &r);
        let rt = crate::db::tripwires::for_task(&conn, &t.id).unwrap().unwrap();
        let note = rt.last_note.unwrap();
        assert!(note.contains("arg=[it's; $HOME `x`]"), "{note}");
        assert!(note.contains(&format!("ref=T{}", t.short_id.unwrap())), "{note}");
        assert!(note.contains("tw=1") && note.contains("title=gate"), "{note}");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_spec_this_runner_cannot_evaluate_is_recorded_rather_than_left_armed() {
        let conn = test_conn();
        let r = root("unsupported");
        let t = armed(&conn, "script:fine", None, None);
        // Simulate a row synced from a client that speaks a grammar this
        // build refuses: write the column directly.
        conn.execute(
            "UPDATE tasks SET tripwire = 'oracle:the moon is full' WHERE id = ?1",
            rusqlite::params![t.id],
        )
        .unwrap();
        crate::db::tripwires::request_check(&conn, &t.id).unwrap();
        let transitions = pass(&conn, &r);
        assert!(matches!(&transitions[..], [Transition::Failed { .. }]), "{transitions:?}");
        assert_eq!(get(&conn, &t.id).unwrap().unwrap().tripwire_state.as_deref(), Some("error"));
        let _ = std::fs::remove_dir_all(&r);
    }

    /// The script lane must not touch an agent spec — not claim it, not mark
    /// it — because "not mine" is not an outcome a tripwire has.
    #[test]
    fn the_script_lane_leaves_an_agent_tripwire_for_the_agent_lane() {
        let conn = test_conn();
        let r = root("agent-skip");
        let t = armed(&conn, "agent:the moon is full", Some(60), None);
        assert!(pass(&conn, &r).is_empty());
        let rt = crate::db::tripwires::for_task(&conn, &t.id).unwrap().unwrap();
        assert!(rt.lease_id.is_none(), "not claimed");
        assert!(rt.checked_at.is_none(), "not recorded");
        assert!(get(&conn, &t.id).unwrap().unwrap().tripwire_armed());
        // And it is exactly what the agent lane picks next.
        let next = next_due_agent(&conn).unwrap().unwrap();
        assert_eq!(next.0.id, t.id);
        assert_eq!(next.2, "the moon is full");
        let _ = std::fs::remove_dir_all(&r);
    }

    /// The sub-cap reservation: a check counts in full before it runs.
    #[test]
    fn admission_reserves_the_whole_ceiling_against_the_sub_cap() {
        assert_eq!(admit_check(0, 3_000_000, 5_000_000), CheckAdmission::Admitted);
        assert_eq!(admit_check(2_000_000, 3_000_000, 5_000_000), CheckAdmission::Admitted);
        assert!(matches!(
            admit_check(2_000_001, 3_000_000, 5_000_000),
            CheckAdmission::SubCapReached { .. }
        ));
        // Off means off; the daily cap is the gate's business.
        assert_eq!(admit_check(99_000_000, 3_000_000, 0), CheckAdmission::Admitted);
    }

    /// Bounds compose by `min_nonzero`: a task's "no ceiling" for its work is
    /// never a check's "no ceiling".
    #[test]
    fn check_limits_never_exceed_the_constants_even_when_the_task_is_uncapped() {
        let conn = test_conn();
        let mut t = armed(&conn, "agent:x", Some(60), None);
        t.bg_run_token_cap = Some(0);
        t.bg_timeout_minutes = Some(120);
        let settings = BudgetSettings { max_turns: 150, ..Default::default() };
        let l = check_limits(&t, &settings);
        assert_eq!(l.token_ceiling, CHECK_TOKEN_CEILING);
        assert_eq!(l.timeout, Duration::from_secs(CHECK_TIMEOUT_MINUTES as u64 * 60));
        assert_eq!(l.max_turns, CHECK_MAX_TURNS);
        // A tighter task bound still wins.
        t.bg_run_token_cap = Some(500_000);
        t.bg_timeout_minutes = Some(3);
        let l = check_limits(&t, &settings);
        assert_eq!(l.token_ceiling, 500_000);
        assert_eq!(l.timeout, Duration::from_secs(180));
        // A Codex task's check still runs on Claude, with Claude's default model.
        t.bg_executor = BackgroundExecutor::Codex;
        t.model = Some("gpt-5-codex".into());
        assert_eq!(check_model_for(&t, &settings).as_deref(), Some(settings.model.as_str()));
        t.bg_executor = BackgroundExecutor::Claude;
        t.model = Some("claude-haiku-4-5-20251001".into());
        assert_eq!(check_model_for(&t, &settings).as_deref(), Some("claude-haiku-4-5-20251001"));
    }

    /// The verdict mapping, including the two cases that must be ERRORS.
    #[test]
    fn a_run_outcome_becomes_a_tripwire_outcome_by_its_last_verdict_line() {
        let ok = |text: &str| RunOutcome {
            status: "succeeded".into(),
            result_text: Some(text.into()),
            ..Default::default()
        };
        assert_eq!(
            verdict_outcome(&ok("looked\nVERDICT: TRIPPED\nEVIDENCE: 51 users")),
            Outcome::Tripped("51 users".into())
        );
        assert_eq!(
            verdict_outcome(&ok("VERDICT: NOT_YET\nEVIDENCE: 4 signups, none linked")),
            Outcome::NotYet("4 signups, none linked".into())
        );
        assert!(matches!(verdict_outcome(&ok("VERDICT: UNSURE\nEVIDENCE: no changelog")), Outcome::Error(e) if e.contains("UNSURE") && e.contains("no changelog")));
        assert!(matches!(verdict_outcome(&ok("I have no idea.")), Outcome::Error(e) if e.contains("no VERDICT")));
        let stopped = RunOutcome {
            status: "budget_stopped".into(),
            stop_detail: Some("was stopped by its per-run token ceiling (3000001 of 3000000 tokens)".into()),
            result_text: Some("VERDICT: TRIPPED".into()),
            ..Default::default()
        };
        assert!(matches!(verdict_outcome(&stopped), Outcome::Error(e) if e.contains("budget_stopped") && e.contains("ceiling")), "a killed run's verdict line does not count");
    }

    /// Tick ordering, as a pure function of the gate: scripts run under every
    /// budget gate and under none of the switches.
    #[test]
    fn the_free_lane_runs_under_a_spent_budget_and_stops_with_the_switches() {
        for g in [
            Gate::TokenCapReached { today: 1, cap: 1 },
            Gate::DailyCapReached { today: 1, cap: 1 },
            Gate::AtConcurrencyLimit { running: 2, max: 2 },
            Gate::HostHeadroomReached { today: 1, ceiling: 1 },
        ] {
            let l = lanes(&g);
            assert!(l.scripts, "scripts must run under {g:?}");
            assert_eq!(l.slots, 0, "{g:?} starts no agent runs");
        }
        for g in [Gate::Paused, Gate::HostPaused, Gate::HostDisabled] {
            assert!(g.is_switched_off());
            let l = lanes(&g);
            assert!(!l.scripts, "nothing runs under {g:?}");
            assert_eq!(l.slots, 0);
        }
        let l = lanes(&Gate::Open { slots: 3 });
        assert!(l.scripts);
        assert_eq!(l.slots, 3);
    }
}
