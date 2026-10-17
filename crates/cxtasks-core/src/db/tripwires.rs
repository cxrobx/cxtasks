//! The tripwire ATTEMPT — leased, recorded, and never written onto `tasks`.
//!
//! `tasks` carries the five stable columns (spec, cadence, policy, state,
//! tripped-at). Everything that changes while a check is merely being *tried*
//! — the pending request, the lease, the attempt counters, the last note —
//! lives here on a 1:1 row, `tripwire_runtime`. That split is invariant 26's
//! load-bearing half: sync applies whole-row post-images (`sync/apply.rs`), so
//! a "not yet, checked at 14:05" written onto `tasks` would rewrite the task
//! row the phone is editing at that moment. This table's rows clobber nothing
//! but themselves.
//!
//! # One state machine, both kinds
//!
//! ```text
//! list_due  → the task is LIVE, open (or a MONITOR, open or not), armed,
//!             past its date; the lease is
//!             absent or EXPIRED; and it is requested or its (backed-off)
//!             interval has elapsed
//! claim     → one CAS UPDATE: takes the lease iff nobody holds a live one
//! (run the check, outside any transaction)
//! record    → BEGIN IMMEDIATE; re-read; CAS on the lease id; write the
//!             outcome; move `tasks.tripwire_state` if it is a transition;
//!             comment; auto-complete; COMMIT. Notify AFTER commit, from the
//!             caller.
//! ```
//!
//! The lease expiry is the ONE reclaim rule. A runner that crashes after the
//! claim leaves nothing recorded and the request untouched; the lease expires
//! and the next tick re-runs it. Two runner processes cannot both run one
//! attempt, because the CAS admits one. A late result from a reclaimed attempt
//! is dropped by `record`'s own CAS.
//!
//! # Reset, never deleted
//!
//! Clearing or re-arming a tripwire RESETS this row. It is removed only by the
//! cascade when its task is destroyed, which is what keeps every verb that
//! touches a tripwire out of `api::DELETES_ROWS` — a deletion travels only as a
//! verb, and there is none to record.
//!
//! # Monitors
//!
//! `tripwire_on_trip = 'monitor'` is a standing check, not a gate, and the one
//! place status stops mattering. `list_due` and `record` accept a done monitor
//! (only archive, trash or removing the spec stops the check); a trip or a
//! surfacing error on a done monitor REOPENS it to `todo`; completing it
//! re-arms it (`db::tasks::update`); and [`rearm_handled_monitors`] repairs a
//! done-and-tripped monitor that arrived without passing through that update,
//! by sync from an older build. Nothing has to REFUSE `done` — the runner is
//! the one execution site, so that is where the rule lives.
//!
//! Known limit, documented rather than solved: whole-row merge on this small
//! row can lose a phone's request that races a Mac attempt (re-issuable) or
//! re-check one early. If that ever bites, requests become an append-only
//! `tripwire_requests` table.

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::tasks::{self, Task, CLOSED_STATUSES, DEFER_PASSED, LIVE};
use crate::error::AppError;
use crate::tripwire::{self, OnTrip, Outcome, OutcomeKind};

/// One row of `tripwire_runtime`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Runtime {
    pub task_id: String,
    /// "Check now" — retained until the attempt that served it records.
    pub requested_at: Option<String>,
    pub lease_id: Option<String>,
    pub lease_host: Option<String>,
    /// `tasks.tripwire` as it read at claim time.
    pub lease_spec: Option<String>,
    /// The `requested_at` this attempt is serving, if any.
    pub lease_request_at: Option<String>,
    pub lease_expires_at: Option<String>,
    /// The last TERMINAL attempt.
    pub checked_at: Option<String>,
    /// `tripped` | `not_yet` | `error`.
    pub last_outcome: Option<String>,
    pub last_note: Option<String>,
    /// Consecutive errors. Any success resets it.
    pub failures: i64,
    pub updated_at: String,
}

const COLUMNS: &str = "task_id, requested_at, lease_id, lease_host, lease_spec, lease_request_at, \
     lease_expires_at, checked_at, last_outcome, last_note, failures, updated_at";

fn row_to_runtime(r: &rusqlite::Row) -> rusqlite::Result<Runtime> {
    Ok(Runtime {
        task_id: r.get(0)?,
        requested_at: r.get(1)?,
        lease_id: r.get(2)?,
        lease_host: r.get(3)?,
        lease_spec: r.get(4)?,
        lease_request_at: r.get(5)?,
        lease_expires_at: r.get(6)?,
        checked_at: r.get(7)?,
        last_outcome: r.get(8)?,
        last_note: r.get(9)?,
        failures: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

/// Make the row exist. Idempotent; nothing on an existing row is touched.
pub fn ensure_runtime(conn: &Connection, task_id: &str) -> Result<(), AppError> {
    conn.execute(
        "INSERT OR IGNORE INTO tripwire_runtime (task_id) VALUES (?1)",
        params![task_id],
    )?;
    Ok(())
}

/// Back to "never checked": no request, no lease, no history. What a new or
/// changed spec, a clear, and a re-arm all do.
pub fn reset_runtime(conn: &Connection, task_id: &str) -> Result<(), AppError> {
    ensure_runtime(conn, task_id)?;
    conn.execute(
        "UPDATE tripwire_runtime
            SET requested_at = NULL, lease_id = NULL, lease_host = NULL, lease_spec = NULL,
                lease_request_at = NULL, lease_expires_at = NULL, checked_at = NULL,
                last_outcome = NULL, last_note = NULL, failures = 0,
                updated_at = datetime('now')
          WHERE task_id = ?1",
        params![task_id],
    )?;
    Ok(())
}

/// "Check now". Millisecond stamp so two requests inside one second are still
/// two requests — `record` compares it byte-wise against the one it served.
pub fn request_check(conn: &Connection, task_id: &str) -> Result<(), AppError> {
    ensure_runtime(conn, task_id)?;
    conn.execute(
        "UPDATE tripwire_runtime
            SET requested_at = strftime('%Y-%m-%d %H:%M:%f', 'now'),
                updated_at = datetime('now')
          WHERE task_id = ?1",
        params![task_id],
    )?;
    Ok(())
}

pub fn for_task(conn: &Connection, task_id: &str) -> Result<Option<Runtime>, AppError> {
    let sql = format!("SELECT {COLUMNS} FROM tripwire_runtime WHERE task_id = ?1");
    Ok(conn
        .query_row(&sql, params![task_id], row_to_runtime)
        .optional()?)
}

/// Every live task's runtime row, keyed by task id — what the board renders.
/// One query for the whole list, the `tagsByTask` shape.
pub fn runtime_map(conn: &Connection) -> Result<HashMap<String, Runtime>, AppError> {
    let cols = COLUMNS
        .split(", ")
        .map(|c| format!("r.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {cols} FROM tripwire_runtime r
           JOIN tasks t ON t.id = r.task_id
          WHERE t.tripwire IS NOT NULL AND t.{LIVE}",
        LIVE = LIVE.replace(" AND ", " AND t.")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], row_to_runtime)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().map(|r| (r.task_id.clone(), r)).collect())
}

/// "Still being checked" as far as STATUS goes: open, or a monitor, which the
/// runner keeps checking while it is done.
const OPEN_OR_MONITOR: &str = "tripwire_on_trip = 'monitor'";

/// The lease predicate: nobody holds one, or the one held has expired.
const LEASE_FREE: &str = "(r.lease_id IS NULL OR datetime(r.lease_expires_at) < datetime('now'))";

/// Armed tripwires whose check is due now, oldest-requested first.
///
/// SQL decides everything about the TASK — live, open, armed, past its date —
/// and the lease; the cadence arithmetic is [`tripwire::is_due`], applied in
/// Rust so the backoff table is tested as a table. A task with a spec but no
/// runtime row (hand-written, or a client that knew the column and not the
/// rule) gets one made here, because `claim` needs a row to CAS against.
pub fn list_due(conn: &Connection, limit: usize) -> Result<Vec<(Task, Runtime)>, AppError> {
    let closed = tasks::closed_list();
    let sql = format!(
        "SELECT t.id, r.task_id IS NOT NULL
           FROM tasks t
           LEFT JOIN tripwire_runtime r ON r.task_id = t.id
          WHERE {LIVE}
            AND (t.status NOT IN ({closed}) OR t.{OPEN_OR_MONITOR})
            AND t.tripwire IS NOT NULL
            AND t.tripwire_state = 'armed'
            AND {DEFER_PASSED}
            AND (r.task_id IS NULL OR {LEASE_FREE})
          ORDER BY (r.requested_at IS NULL) ASC, COALESCE(r.checked_at, '') ASC, t.created_at ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let candidates: Vec<(String, bool)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;

    let now = chrono::Utc::now();
    let mut out = Vec::new();
    for (id, has_runtime) in candidates {
        if !has_runtime {
            ensure_runtime(conn, &id)?;
        }
        let Some(task) = tasks::get(conn, &id)? else { continue };
        let Some(rt) = for_task(conn, &id)? else { continue };
        let checked = rt.checked_at.as_deref().and_then(tripwire::parse_stamp);
        if tripwire::is_due(
            task.tripwire_every_minutes,
            rt.failures,
            checked,
            rt.requested_at.is_some(),
            now,
        ) {
            out.push((task, rt));
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(out)
}

/// Take the lease. One UPDATE, one CAS: the row must have no live lease.
/// `true` = ours. `spec` is the task's spec as read now, so `record` can tell
/// whether the user changed the condition while the attempt ran.
pub fn claim(
    conn: &Connection,
    task_id: &str,
    lease_id: &str,
    host: &str,
    spec: &str,
    expires_minutes: i64,
) -> Result<bool, AppError> {
    let n = conn.execute(
        "UPDATE tripwire_runtime
            SET lease_id = ?2, lease_host = ?3, lease_spec = ?4,
                lease_request_at = requested_at,
                lease_expires_at = datetime('now', '+' || ?5 || ' minutes'),
                updated_at = datetime('now')
          WHERE task_id = ?1
            AND (lease_id IS NULL OR datetime(lease_expires_at) < datetime('now'))",
        params![task_id, lease_id, host, spec, expires_minutes],
    )?;
    Ok(n == 1)
}

/// What `record` did, and therefore what the caller should announce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The attempt no longer owned its lease (reclaimed, or the task changed
    /// under it). Nothing was written except releasing the lease.
    pub discarded: bool,
    /// A state change on `tasks` happened — the thing worth a banner.
    pub transition: Option<Transition>,
    /// Consecutive failures after this attempt.
    pub failures: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    Tripped {
        title: String,
        note: String,
        /// `tripwire_on_trip = 'complete'` fired and the task is done.
        completed: bool,
    },
    Failed {
        title: String,
        note: String,
        failures: i64,
    },
}

// Test seam: fail inside the transaction after the state has been written,
// to prove nothing half-applies. Thread-local, because the test harness runs
// tests in parallel on their own threads and a process-wide flag would fire
// inside whichever test happened to be mid-`record`.
#[cfg(test)]
thread_local! {
    pub(crate) static FAIL_AFTER_TRANSITION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Record an attempt's outcome — the ONE transaction.
///
/// 1. Re-read: the lease must still be ours, and the task must still be live,
///    open, armed and carrying the spec we claimed. Anything else is DISCARD —
///    release the lease, write nothing else. The user finished or changed the
///    task while the check ran, and their change wins.
/// 2. CAS-update the runtime row: clear the lease, stamp `checked_at`, record
///    the outcome, bump or reset `failures`, and clear `requested_at` only if
///    it is the request this attempt served.
/// 3. On a trip: `tasks.tripwire_state = 'tripped'`, a comment with the
///    evidence, and — when the policy says so — `complete_with_recurrence`,
///    which opens no transaction of its own and so composes here. On the
///    error that crosses the threshold: `tripwire_state = 'error'` and a
///    comment. A not-yet touches `tasks` not at all.
///
/// Notification is the CALLER's job, after this returns — a banner posted
/// inside a transaction that then rolls back announces something that did not
/// happen.
pub fn record(
    conn: &Connection,
    task_id: &str,
    lease_id: &str,
    outcome: &Outcome,
) -> Result<Recorded, AppError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<Recorded, AppError> {
        let discarded = |conn: &Connection| -> Result<Recorded, AppError> {
            conn.execute(
                "UPDATE tripwire_runtime
                    SET lease_id = NULL, lease_host = NULL, lease_spec = NULL,
                        lease_request_at = NULL, lease_expires_at = NULL,
                        updated_at = datetime('now')
                  WHERE task_id = ?1 AND lease_id = ?2",
                params![task_id, lease_id],
            )?;
            Ok(Recorded { discarded: true, transition: None, failures: 0 })
        };

        let Some(rt) = for_task(conn, task_id)?.filter(|r| r.lease_id.as_deref() == Some(lease_id))
        else {
            // Reclaimed by a later attempt, or reset by the user. Our result is
            // about a check nobody is waiting for any more.
            return Ok(Recorded { discarded: true, transition: None, failures: 0 });
        };
        let Some(task) = tasks::get(conn, task_id)? else {
            return discarded(conn);
        };
        let closed = CLOSED_STATUSES.contains(&task.status.as_str());
        let monitor = task.is_monitor();
        let still_ours = task.deleted_at.is_none()
            && task.archived_at.is_none()
            && (!closed || monitor)
            && task.tripwire_armed()
            && task.tripwire.as_deref() == rt.lease_spec.as_deref();
        if !still_ours {
            return discarded(conn);
        }

        let note = tripwire::bound_note(outcome.note());
        let kind = outcome.kind();
        let n = conn.execute(
            "UPDATE tripwire_runtime
                SET lease_id = NULL, lease_host = NULL, lease_spec = NULL,
                    lease_request_at = NULL, lease_expires_at = NULL,
                    checked_at = datetime('now'),
                    last_outcome = ?3,
                    last_note = ?4,
                    failures = CASE WHEN ?3 = 'error' THEN failures + 1 ELSE 0 END,
                    requested_at = CASE WHEN requested_at IS lease_request_at THEN NULL
                                        ELSE requested_at END,
                    updated_at = datetime('now')
              WHERE task_id = ?1 AND lease_id = ?2",
            params![task_id, lease_id, kind.as_str(), note],
        )?;
        if n == 0 {
            return Ok(Recorded { discarded: true, transition: None, failures: 0 });
        }
        let failures: i64 = conn.query_row(
            "SELECT failures FROM tripwire_runtime WHERE task_id = ?1",
            params![task_id],
            |r| r.get(0),
        )?;

        let spec = task.tripwire.clone().unwrap_or_default();
        let transition = match kind {
            OutcomeKind::Tripped => {
                conn.execute(
                    "UPDATE tasks
                        SET tripwire_state = 'tripped', tripwire_tripped_at = datetime('now'),
                            updated_at = datetime('now')
                      WHERE id = ?1",
                    params![task_id],
                )?;
                let evidence = if note.is_empty() { "(no output)" } else { note.as_str() };
                crate::db::comments::add(
                    conn,
                    task_id,
                    crate::db::comments::AUTHOR_TRIPWIRE,
                    &format!("Tripwire tripped · {spec}\n\n{evidence}"),
                )?;
                let completed = OnTrip::from_db(task.tripwire_on_trip.as_deref()) == OnTrip::Complete;
                if completed {
                    // No `result`: the evidence is already in the log under
                    // its own author, and a "foreground session" handoff row
                    // would misdescribe what happened.
                    crate::recurrence::complete_with_recurrence(conn, task_id, None)?;
                }
                if monitor && closed {
                    reopen(conn, task_id)?;
                }
                Some(Transition::Tripped { title: task.title.clone(), note: note.clone(), completed })
            }
            OutcomeKind::NotYet => None,
            OutcomeKind::Error => {
                if failures >= tripwire::failure_threshold(task.tripwire_every_minutes) {
                    conn.execute(
                        "UPDATE tasks
                            SET tripwire_state = 'error', updated_at = datetime('now')
                          WHERE id = ?1",
                        params![task_id],
                    )?;
                    // "Back on the board" has to be true for a done monitor too.
                    if monitor && closed {
                        reopen(conn, task_id)?;
                    }
                    let times = if failures == 1 { String::new() } else { format!(" ({failures}× in a row)") };
                    crate::db::comments::add(
                        conn,
                        task_id,
                        crate::db::comments::AUTHOR_TRIPWIRE,
                        &format!(
                            "Tripwire check failed{times} · {spec}\n\n{}\n\nThe task is back on the \
                             board so this is seen. Fix the check and re-arm.",
                            if note.is_empty() { "(no detail)" } else { note.as_str() }
                        ),
                    )?;
                    Some(Transition::Failed { title: task.title.clone(), note: note.clone(), failures })
                } else {
                    None
                }
            }
        };

        #[cfg(test)]
        if FAIL_AFTER_TRANSITION.with(|f| f.get()) {
            return Err(AppError::General("injected failure after the transition".into()));
        }

        Ok(Recorded { discarded: false, transition, failures })
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

/// A done monitor that tripped goes back to `todo` — the ordinary status
/// write, so `completed_at` clears in the one place the lifecycle is kept.
fn reopen(conn: &Connection, task_id: &str) -> Result<(), AppError> {
    tasks::update(
        conn,
        task_id,
        &tasks::TaskPatch { status: Some("todo".into()), ..Default::default() },
    )?;
    Ok(())
}

/// Re-arm every monitor that is done but still `tripped` / `error` — the
/// backstop for a completion that arrived without passing through
/// `db::tasks::update`'s monitor rule: a post-image synced from an older
/// build, the iPhone talking to an older server, a hand edit. Through the
/// same patch the Re-arm button sends, so the runtime row resets identically.
/// Run by the runner at the top of every tick; returns the ids it re-armed.
pub fn rearm_handled_monitors(conn: &Connection) -> Result<Vec<String>, AppError> {
    let closed = tasks::closed_list();
    let sql = format!(
        "SELECT id FROM tasks
          WHERE {LIVE}
            AND tripwire IS NOT NULL
            AND {OPEN_OR_MONITOR}
            AND status IN ({closed})
            AND COALESCE(tripwire_state, 'armed') <> 'armed'"
    );
    let ids: Vec<String> = conn
        .prepare(&sql)?
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for id in &ids {
        tasks::update(
            conn,
            id,
            &tasks::TaskPatch { tripwire_state: Some("armed".into()), ..Default::default() },
        )?;
    }
    Ok(ids)
}

/// What `--status` prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub armed: i64,
    pub due_now: i64,
    pub leased: i64,
}

pub fn counts(conn: &Connection) -> Result<Counts, AppError> {
    let armed: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM tasks
              WHERE {LIVE} AND tripwire IS NOT NULL AND tripwire_state = 'armed'"
        ),
        [],
        |r| r.get(0),
    )?;
    let leased: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tripwire_runtime r
          WHERE r.lease_id IS NOT NULL AND datetime(r.lease_expires_at) >= datetime('now')",
        [],
        |r| r.get(0),
    )?;
    let due_now = list_due(conn, usize::MAX)?.len() as i64;
    Ok(Counts { armed, due_now, leased })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{create, update, NewTask, TaskPatch};

    fn armed(conn: &Connection, every: Option<i64>) -> Task {
        create(
            conn,
            &NewTask {
                title: "wait for it".into(),
                tripwire: Some("script:always-true".into()),
                tripwire_every_minutes: every,
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn claimed(conn: &Connection, t: &Task, lease: &str) -> bool {
        claim(conn, &t.id, lease, "mac", t.tripwire.as_deref().unwrap(), 5).unwrap()
    }

    // ── claim ──────────────────────────────────────────────────────────────

    /// Two claimers, one lease. The second runner process — a hand-run
    /// `--once` beside the LaunchAgent — must find the row taken.
    #[test]
    fn a_live_lease_admits_exactly_one_claimer() {
        let conn = test_conn();
        let t = armed(&conn, Some(5));
        assert!(claimed(&conn, &t, "lease-a"));
        assert!(!claimed(&conn, &t, "lease-b"), "a live lease must refuse a second claimer");
        let rt = for_task(&conn, &t.id).unwrap().unwrap();
        assert_eq!(rt.lease_id.as_deref(), Some("lease-a"));
        assert_eq!(rt.lease_host.as_deref(), Some("mac"));
        assert_eq!(rt.lease_spec.as_deref(), Some("script:always-true"));
    }

    /// The one reclaim rule: expiry. A crashed runner's lease is taken over
    /// by the next tick with nothing else needing to notice.
    #[test]
    fn an_expired_lease_is_reclaimable() {
        let conn = test_conn();
        let t = armed(&conn, Some(5));
        assert!(claimed(&conn, &t, "lease-a"));
        conn.execute(
            "UPDATE tripwire_runtime SET lease_expires_at = '2000-01-01 00:00:00' WHERE task_id = ?1",
            params![t.id],
        )
        .unwrap();
        assert!(claimed(&conn, &t, "lease-b"));
        assert_eq!(for_task(&conn, &t.id).unwrap().unwrap().lease_id.as_deref(), Some("lease-b"));
        // And the reclaimed attempt's late result is dropped.
        let r = record(&conn, &t.id, "lease-a", &Outcome::Tripped("late".into())).unwrap();
        assert!(r.discarded);
        let after = tasks::get(&conn, &t.id).unwrap().unwrap();
        assert!(after.tripwire_armed(), "a stale attempt must not move the state");
    }

    /// A leased row is not due, so a second tick cannot double-claim through
    /// `list_due` either.
    #[test]
    fn a_leased_row_is_not_listed_as_due() {
        let conn = test_conn();
        let t = armed(&conn, Some(5));
        assert_eq!(list_due(&conn, 10).unwrap().len(), 1);
        assert!(claimed(&conn, &t, "lease-a"));
        assert!(list_due(&conn, 10).unwrap().is_empty());
    }

    // ── record ─────────────────────────────────────────────────────────────

    #[test]
    fn not_yet_records_the_attempt_and_moves_nothing_on_the_task() {
        let conn = test_conn();
        let t = armed(&conn, Some(5));
        assert!(claimed(&conn, &t, "l1"));
        let r = record(&conn, &t.id, "l1", &Outcome::NotYet("0 users".into())).unwrap();
        assert!(!r.discarded);
        assert!(r.transition.is_none());
        let rt = for_task(&conn, &t.id).unwrap().unwrap();
        assert!(rt.lease_id.is_none());
        assert!(rt.checked_at.is_some());
        assert_eq!(rt.last_outcome.as_deref(), Some("not_yet"));
        assert_eq!(rt.last_note.as_deref(), Some("0 users"));
        assert_eq!(rt.failures, 0);
        let after = tasks::get(&conn, &t.id).unwrap().unwrap();
        assert!(after.tripwire_armed());
        assert!(crate::db::comments::for_task(&conn, &t.id).unwrap().is_empty());
        // Just checked, so not due again for five minutes.
        assert!(list_due(&conn, 10).unwrap().is_empty());
    }

    #[test]
    fn a_changed_spec_mid_attempt_discards_the_result() {
        let conn = test_conn();
        let t = armed(&conn, Some(5));
        assert!(claimed(&conn, &t, "l1"));
        // The user re-pointed the tripwire while the script ran.
        update(
            &conn,
            &t.id,
            &TaskPatch { tripwire: Some(Some("script:something-else".into())), ..Default::default() },
        )
        .unwrap();
        let r = record(&conn, &t.id, "l1", &Outcome::Tripped("yes".into())).unwrap();
        assert!(r.discarded);
        let after = tasks::get(&conn, &t.id).unwrap().unwrap();
        assert!(after.tripwire_armed());
        assert_eq!(after.tripwire.as_deref(), Some("script:something-else"));
        assert!(crate::db::comments::for_task(&conn, &t.id).unwrap().is_empty());
    }

    #[test]
    fn a_task_finished_mid_attempt_discards_the_result_and_releases_the_lease() {
        let conn = test_conn();
        let t = armed(&conn, Some(5));
        assert!(claimed(&conn, &t, "l1"));
        tasks::complete(&conn, &t.id).unwrap();
        let r = record(&conn, &t.id, "l1", &Outcome::Tripped("yes".into())).unwrap();
        assert!(r.discarded);
        let rt = for_task(&conn, &t.id).unwrap().unwrap();
        assert!(rt.lease_id.is_none(), "the lease is released, not left to expire");
        assert!(rt.checked_at.is_none(), "nothing else was recorded");
    }

    /// The request is retained when a NEWER one arrived during the attempt,
    /// and cleared when it was the one served.
    #[test]
    fn a_request_is_cleared_only_when_it_is_the_one_served() {
        let conn = test_conn();
        let t = armed(&conn, None);
        request_check(&conn, &t.id).unwrap();
        let first = for_task(&conn, &t.id).unwrap().unwrap().requested_at.unwrap();
        assert!(claimed(&conn, &t, "l1"));
        assert_eq!(
            for_task(&conn, &t.id).unwrap().unwrap().lease_request_at.as_deref(),
            Some(first.as_str())
        );

        // A second "check now" lands while the attempt is in flight. It must
        // be a DIFFERENT stamp, so force one rather than racing the clock.
        conn.execute(
            "UPDATE tripwire_runtime SET requested_at = '2999-01-01 00:00:00.000' WHERE task_id = ?1",
            params![t.id],
        )
        .unwrap();
        record(&conn, &t.id, "l1", &Outcome::NotYet(String::new())).unwrap();
        let rt = for_task(&conn, &t.id).unwrap().unwrap();
        assert_eq!(
            rt.requested_at.as_deref(),
            Some("2999-01-01 00:00:00.000"),
            "the newer request must survive the attempt that served the older one"
        );
        // And it is still due, because of that request.
        assert_eq!(list_due(&conn, 10).unwrap().len(), 1);

        // Serve it: now the request goes.
        assert!(claimed(&conn, &t, "l2"));
        record(&conn, &t.id, "l2", &Outcome::NotYet(String::new())).unwrap();
        let rt = for_task(&conn, &t.id).unwrap().unwrap();
        assert!(rt.requested_at.is_none());
        assert!(list_due(&conn, 10).unwrap().is_empty(), "manual and unrequested: never due");
    }

    #[test]
    fn a_manual_tripwire_surfaces_on_its_first_error() {
        let conn = test_conn();
        let t = armed(&conn, None);
        request_check(&conn, &t.id).unwrap();
        assert!(claimed(&conn, &t, "l1"));
        let r = record(&conn, &t.id, "l1", &Outcome::Error("exit 2: boom".into())).unwrap();
        assert!(matches!(r.transition, Some(Transition::Failed { failures: 1, .. })), "{r:?}");
        let after = tasks::get(&conn, &t.id).unwrap().unwrap();
        assert_eq!(after.tripwire_state.as_deref(), Some("error"));
        assert!(!after.tripwire_armed(), "error SURFACES the task");
        let log = crate::db::comments::for_task(&conn, &t.id).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].author, crate::db::comments::AUTHOR_TRIPWIRE);
        assert!(log[0].body.contains("boom"), "{}", log[0].body);
    }

    #[test]
    fn a_cadenced_tripwire_backs_off_and_surfaces_on_the_fifth_error_and_a_success_resets() {
        let conn = test_conn();
        let t = armed(&conn, Some(5));
        for i in 1..=4 {
            // Backdate the last check so the backoff has elapsed.
            conn.execute(
                "UPDATE tripwire_runtime SET checked_at = '2000-01-01 00:00:00' WHERE task_id = ?1",
                params![t.id],
            )
            .unwrap();
            assert!(claimed(&conn, &t, &format!("l{i}")));
            let r = record(&conn, &t.id, &format!("l{i}"), &Outcome::Error(format!("fail {i}"))).unwrap();
            assert!(r.transition.is_none(), "attempt {i} must not surface yet");
            assert_eq!(r.failures, i);
            assert!(tasks::get(&conn, &t.id).unwrap().unwrap().tripwire_armed());
        }
        // Freshly failed four times: the backoff (5 × 2^4 = 80 min) has not
        // elapsed, so it is NOT due yet.
        assert!(list_due(&conn, 10).unwrap().is_empty(), "backoff must hold it back");

        // A success in between resets the counter…
        conn.execute(
            "UPDATE tripwire_runtime SET checked_at = '2000-01-01 00:00:00' WHERE task_id = ?1",
            params![t.id],
        )
        .unwrap();
        assert!(claimed(&conn, &t, "ok"));
        let r = record(&conn, &t.id, "ok", &Outcome::NotYet(String::new())).unwrap();
        assert_eq!(r.failures, 0);

        // …so it takes five more to surface.
        for i in 1..=5 {
            conn.execute(
                "UPDATE tripwire_runtime SET checked_at = '2000-01-01 00:00:00' WHERE task_id = ?1",
                params![t.id],
            )
            .unwrap();
            assert!(claimed(&conn, &t, &format!("m{i}")));
            let r = record(&conn, &t.id, &format!("m{i}"), &Outcome::Error("still".into())).unwrap();
            if i < 5 {
                assert!(r.transition.is_none());
            } else {
                assert!(matches!(r.transition, Some(Transition::Failed { failures: 5, .. })));
            }
        }
        let after = tasks::get(&conn, &t.id).unwrap().unwrap();
        assert_eq!(after.tripwire_state.as_deref(), Some("error"));
        assert!(!list_due(&conn, 10).unwrap().iter().any(|(x, _)| x.id == t.id), "error is not armed");
    }

    /// The trip transaction: state, comment, AND — with `complete` — the
    /// completion that releases a dependent.
    #[test]
    fn a_trip_with_complete_finishes_the_task_and_releases_what_it_blocked() {
        let conn = test_conn();
        let gate = create(
            &conn,
            &NewTask {
                title: "TRIPWIRE: first outside user".into(),
                tripwire: Some("script:pocketbuddy-first-user".into()),
                tripwire_every_minutes: Some(360),
                tripwire_on_trip: Some("complete".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let legal = create(&conn, &NewTask { title: "privacy policy".into(), ..Default::default() }).unwrap();
        crate::db::deps::add(&conn, &legal.id, &gate.id).unwrap();
        assert_eq!(crate::db::deps::blocked_task_ids(&conn).unwrap(), vec![legal.id.clone()]);

        assert!(claimed(&conn, &gate, "l1"));
        let r = record(&conn, &gate.id, "l1", &Outcome::Tripped("user 7 linked a bank".into())).unwrap();
        assert_eq!(
            r.transition,
            Some(Transition::Tripped {
                title: gate.title.clone(),
                note: "user 7 linked a bank".into(),
                completed: true
            })
        );
        let after = tasks::get(&conn, &gate.id).unwrap().unwrap();
        assert_eq!(after.status, "done");
        assert_eq!(after.tripwire_state.as_deref(), Some("tripped"));
        assert!(after.tripwire_tripped_at.is_some());
        let log = crate::db::comments::for_task(&conn, &gate.id).unwrap();
        assert_eq!(log.len(), 1);
        assert!(log[0].body.contains("user 7 linked a bank"));
        assert!(
            crate::db::deps::blocked_task_ids(&conn).unwrap().is_empty(),
            "the dependent must be released by the same transaction"
        );
    }

    #[test]
    fn a_trip_with_surface_leaves_the_task_open_and_visible() {
        let conn = test_conn();
        let t = armed(&conn, Some(5));
        assert!(claimed(&conn, &t, "l1"));
        let r = record(&conn, &t.id, "l1", &Outcome::Tripped(String::new())).unwrap();
        assert!(matches!(r.transition, Some(Transition::Tripped { completed: false, .. })));
        let after = tasks::get(&conn, &t.id).unwrap().unwrap();
        assert_eq!(after.status, "inbox");
        assert_eq!(after.tripwire_state.as_deref(), Some("tripped"));
        assert!(!after.tripwire_armed());
        let log = crate::db::comments::for_task(&conn, &t.id).unwrap();
        assert!(log[0].body.contains("(no output)"));
    }

    /// An injected failure after the state write leaves NOTHING applied — not
    /// the state, not the comment, not the completion, not even the lease
    /// release. The attempt is retried when the lease expires.
    #[test]
    fn a_failure_mid_transaction_leaves_nothing_half_applied() {
        let conn = test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "atomic".into(),
                tripwire: Some("script:always-true".into()),
                tripwire_on_trip: Some("complete".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(claimed(&conn, &t, "l1"));
        FAIL_AFTER_TRANSITION.with(|f| f.set(true));
        let err = record(&conn, &t.id, "l1", &Outcome::Tripped("yes".into()));
        FAIL_AFTER_TRANSITION.with(|f| f.set(false));
        assert!(err.is_err());

        let after = tasks::get(&conn, &t.id).unwrap().unwrap();
        assert!(after.tripwire_armed(), "state rolled back");
        assert_eq!(after.status, "inbox", "completion rolled back");
        assert!(crate::db::comments::for_task(&conn, &t.id).unwrap().is_empty(), "comment rolled back");
        let rt = for_task(&conn, &t.id).unwrap().unwrap();
        assert_eq!(rt.lease_id.as_deref(), Some("l1"), "the lease is still held, to expire");
        assert!(rt.checked_at.is_none());
    }

    // ── the runtime row's lifecycle ────────────────────────────────────────

    #[test]
    fn clearing_a_tripwire_resets_the_runtime_row_and_never_deletes_it() {
        let conn = test_conn();
        let t = armed(&conn, Some(5));
        request_check(&conn, &t.id).unwrap();
        assert!(claimed(&conn, &t, "l1"));
        record(&conn, &t.id, "l1", &Outcome::Error("x".into())).unwrap();
        let before = for_task(&conn, &t.id).unwrap().unwrap();
        assert_eq!(before.failures, 1);

        update(&conn, &t.id, &TaskPatch { tripwire: Some(None), ..Default::default() }).unwrap();
        let after = tasks::get(&conn, &t.id).unwrap().unwrap();
        assert!(after.tripwire.is_none() && after.tripwire_state.is_none());
        let rt = for_task(&conn, &t.id).unwrap().expect("reset, not deleted");
        assert_eq!(rt.failures, 0);
        assert!(rt.checked_at.is_none() && rt.requested_at.is_none() && rt.lease_id.is_none());
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM tripwire_runtime", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn the_runtime_row_goes_with_its_task_by_cascade() {
        let conn = test_conn();
        let t = armed(&conn, Some(5));
        assert!(for_task(&conn, &t.id).unwrap().is_some());
        tasks::delete(&conn, &t.id).unwrap();
        assert!(for_task(&conn, &t.id).unwrap().is_none());
    }

    #[test]
    fn the_runtime_map_covers_live_tripwire_tasks_only() {
        let conn = test_conn();
        let a = armed(&conn, Some(5));
        let b = armed(&conn, Some(5));
        let _plain = create(&conn, &NewTask { title: "plain".into(), ..Default::default() }).unwrap();
        tasks::trash(&conn, &b.id).unwrap();
        let map = runtime_map(&conn).unwrap();
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&a.id));
    }

    // ── monitors ───────────────────────────────────────────────────────────

    fn monitor(conn: &Connection) -> Task {
        create(
            conn,
            &NewTask {
                title: "weekly audit".into(),
                tripwire: Some("script:always-true".into()),
                tripwire_every_minutes: Some(10080),
                tripwire_on_trip: Some("monitor".into()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn trip(conn: &Connection, t: &Task, lease: &str) -> Recorded {
        assert!(claimed(conn, t, lease));
        record(conn, &t.id, lease, &Outcome::Tripped("2 broken paths".into())).unwrap()
    }

    /// The whole loop: trip → work → Complete → re-armed and due AT ONCE
    /// (the runtime reset clears `checked_at`, so the fix is re-checked on the
    /// next tick, not a week later) → a trip on the DONE task reopens it.
    #[test]
    fn a_monitor_survives_completion_and_reopens_on_its_next_trip() {
        let conn = test_conn();
        let m = monitor(&conn);
        assert_eq!(m.tripwire_on_trip.as_deref(), Some("monitor"));

        let r = trip(&conn, &m, "l1");
        assert!(matches!(r.transition, Some(Transition::Tripped { completed: false, .. })));
        assert_eq!(tasks::get(&conn, &m.id).unwrap().unwrap().status, "inbox");

        let done = tasks::complete(&conn, &m.id).unwrap();
        assert_eq!(done.status, "done");
        assert_eq!(done.tripwire_state.as_deref(), Some("armed"), "Complete = handled = re-armed");
        assert!(done.tripwire_tripped_at.is_none());
        let rt = for_task(&conn, &m.id).unwrap().unwrap();
        assert!(rt.checked_at.is_none(), "the runtime row is reset");
        let due = list_due(&conn, 10).unwrap();
        assert_eq!(due.len(), 1, "a done monitor is still checked, and at once");

        let r = trip(&conn, &due[0].0, "l2");
        assert!(!r.discarded, "a done monitor's attempt is still ours");
        let after = tasks::get(&conn, &m.id).unwrap().unwrap();
        assert_eq!(after.status, "todo", "a trip reopens a done monitor");
        assert!(after.completed_at.is_none());
        assert_eq!(after.tripwire_state.as_deref(), Some("tripped"));
    }

    /// A monitor that is done and quiet stays done — a not-yet moves nothing.
    #[test]
    fn a_quiet_check_leaves_a_done_monitor_done() {
        let conn = test_conn();
        let m = monitor(&conn);
        tasks::complete(&conn, &m.id).unwrap();
        assert!(claimed(&conn, &m, "l1"));
        let r = record(&conn, &m.id, "l1", &Outcome::NotYet(String::new())).unwrap();
        assert!(!r.discarded);
        assert_eq!(tasks::get(&conn, &m.id).unwrap().unwrap().status, "done");
    }

    /// Invariant 26's "never stays hidden after a failure" holds for a done
    /// monitor too: the error that surfaces it reopens it.
    #[test]
    fn a_failing_check_reopens_a_done_monitor() {
        let conn = test_conn();
        let m = create(
            &conn,
            &NewTask {
                title: "manual monitor".into(),
                tripwire: Some("script:always-true".into()),
                tripwire_on_trip: Some("monitor".into()),
                ..Default::default()
            },
        )
        .unwrap();
        tasks::complete(&conn, &m.id).unwrap();
        request_check(&conn, &m.id).unwrap();
        assert!(claimed(&conn, &m, "l1"));
        let r = record(&conn, &m.id, "l1", &Outcome::Error("exit 2: boom".into())).unwrap();
        assert!(matches!(r.transition, Some(Transition::Failed { .. })), "{r:?}");
        let after = tasks::get(&conn, &m.id).unwrap().unwrap();
        assert_eq!(after.status, "todo");
        assert_eq!(after.tripwire_state.as_deref(), Some("error"));
        // Completing it after fixing the script re-arms it, same as a trip.
        let done = tasks::complete(&conn, &after.id).unwrap();
        assert_eq!(done.tripwire_state.as_deref(), Some("armed"));
    }

    /// A GATE is untouched by all of this: done still ends its check, and a
    /// gate's completion never re-arms it.
    #[test]
    fn a_done_gate_is_not_checked_and_not_rearmed() {
        let conn = test_conn();
        let g = armed(&conn, Some(5));
        assert!(claimed(&conn, &g, "l1"));
        record(&conn, &g.id, "l1", &Outcome::Tripped("yes".into())).unwrap();
        let done = tasks::complete(&conn, &g.id).unwrap();
        assert_eq!(done.tripwire_state.as_deref(), Some("tripped"));
        assert!(rearm_handled_monitors(&conn).unwrap().is_empty());
        conn.execute("UPDATE tasks SET tripwire_state = 'armed' WHERE id = ?1", params![g.id]).unwrap();
        reset_runtime(&conn, &g.id).unwrap();
        assert!(list_due(&conn, 10).unwrap().is_empty(), "a done gate is never checked");
    }

    /// The backstop: a done-and-tripped monitor that arrived WITHOUT passing
    /// through `update`'s rule (a post-image synced from an older build) is
    /// re-armed by the runner, and then checked.
    #[test]
    fn the_runner_rearms_a_monitor_completed_by_an_older_build() {
        let conn = test_conn();
        let m = monitor(&conn);
        trip(&conn, &m, "l1");
        // What an older build's completion looks like on arrival: status only.
        conn.execute(
            "UPDATE tasks SET status = 'done', completed_at = datetime('now') WHERE id = ?1",
            params![m.id],
        )
        .unwrap();
        assert!(list_due(&conn, 10).unwrap().is_empty(), "tripped, so not due yet");
        assert_eq!(rearm_handled_monitors(&conn).unwrap(), vec![m.id.clone()]);
        let after = tasks::get(&conn, &m.id).unwrap().unwrap();
        assert_eq!(after.status, "done");
        assert_eq!(after.tripwire_state.as_deref(), Some("armed"));
        assert_eq!(list_due(&conn, 10).unwrap().len(), 1);
        assert!(rearm_handled_monitors(&conn).unwrap().is_empty(), "idempotent");
    }

    /// Archiving is how a monitor is retired.
    #[test]
    fn an_archived_monitor_is_not_checked() {
        let conn = test_conn();
        let m = monitor(&conn);
        tasks::complete(&conn, &m.id).unwrap();
        tasks::archive(&conn, &m.id).unwrap();
        assert!(list_due(&conn, 10).unwrap().is_empty());
    }

    /// Completing a recurring task hands the tripwire to nobody (one-shot
    /// across recurrence), so the pair is refused on every route.
    #[test]
    fn a_monitor_cannot_also_repeat() {
        let conn = test_conn();
        let err = create(
            &conn,
            &NewTask {
                title: "x".into(),
                recurrence: Some("weekly:MO".into()),
                tripwire: Some("script:always-true".into()),
                tripwire_on_trip: Some("monitor".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("can't also repeat"), "{err}");
        let m = monitor(&conn);
        let err = update(
            &conn,
            &m.id,
            &TaskPatch { recurrence: Some(Some("daily".into())), ..Default::default() },
        )
        .unwrap_err();
        assert!(err.to_string().contains("can't also repeat"), "{err}");
        // And the other order: a recurring gate can't be turned into a monitor.
        let g = create(
            &conn,
            &NewTask {
                title: "y".into(),
                recurrence: Some("daily".into()),
                tripwire: Some("script:always-true".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(update(
            &conn,
            &g.id,
            &TaskPatch { tripwire_on_trip: Some(Some("monitor".into())), ..Default::default() },
        )
        .is_err());
        // Clearing the recurrence in the same patch is fine.
        update(
            &conn,
            &g.id,
            &TaskPatch {
                recurrence: Some(None),
                tripwire_on_trip: Some(Some("monitor".into())),
                ..Default::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn counts_reflect_armed_due_and_leased() {
        let conn = test_conn();
        let a = armed(&conn, Some(5));
        let _manual = armed(&conn, None);
        assert_eq!(counts(&conn).unwrap(), Counts { armed: 2, due_now: 1, leased: 0 });
        assert!(claimed(&conn, &a, "l1"));
        assert_eq!(counts(&conn).unwrap(), Counts { armed: 2, due_now: 0, leased: 1 });
    }
}
