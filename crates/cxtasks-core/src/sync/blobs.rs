//! The bytes behind an attachment row.
//!
//! # Why this is not just another synced table
//!
//! Everything else in this module travels as JSON: a row is a handful of
//! strings and the whole database gzips to a few hundred KB. An attachment is
//! up to 25 MB of binary, and `snapshot::to_json` **refuses a BLOB on purpose**
//! rather than base64-ing one — "a silent lossy encoding is exactly the kind of
//! thing that works in testing and corrupts an attachment in production". So
//! the bytes need a channel of their own, and this is it.
//!
//! # Where bytes are canonical
//!
//! **On the canonical node.** A local node's `attachments/` directory is a
//! CACHE: every file in it can be re-fetched, and deleting the whole directory
//! on a paired host costs nothing but a reconcile. Three things make that claim
//! true rather than aspirational:
//!
//! * `stored_name` is a UUID minted once and never reused, so it addresses one
//!   immutable byte sequence for the life of the row;
//! * `task_attachments.sha256` (v22) is the CONTENT address, so a cached file
//!   can be checked rather than assumed;
//! * [`crate::attachments::store_bytes`] verifies before it writes and writes
//!   atomically, so a file that exists is a file that is complete.
//!
//! Without the third, "the file is present" would eventually mean "a truncated
//! download is present", permanently, because nothing would ever look again.
//!
//! # How a missing file is fetched
//!
//! At reconcile, and only at reconcile. [`reconcile_bytes`] asks the peer what
//! it is missing, sends those, then fetches everything this node has a row for
//! and no file for. There is deliberately **no lazy per-render fetch**: the app
//! renders attachments through scoped `asset:` URLs that Rust never sees, so a
//! fetch hung off a read would have to be hung off `list_task_attachments` —
//! putting a network round trip, and a hang when the peer is down, on a read
//! verb. The cost is that a newly pulled image appears at the next reconcile
//! rather than instantly; the alternative is a task detail pane that blocks on
//! a NAS.
//!
//! # Rows first, then bytes
//!
//! [`crate::sync::reconcile::reconcile`] pushes rows before it moves bytes, and
//! the peer only ever accepts bytes for a row it already holds. That ordering
//! is chosen for its failure mode. Reversed, a crash between "bytes uploaded"
//! and "row pushed" leaves the peer holding a file nothing claims — which its
//! own pruning would collect, destroying the only copy that ever left this
//! machine. This way the window holds a row with no bytes instead: a missing
//! image, visible, and refilled by the next reconcile. Staying behind is
//! recoverable; losing the bytes is not — the same trade the push-before-pull
//! rule makes one layer up.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::attachments;
use crate::db;
use crate::error::AppError;
use crate::paths;
use crate::sync::pair::Peer;

/// How many files move per direction per reconcile.
///
/// Bounded for `reconcile::PUSH_BATCH`'s reason: a host coming back after a
/// long gap must not build one enormous transfer that times out and is then
/// rebuilt identically forever. The next reconcile takes the next batch.
pub const BLOB_BATCH: usize = 50;

/// ...and how many bytes, because a count alone is not a bound.
///
/// Fifty files is a small number and 50 × 25 MB is not. The budget is what
/// makes "one reconcile" a predictable amount of work regardless of what is in
/// the queue.
pub const BLOB_BUDGET_BYTES: u64 = 64 * 1024 * 1024;

/// What a byte-transfer pass did.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BlobReport {
    pub uploaded: usize,
    pub downloaded: usize,
    /// Never dropped, and never fatal — see [`reconcile_bytes`].
    pub failed: Vec<BlobFailure>,
    /// A cap stopped the pass early. The next reconcile continues.
    pub more: bool,
}

impl BlobReport {
    /// Did anything appear on screen? Only a download can.
    ///
    /// An upload changes nothing here, so counting it would redraw the UI every
    /// time this host had something to send — and a redraw discards
    /// in-progress edits (gotcha #24). Same rule as
    /// `ReconcileReport::changed`, for the same reason.
    pub fn changed(&self) -> bool {
        self.downloaded > 0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlobFailure {
    pub stored_name: String,
    pub err: String,
}

/// Managed filenames this database has a row for and no file for.
///
/// **Presence, not the digest.** Hashing every attachment on every reconcile
/// would put a full read of the whole store on a path that runs on a timer, to
/// re-check bytes that were already verified when they were written. The
/// verification lives at the two boundaries where it is cheap and where being
/// wrong is expensive instead: on the way in (`store_bytes` refuses a body that
/// does not match the row) and on the way out ([`reconcile_bytes`] will not
/// upload a file whose digest disagrees, because a bad upload poisons the
/// master copy rather than one cache).
pub fn wanted_here(conn: &Connection) -> Result<Vec<String>, AppError> {
    wanted_here_in(&paths::attachments_dir(), conn)
}

/// [`wanted_here`], against a store the caller names. See
/// [`crate::attachments::managed_path_in`] for why the directory is a
/// parameter.
pub fn wanted_here_in(dir: &Path, conn: &Connection) -> Result<Vec<String>, AppError> {
    let mut wanted = Vec::new();
    for record in db::attachments::all(conn)? {
        let Ok(path) = attachments::managed_path_in(dir, &record.stored_name) else {
            // A name this app could not have minted. Refusing to go looking for
            // it is the same refusal that keeps it off the filesystem at all.
            log::warn!("ignoring unmanageable attachment name {:?}", record.stored_name);
            continue;
        };
        if !path.is_file() {
            wanted.push(record.stored_name);
        }
    }
    Ok(wanted)
}

/// Move bytes in both directions, as far as one pass is allowed to.
///
/// # Failures are reported, never raised
///
/// One unreachable file must not fail a reconcile whose rows all landed. A peer
/// that has the row and lost the file, a name that arrived from a build with a
/// different idea of what is storable, a transfer that timed out — each is one
/// missing image, and each is retried on the next pass because the work is
/// re-derived from state rather than from a queue. Raising instead would mean a
/// single corrupt attachment could stop the whole database syncing, which is
/// the failure this design is least willing to have.
pub fn reconcile_bytes(conn: &Connection, peer: &dyn Peer) -> Result<BlobReport, AppError> {
    reconcile_bytes_in(&paths::attachments_dir(), conn, peer)
}

/// [`reconcile_bytes`], against a store the caller names.
pub fn reconcile_bytes_in(
    dir: &Path,
    conn: &Connection,
    peer: &dyn Peer,
) -> Result<BlobReport, AppError> {
    let mut report = BlobReport::default();
    let mut budget = BLOB_BUDGET_BYTES;

    // ── out ─────────────────────────────────────────────────────────────────
    // Uploads go first because they are the direction that can LOSE something:
    // a file only this machine holds is one power cut from being the only copy
    // that never existed anywhere else. A download is a cache refill and can
    // always wait for the next pass.
    let wanted_there = peer.wanted_blobs()?;
    if !wanted_there.is_empty() {
        let by_name = held_here(conn)?;
        for name in wanted_there {
            if report.uploaded >= BLOB_BATCH || budget == 0 {
                report.more = true;
                break;
            }
            let Some(record) = by_name.get(&name) else {
                // The peer wants bytes for a row we do not have. Not an error:
                // another node owns that attachment and will send it.
                continue;
            };
            if !attachments::have_bytes_for(dir, record) {
                // Either we never had it, or what we have is not what the row
                // describes. Sending it would overwrite the canonical copy with
                // something wrong, which is strictly worse than sending nothing.
                continue;
            }
            match attachments::read_bytes_in(dir, &name)
                .and_then(|bytes| peer.put_blob(&name, &bytes).map(|_| bytes.len()))
            {
                Ok(len) => {
                    report.uploaded += 1;
                    budget = budget.saturating_sub(len as u64);
                }
                Err(e) => report.failed.push(BlobFailure { stored_name: name, err: e.to_string() }),
            }
        }
    }

    // ── in ──────────────────────────────────────────────────────────────────
    let digests = digests_here(conn)?;
    for name in wanted_here_in(dir, conn)? {
        if report.downloaded >= BLOB_BATCH || budget == 0 {
            report.more = true;
            break;
        }
        let expected = digests.get(&name).and_then(|d| d.clone());
        match peer
            .get_blob(&name)
            .and_then(|bytes| {
                let len = bytes.len();
                attachments::store_bytes_in(dir, &name, &bytes, expected.as_deref()).map(|_| len)
            }) {
            Ok(len) => {
                report.downloaded += 1;
                budget = budget.saturating_sub(len as u64);
            }
            Err(e) => report.failed.push(BlobFailure { stored_name: name, err: e.to_string() }),
        }
    }

    Ok(report)
}

/// Every attachment row, by managed filename.
fn held_here(
    conn: &Connection,
) -> Result<std::collections::HashMap<String, db::attachments::AttachmentRecord>, AppError> {
    Ok(db::attachments::all(conn)?
        .into_iter()
        .map(|r| (r.stored_name.clone(), r))
        .collect())
}

/// The content address of each row, so a download can be checked against the
/// row it is for rather than against whatever the peer says it sent.
fn digests_here(
    conn: &Connection,
) -> Result<std::collections::HashMap<String, Option<String>>, AppError> {
    Ok(db::attachments::all(conn)?
        .into_iter()
        .map(|r| (r.stored_name, r.sha256))
        .collect())
}

/// Names this store holds files for, for a caller that wants the set rather
/// than the rows. Used by the server's inventory route's test.
#[allow(dead_code)]
pub(crate) fn files_present_in(dir: &Path) -> HashSet<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{self, NewTask};
    use crate::sync::leases;
    use crate::sync::outbox::{Intent, PushResult};
    use crate::sync::pair::PeerInfo;
    use crate::sync::snapshot::Snapshot;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A peer that is nothing but a byte store, which is exactly what a peer is
    /// from this module's side. Everything else on the trait is unreachable
    /// here and says so rather than pretending.
    struct BlobPeer {
        store: RefCell<HashMap<String, Vec<u8>>>,
        wants: RefCell<Vec<String>>,
        /// Serve corrupted bytes, to prove the digest check is load-bearing.
        corrupt: bool,
    }

    impl BlobPeer {
        fn new() -> Self {
            Self { store: RefCell::new(HashMap::new()), wants: RefCell::new(Vec::new()), corrupt: false }
        }
    }

    impl Peer for BlobPeer {
        fn info(&self) -> Result<PeerInfo, AppError> {
            unreachable!("blob transfer does not consult /sync/info")
        }
        fn seed(&self, _: &Snapshot, _: &str) -> Result<(), AppError> {
            unreachable!()
        }
        fn request_lease(&self, _: &str, _: i64) -> Result<leases::Lease, AppError> {
            unreachable!()
        }
        fn snapshot(&self) -> Result<Snapshot, AppError> {
            unreachable!()
        }
        fn push(&self, _: &[Intent]) -> Result<PushResult, AppError> {
            unreachable!()
        }
        fn wanted_blobs(&self) -> Result<Vec<String>, AppError> {
            Ok(self.wants.borrow().clone())
        }
        fn get_blob(&self, stored_name: &str) -> Result<Vec<u8>, AppError> {
            let store = self.store.borrow();
            let bytes = store
                .get(stored_name)
                .cloned()
                .ok_or_else(|| AppError::NotFound(format!("no bytes for {stored_name}")))?;
            Ok(if self.corrupt { b"not the file".to_vec() } else { bytes })
        }
        fn put_blob(&self, stored_name: &str, bytes: &[u8]) -> Result<(), AppError> {
            self.store.borrow_mut().insert(stored_name.to_string(), bytes.to_vec());
            Ok(())
        }
    }

    /// A throwaway attachment store. Never `paths::attachments_dir()` — the
    /// real one is the live Mac's, and a test must not write into it.
    fn temp_store() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cxtasks-blobs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A task with one attachment row whose bytes are `content`, and whose
    /// `sha256` is the honest digest of them.
    fn seeded(conn: &Connection, dir: Option<&Path>, content: &[u8]) -> String {
        let task = tasks::create(conn, &NewTask { title: "with an image".into(), ..Default::default() }).unwrap();
        let stored_name = format!("{}.png", uuid::Uuid::new_v4());
        let sha = attachments::digest_of(content);
        db::attachments::add(
            conn,
            &task.id,
            None,
            "shot.png",
            "image/png",
            &stored_name,
            Some(&sha),
        )
        .unwrap();
        if let Some(dir) = dir {
            std::fs::write(dir.join(&stored_name), content).unwrap();
        }
        stored_name
    }

    /// A row with no file is wanted; a row with one is not.
    #[test]
    fn a_row_without_its_file_is_what_gets_fetched() {
        let dir = temp_store();
        let conn = test_conn();
        let missing = seeded(&conn, None, b"absent");
        let present = seeded(&conn, Some(&dir), b"here");

        let wanted = wanted_here_in(&dir, &conn).unwrap();

        assert_eq!(wanted, vec![missing], "only the row with no file should be fetched");
        assert!(!wanted.contains(&present));
    }

    /// **The round trip**, in both directions, in one pass.
    #[test]
    fn a_pass_sends_what_the_peer_wants_and_fetches_what_is_missing_here() {
        let dir = temp_store();
        let conn = test_conn();
        let mine = seeded(&conn, Some(&dir), b"bytes only this machine has");
        let theirs = seeded(&conn, None, b"bytes only the peer has");

        let peer = BlobPeer::new();
        peer.wants.borrow_mut().push(mine.clone());
        peer.store.borrow_mut().insert(theirs.clone(), b"bytes only the peer has".to_vec());

        let report = reconcile_bytes_in(&dir, &conn, &peer).unwrap();

        assert_eq!(report.uploaded, 1, "the peer asked for it and we had it: {report:?}");
        assert_eq!(report.downloaded, 1, "we had the row and no file: {report:?}");
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(
            peer.store.borrow().get(&mine).map(Vec::as_slice),
            Some(&b"bytes only this machine has"[..]),
            "the uploaded bytes must be the ones on disk"
        );
        assert_eq!(
            std::fs::read(dir.join(&theirs)).unwrap(),
            b"bytes only the peer has",
            "the downloaded bytes must land under the managed name"
        );
        assert!(report.changed(), "an image appearing is a visible change");
    }

    /// **The digest is load-bearing.** A peer serving the wrong bytes must not
    /// produce a file, or a corrupt image becomes permanent — nothing looks at
    /// a file that is already present.
    #[test]
    fn bytes_that_do_not_match_the_row_are_refused_rather_than_stored() {
        let dir = temp_store();
        let conn = test_conn();
        let name = seeded(&conn, None, b"the real image");

        let mut peer = BlobPeer::new();
        peer.store.borrow_mut().insert(name.clone(), b"the real image".to_vec());
        peer.corrupt = true;

        let report = reconcile_bytes_in(&dir, &conn, &peer).unwrap();

        assert_eq!(report.downloaded, 0);
        assert_eq!(report.failed.len(), 1, "the refusal must be reported, not swallowed");
        assert!(report.failed[0].err.contains("hashes to"), "{:?}", report.failed);
        assert!(
            !dir.join(&name).exists(),
            "a body that failed verification must not exist on disk at all"
        );
        // ...and the row is still wanted, so the next pass tries again.
        assert_eq!(wanted_here_in(&dir, &conn).unwrap(), vec![name]);
    }

    /// A file whose bytes disagree with the row is not uploaded: a bad upload
    /// overwrites the canonical copy, which is worse than sending nothing.
    #[test]
    fn a_local_file_that_disagrees_with_its_row_is_never_sent() {
        let dir = temp_store();
        let conn = test_conn();
        let name = seeded(&conn, Some(&dir), b"the real image");
        std::fs::write(dir.join(&name), b"something else entirely").unwrap();

        let peer = BlobPeer::new();
        peer.wants.borrow_mut().push(name.clone());

        let report = reconcile_bytes_in(&dir, &conn, &peer).unwrap();

        assert_eq!(report.uploaded, 0, "a file that is not what the row describes must not travel");
        assert!(peer.store.borrow().is_empty());
    }

    /// A peer that has lost a file must not fail the whole pass.
    #[test]
    fn one_unreachable_file_does_not_sink_the_rest() {
        let dir = temp_store();
        let conn = test_conn();
        let gone = seeded(&conn, None, b"the peer lost this");
        let fine = seeded(&conn, None, b"but not this");

        let peer = BlobPeer::new();
        peer.store.borrow_mut().insert(fine.clone(), b"but not this".to_vec());

        let report = reconcile_bytes_in(&dir, &conn, &peer).unwrap();

        assert_eq!(report.downloaded, 1, "the reachable one still arrived: {report:?}");
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].stored_name, gone);
    }

    /// The peer asking for a row we do not have is ordinary, not an error:
    /// some other node owns that attachment.
    #[test]
    fn a_request_for_an_attachment_we_do_not_know_is_not_a_failure() {
        let dir = temp_store();
        let conn = test_conn();
        let peer = BlobPeer::new();
        peer.wants.borrow_mut().push("11111111-2222-3333-4444-555555555555.png".into());

        let report = reconcile_bytes_in(&dir, &conn, &peer).unwrap();

        assert_eq!(report.uploaded, 0);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
    }

    /// A settled pass reports no change, so the UI does not redraw on a timer.
    #[test]
    fn a_pass_with_nothing_to_move_reports_no_change() {
        let dir = temp_store();
        let conn = test_conn();
        seeded(&conn, Some(&dir), b"already here");

        let report = reconcile_bytes_in(&dir, &conn, &BlobPeer::new()).unwrap();

        assert_eq!(report, BlobReport::default());
        assert!(!report.changed());
    }

    /// The batch cap bounds a pass and says it was bounded, so the next one
    /// knows to keep going.
    #[test]
    fn a_pass_stops_at_the_batch_cap_and_says_so() {
        let dir = temp_store();
        let conn = test_conn();
        let peer = BlobPeer::new();
        for i in 0..(BLOB_BATCH + 3) {
            let content = format!("image {i}").into_bytes();
            let name = seeded(&conn, None, &content);
            peer.store.borrow_mut().insert(name, content);
        }

        let report = reconcile_bytes_in(&dir, &conn, &peer).unwrap();

        assert_eq!(report.downloaded, BLOB_BATCH);
        assert!(report.more, "a bounded pass must say it was bounded");
        assert_eq!(wanted_here_in(&dir, &conn).unwrap().len(), 3, "the rest waits for the next pass");
    }
}
