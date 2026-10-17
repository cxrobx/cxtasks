//! The three routes a peer needs: take a snapshot, accept a push, issue a lease.
//!
//! Every one of them requires the **full** scope. A board token can read and
//! edit tasks from a phone; it must not be able to overwrite the database
//! wholesale or mint T-number ranges, because those are not "editing a task"
//! however much they look like data.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{self, Scope};
use crate::ServerState;

/// Reject anything but a full-scope caller.
///
/// Shared with the attachment routes, which make exactly the same claim for
/// exactly the same reason — replicating the store is not editing a task.
pub(crate) fn require_full<B>(req: &Request<B>, state: &ServerState) -> Result<(), axum::response::Response> {
    match auth::scope_for(req, &state.config.token_full, state.config.token_board.as_deref()) {
        Some(Scope::Full) => Ok(()),
        Some(Scope::Board) => Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "err": "a board token may edit tasks but not replicate the database"
            })),
        )
            .into_response()),
        None => Err((auth::UNAUTHORIZED, Json(json!({ "err": "not authorized" }))).into_response()),
    }
}

#[derive(Deserialize)]
pub struct SnapshotQuery {
    /// Reserved. v1 always answers a full snapshot; see
    /// `cxtasks_core::sync::snapshot` for why a delta is not worth its
    /// correctness risk. Accepted now so the shape exists on the wire before
    /// anything depends on it.
    #[allow(dead_code)]
    pub since: Option<String>,
}

pub async fn snapshot(
    State(state): State<Arc<ServerState>>,
    Query(_q): Query<SnapshotQuery>,
    req: Request<axum::body::Body>,
) -> axum::response::Response {
    if let Err(refusal) = require_full(&req, &state) {
        return refusal;
    }
    let conn = match state.db.lock() {
        Ok(c) => c,
        Err(poisoned) => poisoned.into_inner(),
    };
    match cxtasks_core::sync::snapshot::take(&conn) {
        Ok(snap) => Json(json!({ "ok": snap })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "err": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct PushRequest {
    pub intents: Vec<cxtasks_core::sync::outbox::Intent>,
}

use cxtasks_core::sync::outbox::{PushFailure, PushResult};

pub async fn push(
    State(state): State<Arc<ServerState>>,
    req: Request<axum::body::Body>,
) -> axum::response::Response {
    if let Err(refusal) = require_full(&req, &state) {
        return refusal;
    }
    let body = match axum::body::to_bytes(req.into_body(), 32 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "err": format!("could not read body: {e}") })))
                .into_response()
        }
    };
    let call: PushRequest = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "err": format!("bad request: {e}") })))
                .into_response()
        }
    };

    let mut conn = match state.db.lock() {
        Ok(c) => c,
        Err(poisoned) => poisoned.into_inner(),
    };

    let mut accepted = Vec::new();
    let mut failed = Vec::new();
    // In order, and STOPPING at the first failure. Two edits to one task have
    // to arrive in the order they happened, so continuing past a failure would
    // apply a later intent on top of a state the earlier one never produced.
    // The sender keeps everything from the failure onward and retries.
    for intent in call.intents {
        match apply_one(&mut conn, &intent) {
            Ok(()) => accepted.push(intent.local_seq),
            Err(e) => {
                failed.push(PushFailure { local_seq: intent.local_seq, err: e.to_string() });
                break;
            }
        }
    }
    Json(json!({ "ok": PushResult { accepted, failed } })).into_response()
}

fn apply_one(
    conn: &mut rusqlite::Connection,
    intent: &cxtasks_core::sync::outbox::Intent,
) -> Result<(), cxtasks_core::error::AppError> {
    use cxtasks_core::error::AppError;

    if intent.cmd == cxtasks_core::sync::sweep::SYNC_UPSERT {
        let table = intent.args["table"]
            .as_str()
            .ok_or_else(|| AppError::Validation("sync_upsert needs a table".into()))?;
        let data: cxtasks_core::sync::snapshot::TableSnapshot =
            serde_json::from_value(json!({
                "columns": intent.args["columns"],
                "rows": intent.args["rows"],
            }))
            .map_err(|e| AppError::Validation(format!("malformed sync_upsert: {e}")))?;
        cxtasks_core::sync::apply::upsert_atomic(conn, table, &data)?;
        return Ok(());
    }

    // Everything else is a real verb, replayed through the same table the
    // sender used — so the compositions travel with it. A completed recurring
    // task materializes its next instance HERE too, which a row-level push
    // could never do.
    cxtasks_core::api::dispatch_synced(conn, &intent.cmd, &intent.args).map(|_| ())
}

#[derive(Deserialize)]
pub struct SeedRequest {
    pub snapshot: cxtasks_core::sync::snapshot::Snapshot,
    pub from_host: String,
}

/// Take a whole database, once, and remember whose it was.
///
/// Refuses a node that has already been seeded. Adoption is not an operation
/// you repeat: doing it twice against different origins is precisely the merge
/// of two databases that both counted T-numbers from 1, and the refusal on the
/// client cannot be the only one — a second client, or a retry after a partial
/// failure, would reach here without ever consulting it.
pub async fn seed(
    State(state): State<Arc<ServerState>>,
    req: Request<axum::body::Body>,
) -> axum::response::Response {
    if let Err(refusal) = require_full(&req, &state) {
        return refusal;
    }
    let body = match axum::body::to_bytes(req.into_body(), 128 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "err": format!("{e}") }))).into_response()
        }
    };
    let call: SeedRequest = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "err": format!("bad request: {e}") })))
                .into_response()
        }
    };
    let mut conn = match state.db.lock() {
        Ok(c) => c,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(existing) = cxtasks_core::sync::state::seeded_from(&conn) {
        if existing != call.from_host {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "err": format!(
                    "this node was already seeded from {existing}; refusing to re-seed it from                      {}. Wipe its data directory if you meant to replace it.", call.from_host
                )})),
            )
                .into_response();
        }
    }
    match cxtasks_core::sync::apply::apply_snapshot(&mut conn, &call.snapshot) {
        Ok(stats) => {
            // Adopt the source's T-number cursor, so leases carved here start
            // above every number the source has already issued.
            if let Err(e) = cxtasks_core::sync::state::set_seeded_from(&conn, &call.from_host) {
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "err": e.to_string() })))
                    .into_response();
            }
            Json(json!({ "ok": stats })).into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "err": e.to_string() }))).into_response(),
    }
}

#[derive(Deserialize)]
pub struct LeaseRequest {
    pub host: String,
    pub size: Option<i64>,
}

pub async fn lease(
    State(state): State<Arc<ServerState>>,
    req: Request<axum::body::Body>,
) -> axum::response::Response {
    if let Err(refusal) = require_full(&req, &state) {
        return refusal;
    }
    let body = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "err": format!("{e}") }))).into_response()
        }
    };
    let call: LeaseRequest = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "err": format!("bad request: {e}") })))
                .into_response()
        }
    };
    let mut conn = match state.db.lock() {
        Ok(c) => c,
        Err(poisoned) => poisoned.into_inner(),
    };
    let size = call.size.unwrap_or(cxtasks_core::sync::leases::LEASE_SIZE);
    match cxtasks_core::sync::leases::issue(&mut conn, &call.host, size) {
        Ok(l) => Json(json!({ "ok": l })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "err": e.to_string() }))).into_response(),
    }
}

#[derive(Deserialize)]
pub struct PairRequest {
    pub url: String,
    /// The PEER's token, not this node's. They are different secrets, and a
    /// node cannot guess another's.
    pub token: String,
}

/// Pair this node with a peer, through `sync::pair::pair`.
///
/// Deliberately a thin wrapper over that function and nothing else. Pairing is
/// the highest-risk operation in the system and its refusals — schema skew,
/// wire skew, a non-canonical peer, and above all a peer seeded by somebody
/// ELSE while this database holds data — are the reason it exists. Reproducing
/// any of that here would be reproducing the part that must not drift.
///
/// Exists so a cutover can be a script rather than a sequence of hand-typed
/// SQL, which is the version that goes wrong at 2am.
pub async fn pair_with(
    State(state): State<Arc<ServerState>>,
    req: Request<axum::body::Body>,
) -> axum::response::Response {
    if let Err(refusal) = require_full(&req, &state) {
        return refusal;
    }
    let body = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "err": format!("{e}") }))).into_response()
        }
    };
    let call: PairRequest = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "err": format!("bad request: {e}") })))
                .into_response()
        }
    };
    let url = call.url.trim().trim_end_matches('/').to_string();
    let url = if url.contains("://") { url } else { format!("http://{url}") };
    let peer = cxtasks_core::sync::http::HttpPeer::new(&url, call.token.trim());

    let mut conn = match state.db.lock() {
        Ok(c) => c,
        Err(poisoned) => poisoned.into_inner(),
    };
    match cxtasks_core::sync::pair::pair(&mut conn, &url, &peer, Some(call.token.trim())) {
        Ok(outcome) => Json(json!({ "ok": format!("{outcome:?}") })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "err": e.to_string() }))).into_response(),
    }
}

/// Drive one reconcile FROM this node toward its configured peer.
///
/// Exists so a local-role node can be reconciled without a desktop app in the
/// loop — the runner will call `sync::reconcile::reconcile` directly, and this
/// is the same call reachable over HTTP for tests and for a headless host.
pub async fn reconcile_now(
    State(state): State<Arc<ServerState>>,
    req: Request<axum::body::Body>,
) -> axum::response::Response {
    if let Err(refusal) = require_full(&req, &state) {
        return refusal;
    }
    let mut conn = match state.db.lock() {
        Ok(c) => c,
        Err(poisoned) => poisoned.into_inner(),
    };
    let Some(url) = cxtasks_core::sync::state::peer_url(&conn) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "err": "this node has no peer configured" })))
            .into_response();
    };
    // The PEER's token, not this node's. They are different secrets; using the
    // local one means the peer rejects before reading the body, and a large
    // push dies as "Broken pipe", which reads as a network fault.
    let token = match cxtasks_core::sync::state::peer_token() {
        Ok(t) => t,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "err": e.to_string() }))).into_response()
        }
    };
    let peer = cxtasks_core::sync::http::HttpPeer::new(&url, &token);
    match cxtasks_core::sync::reconcile::reconcile(&mut conn, &peer) {
        Ok(report) => Json(json!({ "ok": report })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "err": e.to_string() }))).into_response(),
    }
}

/// What a client learns before deciding whether to pair.
#[derive(Serialize)]
pub struct SyncInfo {
    pub host: String,
    pub schema_version: i32,
    pub protocol_version: u32,
    pub canonical: bool,
    /// Which host's data seeded this node, if any. **The pairing refusal is
    /// built on this**: adopting an already-seeded node from a different origin
    /// would merge two databases that both issued T-numbers from 1.
    pub seeded_from: Option<String>,
}

pub async fn info(
    State(state): State<Arc<ServerState>>,
    req: Request<axum::body::Body>,
) -> axum::response::Response {
    if let Err(refusal) = require_full(&req, &state) {
        return refusal;
    }
    let conn = match state.db.lock() {
        Ok(c) => c,
        Err(poisoned) => poisoned.into_inner(),
    };
    Json(json!({ "ok": SyncInfo {
        host: cxtasks_core::host::host_id(),
        schema_version: cxtasks_core::db::schema::CURRENT_VERSION,
        protocol_version: cxtasks_core::db::schema::SYNC_PROTOCOL_VERSION,
        canonical: cxtasks_core::sync::state::is_canonical(&conn),
        seeded_from: cxtasks_core::sync::state::seeded_from(&conn),
    }}))
    .into_response()
}
