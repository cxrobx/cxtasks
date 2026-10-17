//! CXTasks MCP server — stdio transport for Claude CLI integration.
//!
//! Usage: `cxtasks-mcp` (argless stdio server — takes no flags)
//! Register with: `claude mcp add --scope user cxtasks /abs/path/to/cxtasks-mcp`
//!
//! ⚠️ Once registered, this binary is LIVE infrastructure, not build output.
//! `cargo clean` / `rm -rf target/` silently breaks the `cxtasks` MCP in every
//! Claude session until it is rebuilt (session rule #8). Rebuild with:
//!   source "$HOME/.cargo/env" && cargo build --release --bin cxtasks-mcp

use cxtasks_core::mcp::server::CxTasksMcp;
use rmcp::{transport::stdio, ServiceExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logging to stderr — stdout is the MCP wire protocol.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("cxtasks=info".parse()?),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    // `file_task`/`queue_background` route through
    // `dispatch::queue_for_background`, which used to ask launchd directly and
    // now asks the platform hook. Without this line the MCP route would keep
    // queueing correctly and stop warning that nothing is draining — gotcha
    // #18 says that warning is needed on BOTH routes, and losing it on the one
    // Chris uses from a terminal is the worse half to lose.
    //
    // No notifier is installed here on purpose: this binary runs inside
    // somebody else's Claude session and has no business raising banners.
    #[cfg(target_os = "macos")]
    cxtasks_core::platform::set_drain_probe(Box::new(cxtasks_core::launchd::is_installed));

    let db_path = cxtasks_core::paths::db_path();
    tracing::info!("CXTasks MCP server starting, DB: {:?}", db_path);

    // Run migrations. The Tauri app also does this at startup, but the MCP
    // binary is frequently the FIRST thing to touch the DB on a fresh machine
    // (a `/task` from a terminal before the app has ever been opened), so it
    // must be able to create the schema itself. Idempotent + version-gated.
    if let Some(parent) = db_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!("Could not create app data dir {:?}: {}", parent, e);
        }
    }
    match rusqlite::Connection::open(&db_path) {
        Ok(conn) => {
            let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
            if let Err(e) = cxtasks_core::db::schema::initialize(&conn) {
                tracing::warn!("Schema migration on MCP startup failed: {}", e);
            }
        }
        Err(e) => tracing::warn!("Could not open DB for migrations: {}", e),
    }

    let server = CxTasksMcp::new(db_path.to_string_lossy().to_string());

    // Self-reap so abandoned processes don't accumulate (see fn docs below).
    spawn_orphan_watchdog(server.claimed_flag());

    let service = server
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("MCP init error: {:?}", e))?;

    tracing::info!("MCP server running");
    service.waiting().await?;
    Ok(())
}

/// Background self-reaper so abandoned `cxtasks-mcp` processes don't pile up.
///
/// A clean client exit already terminates us: closing stdin makes rmcp's
/// `serve(stdio())` loop finish and `main` return. This covers the two cases
/// stdin-EOF does not, checked on a short interval:
///
/// 1. **Orphaned** — `getppid() == 1` means the launching Claude session died
///    and we were reparented to launchd without stdin closing; exit.
/// 2. **Never claimed** — no client completed `initialize` within the grace
///    window, so we were spawned but never connected. Real clients initialize
///    within milliseconds, so a long-idle *claimed* session is never affected.
fn spawn_orphan_watchdog(claimed: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::Ordering;

    let grace_secs: u64 = std::env::var("CXTASKS_MCP_IDLE_GRACE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    if grace_secs == 0 {
        tracing::info!("cxtasks-mcp: orphan watchdog disabled (CXTASKS_MCP_IDLE_GRACE_SECS=0)");
        return;
    }
    let check = std::time::Duration::from_secs(grace_secs.clamp(1, 30));

    tokio::spawn(async move {
        let mut elapsed: u64 = 0;
        loop {
            tokio::time::sleep(check).await;
            elapsed = elapsed.saturating_add(check.as_secs());

            // 1. Parent gone → reparented to launchd. SAFETY: getppid is always safe.
            if unsafe { libc::getppid() } == 1 {
                tracing::info!("cxtasks-mcp: parent gone (reparented to launchd); exiting");
                std::process::exit(0);
            }

            // 2. Spawned but never claimed by a client within the grace window.
            if elapsed >= grace_secs && !claimed.load(Ordering::Relaxed) {
                tracing::info!(
                    "cxtasks-mcp: no client initialized within {grace_secs}s; exiting abandoned spawn"
                );
                std::process::exit(0);
            }
        }
    });
}
