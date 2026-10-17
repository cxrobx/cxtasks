//! The two routes that matter: the handshake, and the verb table over HTTP.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth;
use crate::ServerState;

/// What a client learns before it trusts anything else.
///
/// Auth-optional, and it answers the question a failed connection cannot:
/// "is sync turned off here, or am I simply not paired?" Those need different
/// words on screen — one is an instruction, the other is an error — and a
/// client that cannot tell them apart says the wrong one.
#[derive(Serialize)]
pub struct Health {
    pub ok: bool,
    pub host: String,
    pub schema_version: i32,
    /// Bumped on any breaking change to a request or response shape. Two
    /// separately deployed artifacts WILL skew; this turns that into a refusal
    /// that names both numbers instead of fields silently not arriving.
    pub protocol_version: u32,
    /// What this process thinks "today" means. A client comparing it against
    /// its own can explain a board that looks a few hours out of step, which is
    /// otherwise an unfalsifiable complaint.
    pub tz: String,
}

pub async fn health(State(state): State<Arc<ServerState>>) -> Json<Health> {
    Json(Health {
        ok: true,
        host: cxtasks_core::host::host_id(),
        schema_version: cxtasks_core::db::schema::CURRENT_VERSION,
        protocol_version: cxtasks_core::db::schema::SYNC_PROTOCOL_VERSION,
        tz: state.config.config_tz().to_string(),
    })
}

#[derive(Deserialize)]
pub struct RpcRequest {
    pub cmd: String,
    #[serde(default)]
    pub args: Value,
}

/// One verb, executed against this host's database.
///
/// Errors come back as `{ "err": "..." }` with a status, never as a panic and
/// never as a bare 500 — a client showing "something went wrong" about a
/// refused capability is how a user concludes the app is broken when it is
/// working correctly.
pub async fn rpc(
    State(state): State<Arc<ServerState>>,
    req: Request<axum::body::Body>,
) -> axum::response::Response {
    let Some(scope) = auth::scope_for(
        &req,
        &state.config.token_full,
        state.config.token_board.as_deref(),
    ) else {
        return (auth::UNAUTHORIZED, Json(json!({ "err": "not authorized" }))).into_response();
    };

    let body = match axum::body::to_bytes(req.into_body(), 8 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "err": format!("could not read body: {e}") })),
            )
                .into_response()
        }
    };
    let call: RpcRequest = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "err": format!("bad request: {e}") })),
            )
                .into_response()
        }
    };

    // Refuse host-local verbs HERE rather than letting the table do it, so the
    // status code carries the meaning too. Executing one would act on THIS
    // machine's disk while the caller meant theirs — a `restore_checkpoint`
    // that resets the wrong working tree.
    if cxtasks_core::api::is_host_local(&call.cmd) {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "err": format!(
                    "'{}' only runs on the host that owns the files", call.cmd
                )
            })),
        )
            .into_response();
    }

    if !scope.may_call(&call.cmd) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                // Names only what is still refused. It used to say "or starts
                // an agent", which stopped being true when `Scope::Board`
                // gained the ability to queue one — leaving the server telling
                // the phone a rule the server no longer enforces.
                "err": format!(
                    "this token may not call '{}' — it changes what this account may spend",
                    call.cmd
                )
            })),
        )
            .into_response();
    }

    let result = {
        let mut conn = match state.db.lock() {
            Ok(c) => c,
            // A poisoned mutex means a previous request panicked mid-write. The
            // connection is still usable and the alternative is refusing every
            // request thereafter, so recover — same posture as the app.
            Err(poisoned) => {
                log::warn!("recovered a poisoned DB mutex");
                poisoned.into_inner()
            }
        };
        // Synced, so a server pointed at a LOCAL-role database records what it
        // does. On the canonical node the wrapper records nothing — that node
        // is what everyone else reconciles against.
        cxtasks_core::api::dispatch_synced(&mut conn, &call.cmd, &call.args)
    };

    match result {
        Ok(value) => Json(json!({ "ok": value })).into_response(),
        Err(e) => {
            let status = match &e {
                cxtasks_core::error::AppError::NotFound(_) => StatusCode::NOT_FOUND,
                cxtasks_core::error::AppError::Validation(_) => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({ "err": e.to_string() }))).into_response()
        }
    }
}
