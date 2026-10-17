//! `cxtasks-server` — the canonical role, over HTTP.
//!
//! Thin by design. It owns transport, auth and framing; every verb it executes
//! comes from `cxtasks_core::api::dispatch`. That is what Phase 1a's verb table
//! bought: this file cannot drift from the app, because it has no
//! implementation of its own to drift with.

mod attachments;
mod auth;
mod config;
mod rpc;
mod sync;

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::routing::{get, post};
use axum::Router;

pub struct ServerState {
    pub db: Mutex<rusqlite::Connection>,
    pub config: config::Config,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let config = config::Config::from_env()?;

    let db_path = cxtasks_core::paths::db_path();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let conn = rusqlite::Connection::open(&db_path)
        .with_context(|| format!("could not open {}", db_path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    cxtasks_core::db::schema::initialize(&conn).context("schema migration failed")?;

    // Applied on every start, so the container's configuration is the truth
    // rather than whatever the database was last told. Turning it OFF is
    // deliberate too: a node demoted back to local must stop claiming the role.
    cxtasks_core::sync::state::set_canonical(&conn, config.canonical)
        .context("could not record this node's role")?;

    log::info!(
        "cxtasks-server: host {} · db {} · schema v{} · wire v{} · TZ {} · role {}",
        cxtasks_core::host::host_id(),
        db_path.display(),
        cxtasks_core::db::schema::CURRENT_VERSION,
        cxtasks_core::db::schema::SYNC_PROTOCOL_VERSION,
        config.config_tz(),
        if config.canonical { "canonical" } else { "local" },
    );

    let bind = config.bind.clone();
    let state = Arc::new(ServerState { db: Mutex::new(conn), config });

    let app = Router::new()
        // Auth-OPTIONAL on purpose: a client has to be able to tell "sync is
        // turned off here" from "I am not paired", and asking it to
        // authenticate before it can learn that makes the two indistinguishable.
        .route("/health", get(rpc::health))
        .route("/rpc", post(rpc::rpc))
        // Replication. Full scope only — a board token edits tasks, it does not
        // overwrite the database or mint T-number ranges.
        .route("/sync/info", get(sync::info))
        .route("/sync/snapshot", get(sync::snapshot))
        .route("/sync/push", post(sync::push))
        .route("/sync/seed", post(sync::seed))
        .route("/sync/reconcile", post(sync::reconcile_now))
        .route("/sync/pair", post(sync::pair_with))
        .route("/sync/lease", post(sync::lease))
        // Attachment BYTES. Not `/sync/*` because they are not part of the
        // row protocol — a client fetching an image is doing something the
        // snapshot cannot express (`snapshot::to_json` refuses a BLOB on
        // purpose). And NOT the same scope rule: reading an image is reading a
        // task, so GET takes a board token while POST stays full. Decided,
        // with the argument, in `attachments.rs`.
        //
        // `missing` is declared BEFORE the parameterised route so it stays a
        // route rather than a filename: axum prefers a static segment, and
        // `is_safe_stored_name` would accept "missing" as a name.
        .route("/attachments/missing", get(attachments::missing))
        .route(
            "/attachments/{stored_name}",
            get(attachments::get).post(attachments::put),
        )
        // The snapshot is ~850 KB of JSON and compresses about 2.6x. Applied
        // to every route rather than just that one: it is negotiated, so a
        // client that does not ask still gets plain bytes.
        .layer(tower_http::compression::CompressionLayer::new().gzip(true))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("could not bind {bind}"))?;
    log::info!("listening on {bind}");
    axum::serve(listener, app).await?;
    Ok(())
}
