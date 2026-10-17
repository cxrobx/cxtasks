//! One code path for a five-second gap and a five-day one.
//!
//! There is no "catch up" mode and no separate first-run path. A host that has
//! been shut in a bag for a week runs exactly what a host that reconciled ten
//! seconds ago runs — sweep, push, pull — because a second code path is a
//! second set of bugs that only appear after the situation nobody can reproduce
//! on purpose.
//!
//! # The ordering rule that keeps work
//!
//! Push **before** pull, and **do not pull at all if anything failed to push.**
//!
//! A pull applies post-images and deletes local rows the peer does not have.
//! So pulling on top of an un-pushed change does not merely leave it behind —
//! it actively reverts it, or deletes the row entirely, and the user watches
//! work they saw succeed disappear with no error anywhere. Staying behind is
//! recoverable; being silently rolled back is not.

use rusqlite::Connection;

use crate::error::AppError;
use crate::sync::apply::ApplyStats;
use crate::sync::blobs::BlobReport;
use crate::sync::outbox::PushFailure;
use crate::sync::pair::Peer;
use crate::sync::{leases, outbox, state, sweep};

/// How many intents go up per round trip.
///
/// Bounded so a host returning from a long offline stretch does not build one
/// enormous request that times out and then rebuilds it identically forever.
/// The next reconcile takes the next batch.
const PUSH_BATCH: u32 = 500;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ReconcileReport {
    /// Synthetic upserts the sweep queued for changes that bypassed the verb table.
    pub swept: usize,
    pub pushed: usize,
    /// Never dropped. If this is non-empty the pull did not run.
    pub failed: Vec<PushFailure>,
    pub pulled: ApplyStats,
    /// True when the pull was skipped because something is still un-pushed.
    pub held_back: bool,
    /// Attachment bytes moved. Empty when the pull was held back — bytes
    /// follow rows, and there is no point sending a file for a row the peer
    /// has not been given yet.
    pub blobs: BlobReport,
    pub lease_renewed: Option<leases::Lease>,
}

impl ReconcileReport {
    /// Did anything change HERE? The UI redraws on this.
    ///
    /// Only the pull counts. A push is outbound and alters nothing locally, so
    /// including it would redraw the screen every time this host had something
    /// to say — and a redraw resets in-progress edits (gotcha #24). The symptom
    /// would be losing characters while typing, on a timer, whenever you had
    /// just made a change.
    pub fn changed(&self) -> bool {
        // A DOWNLOAD counts and an upload does not, for the reason above: an
        // image appearing where a broken thumbnail was is a visible change,
        // while sending one alters nothing here. It cannot loop, either — a
        // file that arrived is no longer missing, so the next pass has nothing
        // to fetch and reports quiet.
        self.pulled.changed() || self.blobs.changed()
    }
}

/// Reconcile this database with its peer.
pub fn reconcile(conn: &mut Connection, peer: &dyn Peer) -> Result<ReconcileReport, AppError> {
    if !state::enabled(conn) {
        return Err(AppError::Validation("sync is not enabled on this host".into()));
    }
    if state::is_canonical(conn) {
        return Err(AppError::Validation(
            "the canonical node does not reconcile — everyone else reconciles onto it".into(),
        ));
    }
    let mut report = ReconcileReport::default();

    // 1. Sweep, so changes that bypassed the verb table — the MCP's, the
    //    runner's — become intents before anything is sent.
    {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        report.swept = sweep::run(&tx)?;
        tx.commit()?;
    }

    // 2. Push, oldest first.
    let pending = outbox::pending(conn, PUSH_BATCH)?;
    if !pending.is_empty() {
        let result = peer.push(&pending)?;
        outbox::clear(conn, &result.accepted)?;
        report.pushed = result.accepted.len();
        report.failed = result.failed;
        if let Some(highest) = result.accepted.iter().max() {
            state::set_pushed_through(conn, *highest)?;
        }
    }

    // 3. Pull — but only when nothing is left behind.
    //
    // `outbox::count` rather than `report.failed.is_empty()`: the peer stops at
    // its first failure, so everything after it is still queued and equally
    // un-represented upstream. Asking the outbox covers both, and covers a
    // batch that was merely larger than PUSH_BATCH.
    let still_queued = outbox::count(conn)?;
    if still_queued > 0 {
        report.held_back = true;
        return Ok(report);
    }

    let snapshot = peer.snapshot()?;
    report.pulled = crate::sync::apply::apply_snapshot(conn, &snapshot)?;
    // What just arrived is the peer's, not ours — see `adopt_digests`.
    sweep::adopt_digests(conn)?;
    crate::db::meta::set(
        conn,
        state::KEY_LAST_PULL_AT,
        &crate::recurrence::to_iso(chrono::Utc::now()),
    )?;

    // 3b. Attachment bytes, AFTER the rows — see `sync::blobs`. The peer only
    //     accepts bytes for a row it already holds, so this has to follow both
    //     the push (which gave it our rows) and the pull (which gave us its).
    //
    //     Its failures are reported and never raised: one unreachable file is
    //     one missing image, and it must not fail a reconcile whose rows all
    //     landed. The next pass retries it, because the work is re-derived
    //     from what is on disk rather than from a queue.
    report.blobs = crate::sync::blobs::reconcile_bytes(conn, peer)?;

    // 4. Top the lease up while the peer is known to be reachable. Doing it
    //    here rather than when the lease runs out is the point: running out
    //    happens offline, which is exactly when it cannot be fixed.
    if leases::needs_refill(conn)? {
        let lease = peer.request_lease(&crate::host::host_id(), leases::LEASE_SIZE)?;
        leases::adopt(conn, lease)?;
        report.lease_renewed = Some(lease);
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{self, NewTask};
    use crate::sync::outbox::{Intent, PushResult};
    use crate::sync::pair::PeerInfo;
    use crate::sync::snapshot::Snapshot;
    use std::cell::RefCell;

    /// A peer backed by a second real database, so a push actually applies and
    /// a snapshot actually reflects it.
    struct TwinPeer {
        db: RefCell<Connection>,
        /// Refuse the first intent of any kind. Which one it is does not
        /// matter — the property under test is that ANY failure stops the pull.
        refuse: bool,
        pushes: RefCell<usize>,
        /// The twin's attachment bytes. In memory rather than on disk because
        /// `paths::attachments_dir()` is process-global — two "nodes" inside
        /// one test binary cannot have two of them, and a test must never write
        /// into the real Mac's store. The local half of the transfer still goes
        /// through the real filesystem code, against a temp directory.
        blobs: RefCell<std::collections::HashMap<String, Vec<u8>>>,
    }

    impl TwinPeer {
        fn new() -> Self {
            let conn = test_conn();
            crate::sync::state::set_canonical(&conn, true).unwrap();
            Self {
                db: RefCell::new(conn),
                refuse: false,
                pushes: RefCell::new(0),
                blobs: RefCell::new(std::collections::HashMap::new()),
            }
        }
    }

    impl Peer for TwinPeer {
        fn info(&self) -> Result<PeerInfo, AppError> {
            Ok(PeerInfo {
                host: "twin".into(),
                schema_version: crate::db::schema::CURRENT_VERSION,
                protocol_version: crate::db::schema::SYNC_PROTOCOL_VERSION,
                canonical: true,
                seeded_from: None,
            })
        }
        fn seed(&self, snap: &Snapshot, from: &str) -> Result<(), AppError> {
            let mut db = self.db.borrow_mut();
            crate::sync::apply::apply_snapshot(&mut db, snap)?;
            crate::sync::state::set_seeded_from(&db, from)?;
            Ok(())
        }
        fn request_lease(&self, host: &str, size: i64) -> Result<leases::Lease, AppError> {
            let mut db = self.db.borrow_mut();
            leases::issue(&mut db, host, size)
        }
        fn snapshot(&self) -> Result<Snapshot, AppError> {
            crate::sync::snapshot::take(&self.db.borrow())
        }
        fn push(&self, intents: &[Intent]) -> Result<PushResult, AppError> {
            *self.pushes.borrow_mut() += 1;
            let mut db = self.db.borrow_mut();
            let mut out = PushResult::default();
            for i in intents {
                if self.refuse {
                    out.failed.push(PushFailure {
                        local_seq: i.local_seq,
                        err: "refused on purpose".into(),
                    });
                    break;
                }
                let applied = if i.cmd == sweep::SYNC_UPSERT {
                    let table = i.args["table"].as_str().unwrap();
                    let data = serde_json::from_value(serde_json::json!({
                        "columns": i.args["columns"], "rows": i.args["rows"]
                    }))
                    .unwrap();
                    crate::sync::apply::upsert(&db, table, &data).map(|_| ())
                } else {
                    crate::api::dispatch_synced(&mut db, &i.cmd, &i.args).map(|_| ())
                };
                match applied {
                    Ok(()) => out.accepted.push(i.local_seq),
                    Err(e) => {
                        out.failed.push(PushFailure { local_seq: i.local_seq, err: e.to_string() });
                        break;
                    }
                }
            }
            Ok(out)
        }
        /// Rows the twin holds and has no bytes for — the same question the
        /// real `/attachments/missing` answers, asked of the twin's own DB.
        fn wanted_blobs(&self) -> Result<Vec<String>, AppError> {
            let held = self.blobs.borrow();
            Ok(crate::db::attachments::all(&self.db.borrow())?
                .into_iter()
                .map(|r| r.stored_name)
                .filter(|n| !held.contains_key(n))
                .collect())
        }
        fn get_blob(&self, stored_name: &str) -> Result<Vec<u8>, AppError> {
            self.blobs
                .borrow()
                .get(stored_name)
                .cloned()
                .ok_or_else(|| AppError::NotFound(format!("no bytes for {stored_name}")))
        }
        fn put_blob(&self, stored_name: &str, bytes: &[u8]) -> Result<(), AppError> {
            // Defence in depth, exactly as the server does it: bytes are never
            // stored for a row this node does not have.
            if crate::db::attachments::by_stored_name(&self.db.borrow(), stored_name)?.is_none() {
                return Err(AppError::NotFound(format!("no row for {stored_name}")));
            }
            self.blobs.borrow_mut().insert(stored_name.to_string(), bytes.to_vec());
            Ok(())
        }
    }

    fn local() -> Connection {
        let conn = test_conn();
        crate::sync::state::set_enabled(&conn, true).unwrap();
        conn
    }

    /// The ordinary round trip: local work reaches the peer, and the peer's
    /// state comes back.
    #[test]
    fn a_reconcile_pushes_local_work_and_pulls_the_peers_answer() {
        let mut conn = local();
        let peer = TwinPeer::new();
        crate::api::dispatch_synced(&mut conn, "create_task", &serde_json::json!({ "task": { "title": "from the mac" } })).unwrap();

        let report = reconcile(&mut conn, &peer).unwrap();

        assert!(report.pushed > 0);
        assert!(report.failed.is_empty(), "push failures: {:?}", report.failed);
        assert!(!report.held_back);
        assert_eq!(outbox::count(&conn).unwrap(), 0, "everything accepted is cleared");

        let on_peer: i64 = peer
            .db
            .borrow()
            .query_row("SELECT COUNT(*) FROM tasks WHERE title = 'from the mac'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(on_peer, 1);
    }

    /// **The ordering rule.** A failed push must stop the pull, or the pull
    /// reverts the change that failed to travel.
    #[test]
    fn a_failed_push_holds_back_the_pull_instead_of_reverting_the_work() {
        let mut conn = local();
        let mut peer = TwinPeer::new();
        crate::api::dispatch_synced(&mut conn, "create_task", &serde_json::json!({ "task": { "title": "stranded" } })).unwrap();
        peer.refuse = true;

        let report = reconcile(&mut conn, &peer).unwrap();

        assert!(!report.failed.is_empty(), "the failure must be reported");
        assert!(report.held_back, "the pull must not run");
        assert_eq!(report.pulled, ApplyStats::default());

        // And the work is still here, and still queued.
        let kept: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE title = 'stranded'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 1, "an un-pushed change must not be reverted by a pull");
        assert!(outbox::count(&conn).unwrap() > 0, "and must stay queued for the next attempt");
    }

    /// **A recorded verb must never overtake the rows it refers to.** Found
    /// live 2026-10-02: the MCP created T231 (a direct write, so only the
    /// sweep would ever queue it), the app then retagged it, and
    /// `set_task_tags` went into the outbox minutes ahead of the sweep's upsert
    /// of the task. The peer refused the verb — "task not found" — and since a
    /// push stops at its first failure, sync stood still for hours with 81
    /// changes queued behind it.
    #[test]
    fn a_recorded_verb_never_overtakes_an_unswept_row_it_refers_to() {
        let mut conn = local();
        let peer = TwinPeer::new();
        // Out of band, the way `cxtasks-mcp` writes: no intent recorded.
        let t = tasks::create(&conn, &NewTask { title: "made by the mcp".into(), ..Default::default() })
            .unwrap();
        assert_eq!(outbox::count(&conn).unwrap(), 0, "the fixture must bypass the verb table");

        crate::api::dispatch_synced(
            &mut conn,
            "set_task_tags",
            &serde_json::json!({ "task_id": t.id, "tags": ["cxtasks"] }),
        )
        .unwrap();

        let report = reconcile(&mut conn, &peer).unwrap();
        assert!(report.failed.is_empty(), "push failures: {:?}", report.failed);
        assert!(!report.held_back);
        let tagged: i64 = peer
            .db
            .borrow()
            .query_row(
                "SELECT COUNT(*) FROM task_tags tt JOIN tags g ON g.id = tt.tag_id \
                  WHERE tt.task_id = ?1 AND g.name = 'cxtasks'",
                [&t.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tagged, 1, "the task and its tag both reach the peer");
    }

    /// The other half: a deletion of a row the peer has never seen. Swept
    /// ahead of the verb, so the peer gets the row and then removes it,
    /// rather than refusing the delete as "not found".
    #[test]
    fn deleting_an_unswept_row_reaches_the_peer_as_a_delete_not_a_refusal() {
        let mut conn = local();
        let peer = TwinPeer::new();
        let t = tasks::create(&conn, &NewTask { title: "short-lived".into(), ..Default::default() })
            .unwrap();
        crate::api::dispatch_synced(&mut conn, "destroy_task", &serde_json::json!({ "id": t.id }))
            .unwrap();

        let report = reconcile(&mut conn, &peer).unwrap();
        assert!(report.failed.is_empty(), "push failures: {:?}", report.failed);
        let left: i64 = peer
            .db
            .borrow()
            .query_row("SELECT COUNT(*) FROM tasks WHERE id = ?1", [&t.id], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }

    /// The everyday shape: make a task in the app and give it a tag nobody has
    /// used before, all before the next reconcile. The tag must reach the peer
    /// under THIS host's id, or the next sweep collides on `tags.name`.
    #[test]
    fn a_new_tag_set_on_a_new_task_travels_under_one_id() {
        let mut conn = local();
        let peer = TwinPeer::new();
        let made = crate::api::dispatch_synced(
            &mut conn,
            "create_task",
            &serde_json::json!({ "task": { "title": "fresh" } }),
        )
        .unwrap();
        let id = made["id"].as_str().unwrap().to_string();
        crate::api::dispatch_synced(
            &mut conn,
            "set_task_tags",
            &serde_json::json!({ "task_id": id, "tags": ["brand-new"] }),
        )
        .unwrap();

        let report = reconcile(&mut conn, &peer).unwrap();
        assert!(report.failed.is_empty(), "push failures: {:?}", report.failed);
        // A second pass is the one that used to collide.
        let again = reconcile(&mut conn, &peer).unwrap();
        assert!(again.failed.is_empty(), "push failures: {:?}", again.failed);

        let local_id: String = conn
            .query_row("SELECT id FROM tags WHERE name = 'brand-new'", [], |r| r.get(0))
            .unwrap();
        let peer_id: String = peer
            .db
            .borrow()
            .query_row("SELECT id FROM tags WHERE name = 'brand-new'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(local_id, peer_id);
    }

    /// A quiet reconcile reports no change, so nothing on screen moves.
    #[test]
    fn a_reconcile_with_nothing_to_do_reports_no_change() {
        let mut conn = local();
        let peer = TwinPeer::new();
        reconcile(&mut conn, &peer).unwrap();
        // Second pass: both ends already agree.
        let report = reconcile(&mut conn, &peer).unwrap();
        assert!(
            !report.changed(),
            "a settled reconcile must not report movement — the UI redraws on it, \
             and a redraw discards in-progress edits (gotcha #24). Got {report:?}"
        );
    }

    /// The canonical node reconciles onto nobody.
    #[test]
    fn the_canonical_node_refuses_to_reconcile() {
        let mut conn = local();
        crate::sync::state::set_canonical(&conn, true).unwrap();
        let err = reconcile(&mut conn, &TwinPeer::new()).unwrap_err().to_string();
        assert!(err.contains("does not reconcile"), "{err}");
    }

    /// With sync off it refuses rather than quietly doing nothing, because a
    /// caller asking for a reconcile has been told sync is on.
    #[test]
    fn reconciling_with_sync_off_is_refused() {
        let mut conn = test_conn();
        let err = reconcile(&mut conn, &TwinPeer::new()).unwrap_err().to_string();
        assert!(err.contains("not enabled"), "{err}");
    }

    /// A nearly-spent lease is topped up while the peer is reachable — which is
    /// the only time it can be.
    #[test]
    fn a_lease_is_renewed_before_it_runs_out() {
        let mut conn = local();
        let peer = TwinPeer::new();
        leases::adopt(&conn, leases::Lease { lo: 1, hi: 10 }).unwrap();
        assert!(leases::needs_refill(&conn).unwrap());

        let report = reconcile(&mut conn, &peer).unwrap();

        assert!(report.lease_renewed.is_some(), "a nearly-spent lease must be topped up");
        assert!(!leases::needs_refill(&conn).unwrap());
    }

    /// **Bytes follow rows, and a byte that cannot be found does not sink the
    /// reconcile.**
    ///
    /// The attachment row travels as a post-image like any other, and the peer
    /// then *asks* for the file — which is the ordering `sync::blobs` argues
    /// for, observed end to end. Here neither end holds the bytes, so the
    /// fetch fails; the reconcile must still succeed, because one missing
    /// image must never stop a database syncing.
    ///
    /// It deliberately moves no bytes: `reconcile` resolves the local store
    /// through `paths::attachments_dir()`, which is process-global and, in a
    /// test binary, is the real Mac's. The transfer itself is proven against a
    /// temp directory in `sync::blobs`, and end to end by
    /// `scripts/sync-dress-rehearsal.sh`.
    #[test]
    fn an_attachment_row_travels_and_the_peer_then_asks_for_its_bytes() {
        let mut conn = local();
        let peer = TwinPeer::new();
        let task = tasks::create(&conn, &NewTask { title: "with an image".into(), ..Default::default() }).unwrap();
        let stored_name = format!("{}.png", uuid::Uuid::new_v4());
        crate::db::attachments::add(
            &conn, &task.id, None, "shot.png", "image/png", &stored_name, Some("deadbeef"),
        )
        .unwrap();

        let report = reconcile(&mut conn, &peer).unwrap();

        assert!(report.failed.is_empty(), "the ROW must land: {:?}", report.failed);
        let landed: i64 = peer
            .db
            .borrow()
            .query_row("SELECT COUNT(*) FROM task_attachments WHERE stored_name = ?1", [&stored_name], |r| r.get(0))
            .unwrap();
        assert_eq!(landed, 1, "the metadata travels as a post-image like anything else");

        assert!(
            peer.wanted_blobs().unwrap().contains(&stored_name),
            "having the row and no file is exactly what the peer asks for"
        );
        assert_eq!(report.blobs.downloaded, 0);
        assert_eq!(report.blobs.failed.len(), 1, "the miss is reported: {:?}", report.blobs);
        assert!(!report.blobs.changed(), "nothing arrived, so nothing on screen moves");
    }

    /// **An edit to an existing project travels.** `projects` has no
    /// `updated_at`, so a `created_at` watermark never saw the edit; the pull
    /// then reverted it. This is the drag-into-an-area bug of 2026-09-26.
    #[test]
    fn an_edit_to_an_existing_project_reaches_the_peer_and_survives_the_pull() {
        let mut conn = local();
        let peer = TwinPeer::new();
        for (name, kind) in [("products", "area"), ("pocketbuddy", "area")] {
            crate::api::dispatch_synced(
                &mut conn,
                "create_project",
                &serde_json::json!({ "name": name, "kind": kind }),
            )
            .unwrap();
        }
        reconcile(&mut conn, &peer).unwrap();

        // Make both rows older than the sweep watermark — the real case, a
        // project made weeks ago. Without this the whole test runs inside one
        // second, the `>=` cursor still sees the rows, and it proves nothing.
        for c in [&conn, &*peer.db.borrow()] {
            c.execute("UPDATE projects SET created_at = '2026-01-01 00:00:00'", []).unwrap();
        }
        crate::api::dispatch_synced(&mut conn, "set_project_kind", &serde_json::json!({ "name": "pocketbuddy", "kind": "project" })).unwrap();
        crate::api::dispatch_synced(&mut conn, "set_project_parent", &serde_json::json!({ "name": "pocketbuddy", "parent": "products" })).unwrap();
        reconcile(&mut conn, &peer).unwrap();

        let parent_of = |c: &Connection| -> Option<String> {
            c.query_row(
                "SELECT a.name FROM projects p LEFT JOIN projects a ON a.id = p.parent_id
                  WHERE p.name = 'pocketbuddy'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(parent_of(&peer.db.borrow()).as_deref(), Some("products"), "the peer has it");
        assert_eq!(parent_of(&conn).as_deref(), Some("products"), "and the pull did not revert it");
    }

    /// **A chosen icon travels.** `set_project_appearance` / `set_tag_appearance`
    /// edit rows made long ago — the case a `created_at` watermark never sees —
    /// so this pins that the appearance rides the same whole-table digest the
    /// drag-into-an-area fix put `projects` and `tags` behind (v29).
    #[test]
    fn a_chosen_icon_reaches_the_peer_and_survives_the_pull() {
        let mut conn = local();
        let peer = TwinPeer::new();
        crate::api::dispatch_synced(&mut conn, "create_project", &serde_json::json!({ "name": "cxmail", "kind": "area" })).unwrap();
        crate::db::tags::upsert(&conn, "legal", None).unwrap();
        reconcile(&mut conn, &peer).unwrap();
        for c in [&conn, &*peer.db.borrow()] {
            c.execute("UPDATE projects SET created_at = '2026-01-01 00:00:00'", []).unwrap();
            c.execute("UPDATE tags SET created_at = '2026-01-01 00:00:00'", []).unwrap();
        }

        crate::api::dispatch_synced(&mut conn, "set_project_appearance", &serde_json::json!({ "name": "cxmail", "color": null, "icon": "lucide:mail" })).unwrap();
        crate::api::dispatch_synced(&mut conn, "set_tag_appearance", &serde_json::json!({ "name": "legal", "color": "purple", "icon": "lucide:scale" })).unwrap();
        reconcile(&mut conn, &peer).unwrap();

        for (who, c) in [("peer", &*peer.db.borrow()), ("local after the pull", &conn)] {
            let p: Option<String> = c.query_row("SELECT icon FROM projects WHERE name = 'cxmail'", [], |r| r.get(0)).unwrap();
            let t: (Option<String>, Option<String>) = c
                .query_row("SELECT color, icon FROM tags WHERE name = 'legal'", [], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap();
            assert_eq!(p.as_deref(), Some("lucide:mail"), "{who}: project icon");
            assert_eq!((t.0.as_deref(), t.1.as_deref()), (Some("purple"), Some("lucide:scale")), "{who}: tag");
        }
    }

    /// **An edited comment travels.** Comments are edited in place; before
    /// v28's `edited_at` the sweep saw only `created_at`, so the edit never
    /// left the Mac and the next pull restored the old text.
    #[test]
    fn an_edited_comment_reaches_the_peer_and_survives_the_pull() {
        let mut conn = local();
        let peer = TwinPeer::new();
        let task = tasks::create(&conn, &NewTask { title: "t".into(), ..Default::default() }).unwrap();
        let c = crate::db::comments::add(&conn, &task.id, "me", "first draft").unwrap();
        reconcile(&mut conn, &peer).unwrap();
        // Older than the sweep watermark, as a real comment would be.
        for db in [&conn, &*peer.db.borrow()] {
            db.execute("UPDATE task_comments SET created_at = '2026-01-01 00:00:00.000'", []).unwrap();
        }

        crate::db::comments::edit(&conn, &c.id, "second draft").unwrap();
        reconcile(&mut conn, &peer).unwrap();

        let body = |db: &Connection| -> String {
            db.query_row("SELECT body FROM task_comments WHERE id = ?1", [&c.id], |r| r.get(0)).unwrap()
        };
        assert_eq!(body(&peer.db.borrow()), "second draft", "the peer has the edit");
        assert_eq!(body(&conn), "second draft", "and the pull did not revert it");
    }

    /// A pulled change is not a local one: the next sweep must not push the
    /// peer's own rows back at it, or it reverts whatever changed upstream in
    /// between.
    #[test]
    fn a_pull_does_not_count_as_a_local_change() {
        let mut conn = local();
        let peer = TwinPeer::new();
        crate::api::dispatch_synced(&mut conn, "create_project", &serde_json::json!({ "name": "products", "kind": "area" })).unwrap();
        reconcile(&mut conn, &peer).unwrap();

        // An edit made on the peer (the phone, say) arrives by pull...
        crate::db::projects::set_kind(&peer.db.borrow(), "products", crate::db::projects::ProjectKind::Project).unwrap();
        reconcile(&mut conn, &peer).unwrap();

        // ...and the sweep after it finds nothing of ours to send.
        let tx = conn.transaction().unwrap();
        assert_eq!(sweep::run(&tx).unwrap(), 0);
    }

    /// A write the MCP made — never through the verb table — still reaches the
    /// peer, because the sweep runs first.
    #[test]
    fn a_write_that_bypassed_the_verb_table_reaches_the_peer() {
        let mut conn = local();
        let peer = TwinPeer::new();
        // Exactly what cxtasks-mcp does.
        tasks::create(&conn, &NewTask { title: "filed by the mcp".into(), ..Default::default() }).unwrap();
        assert_eq!(outbox::count(&conn).unwrap(), 0, "nothing recorded it");

        reconcile(&mut conn, &peer).unwrap();

        let landed: i64 = peer
            .db
            .borrow()
            .query_row("SELECT COUNT(*) FROM tasks WHERE title = 'filed by the mcp'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(landed, 1, "the sweep is what makes the MCP's writes travel");
    }
}
