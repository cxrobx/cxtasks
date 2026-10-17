//! Catching changes that never went through the verb table.
//!
//! # Why this is not a belt-and-braces nicety
//!
//! Two writers in this codebase bypass `api::dispatch` entirely today:
//!
//! * **`cxtasks-mcp`** calls `db::tasks::create`, `db::tags::attach`,
//!   `db::comments::add` and friends directly. It is the app's most-used entry
//!   point — every `/cxtasks:task`, every write-back from a background run —
//!   and none of it appends an intent.
//! * **`cxtasks-runner`** writes `task_runs` through `db::runs::*`.
//!
//! Without a sweep their changes apply locally, are never pushed, and are then
//! reverted by the next pull. That is silent data loss on the busiest path in
//! the system, so the sweep is load-bearing rather than defensive — and it stays
//! load-bearing even after a deploy, because it is not about stale binaries.
//!
//! It also covers, for free, the raw-SQL bypasses inside `dispatch` itself
//! (`empty_trash`'s bare `DELETE`, `reorder_tasks`) and any future one, without
//! anybody having to remember they exist.
//!
//! # What it cannot cover
//!
//! **Hard deletes.** Nothing is left to sweep, and the next pull resurrects the
//! row. That is why `destroy_task` and `empty_trash` require a connection and
//! `purge_expired` runs only on the canonical node: those go through the verb
//! table, get recorded as intents, and replay properly.

use rusqlite::Connection;
use serde_json::json;

use crate::error::AppError;
use crate::sync::{outbox, snapshot, tables};

/// How far the sweep has already looked.
pub const KEY_SWEPT_THROUGH: &str = "sync_swept_through";

/// How a table reports "this row changed".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watermark {
    /// A SQL expression yielding the row's last-touched stamp. Compared as a
    /// STRING, which is correct because every one of these is written by
    /// `datetime('now')` in fixed-width `YYYY-MM-DD HH:MM:SS` UTC — so the
    /// lexical order is the chronological one.
    Column(&'static str),
    /// No timestamp of any kind, so the whole table travels every sweep.
    ///
    /// Only acceptable because these are tiny — `task_tags` is ~100 rows.
    /// Adding a large table here would quietly make every sweep expensive, and
    /// [`tests::only_small_tables_are_swept_wholesale`] says so.
    Whole,
}

/// Every synced table and how it is swept.
///
/// Exhaustive by test: [`tests::every_synced_table_is_swept`] fails if a table
/// is classified `Synced` and missing here, because a synced table nobody
/// sweeps is one whose out-of-band writes vanish.
pub const SWEEP: &[(&str, Watermark)] = &[
    // A heartbeat is a write, so this is the one table whose watermark moves on
    // a timer rather than on a user action. That is why the control thread
    // beats once a MINUTE rather than on its 3-second control loop: the loop
    // reads cancels and pauses, which have to be prompt, while liveness is
    // judged in minutes and a 3-second heartbeat would push a row over the
    // network twenty times for every one thing it told anybody.
    ("hosts", Watermark::Column("updated_at")),
    ("tasks", Watermark::Column("updated_at")),
    // Every write path here bumps `updated_at` — the claim, the record, the
    // request, the reset — so the attempt bookkeeping travels as it happens.
    ("tripwire_runtime", Watermark::Column("updated_at")),
    // These five are EDITED IN PLACE and have no `updated_at`, so a
    // `created_at` watermark never sees the edit: moving a project into an
    // area, renaming one (which rewrites `sections`, `project_updates` and
    // `project_repos` too), closing or dating one, reordering the sidebar,
    // setting a repo primary, renaming or recolouring a tag. The edit applied
    // locally, was never pushed, and the next pull put the old row back —
    // found 2026-09-26 as "drag pocketbuddy into Products and it jumps back
    // out". All five are tiny (tens of rows; `tags` ~120), so they travel
    // whole, gated by the digest, rather than growing a column and a
    // migration on both ends.
    ("projects", Watermark::Whole),
    ("sections", Watermark::Whole),
    ("project_repos", Watermark::Whole),
    ("project_updates", Watermark::Whole),
    ("tags", Watermark::Whole),
    ("task_deps", Watermark::Column("created_at")),
    ("task_relations", Watermark::Column("created_at")),
    // Edited in place, so `created_at` alone never saw an edit (schema v28).
    ("task_comments", Watermark::Column("MAX(created_at, COALESCE(edited_at, ''))")),
    ("task_attachments", Watermark::Column("created_at")),
    // A run is touched three times — started, heartbeaten, finished — and only
    // the first sets `started_at`. Taking the max of all three is what makes a
    // run that FINISHED since the last sweep visible; watching `started_at`
    // alone would push the row once, at the beginning, and never again.
    (
        "task_runs",
        Watermark::Column(
            "MAX(started_at, COALESCE(heartbeat_at,''), COALESCE(finished_at,''), \
             COALESCE(cancel_requested_at,''))",
        ),
    ),
    // No timestamp column at all, and `tags::attach` does not bump the task it
    // tags — so a tag added by the MCP is invisible to every other watermark
    // here. ~100 rows.
    ("task_tags", Watermark::Whole),
];

fn watermark_for(table: &str) -> Option<Watermark> {
    SWEEP.iter().find(|(t, _)| *t == table).map(|(_, w)| *w)
}

/// Rows changed since the last sweep, as `sync_upsert` intents.
///
/// Returns how many intents were appended. Runs inside the caller's
/// transaction, like [`outbox::append`], so a sweep and the watermark it
/// advances commit together — otherwise a crash between them loses exactly the
/// window the sweep was covering.
pub fn run(conn: &Connection) -> Result<usize, AppError> {
    let since = crate::db::meta::get(conn, KEY_SWEPT_THROUGH)?.unwrap_or_default();
    let host = crate::host::host_id();
    let mut appended = 0usize;

    // Read the clock BEFORE reading rows. A row written between the two reads
    // is then swept again next time — wasteful and harmless. Reading it after
    // would let such a row fall in the gap and never be swept at all.
    let now: String = conn.query_row("SELECT datetime('now')", [], |r| r.get(0))?;

    // Emit in the applier's declared order — parents before the rows that
    // reference them — rather than in this list's order.
    //
    // Getting that wrong is not a style point. `tasks.section_id` references
    // `sections`, so a `tasks` intent that arrives first fails a foreign key
    // against a section the receiver has not been given yet, and the whole push
    // stops. Deriving the order from `synced_tables()` means there is one
    // ordering rather than two that must agree — this list says only HOW each
    // table is swept, never WHEN.
    for table in tables::synced_tables() {
        let Some(mark) = watermark_for(table) else { continue };
        let mark = &mark;
        let changed = match mark {
            Watermark::Whole => {
                // A wholesale table has no timestamp, so "has it changed" can
                // only be answered by looking at it. Hashing the result and
                // remembering the hash turns an unconditional re-send into one
                // that happens when something actually moved — otherwise every
                // reconcile forever pushes ~100 rows that the receiver already
                // has, and no reconcile is ever genuinely quiet.
                let data = snapshot::read_table_where(conn, table, None, &[])?;
                let digest = digest_of(&data);
                let key = format!("sync_digest_{table}");
                if crate::db::meta::get(conn, &key)?.as_deref() == Some(digest.as_str()) {
                    continue;
                }
                crate::db::meta::set(conn, &key, &digest)?;
                data
            }
            Watermark::Column(expr) => {
                if since.is_empty() {
                    // First sweep on a freshly paired host: everything counts,
                    // because nothing has ever been pushed.
                    snapshot::read_table_where(conn, table, None, &[])?
                } else {
                    // `>=`, NOT `>`. Every one of these stamps is
                    // `datetime('now')`, which is SECOND-resolution, so a
                    // strict cursor skips any row written in the same second as
                    // the watermark — permanently, because the watermark has
                    // already moved past it. The cost of `>=` is re-sweeping
                    // the boundary second, which is idempotent; the cost of `>`
                    // is a change that never travels and no way to notice.
                    //
                    // This is the same reason the plan rejects a delta snapshot
                    // outright, and it is not theoretical: it was found by a
                    // test whose run finished in the same second as the sweep
                    // that preceded it.
                    snapshot::read_table_where(
                        conn,
                        table,
                        Some(&format!("{expr} >= ?1")),
                        &[&since],
                    )?
                }
            }
        };
        if changed.rows.is_empty() {
            continue;
        }
        outbox::append(
            conn,
            SYNC_UPSERT,
            &json!({ "table": table, "columns": changed.columns, "rows": changed.rows }),
            &host,
        )?;
        appended += 1;
    }

    crate::db::meta::set(conn, KEY_SWEPT_THROUGH, &now)?;
    Ok(appended)
}

/// Take the tables swept whole as the new baseline, after a pull.
///
/// A pull replaces them with the peer's copy, which changes their digest. Left
/// alone, the next sweep reads that as a LOCAL change and pushes the pulled
/// rows straight back — harmless when nothing moved upstream in between, and a
/// silent revert of the phone's edit when something did. Called right after
/// the snapshot applies, so only changes made here after the pull count.
pub fn adopt_digests(conn: &Connection) -> Result<(), AppError> {
    for (table, mark) in SWEEP {
        if *mark != Watermark::Whole {
            continue;
        }
        let data = snapshot::read_table_where(conn, table, None, &[])?;
        crate::db::meta::set(conn, &format!("sync_digest_{table}"), &digest_of(&data))?;
    }
    Ok(())
}

/// A stable fingerprint of a table's contents.
///
/// `DefaultHasher` is not cryptographic and does not need to be: this compares
/// a table against its own previous state on the same machine, and the cost of
/// a collision is one skipped re-send of data the peer already has.
fn digest_of(data: &snapshot::TableSnapshot) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data.columns.hash(&mut h);
    // Rows in the order SQLite yields them. A reordering would produce a
    // different digest and cost one redundant push, which is the harmless
    // direction.
    for row in &data.rows {
        for value in row {
            value.to_string().hash(&mut h);
        }
    }
    format!("{:016x}", h.finish())
}

/// The synthetic verb a sweep emits.
///
/// Not a member of the verb table, deliberately: it is not something a person
/// can do, it takes a table name and post-images, and `api::dispatch` must go
/// on refusing it as unknown. The peer's push endpoint handles it separately.
pub const SYNC_UPSERT: &str = "sync_upsert";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{self, NewTask};

    /// **Every synced table is swept.**
    ///
    /// A table that travels but is never swept is one whose out-of-band writes
    /// — from the MCP, the runner, or a raw `DELETE` inside a verb — apply
    /// locally, never push, and get reverted by the next pull.
    #[test]
    fn every_synced_table_is_swept() {
        let missing: Vec<&str> = tables::synced_tables()
            .filter(|t| watermark_for(t).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "these tables sync but are never swept, so a write that bypasses the verb \
             table would be silently lost: {missing:?}"
        );
    }

    /// And nothing here names a table that does not sync.
    #[test]
    fn the_sweep_does_not_name_an_unsynced_table() {
        let synced: Vec<&str> = tables::synced_tables().collect();
        let stray: Vec<&str> = SWEEP
            .iter()
            .map(|(t, _)| *t)
            .filter(|t| !synced.contains(t))
            .collect();
        assert!(stray.is_empty(), "swept but not synced: {stray:?}");
    }

    /// Wholesale sweeping is only affordable for small tables.
    #[test]
    fn only_small_tables_are_swept_wholesale() {
        let whole: Vec<&str> = SWEEP
            .iter()
            .filter(|(_, w)| *w == Watermark::Whole)
            .map(|(t, _)| *t)
            .collect();
        assert_eq!(
            whole,
            vec!["projects", "sections", "project_repos", "project_updates", "tags", "task_tags"],
            "a table added here travels IN FULL on every sweep — make sure it is tiny, \
             and say why in the comment"
        );
    }

    /// **Intents come out parents-first**, because a receiver checks foreign
    /// keys.
    ///
    /// `tasks.section_id` references `sections`, so a `tasks` intent ahead of
    /// its `sections` intent fails a constraint and stops the entire push.
    /// Found by pushing a real database into an empty node and watching intent
    /// 1 fail with FOREIGN KEY.
    #[test]
    fn intents_are_emitted_in_the_appliers_declared_order() {
        let conn = test_conn();
        crate::db::projects::create(&conn, "P", crate::db::projects::ProjectKind::Area, None).unwrap();
        let section = crate::db::projects::create_section(&conn, Some("P"), "S").unwrap();
        let t = tasks::create(&conn, &NewTask { title: "x".into(), project: Some("P".into()), ..Default::default() }).unwrap();
        crate::db::projects::move_task_to_section(&conn, &t.id, Some(&section.id)).unwrap();
        crate::db::tags::attach(&conn, &t.id, "tag").unwrap();

        run(&conn).unwrap();

        let emitted: Vec<String> = outbox::pending(&conn, 100)
            .unwrap()
            .iter()
            .map(|i| i.args["table"].as_str().unwrap_or_default().to_string())
            .collect();
        let declared: Vec<&str> = tables::synced_tables().collect();

        let position = |t: &str| emitted.iter().position(|e| e == t);
        for (a, b) in [("sections", "tasks"), ("projects", "sections"), ("tasks", "task_tags"), ("tags", "task_tags")] {
            if let (Some(i), Some(j)) = (position(a), position(b)) {
                assert!(i < j, "{a} must be pushed before {b}: got {emitted:?}");
            }
        }
        // And the emitted order is a subsequence of the declared one, so the
        // two cannot drift.
        let mut it = declared.iter();
        assert!(
            emitted.iter().all(|e| it.any(|d| d == e)),
            "emitted {emitted:?} is not in declared order {declared:?}"
        );
    }

    /// **The MCP's bypass, caught.**
    ///
    /// A write that never touches `api::dispatch` still ends up in the outbox.
    #[test]
    fn a_write_that_bypassed_the_verb_table_is_swept_up() {
        let conn = test_conn();
        crate::db::meta::set(&conn, KEY_SWEPT_THROUGH, "2999-01-01 00:00:00").unwrap();

        // Exactly what `cxtasks-mcp` does: straight to the db layer.
        tasks::create(&conn, &NewTask { title: "filed by the mcp".into(), ..Default::default() }).unwrap();
        assert_eq!(outbox::count(&conn).unwrap(), 0, "nothing recorded it, which is the problem");

        // The watermark is in the future, so nothing is "changed" yet...
        run(&conn).unwrap();
        let after_future_watermark = outbox::count(&conn).unwrap();

        // ...but with an honest watermark the row is picked up.
        crate::db::meta::set(&conn, KEY_SWEPT_THROUGH, "2000-01-01 00:00:00").unwrap();
        run(&conn).unwrap();

        let intents = outbox::pending(&conn, 50).unwrap();
        let tasks_intent = intents
            .iter()
            .find(|i| i.cmd == SYNC_UPSERT && i.args["table"] == "tasks")
            .expect("the bypassed write must be swept into an intent");
        let titles: Vec<String> = tasks_intent.args["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r[1].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(titles.contains(&"filed by the mcp".to_string()));
        let _ = after_future_watermark;
    }

    /// A tag attached out of band is swept even though nothing timestamps it.
    #[test]
    fn a_tag_attached_out_of_band_still_travels() {
        let conn = test_conn();
        let t = tasks::create(&conn, &NewTask { title: "x".into(), ..Default::default() }).unwrap();
        crate::db::tags::attach(&conn, &t.id, "urgent").unwrap();

        run(&conn).unwrap();

        let intents = outbox::pending(&conn, 50).unwrap();
        assert!(
            intents.iter().any(|i| i.args["table"] == "task_tags"
                && !i.args["rows"].as_array().unwrap().is_empty()),
            "task_tags has no timestamp, so it must travel wholesale"
        );
    }

    /// **A run that FINISHES after the last sweep still travels.**
    ///
    /// `started_at` is set once, at the beginning, so watching it alone would
    /// push the row while the run was still going and never again — the peer
    /// would hold every run permanently at `running`, with its tokens, its
    /// result and its exit code never arriving. Found by sabotaging the
    /// watermark and noticing nothing failed.
    #[test]
    fn a_run_that_finished_since_the_last_sweep_is_swept() {
        let conn = test_conn();
        let task = tasks::create(&conn, &NewTask { title: "t".into(), ..Default::default() }).unwrap();
        let run = crate::db::runs::start(
            &conn, &task.id, "background", None,
            crate::executor::BackgroundExecutor::Claude,
            crate::intent::BackgroundIntent::Work, None,
        )
        .unwrap();

        // Everything so far has been swept and pushed.
        super::run(&conn).unwrap();
        let seqs: Vec<i64> = outbox::pending(&conn, 100).unwrap().iter().map(|i| i.local_seq).collect();
        outbox::clear(&conn, &seqs).unwrap();

        // Backdate the START so it is strictly older than the watermark; only
        // the FINISH is new. Without `MAX(...)` in the watermark, this row is
        // invisible from here on.
        conn.execute(
            "UPDATE task_runs SET started_at = '2000-01-01 00:00:00',
                                  finished_at = datetime('now'),
                                  status = 'succeeded'
              WHERE id = ?1",
            rusqlite::params![run.id],
        )
        .unwrap();

        super::run(&conn).unwrap();

        let intents = outbox::pending(&conn, 100).unwrap();
        let runs = intents
            .iter()
            .find(|i| i.args["table"] == "task_runs")
            .expect("a finished run must be swept, or the peer never learns it ended");
        assert!(
            !runs.args["rows"].as_array().unwrap().is_empty(),
            "the finished run row must be in the intent"
        );
    }

    /// **Re-sweeping is idempotent, not silent** — and that is the deliberate
    /// cost of `>=`.
    ///
    /// The watermark sits inside a second that may still be being written to,
    /// so the next sweep re-covers it. Applying a full-row upsert twice changes
    /// nothing, so the price is bytes rather than correctness — whereas `>`
    /// would buy silence by losing any row written during that second.
    #[test]
    fn a_repeat_sweep_re_covers_the_boundary_second_harmlessly() {
        let conn = test_conn();
        tasks::create(&conn, &NewTask { title: "x".into(), ..Default::default() }).unwrap();

        run(&conn).unwrap();
        let first: Vec<_> = outbox::pending(&conn, 100).unwrap();
        let seqs: Vec<i64> = first.iter().map(|i| i.local_seq).collect();
        outbox::clear(&conn, &seqs).unwrap();

        run(&conn).unwrap();
        let second = outbox::pending(&conn, 100).unwrap();

        let rows_of = |v: &[outbox::Intent], table: &str| {
            v.iter().find(|i| i.args["table"] == table).map(|i| i.args["rows"].clone())
        };
        assert_eq!(
            rows_of(&second, "tasks"),
            rows_of(&first, "tasks"),
            "a repeat sweep must carry the same post-images, so applying it twice is a no-op"
        );
    }

    /// And once the watermark is genuinely ahead of every row, a sweep is quiet.
    ///
    /// The watermark is set forward rather than by sleeping: the property is
    /// about the comparison, not about the passage of time, and a test that
    /// sleeps a second to prove it is a test people delete.
    #[test]
    fn a_sweep_whose_watermark_is_ahead_of_everything_appends_nothing() {
        let conn = test_conn();
        tasks::create(&conn, &NewTask { title: "x".into(), ..Default::default() }).unwrap();
        crate::db::meta::set(&conn, KEY_SWEPT_THROUGH, "2999-01-01 00:00:00").unwrap();

        let appended = run(&conn).unwrap();

        assert_eq!(appended, 0, "nothing is newer than the watermark, so nothing should queue");
        assert_eq!(outbox::count(&conn).unwrap(), 0);
    }
}
