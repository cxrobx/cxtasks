//! Pairing, pausing and unpairing, from the app.
//!
//! # Why these are host-local
//!
//! Every one of them configures THIS machine's relationship to a peer, so
//! running one through a server would configure the server instead — and worse,
//! `/rpc`'s board scope only refuses three verbs by name, so a phone's token
//! would otherwise be able to unpair the Mac it syncs with. They are in
//! `api::HOST_LOCAL`, which makes that a refusal that says why rather than a
//! surprise.
//!
//! The protocol itself is a different thing and stays on the server:
//! `/sync/{info,snapshot,push,lease,seed}` is how two nodes talk. This is how
//! one node is told who to talk to.

use serde::Serialize;
use tauri::State;

use crate::{AppState, LockExt};
use cxtasks_core::error::AppError;
use cxtasks_core::sync::{auto, leases, outbox, pair, reconcile, state};

/// Everything Settings needs to draw the panel, in one call.
#[derive(Debug, Serialize)]
pub struct SyncStatus {
    pub enabled: bool,
    /// Set but switched off — paused, rather than never configured. The two
    /// need different words: one is a state you chose, the other is a setup
    /// prompt.
    pub paired: bool,
    pub peer_id: Option<String>,
    pub peer_url: Option<String>,
    pub last_pull_at: Option<String>,
    /// Deletions waiting to be pushed. The only thing that cannot travel as
    /// state, which is why unpairing while this is non-zero loses work.
    pub pending: i64,
    pub lease_remaining: Option<i64>,
    pub next_short_id: Option<i64>,
    pub host: String,
    /// When a reconcile was last **attempted**, by the timer or by hand.
    ///
    /// The panel shows this beside nothing else, and that is the point: the
    /// bug this whole loop exists for was a `last_pull_at` four days old with
    /// no evidence anywhere that nobody had tried. A pull time alone cannot
    /// tell "we are trying and the NAS refuses" from "nothing is trying".
    pub last_attempt_at: Option<String>,
    /// Why the last attempt failed, or `None` when it worked.
    pub last_error: Option<String>,
    /// Seconds between automatic passes, so the panel can say the cadence
    /// rather than leave the user to guess whether one exists at all.
    pub auto_interval_seconds: u64,
}

#[tauri::command]
pub async fn sync_status(state: State<'_, AppState>) -> Result<SyncStatus, AppError> {
    let conn = state.db.safe_lock();
    let (last_attempt_at, last_error) = auto::last_attempt(&conn);
    Ok(SyncStatus {
        enabled: state::enabled(&conn),
        paired: state::peer_id(&conn).is_some(),
        peer_id: state::peer_id(&conn),
        peer_url: state::peer_url(&conn),
        last_pull_at: cxtasks_core::db::meta::get(&conn, state::KEY_LAST_PULL_AT)?,
        pending: outbox::count(&conn)?,
        lease_remaining: leases::remaining(&conn)?,
        next_short_id: cxtasks_core::db::meta::get(&conn, cxtasks_core::db::meta::KEY_NEXT_SHORT_ID)?
            .and_then(|v| v.parse().ok()),
        host: cxtasks_core::host::host_id(),
        last_attempt_at,
        last_error,
        auto_interval_seconds: auto::INTERVAL_SECONDS,
    })
}

/// Pair with a node. The refusals live in `sync::pair` — this is transport.
#[tauri::command]
pub async fn sync_pair(
    state: State<'_, AppState>,
    url: String,
    token: String,
) -> Result<String, AppError> {
    let url = url.trim().trim_end_matches('/').to_string();
    let url = if url.contains("://") { url } else { format!("http://{url}") };
    let peer = cxtasks_core::sync::http::HttpPeer::new(&url, token.trim());

    let mut conn = state.db.safe_lock();
    let outcome = pair::pair(&mut conn, &url, &peer, Some(token.trim()))?;
    Ok(match outcome {
        pair::Paired::Seeded { peer, lease } => format!(
            "Paired with {peer}. It was empty, so it now holds a copy of this database. \
             T-numbers T{}–T{} are reserved for this Mac.",
            lease.lo, lease.hi
        ),
        pair::Paired::Rejoined { peer, lease } => format!(
            "Rejoined {peer} — it was already seeded from this Mac. \
             T-numbers T{}–T{} reserved.",
            lease.lo, lease.hi
        ),
        pair::Paired::Joined { peer, lease } => format!(
            "Joined {peer}. This database was empty, so it takes that node's contents. \
             T-numbers T{}–T{} reserved.",
            lease.lo, lease.hi
        ),
    })
}

/// Push, then pull. Returns what moved, so the panel can say something true.
///
/// The same `sync::auto::now` the five-minute loop and `--sync-now` call, which
/// is what keeps the button honest: a manual sync that took a different path
/// from the automatic one would be the one thing you reach for when the
/// automatic one is misbehaving, and it would be testing something else.
#[tauri::command]
pub async fn sync_now(state: State<'_, AppState>) -> Result<reconcile::ReconcileReport, AppError> {
    let mut conn = state.db.safe_lock();
    auto::now(&mut conn)
}

/// Stop talking to the peer, and keep everything.
#[tauri::command]
pub async fn sync_set_enabled(state: State<'_, AppState>, enabled: bool) -> Result<(), AppError> {
    let conn = state.db.safe_lock();
    if enabled && state::peer_id(&conn).is_none() {
        return Err(AppError::Validation(
            "there is no node to resume — pair with one first".into(),
        ));
    }
    state::set_enabled(&conn, enabled)
}

/// Forget the peer entirely.
///
/// Refuses while deletions are still queued rather than discarding them: a task
/// destroyed here and never pushed comes back the next time these two databases
/// meet, which is a resurrection nobody would connect to having unpaired.
#[tauri::command]
pub async fn sync_unpair(state: State<'_, AppState>, force: bool) -> Result<(), AppError> {
    let conn = state.db.safe_lock();
    let pending = outbox::count(&conn)?;
    if pending > 0 && !force {
        return Err(AppError::Validation(format!(
            "{pending} deletion(s) have not reached the node yet. Sync once first — \
             unpairing now means anything deleted here comes back."
        )));
    }
    state::unpair(&conn)
}
