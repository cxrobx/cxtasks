//! The SESSION lane: open the interactive sessions another device asked this
//! machine for (`crate::session::request`, "Open on Mac mini").
//!
//! It runs on every tick ABOVE the budget gate's early return (gotcha #42): an
//! interactive session is a person's own Claude, not the runner spending the
//! background budget, so a spent token cap or two busy slots must not stop
//! one. The three switches (`STOP_BACKGROUND`, the host paused, the host
//! disabled) DO stop it, by refusing each waiting request with the reason
//! rather than leaving it to lapse, so the phone hears why within a tick.
//! Expiry runs whatever the switches say: it opens nothing.
//!
//! `.claude/rules/execution.md` § Sessions requested from another device.

use std::path::{Path, PathBuf};

use crate::db;
use crate::db::tasks::Task;
use crate::error::AppError;
use crate::routing::HostCapability;
use crate::runner::Gate;

/// Why a switched-off host refuses a waiting request, or `None` when the switch
/// is on. The budget gates are not reasons (see the module note).
pub fn refusal_for(gate: &Gate, label: &str) -> Option<String> {
    match gate {
        Gate::Paused => Some(format!(
            "Background runs are paused on {label} (STOP_BACKGROUND), so it didn't open this \
             session."
        )),
        Gate::HostPaused => Some(format!(
            "{label} is paused in CXTasks, so it didn't open this session."
        )),
        Gate::HostDisabled => Some(format!(
            "{label} is switched off in CXTasks, or its Claude Code CLI isn't ready, so it \
             didn't open this session."
        )),
        _ => None,
    }
}

/// Where this host notes each request it claimed. Host-local, never synced.
pub fn claims_dir() -> PathBuf {
    crate::paths::app_data_dir().join("session-claims")
}

/// How long a claim note is kept. Far past any request's life, so a reverted
/// row can always be recognised, and short enough that the folder stays small.
const CLAIM_NOTE_DAYS: u64 = 3;

const FAILED_PREFIX: &str = "failed\n";

/// One pass: expire, then open (or refuse) every fresh request for `cap`.
///
/// `open` is the launch itself, injected so tests never touch Herdr. Returns
/// how many sessions opened.
///
/// **A request is opened at most once on this machine, whatever a pull does.**
/// The claim is one conditional UPDATE (`db::runs::claim_session`), which stops
/// two ticks here and any other machine. What it can't stop is a pull landing
/// between this host's sweep and its own pull and putting the hub's older
/// `requested` row back (invariant 32's open window): the next tick would see a
/// fresh request and open a second session. So every claim leaves a note in
/// `claims`, written before the launch, and a row that comes back to
/// `requested` with a note gets its recorded outcome re-asserted instead.
pub fn session_pass<F>(
    conn: &rusqlite::Connection,
    cap: &HostCapability,
    refusal: Option<&str>,
    claims: &Path,
    mut open: F,
) -> Result<usize, AppError>
where
    F: FnMut(&Task, crate::session::SessionTarget) -> Result<crate::session::Launched, String>,
{
    db::runs::expire_session_requests(conn, &cap.id)?;
    let pending = db::runs::pending_session_requests(conn, &cap.id)?;
    let mut opened = 0;
    for run in pending {
        let note = claims.join(crate::dispatch::sanitize_id(&run.id));
        if let Ok(recorded) = std::fs::read_to_string(&note) {
            let failure = recorded.strip_prefix(FAILED_PREFIX);
            if db::runs::reassert_session(conn, &run.id, failure)? {
                log::info!("session: re-asserted run {} after a pull reverted it", run.id);
            }
            continue;
        }
        if let Some(reason) = refusal {
            db::runs::refuse_session(conn, &run.id, &cap.id, reason)?;
            continue;
        }
        if !cap.is_local() {
            db::runs::refuse_session(
                conn,
                &run.id,
                &cap.id,
                &format!("{} has no checkouts of its own, so nothing was opened.", cap.label),
            )?;
            continue;
        }
        let task = match db::tasks::get(conn, &run.task_id)? {
            Some(t)
                if t.deleted_at.is_none()
                    && t.archived_at.is_none()
                    && !db::tasks::CLOSED_STATUSES.contains(&t.status.as_str()) =>
            {
                t
            }
            _ => {
                db::runs::refuse_session(
                    conn,
                    &run.id,
                    &cap.id,
                    &format!(
                        "The task was closed before {} opened it, so nothing was opened.",
                        cap.label
                    ),
                )?;
                continue;
            }
        };
        if !db::runs::claim_session(conn, &run.id, &cap.id)? {
            continue;
        }
        if let Err(e) = write_note(claims, &note, "dispatched") {
            // Costs only the protection against a reverted row; the request
            // itself is still claimed exactly once.
            log::warn!("session: could not note the claim of run {}: {e}", run.id);
        }
        let target = crate::session::SessionTarget::of_run(run.session_id.as_deref());
        match open(&task, target) {
            Ok(launched) => {
                db::runs::record_session_launch(
                    conn,
                    &run.id,
                    Ok(&launched.scratch.to_string_lossy()),
                )?;
                if let Some(session_ref) = &launched.session_ref {
                    db::runs::record_session_ref(conn, &run.id, session_ref)?;
                }
                // As the app's dispatch does: opening a session starts the work.
                db::tasks::update(
                    conn,
                    &task.id,
                    &db::tasks::TaskPatch {
                        status: Some("in_progress".into()),
                        ..Default::default()
                    },
                )?;
                log::info!(
                    "session: opened task {} in {} (run {})",
                    task.id,
                    target.label(),
                    run.id
                );
                opened += 1;
            }
            Err(reason) => {
                log::warn!("session: could not open task {}: {reason}", task.id);
                db::runs::record_session_launch(conn, &run.id, Err(&reason))?;
                let _ = write_note(claims, &note, &format!("{FAILED_PREFIX}{reason}"));
            }
        }
    }
    prune_notes(claims);
    Ok(opened)
}

fn write_note(dir: &Path, note: &Path, body: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(note, body)
}

fn prune_notes(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let limit = std::time::Duration::from_secs(CLAIM_NOTE_DAYS * 24 * 3600);
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > limit);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::runs::{self, STATUS_REQUESTED};
    use crate::db::tasks::{create, NewTask};
    use rusqlite::params;
    use std::cell::RefCell;

    struct Fixture {
        conn: rusqlite::Connection,
        claims: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // The directory this fixture made, never its parent (gotcha #39).
            let _ = std::fs::remove_dir_all(&self.claims);
        }
    }

    fn fixture() -> Fixture {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        db::schema::initialize(&conn).unwrap();
        for (id, label, default) in [("mini", "Mac mini", true), ("laptop", "MacBook", false)] {
            let mut cap = HostCapability::local_default(id.into(), label.into());
            cap.takes_unassigned = default;
            db::hosts::register_self(&conn, &cap).unwrap();
        }
        db::hosts::set_default(&conn, "mini").unwrap();
        let claims = std::env::temp_dir().join(format!("cxtasks-claims-{}", uuid::Uuid::new_v4()));
        Fixture { conn, claims }
    }

    fn host(f: &Fixture, id: &str) -> HostCapability {
        db::hosts::get(&f.conn, id).unwrap().unwrap()
    }

    fn requested(f: &Fixture, host: &str) -> (String, String) {
        let task = create(&f.conn, &NewTask { title: "Fix the login bug".into(), ..Default::default() })
            .unwrap();
        let run = runs::request_session(&f.conn, &task.id, host, task.bg_intent, None).unwrap();
        (task.id, run.id)
    }

    fn status(f: &Fixture, run: &str) -> String {
        runs::get(&f.conn, run).unwrap().unwrap().status
    }

    fn age(f: &Fixture, run: &str, minutes: i64) {
        f.conn
            .execute(
                "UPDATE task_runs SET started_at = datetime('now', ?2) WHERE id = ?1",
                params![run, format!("-{minutes} minutes")],
            )
            .unwrap();
    }

    /// The existing tests' launches are Herdr launches that return a scratch
    /// folder; this adapts them to the lane's target-aware signature.
    fn herdr<F: Fn(&Task) -> Result<PathBuf, String>>(
        open: F,
    ) -> impl FnMut(&Task, crate::session::SessionTarget) -> Result<crate::session::Launched, String> {
        move |t, target| {
            assert_eq!(target, crate::session::SessionTarget::Herdr, "these requests asked for Herdr");
            open(t).map(|scratch| crate::session::Launched { scratch, session_ref: None })
        }
    }

    #[test]
    fn a_t3_request_opens_a_t3_thread_and_records_which_one() {
        let f = fixture();
        let task = db::tasks::create(&f.conn, &db::tasks::NewTask {
            title: "t3 one".into(),
            ..Default::default()
        })
        .unwrap();
        let run = runs::request_session(
            &f.conn, &task.id, "mini", task.bg_intent, Some(runs::SESSION_TARGET_T3),
        )
        .unwrap();
        let open = |_: &Task, target: crate::session::SessionTarget| {
            assert_eq!(target, crate::session::SessionTarget::T3);
            Ok(crate::session::Launched {
                scratch: PathBuf::from("/scratch/t3"),
                session_ref: Some("t3:thread-123".into()),
            })
        };
        assert_eq!(session_pass(&f.conn, &host(&f, "mini"), None, &f.claims, open).unwrap(), 1);
        let row = runs::get(&f.conn, &run.id).unwrap().unwrap();
        assert_eq!(row.status, runs::FOREGROUND_STATUS);
        assert_eq!(row.session_id.as_deref(), Some("t3:thread-123"));
        assert_eq!(row.scratch_dir.as_deref(), Some("/scratch/t3"));
    }

    fn opens_ok(_: &Task) -> Result<PathBuf, String> {
        Ok(PathBuf::from("/scratch/t"))
    }

    #[test]
    fn the_target_opens_it_once_and_starts_the_task() {
        let f = fixture();
        let (task, run) = requested(&f, "mini");
        let calls = RefCell::new(0);
        let open = |_: &Task| {
            *calls.borrow_mut() += 1;
            Ok(PathBuf::from("/scratch/t"))
        };
        assert_eq!(session_pass(&f.conn, &host(&f, "mini"), None, &f.claims, herdr(open)).unwrap(), 1);
        let row = runs::get(&f.conn, &run).unwrap().unwrap();
        assert_eq!(row.status, runs::FOREGROUND_STATUS);
        assert!(row.finished_at.is_some(), "terminal, like the app's own dispatch");
        assert_eq!(row.scratch_dir.as_deref(), Some("/scratch/t"));
        assert_eq!(db::tasks::get(&f.conn, &task).unwrap().unwrap().status, "in_progress");

        // A second tick, and a second process, find nothing to open.
        let open_again = |_: &Task| -> Result<PathBuf, String> {
            *calls.borrow_mut() += 1;
            Ok(PathBuf::from("/x"))
        };
        assert_eq!(session_pass(&f.conn, &host(&f, "mini"), None, &f.claims, herdr(open_again)).unwrap(), 0);
        assert_eq!(*calls.borrow(), 1);
    }

    #[test]
    fn the_claim_is_exclusive() {
        let f = fixture();
        let (_, run) = requested(&f, "mini");
        assert!(runs::claim_session(&f.conn, &run, "mini").unwrap());
        assert!(!runs::claim_session(&f.conn, &run, "mini").unwrap(), "a second tick loses");
        let (_, other) = requested(&f, "mini");
        assert!(!runs::claim_session(&f.conn, &other, "laptop").unwrap(), "another machine never wins");
        assert_eq!(status(&f, &other), STATUS_REQUESTED);
    }

    #[test]
    fn another_machines_request_is_left_alone() {
        let f = fixture();
        let (_, run) = requested(&f, "mini");
        let open = |_: &Task| -> Result<PathBuf, String> { panic!("the laptop must not open the mini's session") };
        assert_eq!(session_pass(&f.conn, &host(&f, "laptop"), None, &f.claims, herdr(open)).unwrap(), 0);
        assert_eq!(status(&f, &run), STATUS_REQUESTED);
    }

    #[test]
    fn a_failed_launch_records_the_helpers_words_and_is_never_retried() {
        let f = fixture();
        let (task, run) = requested(&f, "mini");
        let calls = RefCell::new(0);
        let open = |_: &Task| -> Result<PathBuf, String> {
            *calls.borrow_mut() += 1;
            Err("Could not open Claude in Herdr: Herdr isn't running on the Herdr host.".into())
        };
        session_pass(&f.conn, &host(&f, "mini"), None, &f.claims, herdr(open)).unwrap();
        let row = runs::get(&f.conn, &run).unwrap().unwrap();
        assert_eq!(row.status, "failed");
        assert!(row.result_text.unwrap().contains("Herdr isn't running"));
        assert_eq!(db::tasks::get(&f.conn, &task).unwrap().unwrap().status, "inbox", "work didn't start");

        let open = |_: &Task| -> Result<PathBuf, String> {
            *calls.borrow_mut() += 1;
            Ok(PathBuf::from("/x"))
        };
        session_pass(&f.conn, &host(&f, "mini"), None, &f.claims, herdr(open)).unwrap();
        assert_eq!(*calls.borrow(), 1);
        assert_eq!(status(&f, &run), "failed");
    }

    #[test]
    fn a_stale_request_is_never_opened_and_says_which_machine() {
        let f = fixture();
        let (_, run) = requested(&f, "mini");
        age(&f, &run, runs::SESSION_REQUEST_MINUTES + 1);
        let open = |_: &Task| -> Result<PathBuf, String> { panic!("a stale tap must not open") };
        assert_eq!(session_pass(&f.conn, &host(&f, "mini"), None, &f.claims, herdr(open)).unwrap(), 0);
        let row = runs::get(&f.conn, &run).unwrap().unwrap();
        assert_eq!(row.status, "failed");
        let text = row.result_text.unwrap();
        assert!(text.starts_with("Mac mini didn't pick this up within 15 minutes"), "{text}");
        assert!(text.contains("Is its runner running?"));
    }

    #[test]
    fn another_machine_expires_a_request_only_after_the_longer_bound() {
        let f = fixture();
        let (_, run) = requested(&f, "mini");
        age(&f, &run, runs::SESSION_REQUEST_MINUTES + 1);
        assert_eq!(runs::expire_session_requests(&f.conn, "laptop").unwrap(), 0);
        assert_eq!(status(&f, &run), STATUS_REQUESTED);
        age(&f, &run, runs::SESSION_REQUEST_FOREIGN_MINUTES + 1);
        assert_eq!(runs::expire_session_requests(&f.conn, "laptop").unwrap(), 1);
        assert_eq!(status(&f, &run), "failed");
    }

    #[test]
    fn a_switched_off_host_refuses_at_once_with_the_reason() {
        let f = fixture();
        let (_, run) = requested(&f, "mini");
        let reason = refusal_for(&Gate::Paused, "Mac mini").unwrap();
        let open = |_: &Task| -> Result<PathBuf, String> { panic!("paused means paused") };
        session_pass(&f.conn, &host(&f, "mini"), Some(&reason), &f.claims, herdr(open)).unwrap();
        let row = runs::get(&f.conn, &run).unwrap().unwrap();
        assert_eq!(row.status, "failed");
        assert!(row.result_text.unwrap().contains("paused on Mac mini"));
        assert!(refusal_for(&Gate::TokenCapReached { today: 9, cap: 1 }, "x").is_none(),
            "a spent budget is no reason to refuse a person's own session");
        assert!(refusal_for(&Gate::AtConcurrencyLimit { running: 2, max: 2 }, "x").is_none());
    }

    #[test]
    fn a_task_closed_in_the_meantime_is_refused_not_opened() {
        let f = fixture();
        let (task, run) = requested(&f, "mini");
        crate::recurrence::complete_with_recurrence(&f.conn, &task, None).unwrap();
        let open = |_: &Task| -> Result<PathBuf, String> { panic!("closed work must not open") };
        session_pass(&f.conn, &host(&f, "mini"), None, &f.claims, herdr(open)).unwrap();
        assert_eq!(status(&f, &run), "failed");
    }

    /// The pull that lands between this host's sweep and its pull puts the
    /// hub's older `requested` back. The note turns that into a re-assert, not
    /// a second session, and the re-asserted row carries a newer stamp so it
    /// travels on the next push.
    #[test]
    fn a_row_a_pull_reverted_is_reasserted_not_opened_twice() {
        let f = fixture();
        let (_, run) = requested(&f, "mini");
        session_pass(&f.conn, &host(&f, "mini"), None, &f.claims, herdr(opens_ok)).unwrap();

        // The hub's copy arrives: back to requested, stamps as they were.
        f.conn
            .execute(
                "UPDATE task_runs SET status = 'requested', finished_at = NULL,
                        started_at = datetime('now', '-1 minutes') WHERE id = ?1",
                params![run],
            )
            .unwrap();
        let open = |_: &Task| -> Result<PathBuf, String> { panic!("opened a second session") };
        session_pass(&f.conn, &host(&f, "mini"), None, &f.claims, herdr(open)).unwrap();
        let row = runs::get(&f.conn, &run).unwrap().unwrap();
        assert_eq!(row.status, runs::FOREGROUND_STATUS);
        assert!(row.finished_at.is_some());
    }

    /// The new status must not leak into anything that reads `running`: the
    /// sweep, the concurrency count, the foreground heal.
    #[test]
    fn a_request_is_invisible_to_the_background_bookkeeping() {
        let f = fixture();
        let (_, run) = requested(&f, "mini");
        age(&f, &run, 600);
        assert_eq!(runs::sweep_abandoned(&f.conn).unwrap(), 0);
        assert_eq!(runs::heal_foreground_runs(&f.conn).unwrap(), 0);
        assert_eq!(runs::count_running_background(&f.conn).unwrap(), 0);
        assert_eq!(runs::count_running_background_all_hosts(&f.conn).unwrap(), 0);
        assert_eq!(status(&f, &run), STATUS_REQUESTED);
    }
}
