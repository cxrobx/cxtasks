//! This host's pairing state, in `app_meta`.
//!
//! Every key here is refused by the snapshot serializer (`sync_*` by prefix),
//! because it describes a NODE rather than the data: copying it to a peer would
//! tell that peer it is paired with itself.

use rusqlite::Connection;

use crate::db::meta;
use crate::error::AppError;

pub const KEY_ENABLED: &str = "sync_enabled";
pub const KEY_PEER_URL: &str = "sync_peer_url";
/// The peer's `host_id`. What "am I still talking to the same machine" is
/// answered against.
pub const KEY_PEER_ID: &str = "sync_peer_id";
/// On a CANONICAL node: which host's data seeded it. The refusal at pairing
/// time compares against this.
pub const KEY_SEEDED_FROM: &str = "sync_seeded_from";
/// The highest `local_seq` this host has successfully pushed. The skew sweep's
/// watermark.
pub const KEY_PUSHED_THROUGH: &str = "sync_pushed_through";
pub const KEY_LAST_PULL_AT: &str = "sync_last_pull_at";
/// `canonical` or absent (meaning local). Refused by the snapshot serializer,
/// because copying it produces two canonical nodes.
pub const KEY_ROLE: &str = "role";

/// Does this node SERVE the truth rather than push toward it?
///
/// The canonical node keeps no outbox. A local host records its intents and
/// pushes them; the canonical applies what it is given and answers snapshots,
/// so intents recorded there would be drained by nobody and the table would
/// grow without bound. One role, one direction.
pub fn is_canonical(conn: &Connection) -> bool {
    matches!(meta::get(conn, KEY_ROLE).ok().flatten().as_deref(), Some("canonical"))
}

pub fn set_canonical(conn: &Connection, canonical: bool) -> Result<(), AppError> {
    if canonical {
        meta::set(conn, KEY_ROLE, "canonical")
    } else {
        conn.execute("DELETE FROM app_meta WHERE key = ?1", [KEY_ROLE])?;
        Ok(())
    }
}

/// Is sync switched on here?
///
/// Read on every dispatched verb, so it is one indexed `app_meta` lookup and
/// nothing more. **Absent means off**, which is what keeps a database that has
/// never seen sync on exactly the path it has always taken.
pub fn enabled(conn: &Connection) -> bool {
    matches!(meta::get(conn, KEY_ENABLED).ok().flatten().as_deref(), Some("1"))
}

/// Turn sync on or off. **Off keeps everything else** — the peer, the outbox,
/// the lease — because pause and unpair are different operations and only one
/// of them throws away the ability to reconcile later.
pub fn set_enabled(conn: &Connection, on: bool) -> Result<(), AppError> {
    meta::set(conn, KEY_ENABLED, if on { "1" } else { "0" })
}

pub fn peer_url(conn: &Connection) -> Option<String> {
    meta::get(conn, KEY_PEER_URL).ok().flatten().filter(|s| !s.is_empty())
}

pub fn peer_id(conn: &Connection) -> Option<String> {
    meta::get(conn, KEY_PEER_ID).ok().flatten().filter(|s| !s.is_empty())
}

pub fn set_peer(conn: &Connection, url: &str, id: &str) -> Result<(), AppError> {
    meta::set(conn, KEY_PEER_URL, url)?;
    meta::set(conn, KEY_PEER_ID, id)
}

pub fn seeded_from(conn: &Connection) -> Option<String> {
    meta::get(conn, KEY_SEEDED_FROM).ok().flatten().filter(|s| !s.is_empty())
}

pub fn set_seeded_from(conn: &Connection, host: &str) -> Result<(), AppError> {
    meta::set(conn, KEY_SEEDED_FROM, host)
}

pub fn pushed_through(conn: &Connection) -> i64 {
    meta::get(conn, KEY_PUSHED_THROUGH)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

pub fn set_pushed_through(conn: &Connection, seq: i64) -> Result<(), AppError> {
    meta::set(conn, KEY_PUSHED_THROUGH, &seq.to_string())
}

/// Where the PEER's bearer token lives.
///
/// A file, and specifically a file **outside the data directory**, for two
/// reasons that both bite. It must not go in `app_meta`: that table is copied
/// into every `.backup`, printed by every debugging `SELECT *`, and — now that
/// sync exists — the allowlist is the only thing standing between a secret and
/// every peer that pulls a snapshot. And it must not sit under the data root
/// either, because the root is what `.backup` and `CXTASKS_DATA_DIR` move
/// around wholesale.
///
/// Beside the server's own tokens, which is where somebody would look for it.
pub fn peer_token_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".config/cxtasks/peer-token")
}

/// The peer's token, or an error naming the file.
///
/// **This is not the same secret as this node's own token**, and conflating the
/// two is not a theoretical mistake: the first cut of `/sync/reconcile`
/// authenticated to the peer with the local server's token, and because the
/// peer rejects the request before reading the body, a large push died as
/// `Broken pipe (os error 32)` — which reads as a network fault and sends you
/// to the wrong layer entirely.
pub fn peer_token() -> Result<String, AppError> {
    let path = peer_token_path();
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        AppError::Validation(format!(
            "could not read the peer's token at {} ({e}). It is written when you pair; \
             if this node was paired some other way, put the PEER's full token there.",
            path.display()
        ))
    })?;
    let token = raw.trim().to_string();
    if token.is_empty() {
        return Err(AppError::Validation(format!("{} is empty", path.display())));
    }
    Ok(token)
}

/// Remember the peer's token, at 0600.
pub fn set_peer_token(token: &str) -> Result<(), AppError> {
    let path = peer_token_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, token.trim())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Forget the peer entirely. Unpair, not pause.
pub fn unpair(conn: &Connection) -> Result<(), AppError> {
    for key in [
        KEY_ENABLED,
        KEY_PEER_URL,
        KEY_PEER_ID,
        KEY_PUSHED_THROUGH,
        KEY_LAST_PULL_AT,
        KEY_ROLE,
        // The record of the last automatic attempt. It describes a
        // relationship that no longer exists, and a "last error" surviving an
        // unpair puts a red line under a node this host has stopped talking to.
        crate::sync::auto::KEY_LAST_ATTEMPT_AT,
        crate::sync::auto::KEY_LAST_ERROR,
    ] {
        conn.execute("DELETE FROM app_meta WHERE key = ?1", [key])?;
    }
    conn.execute("DELETE FROM sync_outbox", [])?;
    // **The peer's token file is deliberately NOT removed here.**
    //
    // It lives at a fixed path under `$HOME`, which no test can redirect —
    // `paths.rs` memoizes its root from a process-global env var precisely
    // because doing otherwise races the rest of the suite. So a `remove_file`
    // in this function is not scoped to the caller's database at all: an
    // ordinary `cargo test` run reaches out of its sandbox and deletes the
    // REAL credential belonging to a live pairing. That is not hypothetical.
    // It happened, and it presented an hour later as a reconcile failing with
    // "could not read the peer's token", long after anything connected it to
    // having run the tests.
    //
    // A token with no peer is inert — nothing reads it without
    // `sync_peer_url`, which this function has just cleared. Removing the file
    // is the operator-facing script's job (`scripts/sync-control.sh unpair`),
    // where deleting something under `$HOME` is what the user asked for.
    crate::sync::leases::clear(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;

    #[test]
    fn a_database_that_has_never_seen_sync_is_off() {
        assert!(!enabled(&test_conn()));
    }

    #[test]
    fn pausing_keeps_the_peer_and_the_outbox() {
        let conn = test_conn();
        set_peer(&conn, "http://node:8787", "peer-host").unwrap();
        set_enabled(&conn, true).unwrap();
        conn.execute("INSERT INTO sync_outbox (cmd, args_json, host) VALUES ('x','{}','h')", []).unwrap();

        set_enabled(&conn, false).unwrap();

        assert!(!enabled(&conn));
        assert_eq!(peer_id(&conn).as_deref(), Some("peer-host"), "pause must not forget the peer");
        let pending: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pending, 1, "pause must not discard un-pushed work");
    }

    /// **Going back to standalone leaves a database that is fully standalone.**
    ///
    /// This is the question worth being able to answer before pairing, not
    /// after: the Mac was never a thin cache — that was rejected precisely so
    /// this would be true — so "come back to local" has to mean the database
    /// behaves exactly as it did before, not that it merely stops syncing.
    ///
    /// Three things behave differently while paired, and all three must revert:
    /// the dispatch wrapper records intents, `claim_short_id` respects a lease
    /// ceiling, and `purge_expired` refuses to run.
    #[test]
    fn unpairing_restores_every_behaviour_that_pairing_changed() {
        let mut conn = test_conn();
        let before = crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask { title: "written before pairing".into(), ..Default::default() },
        )
        .unwrap();

        // Pair: peer, lease, sync on.
        set_peer(&conn, "http://nas:18870", "the-nas").unwrap();
        set_enabled(&conn, true).unwrap();
        crate::sync::leases::adopt(&conn, crate::sync::leases::Lease { lo: 100, hi: 101 }).unwrap();

        // While paired: deletes are recorded, the ceiling bites, purge refuses.
        let doomed = crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask { title: "doomed".into(), ..Default::default() },
        )
        .unwrap();
        crate::api::dispatch_synced(&mut conn, "destroy_task", &serde_json::json!({ "id": &doomed.id })).unwrap();
        assert!(crate::sync::outbox::count(&conn).unwrap() > 0);
        crate::db::meta::claim_short_id(&conn).unwrap();
        assert_eq!(crate::db::meta::claim_short_id(&conn).unwrap(), None, "the lease is spent");
        assert!(crate::db::tasks::purge_expired(&conn).is_err(), "a local node must not purge");

        unpair(&conn).unwrap();

        // Everything paired-ness changed is back.
        assert!(!enabled(&conn));
        assert_eq!(peer_id(&conn), None);
        assert_eq!(crate::sync::leases::ceiling(&conn).unwrap(), None, "no ceiling");
        assert_eq!(crate::sync::outbox::count(&conn).unwrap(), 0);

        assert!(
            crate::db::meta::claim_short_id(&conn).unwrap().is_some(),
            "T-numbers must flow freely again"
        );
        assert!(crate::db::tasks::purge_expired(&conn).is_ok(), "purging must work again");

        // A write records nothing, exactly as it did before any of this.
        let after = crate::api::dispatch_synced(
            &mut conn,
            "create_task",
            &serde_json::json!({ "task": { "title": "written after unpairing" } }),
        )
        .unwrap();
        assert_eq!(crate::sync::outbox::count(&conn).unwrap(), 0);

        // And the data that was here before pairing is still here.
        assert_eq!(
            crate::db::tasks::get(&conn, &before.id).unwrap().unwrap().title,
            "written before pairing"
        );
        assert!(after["short_id"].as_i64().is_some(), "and new tasks still get a T-number");
    }

    /// Unpair is the one that throws things away, and it throws away all of it.
    #[test]
    fn unpairing_forgets_the_peer_the_outbox_and_the_lease() {
        let conn = test_conn();
        set_peer(&conn, "http://node:8787", "peer-host").unwrap();
        set_enabled(&conn, true).unwrap();
        crate::sync::leases::adopt(&conn, crate::sync::leases::Lease { lo: 10, hi: 20 }).unwrap();
        conn.execute("INSERT INTO sync_outbox (cmd, args_json, host) VALUES ('x','{}','h')", []).unwrap();

        unpair(&conn).unwrap();

        assert!(!enabled(&conn));
        assert_eq!(peer_id(&conn), None);
        assert_eq!(crate::sync::leases::ceiling(&conn).unwrap(), None,
                   "a now-standalone host must not keep a ceiling it has no reason to respect");
        let pending: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(pending, 0);
    }
}
