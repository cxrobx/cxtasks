//! The reconcile nobody has to remember to press.
//!
//! # Why this module exists
//!
//! Sync worked. Nothing was erroring. The Mac simply never reconciled unless a
//! person opened Settings and clicked — so on 2026-08-25 `sync_last_pull_at`
//! was found sitting four days old, and the canonical node the phone reads
//! (lid shut, over the tailnet) was missing nine rows including every tripwire
//! value. There was nothing to fix and nothing to see: the failure mode of a
//! manual-only sync is that it looks exactly like a working one.
//!
//! A button is a fine way to say "now"; it is a terrible way to say "keep up".
//!
//! # What is here
//!
//! Two entry points over the same [`reconcile::reconcile`], plus the record of
//! what the last attempt did.
//!
//! * [`now`] — the caller **asked**. No peer is an error, because they are
//!   waiting for an answer and "nothing happened" is not one.
//! * [`pass`] — the timer's tick. No peer is `Ok(None)`, because paused and
//!   unpaired are states a person chose, and a timer that manufactures an error
//!   out of them every five minutes teaches you to ignore the error line.
//!
//! Both record the attempt, which is the part that has to be impossible to
//! forget: the whole bug above was invisible because nothing wrote down that
//! the last successful reconcile was days ago and nobody had tried since. The
//! stamp goes on whether it worked or not, so "we have not even tried since
//! Tuesday" and "we try every five minutes and the NAS refuses" are different
//! sentences instead of the same silence.

use rusqlite::Connection;

use crate::db::meta;
use crate::error::AppError;
use crate::sync::http::HttpPeer;
use crate::sync::reconcile::{self, ReconcileReport};
use crate::sync::state;

/// How long between automatic passes.
///
/// Five minutes, and the number is a trade rather than a preference. A pull is
/// a **full snapshot** — the `?since` cursor was designed and deliberately not
/// implemented, because a delta cannot see a hard delete — so every pass moves
/// the whole synced set (~900 rows here, gzipped) across the tailnet. Tighter
/// buys freshness nobody is waiting on; looser walks back toward the four-day
/// hole this exists to close.
///
/// It is deliberately not a setting. A cadence with a slider is a cadence that
/// gets turned down to daily during one debugging session and left there.
pub const INTERVAL_SECONDS: u64 = 300;

/// Wait before the first pass after launch.
///
/// Startup is already doing migrations, an FTS verification and an attachment
/// prune against this same database. Joining that queue with a 120-second HTTP
/// call in hand buys nothing — the app has just opened, so nobody is looking at
/// four-day-old data yet.
pub const STARTUP_DELAY_SECONDS: u64 = 10;

/// How stale the last attempt must be before the runner steps in.
///
/// One interval and a minute: while the app is open its timer always gets there
/// first, so there is still exactly one thing reconciling. This exists for the
/// app being closed. A runner writes tripwire results and run outcomes either
/// way, and with nobody reconciling they sit on this machine, out of the hub
/// every other device reads.
pub const BACKSTOP_AFTER_SECONDS: i64 = INTERVAL_SECONDS as i64 + 60;

/// Is nothing else keeping this host in step?
pub fn backstop_due(last_attempt: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> bool {
    match last_attempt.and_then(crate::tripwire::parse_stamp) {
        Some(at) => (now - at).num_seconds() >= BACKSTOP_AFTER_SECONDS,
        None => true,
    }
}

/// [`pass`], but only when nothing else has tried recently. The runner's entry
/// point, on a thread of its own: a reconcile can take minutes and a tick must
/// not wait for one.
pub fn backstop_pass(conn: &mut Connection) -> Result<Option<ReconcileReport>, AppError> {
    let (last, _) = last_attempt(conn);
    if !backstop_due(last.as_deref(), chrono::Utc::now()) {
        return Ok(None);
    }
    pass(conn)
}

// ── The quick pass: sooner than five minutes when someone is waiting ────────
//
// The five-minute cadence is right for keeping replicas in step and wrong for
// the one thing a person watches live: a request sent from the phone. That
// request reaches the hub at once, but this host only SAW it at its next pull
// (up to five minutes), and once its run started, the phone saw "Queued" until
// the next push (up to five more) — the Ask thread read as hung while the
// work was already going (T1120, 2026-11-04).
//
// So the runner's sync thread ticks every [`QUICK_TICK_SECONDS`] and
// reconciles early for exactly three reasons, each cheap to detect:
// * a run on THIS host started or finished since the last attempt,
// * a run on this host is in flight and the last attempt is a minute old (the
//   phone streams its trace from the hub),
// * the hub holds queued work this replica doesn't know about — one small
//   `list_tasks` query, not a snapshot.
// Everything else still waits for the ordinary cadence.

/// How often the runner's sync thread looks.
pub const QUICK_TICK_SECONDS: u64 = 20;
/// Never two attempts closer than this, whatever the reason.
pub const QUICK_GAP_SECONDS: i64 = 15;
/// While a run is in flight here, push its trace this often.
pub const IN_FLIGHT_EVERY_SECONDS: i64 = 60;
/// A hub/replica mismatch that one reconcile didn't settle is not re-chased
/// before the ordinary cadence: a disagreement that persists is a sync
/// question, and a loop reconciling every 20 s would only hide it.
const SAME_MISMATCH_SECONDS: u64 = INTERVAL_SECONDS;

/// Why a quick pass reconciled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickReason {
    /// Nothing else has tried for over a cadence (the old backstop).
    Backstop,
    /// A run here started or finished since the last attempt.
    RunChanged,
    /// A run here is in flight; its trace goes up once a minute.
    RunInFlight,
    /// The hub has queued work this replica hasn't pulled.
    NewWork,
}

/// The decisions that need only this database, in priority order. The hub
/// probe is the caller's, because it is the one that costs a round trip.
pub fn local_reason(
    conn: &Connection,
    last_attempt: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<QuickReason> {
    if backstop_due(last_attempt, now) {
        return Some(QuickReason::Backstop);
    }
    let last = last_attempt?;
    let age = seconds_since(last, now);
    if age < QUICK_GAP_SECONDS {
        return None;
    }
    let host = crate::host::host_id();
    if run_changed_since(conn, &host, last) {
        return Some(QuickReason::RunChanged);
    }
    if age >= IN_FLIGHT_EVERY_SECONDS && running_here(conn, &host) {
        return Some(QuickReason::RunInFlight);
    }
    None
}

fn seconds_since(stamp: &str, now: chrono::DateTime<chrono::Utc>) -> i64 {
    crate::tripwire::parse_stamp(stamp)
        .map(|at| (now - at).num_seconds())
        .unwrap_or(i64::MAX)
}

/// Both operands through `datetime()` — run stamps are SQLite's, the attempt
/// stamp is ISO with a `Z` (invariant 1).
fn run_changed_since(conn: &Connection, host: &str, since: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_runs WHERE host = ?1 AND \
           (datetime(started_at) > datetime(?2) OR datetime(finished_at) > datetime(?2)))",
        rusqlite::params![host, since],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

fn running_here(conn: &Connection, host: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_runs WHERE host = ?1 AND status = 'running')",
        [host],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

/// Queued on the hub, not queued here.
pub fn unseen_queued(conn: &Connection, hub_queued: &[String]) -> std::collections::BTreeSet<String> {
    let mut local = std::collections::BTreeSet::new();
    if let Ok(mut stmt) = conn.prepare("SELECT id FROM tasks WHERE status = 'queued'") {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            local.extend(rows.flatten());
        }
    }
    hub_queued.iter().filter(|id| !local.contains(*id)).cloned().collect()
}

static LAST_MISMATCH: std::sync::Mutex<Option<(std::collections::BTreeSet<String>, std::time::Instant)>> =
    std::sync::Mutex::new(None);

/// The runner's sync tick: reconcile now when a reason above holds, else do
/// nothing. `Ok(None)` for unpaired / paused / canonical, as [`pass`].
pub fn quick_pass(conn: &mut Connection) -> Result<Option<(QuickReason, ReconcileReport)>, AppError> {
    if !state::enabled(conn) || state::is_canonical(conn) {
        return Ok(None);
    }
    let Some(url) = state::peer_url(conn) else { return Ok(None) };
    let (last, _) = last_attempt(conn);
    let t = chrono::Utc::now();

    let mut reason = local_reason(conn, last.as_deref(), t);
    if reason.is_none() && last.as_deref().map_or(true, |l| seconds_since(l, t) >= QUICK_GAP_SECONDS) {
        // A failed probe is a blip, not an attempt: it records nothing and
        // the ordinary cadence still runs.
        let hub = state::peer_token().and_then(|token| HttpPeer::new(&url, &token).queued_task_ids());
        match hub {
            Ok(ids) => {
                let unseen = unseen_queued(conn, &ids);
                if !unseen.is_empty() {
                    let mut last = LAST_MISMATCH.lock().unwrap_or_else(|p| p.into_inner());
                    let repeat = matches!(&*last, Some((set, at))
                        if *set == unseen && at.elapsed().as_secs() < SAME_MISMATCH_SECONDS);
                    if !repeat {
                        *last = Some((unseen, std::time::Instant::now()));
                        reason = Some(QuickReason::NewWork);
                    }
                }
            }
            Err(e) => log::debug!("sync: queue probe failed: {e}"),
        }
    }
    match reason {
        Some(why) => now(conn).map(|report| Some((why, report))),
        None => Ok(None),
    }
}

/// When a reconcile was last **attempted**, successful or not.
///
/// Distinct from [`state::KEY_LAST_PULL_AT`], which only moves when a pull
/// actually lands. Two keys because the gap between them is the diagnosis: same
/// value means the last attempt worked, attempt far ahead of pull means we are
/// trying and failing, and both stale means nothing is trying at all — which
/// was the actual bug.
pub const KEY_LAST_ATTEMPT_AT: &str = "sync_last_attempt_at";

/// Why the last attempt failed. **Absent means the last one succeeded**, so the
/// happy path stores nothing and there is no stale error to explain away.
pub const KEY_LAST_ERROR: &str = "sync_last_error";

/// Reconcile with the peer this host is paired to.
///
/// For a caller who asked for it: the button, `--sync-now`. Refuses with a
/// sentence when there is no peer.
pub fn now(conn: &mut Connection) -> Result<ReconcileReport, AppError> {
    let url = state::peer_url(conn)
        .ok_or_else(|| AppError::Validation("this Mac is not paired with a node".into()))?;
    let token = state::peer_token()?;
    let peer = HttpPeer::new(&url, &token);
    let result = reconcile::reconcile(conn, &peer);
    record_attempt(conn, &result);
    result
}

/// One automatic pass, or `Ok(None)` when this host has nothing to reconcile
/// with.
///
/// `None` covers three deliberate states — sync switched off, never paired, and
/// being the canonical node — and none of them is a fault. `Err` is reserved
/// for a pass that tried and could not, which is the only thing worth putting
/// in front of a person.
pub fn pass(conn: &mut Connection) -> Result<Option<ReconcileReport>, AppError> {
    if !state::enabled(conn) || state::is_canonical(conn) || state::peer_url(conn).is_none() {
        return Ok(None);
    }
    now(conn).map(Some)
}

/// Stamp the attempt, and the reason if it failed.
///
/// Deliberately infallible from the caller's side. This is bookkeeping about a
/// reconcile, and a reconcile that worked must not be reported as failed
/// because the note about it could not be written.
fn record_attempt(conn: &Connection, result: &Result<ReconcileReport, AppError>) {
    let stamp = crate::recurrence::to_iso(chrono::Utc::now());
    if let Err(e) = meta::set(conn, KEY_LAST_ATTEMPT_AT, &stamp) {
        log::warn!("could not record the sync attempt: {e}");
    }
    let outcome = match result {
        // Held back is not a failure — it is the ordering rule doing its job,
        // and the panel already says "N waiting to send". Recording it as an
        // error would put a red line under a reconcile that behaved correctly.
        Ok(_) => meta::set(conn, KEY_LAST_ERROR, ""),
        Err(e) => meta::set(conn, KEY_LAST_ERROR, &e.to_string()),
    };
    if let Err(e) = outcome {
        log::warn!("could not record the sync outcome: {e}");
    }
}

/// What the last attempt did: when, and why it failed if it did.
pub fn last_attempt(conn: &Connection) -> (Option<String>, Option<String>) {
    let at = meta::get(conn, KEY_LAST_ATTEMPT_AT).ok().flatten().filter(|s| !s.is_empty());
    let err = meta::get(conn, KEY_LAST_ERROR).ok().flatten().filter(|s| !s.is_empty());
    (at, err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;

    /// The pair: the app's timer keeps the runner out, and its absence lets the
    /// runner in. Either half alone also passes against a broken version.
    #[test]
    fn the_runner_steps_in_only_when_nothing_else_has_tried() {
        let now = chrono::Utc::now();
        let ago = |secs: i64| crate::recurrence::to_iso(now - chrono::Duration::seconds(secs));
        assert!(!backstop_due(Some(&ago(INTERVAL_SECONDS as i64)), now), "the app is on time");
        assert!(backstop_due(Some(&ago(BACKSTOP_AFTER_SECONDS)), now), "the app has gone");
        assert!(backstop_due(None, now), "nothing has ever tried");
    }

    /// Each quick reason fires on its own fact, and the gap holds them all.
    #[test]
    fn a_quick_pass_reconciles_for_run_activity_and_only_after_the_gap() {
        use crate::db::{runs, tasks};
        use crate::executor::BackgroundExecutor;
        use crate::intent::BackgroundIntent;
        let conn = test_conn();
        let now = chrono::Utc::now();
        let ago = |secs: i64| crate::recurrence::to_iso(now - chrono::Duration::seconds(secs));

        // Quiet: nothing ran since an attempt 30 s ago.
        assert_eq!(local_reason(&conn, Some(&ago(30)), now), None);

        let task = tasks::create(&conn, &tasks::NewTask { title: "t".into(), ..Default::default() })
            .unwrap()
            .id;
        let run = runs::start(&conn, &task, "background", None, BackgroundExecutor::Claude,
                              BackgroundIntent::Work, None).unwrap();
        // Started after an attempt 30 s ago: push it.
        assert_eq!(local_reason(&conn, Some(&ago(30)), now), Some(QuickReason::RunChanged));
        // ...but not within the gap.
        assert_eq!(local_reason(&conn, Some(&ago(5)), now), None);
        // Started before the attempt, still running: once a minute.
        let after_start = crate::recurrence::to_iso(chrono::Utc::now() + chrono::Duration::seconds(2));
        let later = now + chrono::Duration::seconds(40);
        assert_eq!(local_reason(&conn, Some(&after_start), later), None, "in flight, under a minute");
        let much_later = now + chrono::Duration::seconds(90);
        assert_eq!(local_reason(&conn, Some(&after_start), much_later), Some(QuickReason::RunInFlight));
        let _ = run;
        // And the old backstop still wins when nothing has tried for a cadence.
        assert_eq!(local_reason(&conn, Some(&ago(BACKSTOP_AFTER_SECONDS)), now), Some(QuickReason::Backstop));
    }

    #[test]
    fn unseen_queued_is_the_hubs_queue_minus_ours() {
        use crate::db::tasks;
        let conn = test_conn();
        let mine = tasks::create(&conn, &tasks::NewTask { title: "t".into(), ..Default::default() })
            .unwrap()
            .id;
        conn.execute("UPDATE tasks SET status = 'queued' WHERE id = ?1", [&mine]).unwrap();
        let unseen = unseen_queued(&conn, &[mine.clone(), "from-the-phone".into()]);
        assert_eq!(unseen.into_iter().collect::<Vec<_>>(), vec!["from-the-phone".to_string()]);
    }

    /// The quick pass is as quiet as the timer on a host with no peer.
    #[test]
    fn a_quick_pass_never_touches_an_unpaired_host() {
        let mut conn = test_conn();
        assert!(quick_pass(&mut conn).unwrap().is_none());
        assert_eq!(last_attempt(&conn), (None, None));
    }

    /// An unpaired machine stays quiet even when the clock says it's due.
    #[test]
    fn the_backstop_never_touches_an_unpaired_host() {
        let mut conn = test_conn();
        assert!(backstop_pass(&mut conn).unwrap().is_none());
        assert_eq!(last_attempt(&conn), (None, None));
    }

    /// A machine that has never paired must not be told it is broken every
    /// five minutes.
    #[test]
    fn an_unpaired_host_passes_quietly() {
        let mut conn = test_conn();
        assert!(pass(&mut conn).unwrap().is_none());
        assert_eq!(last_attempt(&conn), (None, None), "and nothing is recorded — it never tried");
    }

    /// Paused is a decision, not a fault.
    #[test]
    fn a_paused_host_passes_quietly() {
        let mut conn = test_conn();
        state::set_peer(&conn, "http://nas:18870", "the-nas").unwrap();
        state::set_enabled(&conn, false).unwrap();
        assert!(pass(&mut conn).unwrap().is_none());
    }

    /// The canonical node is reconciled ONTO. A timer there would be the node
    /// trying to sync with itself.
    #[test]
    fn the_canonical_node_passes_quietly() {
        let mut conn = test_conn();
        state::set_peer(&conn, "http://nas:18870", "the-nas").unwrap();
        state::set_enabled(&conn, true).unwrap();
        state::set_canonical(&conn, true).unwrap();
        assert!(pass(&mut conn).unwrap().is_none());
    }

    /// **The stamp is what makes a silent stall visible.** A pass that tries
    /// and fails must leave both the time and the reason behind, because the
    /// bug this module exists for looked identical to everything working.
    #[test]
    fn a_failed_attempt_records_when_and_why() {
        let mut conn = test_conn();
        // A peer that cannot resolve, so the attempt genuinely fails without
        // needing a server. The token file is read first, so this asserts on
        // whichever of the two failures comes first — the point is that SOME
        // reason is written down, not which one.
        state::set_peer(&conn, "http://127.0.0.1:1/nope", "unreachable").unwrap();
        state::set_enabled(&conn, true).unwrap();

        let err = pass(&mut conn).unwrap_err();

        let (at, why) = last_attempt(&conn);
        assert!(at.is_some(), "the attempt itself must be stamped even though it failed");
        assert_eq!(why.as_deref(), Some(err.to_string().as_str()), "and the reason kept verbatim");
    }

    /// A success clears the previous failure rather than leaving it to be
    /// re-read as current.
    #[test]
    fn a_success_clears_the_last_error() {
        let conn = test_conn();
        meta::set(&conn, KEY_LAST_ERROR, "the NAS was down").unwrap();
        record_attempt(&conn, &Ok(ReconcileReport::default()));
        assert_eq!(last_attempt(&conn).1, None, "a stale error reads as a current one");
    }

    /// Unpairing forgets these too — they describe a relationship that no
    /// longer exists, and a "last error" surviving an unpair is a red line
    /// under a node this host has stopped talking to.
    #[test]
    fn unpairing_forgets_the_last_attempt() {
        let conn = test_conn();
        state::set_peer(&conn, "http://nas:18870", "the-nas").unwrap();
        state::set_enabled(&conn, true).unwrap();
        record_attempt(&conn, &Err(AppError::Validation("boom".into())));
        assert!(last_attempt(&conn).0.is_some());

        state::unpair(&conn).unwrap();

        assert_eq!(last_attempt(&conn), (None, None));
    }
}
