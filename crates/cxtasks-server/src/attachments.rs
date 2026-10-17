//! The bytes behind an attachment row, over HTTP.
//!
//! Three routes, and the design argument for all of them lives in
//! `cxtasks_core::sync::blobs`. What is decided *here* is who may call them and
//! what a name is allowed to be.
//!
//! # Reading is board scope; writing is full scope
//!
//! The asymmetry is deliberate and is NOT an oversight to tidy up later.
//!
//! `auth::Scope` states the rule the split follows: **spending money and
//! starting an agent are the two things a lost device must not be able to do;
//! everything else on the board is recoverable.** A board token already reads
//! titles, bodies and comments — an image attached to one of those is the same
//! class of thing, and refusing it is not a tighter posture, it is a broken
//! image on the phone. So `GET /attachments/{name}` and
//! `GET /attachments/missing` accept any authenticated scope.
//!
//! `POST` does not, and for a different reason than the verbs
//! `Scope::may_call` refuses. It writes bytes into the master store, which is
//! the copy every other node treats as canonical (invariant 23) — so a lost
//! phone must not be able to put a file there, and nothing on iOS uploads
//! today anyway. A capability nobody uses costs nothing to withhold and would
//! cost a great deal to have granted.
//!
//! # A name is a filename, and nothing else
//!
//! `stored_name` arrives from the network and is joined onto a directory. That
//! is a path traversal waiting to happen, in a `POST` handler, so the check is
//! an **allowlist** (`cxtasks_core::attachments::is_safe_stored_name`) rather
//! than a hunt for `..`: `%2e%2e`, a backslash, a NUL and a bare slash are four
//! ways to write the same attack and a denylist has to know all of them. The
//! join itself only happens inside `managed_path`, which applies that check —
//! so there is no route to the filesystem that skips it.
//!
//! # Bytes are never stored for a row this node does not have
//!
//! `POST` refuses an unknown `stored_name`. That is what makes this node's own
//! orphan-file pruning sound: an untracked file here can only be a partial
//! write or a row that was deleted, never bytes waiting for a row to arrive.
//! The client cooperates — it uploads only what `GET /attachments/missing`
//! asked for — but cooperation is not the guarantee; this refusal is.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, Request, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::auth::{self, Scope};
use crate::ServerState;

/// Any authenticated caller may READ bytes.
///
/// The scope is still resolved rather than ignored: this is "you are one of
/// the two tokens", not "the route is open". `scope_for` is what makes an
/// absent or wrong bearer a 401 here exactly as it is everywhere else.
fn require_read<B>(req: &Request<B>, state: &ServerState) -> Result<Scope, axum::response::Response> {
    auth::scope_for(req, &state.config.token_full, state.config.token_board.as_deref())
        .ok_or_else(|| refuse(auth::UNAUTHORIZED, "not authorized".into()))
}

/// Only a full-scope caller may WRITE bytes into the store.
///
/// Its own gate rather than `sync::require_full`, because the sentence differs
/// and the sentence is the useful part: a board token being refused here is
/// not "you may not replicate the database", it is "you may read this image
/// and not replace it".
fn require_write<B>(req: &Request<B>, state: &ServerState) -> Result<(), axum::response::Response> {
    match require_read(req, state)? {
        Scope::Full => Ok(()),
        Scope::Board => Err(refuse(
            StatusCode::FORBIDDEN,
            "a board token may read an attachment but not write bytes into the store \
             every other node treats as canonical"
                .into(),
        )),
    }
}

/// Managed filenames this node has a row for and no file for.
///
/// The peer asks *us* what we are missing rather than deciding from its own
/// inventory, because only this end knows which rows it has. See
/// `sync::blobs`.
pub async fn missing(
    State(state): State<Arc<ServerState>>,
    req: Request<axum::body::Body>,
) -> axum::response::Response {
    if let Err(refusal) = require_read(&req, &state) {
        return refusal;
    }
    let conn = match state.db.lock() {
        Ok(c) => c,
        Err(poisoned) => poisoned.into_inner(),
    };
    match cxtasks_core::sync::blobs::wanted_here(&conn) {
        Ok(names) => Json(json!({ "ok": names })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "err": e.to_string() })),
        )
            .into_response(),
    }
}

/// Serve one managed file.
///
/// The only route in this server whose success is not the `{ok|err}` envelope —
/// wrapping 25 MB of PNG in JSON would mean base64, which is a third more bytes
/// and a second encoding to get wrong. A **refusal** still answers the
/// envelope, so the peer's sentence survives either way, and the client tells
/// the two apart by status.
pub async fn get(
    State(state): State<Arc<ServerState>>,
    Path(stored_name): Path<String>,
    req: Request<axum::body::Body>,
) -> axum::response::Response {
    if let Err(refusal) = require_read(&req, &state) {
        return refusal;
    }
    let record = {
        let conn = match state.db.lock() {
            Ok(c) => c,
            Err(poisoned) => poisoned.into_inner(),
        };
        match cxtasks_core::db::attachments::by_stored_name(&conn, &stored_name) {
            Ok(Some(record)) => record,
            Ok(None) => return not_found(&stored_name),
            Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        }
    };

    match cxtasks_core::attachments::read_bytes(&stored_name) {
        Ok(bytes) => {
            // The stored MIME type, or a type that promises nothing. A header
            // value built from a database string has to be validated, not
            // assumed — an invalid one would make the whole response fail to
            // build, which reads as "the peer is down".
            let content_type = HeaderValue::from_str(&record.mime_type)
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
            ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
        }
        // A row whose file this node does not hold. Ordinary rather than
        // exceptional — it is exactly the state `missing` reports — and the
        // caller retries on its next pass.
        Err(e) => refuse(StatusCode::NOT_FOUND, e.to_string()),
    }
}

/// Accept one managed file.
pub async fn put(
    State(state): State<Arc<ServerState>>,
    Path(stored_name): Path<String>,
    req: Request<axum::body::Body>,
) -> axum::response::Response {
    if let Err(refusal) = require_write(&req, &state) {
        return refusal;
    }
    // The row is fetched BEFORE the body is read, so an upload for a row this
    // node does not have costs nothing and cannot be a way to make it allocate.
    let record = {
        let conn = match state.db.lock() {
            Ok(c) => c,
            Err(poisoned) => poisoned.into_inner(),
        };
        match cxtasks_core::db::attachments::by_stored_name(&conn, &stored_name) {
            Ok(Some(record)) => record,
            Ok(None) => return not_found(&stored_name),
            Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        }
    };

    // The importer's cap, shared rather than re-typed: a server that accepted
    // more would be accepting bytes no row could ever have described, and one
    // that accepted less would refuse an attachment the app had already stored.
    let cap = cxtasks_core::attachments::MAX_IMAGE_BYTES as usize;
    let bytes = match axum::body::to_bytes(req.into_body(), cap).await {
        Ok(b) => b,
        Err(e) => {
            return refuse(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("could not read the body (limit {cap} bytes): {e}"),
            )
        }
    };

    match cxtasks_core::attachments::store_bytes(&stored_name, &bytes, record.sha256.as_deref()) {
        Ok(()) => Json(json!({ "ok": { "stored_name": stored_name, "bytes": bytes.len() } }))
            .into_response(),
        // A digest mismatch is a `Validation` error, and it says both digests.
        Err(e) => refuse(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

fn not_found(stored_name: &str) -> axum::response::Response {
    refuse(
        StatusCode::NOT_FOUND,
        format!(
            "no attachment row names {stored_name} on this node — \
             bytes are never stored for a row that does not exist"
        ),
    )
}

fn refuse(status: StatusCode, err: String) -> axum::response::Response {
    (status, Json(json!({ "err": err }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::sync::Mutex;

    fn state() -> ServerState {
        ServerState {
            db: Mutex::new(rusqlite::Connection::open_in_memory().unwrap()),
            config: Config {
                bind: "127.0.0.1:0".into(),
                token_full: "f".repeat(64),
                token_board: Some("b".repeat(64)),
                tz: "UTC".into(),
                canonical: true,
            },
        }
    }

    fn presenting(token: Option<&str>) -> Request<()> {
        let mut builder = Request::builder();
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(()).unwrap()
    }

    fn status(refusal: Result<(), axum::response::Response>) -> Option<StatusCode> {
        refusal.err().map(|r| r.status())
    }

    /// **The split, asserted in both directions.**
    ///
    /// The same shape as `auth::tests::a_board_token_cannot_spend_money_or_start_an_agent`
    /// and its "...but it must still be able to run the board" twin, because
    /// the failure modes are the two halves of one rule: a gate that denies too
    /// much is a gate nobody uses (the failure mode being Chris putting the
    /// full token on his phone), and one that denies too little hands a lost
    /// device the master store.
    #[test]
    fn a_board_token_may_read_an_attachment_and_may_not_write_one() {
        let state = state();
        let board = presenting(Some(&state.config.token_board.clone().unwrap()));

        assert!(
            require_read(&board, &state).is_ok(),
            "a board token already reads titles, bodies and comments — an image \
             attached to one of those is the same class of thing, and refusing \
             it is a broken image on the phone, not a tighter posture"
        );
        assert_eq!(
            status(require_write(&board, &state)),
            Some(StatusCode::FORBIDDEN),
            "writing bytes into the store every other node treats as canonical \
             is not something a lost phone may do"
        );
    }

    /// Full scope does both, or the test above passes by refusing everything.
    #[test]
    fn a_full_token_may_both_read_and_write() {
        let state = state();
        let full = presenting(Some(&state.config.token_full.clone()));

        assert!(require_read(&full, &state).is_ok());
        assert!(require_write(&full, &state).is_ok());
    }

    /// Widening READ to the board scope must not widen it to NO scope. The
    /// route is "either of the two tokens", never "open".
    #[test]
    fn no_token_reads_nothing() {
        let state = state();
        for req in [presenting(None), presenting(Some("wrong"))] {
            assert_eq!(status(require_read(&req, &state).map(|_| ())), Some(StatusCode::UNAUTHORIZED));
            assert_eq!(status(require_write(&req, &state)), Some(StatusCode::UNAUTHORIZED));
        }
    }

    /// A node with no board token configured must not let the board VALUE
    /// become a second key — the same property `auth`'s own test pins one
    /// layer down, re-checked here because this gate resolves the scope itself.
    #[test]
    fn an_unconfigured_board_token_is_not_a_skeleton_key() {
        let mut state = state();
        let board_value = state.config.token_board.take().unwrap();

        assert_eq!(
            status(require_read(&presenting(Some(&board_value)), &state).map(|_| ())),
            Some(StatusCode::UNAUTHORIZED)
        );
    }
}
