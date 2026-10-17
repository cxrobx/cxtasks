//! Which tables travel, decided ONCE, at the table level.
//!
//! # Why the table and not the function
//!
//! The alternative is classifying at each of the ~105 write sites, which is 105
//! chances to forget — and the failure is not a crash but "one field silently
//! stopped syncing", with no greppable call site to find it from. A table has
//! exactly one classification, `sqlite_master` enumerates every table, and
//! [`tests::every_table_is_classified`] fails the build the moment a migration
//! adds one nobody decided about.
//!
//! That last part is the whole point. An unclassified table defaults to
//! **nothing** — it is neither synced nor deliberately excluded — and this
//! module refuses to let that state exist quietly.

/// What happens to a table when a snapshot is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableClass {
    /// Travels whole-row, both directions.
    Synced,
    /// Stays on the machine that wrote it. A fact about THIS host.
    HostLocal,
    /// Derived or private to the sync layer itself. Never on the wire, and
    /// rebuilt or maintained locally.
    Internal,
}

/// Every table, classified. Ordered as the snapshot emits them: parents before
/// the rows that reference them, so an applier that enforces foreign keys can
/// walk this list in order.
pub const TABLES: &[(&str, TableClass)] = &[
    // ── synced ──────────────────────────────────────────────────────────────
    // The host registry. It has to travel, because the whole point is that the
    // Mac can see what the NAS declares and the NAS can read its own row — and
    // they only ever meet through this database. Every host writes only its
    // OWN row, so whole-row merge has no contention to resolve: two hosts
    // cannot produce two versions of one row.
    ("hosts", TableClass::Synced),
    ("projects", TableClass::Synced),
    ("sections", TableClass::Synced),
    ("project_repos", TableClass::Synced),
    ("project_updates", TableClass::Synced),
    ("tasks", TableClass::Synced),
    // The volatile half of a tripwire (invariant 26). It travels because the
    // phone must see "last checked" and its "check now" must reach the Mac's
    // runner — and it is its OWN table precisely so that a not-yet written
    // every six hours never rewrites the task row somebody is editing. After
    // `tasks`, which it references.
    ("tripwire_runtime", TableClass::Synced),
    ("tags", TableClass::Synced),
    ("task_tags", TableClass::Synced),
    ("task_deps", TableClass::Synced),
    ("task_relations", TableClass::Synced),
    ("task_comments", TableClass::Synced),
    ("task_attachments", TableClass::Synced),
    // A run is a fact about work, not about a machine: which host executed it
    // is a COLUMN on the row (v20), so the row travels and stays honest about
    // where it ran.
    ("task_runs", TableClass::Synced),

    // ── host-local ──────────────────────────────────────────────────────────
    // 2120 rows and growing without bound, and only ever read when somebody
    // opens one run. Fetched on demand instead.
    ("run_events", TableClass::HostLocal),
    // Excluded WHOLESALE, with a named allowlist read back out of it — see
    // [`SYNCED_META_KEYS`]. Carrying the whole table would replicate
    // `schema_version` across a version skew and rewind `next_short_id`.
    ("app_meta", TableClass::HostLocal),

    // ── internal ────────────────────────────────────────────────────────────
    // Derived from `tasks` by trigger. Shipping it would mean shipping an index
    // that the receiving end rebuilds anyway, and invariant 2 says never write
    // it directly.
    ("tasks_fts", TableClass::Internal),
    // The sync layer's own bookkeeping. What THIS host has not yet sent.
    ("sync_outbox", TableClass::Internal),
    // The canonical node's record of which ranges it issued. Replicating it
    // would let two nodes believe they own the same range, which is the exact
    // failure lease blocks exist to prevent.
    ("short_id_leases", TableClass::Internal),
    // SQLite's own, created as a side effect of AUTOINCREMENT.
    ("sqlite_sequence", TableClass::Internal),
];

/// Columns that stay behind even though their table travels.
///
/// **`tasks.reminded_at` is a fact about a MACHINE**, not about the task: it
/// records that this host has already fired a reminder. Pulling another host's
/// value either re-arms a reminder that already fired or disarms one that has
/// not, and both are silent. The iOS client refuses to patch it for the same
/// reason.
pub const HOST_LOCAL_COLUMNS: &[(&str, &str)] = &[("tasks", "reminded_at")];

/// The only `app_meta` keys that travel.
///
/// An allowlist rather than a denylist, because the danger is asymmetric: a
/// setting that fails to sync is an annoyance, and a key that syncs when it
/// should not can rewind `next_short_id` or stamp a schema version from a
/// different build. These are the ones a person deliberately chose.
pub const SYNCED_META_KEYS: &[&str] = &[
    // The one that MUST travel. Two hosts enforcing one daily cap against two
    // different definitions of "today" is the silent over-admission this key
    // exists to prevent, and leaving it host-local would leave that hole open
    // in exactly the deployment it was added for.
    "bg_budget_tz",
    "bg_daily_run_cap",
    "bg_daily_token_cap",
    "bg_max_concurrent",
    "bg_max_turns",
    "bg_model",
    "bg_run_token_cap",
    "bg_timeout_minutes",
    // The agent-check sub-cap is a budget setting like the others above and
    // travels for the same reason `bg_daily_token_cap` does: two hosts
    // admitting checks against two different ceilings is one ceiling doubled.
    "bg_tripwire_daily_token_cap",
];

/// Keys that must NEVER travel, whatever else changes.
///
/// Redundant with the allowlist by construction, and kept anyway: this is the
/// list the refusal test asserts against, so the reason each one is dangerous
/// is written down next to it rather than inferred from an absence.
///
/// * `schema_version` — clobbers the receiver's version across a build skew,
///   making a newer DB re-run migrations it has already applied.
/// * `next_short_id` — rewinds the T-number cursor, which reissues numbers that
///   are already in commit messages. Invariant 14 says never.
/// * `sync_*` — a node's own pairing state. Copying it makes a peer believe it
///   is paired with itself.
/// * `role` — which end is canonical. Copying it produces two canonical nodes.
/// * `bg_budget_reset_at` — a clock this host is running, not a setting.
pub const NEVER_SYNCED_META_KEYS: &[&str] = &[
    "schema_version",
    "next_short_id",
    "role",
    "bg_budget_reset_at",
];

/// Is this `app_meta` key allowed on the wire?
pub fn meta_key_travels(key: &str) -> bool {
    // `sync_*` is a prefix rather than an enumeration: the sync layer will grow
    // keys, and every one of them is this host's own state — the pairing state,
    // the sweep watermark, the per-table digests. A denylist that had to be
    // extended for each would eventually miss one, and this is exactly the
    // growth that would have done it.
    !key.starts_with("sync_") && SYNCED_META_KEYS.contains(&key)
}

pub fn class_of(table: &str) -> Option<TableClass> {
    TABLES.iter().find(|(t, _)| *t == table).map(|(_, c)| *c)
}

/// Tables that travel, in emit order.
pub fn synced_tables() -> impl Iterator<Item = &'static str> {
    TABLES
        .iter()
        .filter(|(_, c)| *c == TableClass::Synced)
        .map(|(t, _)| *t)
}

/// Columns of `table` that must not be emitted.
pub fn skipped_columns(table: &str) -> impl Iterator<Item = &'static str> + '_ {
    HOST_LOCAL_COLUMNS
        .iter()
        .filter(move |(t, _)| *t == table)
        .map(|(_, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn tables_in(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master
                  WHERE type IN ('table')
                    AND name NOT LIKE 'tasks_fts_%'
                  ORDER BY name",
            )
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    /// **The structural guard.** Every table in the database is classified.
    ///
    /// A migration that adds a table nobody classified would otherwise produce
    /// exactly the failure the whole table-level approach exists to avoid: data
    /// that silently does not travel, discovered whenever somebody notices it
    /// missing on the other machine. This turns that into a failing test on the
    /// commit that adds the table.
    #[test]
    fn every_table_is_classified() {
        let conn = crate::db::schema::test_conn();
        let unclassified: Vec<String> = tables_in(&conn)
            .into_iter()
            .filter(|t| class_of(t).is_none())
            .collect();
        assert!(
            unclassified.is_empty(),
            "these tables have no classification, so they would silently not sync: {unclassified:?}\n\
             Add each to `TABLES` with a reason."
        );
    }

    /// And the reverse: nothing here names a table that no longer exists.
    #[test]
    fn the_classification_does_not_name_a_table_that_is_gone() {
        let conn = crate::db::schema::test_conn();
        let real = tables_in(&conn);
        let phantom: Vec<&str> = TABLES
            .iter()
            .map(|(t, _)| *t)
            .filter(|t| !real.contains(&t.to_string()))
            .collect();
        assert!(phantom.is_empty(), "classified but absent from the schema: {phantom:?}");
    }

    /// The dangerous keys are refused whatever the allowlist says.
    #[test]
    fn the_dangerous_meta_keys_never_travel() {
        for key in NEVER_SYNCED_META_KEYS {
            assert!(!meta_key_travels(key), "{key} must never be emitted");
        }
        for key in ["sync_enabled", "sync_peer_id", "sync_seeded_from", "sync_lease_hi"] {
            assert!(!meta_key_travels(key), "{key} must never be emitted");
        }
        // ...and the allowlist still works, or this test passes by refusing
        // everything.
        assert!(meta_key_travels("bg_daily_token_cap"));
    }

    /// `reminded_at` is skipped, and nothing else on `tasks` is.
    #[test]
    fn only_reminded_at_is_withheld_from_tasks() {
        let skipped: Vec<&str> = skipped_columns("tasks").collect();
        assert_eq!(skipped, vec!["reminded_at"]);
        assert_eq!(skipped_columns("projects").count(), 0);
    }
}
