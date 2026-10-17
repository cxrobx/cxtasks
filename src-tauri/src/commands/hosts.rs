//! The host registry, from the app.
//!
//! Thin wrappers over `api::dispatch`, like everything in this directory. What
//! is worth saying is what these deliberately do NOT do: none of them create a
//! host row. A row means "a machine that runs tasks", and the only process
//! entitled to write one is the runner — which does so on every tick, on the
//! machine it is describing. An app that registered on the user's behalf would
//! put a row in the registry for every machine that ever opened the database,
//! including ones with no runner installed, each claiming to be able to do
//! everything and draining nothing.

use serde_json::json;
use tauri::State;

use crate::{AppState, LockExt};
use cxtasks_core::api;
use cxtasks_core::db::hosts::HostPolicy;
use cxtasks_core::error::AppError;

#[tauri::command]
pub async fn list_hosts(state: State<'_, AppState>) -> Result<serde_json::Value, AppError> {
    let mut conn = state.db.safe_lock();
    api::dispatch(&mut conn, "list_hosts", &json!({}))
}

/// Change what a host is ALLOWED to do. Never what it reported about itself —
/// there is no way to hand-edit `executors_ready`, because a settable "Claude
/// is installed" checkbox is a lie waiting to be believed by the router.
#[tauri::command]
pub async fn set_host_policy(
    state: State<'_, AppState>,
    host_id: String,
    policy: HostPolicy,
) -> Result<(), AppError> {
    let mut conn = state.db.safe_lock();
    api::dispatch_synced(
        &mut conn,
        "set_host_policy",
        &json!({ "host_id": host_id, "policy": policy }),
    )?;
    Ok(())
}

/// Where this machine pushes mirrors for a given host — an ssh destination
/// plus a base path, e.g. `nas-ts:/volume2/cxtasks/mirrors`. Clearing it stops
/// mirroring to that host, which is the supported way to take one out of
/// rotation for repo work without disabling it entirely.
#[tauri::command]
pub async fn set_host_mirror_remote(
    state: State<'_, AppState>,
    host_id: String,
    remote: Option<String>,
) -> Result<(), AppError> {
    let mut conn = state.db.safe_lock();
    api::dispatch_synced(
        &mut conn,
        "set_host_mirror_remote",
        &json!({ "host_id": host_id, "remote": remote }),
    )?;
    Ok(())
}

/// Forget a host.
///
/// Not a soft delete: a host that is gone should stop being offered work
/// immediately, and every fact on the row is either policy (re-declarable) or
/// observation (re-observed on its next start). Runs it executed keep their
/// `task_runs.host` string, which is the history this row was not carrying.
#[tauri::command]
pub async fn remove_host(state: State<'_, AppState>, host_id: String) -> Result<bool, AppError> {
    let mut conn = state.db.safe_lock();
    let out = api::dispatch_synced(&mut conn, "remove_host", &json!({ "host_id": host_id }))?;
    Ok(out.as_bool().unwrap_or(false))
}

/// "4 runs finished on the NAS while you were away."
///
/// The gap this closes predates multi-host: if the app was closed when a run
/// finished, it learned nothing on next launch. With another machine executing
/// it stops being an annoyance and becomes the normal case.
#[tauri::command]
pub async fn runs_finished_elsewhere(
    state: State<'_, AppState>,
    since: String,
    limit: Option<u32>,
) -> Result<serde_json::Value, AppError> {
    let mut conn = state.db.safe_lock();
    api::dispatch(
        &mut conn,
        "runs_finished_elsewhere",
        &json!({ "since": since, "limit": limit.unwrap_or(20) }),
    )
}
