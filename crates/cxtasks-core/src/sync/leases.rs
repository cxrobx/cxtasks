//! T-number ranges, handed out once.
//!
//! # Why not `MAX(local, canonical)`
//!
//! Because it races, and the damage is permanent. Two nodes create a task in
//! the same window, both read 91, both claim it, and the merge takes 92 — so
//! there are two T91s. Invariant 14 says a T-number is permanent and never
//! reissued precisely because those numbers end up in commit messages, in notes
//! and in this repo's own history; there is no repair, only a choice between
//! leaving a collision or renumbering, and renumbering breaks every reference
//! already written down.
//!
//! A lease block removes the race instead of detecting it. The canonical node
//! carves a range off one monotonic cursor inside a write transaction, and a
//! host that holds `[lo, hi]` needs no coordination at all to use it.
//!
//! # What running out looks like
//!
//! `short_id = NULL`, which is an already-supported state:
//! `db::tasks::backfill_short_ids` heals it and has run on every `initialize`
//! since v6, for exactly this class of reason. A task with no T-number is
//! usable, merely unaddressable as "T42" until a lease arrives — which is a far
//! better failure than blocking capture, and a far better one than issuing a
//! number this host does not own.

use rusqlite::{params, Connection, OptionalExtension};

use crate::db::meta::KEY_NEXT_SHORT_ID;
use crate::error::AppError;

/// This host's lease ceiling, INCLUSIVE. Absent when sync is off.
pub const KEY_LEASE_HI: &str = "sync_lease_hi";

/// How many numbers a lease carries. 500 is roughly six months of this
/// database's observed rate (92 tasks in about a year), so a laptop that never
/// reaches the peer still has months of headroom rather than days.
pub const LEASE_SIZE: i64 = 500;

/// Refill when fewer than this many remain. Deliberately large relative to any
/// plausible offline burst: the cost of refilling early is a wasted range out of
/// a 64-bit space, and the cost of refilling late is tasks with no T-number.
pub const REFILL_BELOW: i64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Lease {
    pub lo: i64,
    /// Inclusive. `next_short_id > hi` is the out-of-lease test, and having two
    /// conventions for the end of a range is how off-by-ones get written.
    pub hi: i64,
}

impl Lease {
    pub fn size(&self) -> i64 {
        self.hi - self.lo + 1
    }
    pub fn contains(&self, n: i64) -> bool {
        n >= self.lo && n <= self.hi
    }
}

// ── canonical side ──────────────────────────────────────────────────────────

/// Carve a range for `host` off the canonical cursor.
///
/// `BEGIN IMMEDIATE`, so two concurrent requests serialise on the write lock
/// rather than both reading the same cursor. The cursor is `next_short_id` —
/// the same one the canonical node's own task creation advances — which is what
/// makes overlap impossible rather than merely unlikely: there is one number
/// line and every consumer moves along it forwards.
pub fn issue(conn: &mut Connection, host: &str, size: i64) -> Result<Lease, AppError> {
    if size <= 0 {
        return Err(AppError::Validation("a lease must carry at least one number".into()));
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

    // Same idempotent seed `claim_short_id` uses: a DB that predates the key,
    // or a fresh test connection, arrives here without it.
    tx.execute(
        "INSERT OR IGNORE INTO app_meta (key, value)
         SELECT ?1, CAST(COALESCE(MAX(short_id), 0) + 1 AS TEXT) FROM tasks",
        params![KEY_NEXT_SHORT_ID],
    )?;
    // Above the cursor AND above every number actually in use.
    //
    // Those can disagree, and when they do the cursor is the one that is wrong.
    // A canonical node that received rows before it adopted the source's cursor
    // holds tasks numbered up to 92 while `next_short_id` still says 1 — and a
    // lease carved from the cursor alone would hand out [1, 500], reissuing
    // every T-number already in the database. Invariant 14 makes that
    // unrepairable, so the allocator refuses to trust one source for a fact
    // both of them know.
    //
    // Found by watching exactly that happen: a real database pushed into an
    // empty node, then a lease requested before adoption existed.
    let lo: i64 = tx.query_row(
        "SELECT MAX(
                  (SELECT CAST(value AS INTEGER) FROM app_meta WHERE key = ?1),
                  (SELECT COALESCE(MAX(short_id), 0) + 1 FROM tasks))",
        params![KEY_NEXT_SHORT_ID],
        |r| r.get(0),
    )?;
    let hi = lo + size - 1;

    tx.execute(
        "INSERT INTO short_id_leases (lo, hi, host) VALUES (?1, ?2, ?3)",
        params![lo, hi, host],
    )?;
    tx.execute(
        "UPDATE app_meta SET value = ?2 WHERE key = ?1",
        params![KEY_NEXT_SHORT_ID, (hi + 1).to_string()],
    )?;
    tx.commit()?;
    log::info!("issued T-number lease [{lo}, {hi}] to {host}");
    Ok(Lease { lo, hi })
}

/// Every range issued, oldest first. The ledger, for auditing a collision that
/// should be impossible.
pub fn issued(conn: &Connection) -> Result<Vec<(Lease, String)>, AppError> {
    let mut stmt = conn.prepare("SELECT lo, hi, host FROM short_id_leases ORDER BY lo ASC")?;
    let rows = stmt
        .query_map([], |r| Ok((Lease { lo: r.get(0)?, hi: r.get(1)? }, r.get::<_, String>(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ── local side ──────────────────────────────────────────────────────────────

/// Adopt a lease: point this host's cursor at `lo` and remember the ceiling.
///
/// Refuses to move the cursor BACKWARDS. A lease below the current cursor would
/// reissue numbers this host has already used, which is the collision the whole
/// mechanism exists to prevent — and it is a reachable mistake, because a
/// retried request could deliver an older lease after a newer one.
pub fn adopt(conn: &Connection, lease: Lease) -> Result<(), AppError> {
    let current: Option<i64> = conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM app_meta WHERE key = ?1",
            params![KEY_NEXT_SHORT_ID],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(current) = current {
        if lease.lo < current {
            return Err(AppError::Validation(format!(
                "lease [{}, {}] starts below this host's cursor ({current}) — \
                 adopting it would reissue T-numbers already used",
                lease.lo, lease.hi
            )));
        }
    }
    crate::db::meta::set(conn, KEY_NEXT_SHORT_ID, &lease.lo.to_string())?;
    crate::db::meta::set(conn, KEY_LEASE_HI, &lease.hi.to_string())?;
    Ok(())
}

/// This host's ceiling, or `None` when sync has never been enabled.
pub fn ceiling(conn: &Connection) -> Result<Option<i64>, AppError> {
    Ok(conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM app_meta WHERE key = ?1",
            params![KEY_LEASE_HI],
            |r| r.get(0),
        )
        .optional()?)
}

/// How many numbers this host may still issue. `None` means unbounded — no
/// lease, which is the standalone Mac and must stay unbounded forever.
pub fn remaining(conn: &Connection) -> Result<Option<i64>, AppError> {
    let Some(hi) = ceiling(conn)? else { return Ok(None) };
    let next: i64 = conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM app_meta WHERE key = ?1",
            params![KEY_NEXT_SHORT_ID],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(1);
    Ok(Some((hi - next + 1).max(0)))
}

/// Is it time to ask for more?
pub fn needs_refill(conn: &Connection) -> Result<bool, AppError> {
    Ok(matches!(remaining(conn)?, Some(n) if n < REFILL_BELOW))
}

/// Forget the ceiling, restoring unbounded local allocation.
///
/// Called on unpair. Leaving it behind would cap a now-standalone Mac at a
/// number it has no reason to respect, and it would run out silently.
pub fn clear(conn: &Connection) -> Result<(), AppError> {
    conn.execute("DELETE FROM app_meta WHERE key = ?1", params![KEY_LEASE_HI])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;

    #[test]
    fn a_lease_is_carved_above_the_cursor_and_moves_it() {
        let mut conn = test_conn();
        crate::db::meta::set(&conn, KEY_NEXT_SHORT_ID, "93").unwrap();

        let lease = issue(&mut conn, "mac", 500).unwrap();
        assert_eq!(lease, Lease { lo: 93, hi: 592 });
        assert_eq!(lease.size(), 500);

        let next: i64 = conn
            .query_row("SELECT CAST(value AS INTEGER) FROM app_meta WHERE key='next_short_id'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(next, 593, "the canonical cursor must clear the lease it just gave away");
    }

    /// **The property the whole mechanism exists for.**
    ///
    /// Ranges issued in any order, to any hosts, never overlap — checked
    /// pairwise rather than by trusting the allocator's arithmetic.
    #[test]
    fn issued_leases_never_overlap() {
        let mut conn = test_conn();
        let mut all = Vec::new();
        for (i, host) in ["mac", "nas", "phone", "mac", "nas"].iter().enumerate() {
            all.push(issue(&mut conn, host, 10 + i as i64 * 7).unwrap());
        }
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert!(
                    a.hi < b.lo || b.hi < a.lo,
                    "leases {a:?} and {b:?} overlap"
                );
            }
        }
        // And the ledger agrees with what was handed out.
        let ledger = issued(&conn).unwrap();
        assert_eq!(ledger.len(), all.len());
        assert_eq!(ledger.iter().map(|(l, _)| *l).collect::<Vec<_>>(), all);
    }

    /// **A lease never overlaps numbers already in the database**, even when
    /// the cursor disagrees with them.
    ///
    /// This is the shape a canonical node is in between receiving rows and
    /// adopting the source's cursor: tasks numbered to 92, `next_short_id`
    /// still 1. Carving from the cursor alone would reissue every one of them.
    #[test]
    fn a_lease_clears_existing_short_ids_even_when_the_cursor_is_stale() {
        let mut conn = test_conn();
        // Rows arrived without the cursor being adopted.
        for n in 1..=92 {
            conn.execute(
                "INSERT INTO tasks (id, title, short_id) VALUES (?1, 'x', ?2)",
                params![format!("id-{n}"), n],
            )
            .unwrap();
        }
        crate::db::meta::set(&conn, KEY_NEXT_SHORT_ID, "1").unwrap();

        let lease = issue(&mut conn, "mac", 500).unwrap();

        assert_eq!(lease.lo, 93, "a lease must start above every number in use, got {lease:?}");
        let clash: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE short_id BETWEEN ?1 AND ?2",
                params![lease.lo, lease.hi],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(clash, 0, "the lease overlaps T-numbers already issued");
    }

    /// A lease that would rewind the cursor is refused.
    #[test]
    fn adopting_a_lease_below_the_cursor_is_refused() {
        let conn = test_conn();
        crate::db::meta::set(&conn, KEY_NEXT_SHORT_ID, "600").unwrap();
        let err = adopt(&conn, Lease { lo: 93, hi: 592 }).unwrap_err().to_string();
        assert!(err.contains("reissue T-numbers already used"), "{err}");
    }

    #[test]
    fn adopting_sets_the_cursor_and_the_ceiling() {
        let conn = test_conn();
        adopt(&conn, Lease { lo: 1000, hi: 1499 }).unwrap();
        assert_eq!(ceiling(&conn).unwrap(), Some(1499));
        assert_eq!(remaining(&conn).unwrap(), Some(500));
        assert!(!needs_refill(&conn).unwrap());
    }

    #[test]
    fn refill_triggers_below_the_threshold_and_not_above() {
        let conn = test_conn();
        adopt(&conn, Lease { lo: 1, hi: 1000 }).unwrap();
        crate::db::meta::set(&conn, KEY_NEXT_SHORT_ID, &(1000 - REFILL_BELOW + 1).to_string()).unwrap();
        assert!(!needs_refill(&conn).unwrap(), "exactly at the threshold is not yet a refill");
        crate::db::meta::set(&conn, KEY_NEXT_SHORT_ID, &(1000 - REFILL_BELOW + 2).to_string()).unwrap();
        assert!(needs_refill(&conn).unwrap());
    }

    /// **No lease means unbounded**, which is the standalone Mac and every
    /// database that has never seen sync.
    #[test]
    fn without_a_lease_there_is_no_ceiling_at_all() {
        let conn = test_conn();
        assert_eq!(ceiling(&conn).unwrap(), None);
        assert_eq!(remaining(&conn).unwrap(), None);
        assert!(!needs_refill(&conn).unwrap(), "no lease is not a lease that needs refilling");
    }

    #[test]
    fn clearing_a_lease_restores_unbounded_allocation() {
        let conn = test_conn();
        adopt(&conn, Lease { lo: 10, hi: 20 }).unwrap();
        clear(&conn).unwrap();
        assert_eq!(ceiling(&conn).unwrap(), None);
        assert_eq!(remaining(&conn).unwrap(), None);
    }
}
