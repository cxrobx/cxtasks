//! Streamable-HTTP mode for `cxtasks-mcp` (T640): `cxtasks-mcp --http <port>`.
//!
//! One long-lived process, run by a LaunchAgent, serves every Claude Code and
//! Codex session on the machine at `http://127.0.0.1:<port>/mcp`, instead of a
//! stdio copy per session. stdio stays the default and is untouched.
//!
//! Three things differ from stdio, and each is a trap if missed:
//!
//! 1. **Per-session state.** rmcp calls the factory once per MCP session, and
//!    the factory builds a NEW [`CxTasksMcp`] each time. Never clone one
//!    template: `#[derive(Clone)]` shares the `client_name` Arc, and the last
//!    session to initialize would sign everybody's comments (T238).
//! 2. **No orphan watchdog.** Under launchd `getppid()` is 1 from the first
//!    instant, and nobody "claims" the process the way a stdio client does.
//!    `bin/mcp.rs` only starts the watchdog on the stdio path.
//! 3. **Interactive only, and identity from `clientInfo`.** Unattended runs
//!    get their own stdio server through `--strict-mcp-config`, so a shared
//!    server must never inherit a reduced surface from a stray env var (or
//!    the reverse). [`refuse_unattended_env`] makes that a startup error, and
//!    [`CxTasksMcp::for_http_session`] ignores `CXTASKS_AGENT`.

use std::future::Future;
use std::sync::Arc;

use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};

use crate::mcp::server::{CxTasksMcp, AGENT_ENV, BACKGROUND_ENV, CHECK_ENV};

/// Refuse to start a shared server from an environment meant for one
/// unattended run. `vars` is the lookup, so this is testable without touching
/// the process environment.
pub fn check_env(vars: impl Fn(&str) -> bool) -> Result<(), String> {
    for name in [CHECK_ENV, BACKGROUND_ENV] {
        if vars(name) {
            return Err(format!(
                "cxtasks-mcp --http refuses to start with {name} set: the HTTP server is shared \
                 by every session and is always the interactive surface. Unattended runs use \
                 their own stdio server (--strict-mcp-config); unset {name} to serve HTTP."
            ));
        }
    }
    Ok(())
}

/// [`check_env`] against the real process environment.
pub fn refuse_unattended_env() -> Result<(), String> {
    check_env(|name| std::env::var_os(name).is_some())
}

/// Bind to `127.0.0.1:<port>` — loopback only, never all interfaces.
pub async fn bind(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await
}

/// Serve MCP at `/mcp` on `listener` until `shutdown` resolves.
pub async fn serve(
    db_path: String,
    listener: tokio::net::TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    if std::env::var_os(AGENT_ENV).is_some() {
        tracing::warn!(
            "cxtasks-mcp --http: ignoring {AGENT_ENV}; each session is signed by its own clientInfo"
        );
    }

    // Default config: stateful sessions, loopback-only Host validation
    // (DNS-rebinding guard), 15 s SSE keep-alive.
    let config = StreamableHttpServerConfig::default();
    let cancel = config.cancellation_token.clone();
    let service = StreamableHttpService::new(
        move || Ok(CxTasksMcp::for_http_session(db_path.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let router = axum::Router::new().nest_service("/mcp", service);

    tracing::info!(
        "cxtasks-mcp: serving streamable HTTP at http://{}/mcp",
        listener.local_addr()?
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.await;
            cancel.cancel();
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn http_mode_refuses_an_unattended_environment() {
        assert!(check_env(|_| false).is_ok());
        let check = check_env(|n| n == CHECK_ENV).unwrap_err();
        assert!(check.contains(CHECK_ENV), "{check}");
        let bg = check_env(|n| n == BACKGROUND_ENV).unwrap_err();
        assert!(bg.contains(BACKGROUND_ENV), "{bg}");
        // The agent declaration is not a refusal — it is ignored instead.
        assert!(check_env(|n| n == AGENT_ENV).is_ok());
    }

    /// Pull the JSON-RPC response for `id` out of an SSE (or plain JSON) body.
    fn response_for(body: &str, id: i64) -> Value {
        if let Ok(v) = serde_json::from_str::<Value>(body) {
            return v;
        }
        body.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
            .find(|v| v.get("id").and_then(Value::as_i64) == Some(id))
            .unwrap_or_else(|| panic!("no response for id {id} in {body:?}"))
    }

    fn post(url: &str, session: Option<&str>, msg: &Value) -> (Option<String>, u16, String) {
        let mut req = ureq::post(url)
            .set("Content-Type", "application/json")
            .set("Accept", "application/json, text/event-stream");
        if let Some(s) = session {
            req = req.set("Mcp-Session-Id", s);
        }
        let resp = req.send_string(&msg.to_string()).expect("POST /mcp");
        let sid = resp.header("mcp-session-id").map(str::to_string);
        let status = resp.status();
        (sid, status, resp.into_string().unwrap_or_default())
    }

    /// Open a session as `client`, returning its session id.
    fn open_session(url: &str, client: &str) -> String {
        let (sid, _, body) = post(
            url,
            None,
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-06-18","capabilities":{},
                "clientInfo":{"name":client,"version":"0"}}}),
        );
        let init = response_for(&body, 1);
        assert!(init["result"]["serverInfo"].is_object(), "{init}");
        let sid = sid.expect("stateful server must issue a session id");
        let (_, status, _) = post(
            url,
            Some(&sid),
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        );
        assert_eq!(status, 202);
        sid
    }

    fn call(url: &str, sid: &str, id: i64, tool: &str, args: Value) -> Value {
        let (_, _, body) = post(
            url,
            Some(sid),
            &json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                    "params":{"name":tool,"arguments":args}}),
        );
        response_for(&body, id)
    }

    /// T640 × T238: two concurrent sessions on ONE server, one Claude and one
    /// Codex, each sign their own comments. A shared `client_name` would sign
    /// both as whichever session initialized last (here, Codex), so Claude's
    /// comment is written AFTER Codex initializes to catch exactly that.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_http_sessions_sign_as_their_own_client() {
        let dir = std::env::temp_dir().join(format!("cxtasks-http-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let task = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            crate::db::schema::initialize(&conn).unwrap();
            crate::db::tasks::create(
                &conn,
                &crate::db::tasks::NewTask {
                    title: "http sessions".into(),
                    ..Default::default()
                },
            )
            .unwrap()
        };

        let listener = bind(0).await.unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(addr.ip().is_loopback(), "must bind loopback only, got {addr}");
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve(db.to_string_lossy().to_string(), listener, async {
            let _ = stop_rx.await;
        }));

        let url = format!("http://{addr}/mcp");
        let task_id = task.id.clone();
        tokio::task::spawn_blocking(move || {
            let claude = open_session(&url, "claude-code");
            let codex = open_session(&url, "codex-mcp-client");
            assert_ne!(claude, codex);

            // tools/list works and is the interactive surface.
            let (_, _, body) = post(
                &url,
                Some(&claude),
                &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            );
            let tools = response_for(&body, 2);
            let names: Vec<&str> = tools["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|t| t["name"].as_str())
                .collect();
            assert!(names.contains(&"file_task") && !names.contains(&"file_followup"));

            for (sid, id, who) in [(&codex, 3, "from codex"), (&claude, 4, "from claude")] {
                let r = call(&url, sid, id, "comment_task", json!({"task_id":task_id,"body":who}));
                assert!(r["result"]["isError"] != json!(true), "{r}");
            }
        })
        .await
        .unwrap();

        let conn = rusqlite::Connection::open(&db).unwrap();
        let mut got: Vec<(String, String)> = crate::db::comments::for_task(&conn, &task.id)
            .unwrap()
            .into_iter()
            .map(|c| (c.body, c.author))
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("from claude".to_string(), "claude".to_string()),
                ("from codex".to_string(), "codex".to_string()),
            ]
        );

        stop_tx.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), server)
            .await
            .expect("server shuts down")
            .unwrap()
            .unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
