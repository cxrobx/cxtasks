//! Deciding whether two databases may be joined.
//!
//! # The refusal is the feature
//!
//! Merging two independently-grown CXTasks databases is a data-loss generator,
//! and not a subtle one: both issued T-numbers from 1, so a merge either
//! collides — two tasks answering to "T42" — or renumbers, which breaks every
//! reference already written into a commit message, a note or this repo's own
//! history. Invariant 14 says a T-number is permanent, and there is no repair
//! afterwards, only a choice between two kinds of wrong.
//!
//! So the interesting path here is the one that says no. Recovery from "I
//! paired the wrong machine" is wipe and re-seed, thirty seconds. Recovery from
//! a silent merge is nothing.

use rusqlite::Connection;

use crate::error::AppError;
use crate::sync::leases::{self, Lease};
use crate::sync::snapshot::Snapshot;
use crate::sync::state;

/// What a peer says about itself. Mirrors the server's `/sync/info`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    pub host: String,
    pub schema_version: i32,
    pub protocol_version: u32,
    pub canonical: bool,
    /// Which host's data this node was seeded from. `None` means it has never
    /// been paired with anybody.
    pub seeded_from: Option<String>,
}

/// The transport, as a trait, so every decision below is testable without a
/// network — and so the runner and the app share one implementation of the
/// protocol rather than one each.
pub trait Peer {
    fn info(&self) -> Result<PeerInfo, AppError>;
    /// Hand the peer this database, wholesale. Used once, at adoption.
    fn seed(&self, snapshot: &Snapshot, from_host: &str) -> Result<(), AppError>;
    fn request_lease(&self, host: &str, size: i64) -> Result<Lease, AppError>;
    /// The peer's whole database, for a pull.
    fn snapshot(&self) -> Result<crate::sync::snapshot::Snapshot, AppError>;
    /// Replay intents onto the peer. Returns what it accepted and what it did not.
    fn push(&self, intents: &[crate::sync::outbox::Intent]) -> Result<crate::sync::outbox::PushResult, AppError>;

    // ── attachment bytes ────────────────────────────────────────────────────
    //
    // On the trait rather than in a second one, because a peer that could
    // replicate rows but not bytes is a peer that syncs an image's name and
    // not the image. Splitting them would let that half-configured state
    // compile.

    /// Managed filenames the peer has a ROW for and no FILE for.
    ///
    /// The peer decides, rather than this end guessing from its own inventory.
    /// That is what makes "never store bytes for a row we do not have" true on
    /// the receiving side: it can only ever ask for what it already knows
    /// about. See `sync::blobs` for the ordering argument behind it.
    fn wanted_blobs(&self) -> Result<Vec<String>, AppError>;

    fn get_blob(&self, stored_name: &str) -> Result<Vec<u8>, AppError>;

    fn put_blob(&self, stored_name: &str, bytes: &[u8]) -> Result<(), AppError>;
}

/// What pairing did.
#[derive(Debug, Clone, PartialEq)]
pub enum Paired {
    /// The peer was empty and now holds this host's data.
    Seeded { peer: String, lease: Lease },
    /// The peer was already seeded from THIS host — re-pairing after an unpair,
    /// or pointing at a node that already knows us.
    Rejoined { peer: String, lease: Lease },
    /// This host had nothing, so it takes the peer's data instead.
    Joined { peer: String, lease: Lease },
}

/// Has this database got anything worth protecting?
///
/// Deliberately counts LIVE and trashed tasks alike. A trashed task is
/// recoverable for thirty days and is exactly the kind of thing somebody would
/// be upset to lose to a pairing they thought was empty.
fn has_local_data(conn: &Connection) -> Result<bool, AppError> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))?;
    Ok(n > 0)
}

/// Pair this database with `peer`.
///
/// Four outcomes, and only one of them is an error — but that one is the
/// reason the function exists.
/// `peer_token` is persisted on success — `None` for tests, and for a caller
/// that has already stored it.
pub fn pair(
    conn: &mut Connection,
    peer_url: &str,
    peer: &dyn Peer,
    peer_token: Option<&str>,
) -> Result<Paired, AppError> {
    let info = peer.info()?;
    let me = crate::host::host_id();

    // Version skew first. Everything below assumes both ends agree on what a
    // row IS, and whole-row sync cannot detect that they don't — columns simply
    // stop travelling. Refusing with both numbers turns a silent failure into
    // an answerable question.
    if info.schema_version != crate::db::schema::CURRENT_VERSION {
        return Err(AppError::Validation(format!(
            "that node is schema v{} and this one is v{}. Update the older one before pairing — \
             joining across versions makes columns silently stop travelling.",
            info.schema_version,
            crate::db::schema::CURRENT_VERSION
        )));
    }
    if info.protocol_version != crate::db::schema::SYNC_PROTOCOL_VERSION {
        return Err(AppError::Validation(format!(
            "that node speaks wire v{} and this one speaks v{}.",
            info.protocol_version,
            crate::db::schema::SYNC_PROTOCOL_VERSION
        )));
    }
    if !info.canonical {
        return Err(AppError::Validation(
            "that node is not running as the canonical role, so it has nothing to be paired WITH. \
             Set its role before pointing anything at it."
                .into(),
        ));
    }

    let local_data = has_local_data(conn)?;

    let outcome = match (info.seeded_from.as_deref(), local_data) {
        // Never paired with anyone: this host seeds it.
        (None, _) => {
            let snap = crate::sync::snapshot::take(conn)?;
            peer.seed(&snap, &me)?;
            Paired::Seeded { peer: info.host.clone(), lease: Lease { lo: 0, hi: 0 } }
        }

        // Already ours. Re-pairing after an unpair, or a second device of the
        // same lineage. Nothing is uploaded — the peer is ahead of us, or
        // level, and the pull will settle it.
        (Some(origin), _) if origin == me => Paired::Rejoined { peer: info.host.clone() , lease: Lease { lo: 0, hi: 0 } },

        // Someone else's node, and we have nothing to lose: join it.
        (Some(_), false) => Paired::Joined { peer: info.host.clone(), lease: Lease { lo: 0, hi: 0 } },

        // **Someone else's node, and we have data. This is the refusal.**
        (Some(origin), true) => {
            return Err(AppError::Validation(format!(
                "refusing to pair: that node was seeded from host {origin}, and this database \
                 was seeded from {me} and has tasks of its own.\n\n\
                 Joining them would merge two databases that both issued T-numbers from 1, so \
                 either two tasks end up answering to the same reference or every reference \
                 already written into a commit message stops resolving. Neither is repairable.\n\n\
                 If this is the wrong node, point at the right one. If you meant to replace \
                 that node's contents, wipe its data directory and pair again."
            )))
        }
    };

    // Only now, once the decision has been made and nothing has refused, does
    // any local state change. A half-paired database — peer recorded, lease
    // absent — is a state nothing else knows how to read.
    let lease = peer.request_lease(&me, leases::LEASE_SIZE)?;
    leases::adopt(conn, lease)?;
    state::set_peer(conn, peer_url, &info.host)?;
    state::set_enabled(conn, true)?;
    // Remember the peer's token so every later reconcile can authenticate
    // without being handed it again. It is a DIFFERENT secret from this node's
    // own, and the caller is the only one who has it.
    if let Some(token) = peer_token {
        state::set_peer_token(token)?;
    }

    Ok(match outcome {
        Paired::Seeded { peer, .. } => Paired::Seeded { peer, lease },
        Paired::Rejoined { peer, .. } => Paired::Rejoined { peer, lease },
        Paired::Joined { peer, .. } => Paired::Joined { peer, lease },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{self, NewTask};
    use std::cell::RefCell;

    struct FakePeer {
        info: PeerInfo,
        seeded: RefCell<Option<String>>,
        next_lease: Lease,
    }

    impl FakePeer {
        fn new(seeded_from: Option<&str>) -> Self {
            Self {
                info: PeerInfo {
                    host: "the-nas".into(),
                    schema_version: crate::db::schema::CURRENT_VERSION,
                    protocol_version: crate::db::schema::SYNC_PROTOCOL_VERSION,
                    canonical: true,
                    seeded_from: seeded_from.map(str::to_string),
                },
                seeded: RefCell::new(None),
                next_lease: Lease { lo: 1_000, hi: 1_499 },
            }
        }
    }

    impl Peer for FakePeer {
        fn info(&self) -> Result<PeerInfo, AppError> {
            Ok(self.info.clone())
        }
        fn seed(&self, _snapshot: &Snapshot, from_host: &str) -> Result<(), AppError> {
            *self.seeded.borrow_mut() = Some(from_host.to_string());
            Ok(())
        }
        fn request_lease(&self, _host: &str, _size: i64) -> Result<Lease, AppError> {
            Ok(self.next_lease)
        }
        fn snapshot(&self) -> Result<Snapshot, AppError> {
            Err(AppError::General("this fake does not serve snapshots".into()))
        }
        fn push(&self, _intents: &[crate::sync::outbox::Intent]) -> Result<crate::sync::outbox::PushResult, AppError> {
            Ok(Default::default())
        }
        // Pairing never moves bytes — adoption seeds ROWS, and the attachments
        // follow on the first reconcile. See `sync::blobs`.
        fn wanted_blobs(&self) -> Result<Vec<String>, AppError> {
            Ok(Vec::new())
        }
        fn get_blob(&self, _stored_name: &str) -> Result<Vec<u8>, AppError> {
            Err(AppError::General("this fake holds no bytes".into()))
        }
        fn put_blob(&self, _stored_name: &str, _bytes: &[u8]) -> Result<(), AppError> {
            Err(AppError::General("this fake holds no bytes".into()))
        }
    }

    fn with_data() -> Connection {
        let conn = test_conn();
        tasks::create(&conn, &NewTask { title: "mine".into(), ..Default::default() }).unwrap();
        conn
    }

    /// **The refusal, and the words it uses.**
    ///
    /// Both hosts are named, because "refused" without them leaves the user
    /// guessing which of two machines is the wrong one.
    #[test]
    fn pairing_with_a_node_seeded_by_someone_else_is_refused_loudly() {
        let mut conn = with_data();
        let peer = FakePeer::new(Some("some-other-macbook"));

        let err = pair(&mut conn, "http://nas:8787", &peer, None).unwrap_err().to_string();

        assert!(err.contains("some-other-macbook"), "the other host must be named: {err}");
        assert!(err.contains(&crate::host::host_id()), "and so must this one: {err}");
        assert!(err.contains("T-numbers from 1"), "and the reason: {err}");

        // And NOTHING was changed by the attempt.
        assert!(!state::enabled(&conn));
        assert_eq!(state::peer_id(&conn), None);
        assert_eq!(leases::ceiling(&conn).unwrap(), None);
    }

    /// An unseeded node is adopted, and learns whose data it holds.
    #[test]
    fn an_unseeded_node_is_seeded_from_here() {
        let mut conn = with_data();
        let peer = FakePeer::new(None);

        let outcome = pair(&mut conn, "http://nas:8787", &peer, None).unwrap();

        assert!(matches!(outcome, Paired::Seeded { .. }));
        assert_eq!(peer.seeded.borrow().as_deref(), Some(crate::host::host_id().as_str()));
        assert!(state::enabled(&conn));
        assert_eq!(state::peer_id(&conn).as_deref(), Some("the-nas"));
        assert_eq!(leases::ceiling(&conn).unwrap(), Some(1_499));
    }

    /// A node this host seeded before is rejoined without re-uploading.
    #[test]
    fn a_node_seeded_from_here_is_rejoined_without_re_uploading() {
        let mut conn = with_data();
        let peer = FakePeer::new(Some(&crate::host::host_id()));

        let outcome = pair(&mut conn, "http://nas:8787", &peer, None).unwrap();

        assert!(matches!(outcome, Paired::Rejoined { .. }));
        assert_eq!(*peer.seeded.borrow(), None, "rejoining must not overwrite the peer");
    }

    /// A device with nothing of its own joins somebody else's node happily.
    /// This is the phone, and the empty-Mac-after-a-reinstall case.
    #[test]
    fn a_host_with_no_data_may_join_a_node_seeded_by_another() {
        let mut conn = test_conn(); // no tasks
        let peer = FakePeer::new(Some("some-other-macbook"));

        let outcome = pair(&mut conn, "http://nas:8787", &peer, None).unwrap();

        assert!(matches!(outcome, Paired::Joined { .. }));
        assert!(state::enabled(&conn));
    }

    /// Even a TRASHED task counts as data worth refusing over.
    #[test]
    fn a_trashed_task_is_still_data_worth_protecting() {
        let mut conn = test_conn();
        let t = tasks::create(&conn, &NewTask { title: "binned".into(), ..Default::default() }).unwrap();
        tasks::trash(&conn, &t.id).unwrap();

        let peer = FakePeer::new(Some("some-other-macbook"));
        assert!(
            pair(&mut conn, "http://nas:8787", &peer, None).is_err(),
            "a task in the trash is recoverable for 30 days and must not be pairing collateral"
        );
    }

    /// A schema skew is refused with both numbers, before anything else is
    /// considered.
    #[test]
    fn a_schema_skew_is_refused_naming_both_versions() {
        let mut conn = with_data();
        let mut peer = FakePeer::new(None);
        peer.info.schema_version = crate::db::schema::CURRENT_VERSION - 1;

        let err = pair(&mut conn, "http://nas:8787", &peer, None).unwrap_err().to_string();
        assert!(err.contains(&format!("v{}", crate::db::schema::CURRENT_VERSION - 1)), "{err}");
        assert!(err.contains(&format!("v{}", crate::db::schema::CURRENT_VERSION)), "{err}");
        assert!(!state::enabled(&conn), "a refused pairing must change nothing");
    }

    /// A node that is not canonical has nothing to be paired with.
    #[test]
    fn pairing_with_a_non_canonical_node_is_refused() {
        let mut conn = with_data();
        let mut peer = FakePeer::new(None);
        peer.info.canonical = false;

        let err = pair(&mut conn, "http://nas:8787", &peer, None).unwrap_err().to_string();
        assert!(err.contains("canonical role"), "{err}");
    }
}
