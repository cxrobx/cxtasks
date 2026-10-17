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
