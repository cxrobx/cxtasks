//! Cross-process notification bridge.
//!
//! `cxtasks-mcp`, `cxtasks-runner` and the CXTasks GUI share only the SQLite
//! DB — when an MCP tool files a task the GUI has no way to know. This bridge
//! carries one-shot JSON notifications over a Unix-domain socket so the app
//! live-refreshes without polling. It is what makes `/task` in another terminal
//! appear in the open window within a second.
//!
//! Wire protocol: connect → write one JSON [`Envelope`] line → close. The app's
//! accept loop (`lib.rs::run()` `.setup()`) reads the line, parses it, and
//! re-emits it to the frontend as the `mcp-activity` Tauri event.
//!
//! Every failure on the sender side is swallowed (debug-logged only): if the
//! app isn't running, the task is already committed to SQLite and shows on the
//! next open, so a missing live update must NEVER become a tool error.
//!
//! Security: the socket lives in the user's app-data dir, so same-user
//! filesystem perms are the access control (no port, no token). The payload
//! carries identifiers only, and the app treats it strictly as a *hint to
//! re-fetch from the DB*, never as trusted content.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

pub use crate::paths::socket_path;

/// One notification. A *hint to re-fetch from the DB*, never trusted content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Schema version. Bump when the shape changes so readers can branch on it.
    pub v: u32,
    /// `"task-created" | "task-updated" | "task-completed" | "task-deleted" |
    /// "run-started" | "run-progress" | "run-finished" |
    /// "project-updated"`.
    pub kind: String,
    /// The affected task. Empty for app-wide refresh hints — `"project-updated"`
    /// always sends it empty, so the board refreshes without flashing a row.
    pub task_id: String,
    /// Originating tool or subsystem, for debugging and toast text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// `run-*` kinds only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Human-readable one-liner the app may surface as a toast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl Envelope {
    pub fn new(kind: &str, task_id: &str) -> Self {
        Self {
            v: 1,
            kind: kind.to_string(),
            task_id: task_id.to_string(),
            tool: None,
            run_id: None,
            summary: None,
        }
    }

    pub fn with_tool(mut self, tool: &str) -> Self {
        self.tool = Some(tool.to_string());
        self
    }

    pub fn with_run_id(mut self, run_id: &str) -> Self {
        self.run_id = Some(run_id.to_string());
        self
    }

    pub fn with_summary(mut self, summary: &str) -> Self {
        self.summary = Some(summary.to_string());
        self
    }
}

/// Connect to the app's socket and write one [`Envelope`] line.
///
/// Best-effort: every failure path — app not running, slow/no accept, serialize
/// error — is debug-logged and swallowed. The 500 ms timeout bounds the
/// pathological case where the socket file exists but nothing is accepting
/// (e.g. the app crashed without cleanup).
pub async fn notify(env: &Envelope) {
    let _ = notify_delivered(env).await;
}

/// Same as [`notify`], but reports whether a running app actually received it.
///
/// The runner uses the answer to decide who posts the notification: if the app
/// got the envelope, IT posts, because it can word the banner around what is
/// already on screen; if not, the runner posts it itself.
pub async fn notify_delivered(env: &Envelope) -> bool {
    // A test binary must never reach the app that happens to be running on the
    // developer's Mac. `socket_path()` resolves the REAL socket unless
    // `CXTASKS_DATA_DIR` is set, and the tripwire tests exercise `announce`
    // with fixtures — which is how "tripwire tripped: gate — agent:the moon is
    // full" once appeared as a genuine macOS banner mid `cargo test`. The
    // process-global env is racy under parallel tests, so the guard lives
    // here, on the one path that talks to a live app, not in each test.
    if cfg!(test) {
        log::debug!("bridge: test build, not delivering {} to a live app", env.kind);
        return false;
    }
    let path = socket_path();

    let mut payload = match serde_json::to_vec(env) {
        Ok(v) => v,
        Err(e) => {
            log::debug!("bridge: serialize failed: {e}");
            return false;
        }
    };
    payload.push(b'\n');

    let mut stream =
        match tokio::time::timeout(Duration::from_millis(500), UnixStream::connect(&path)).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                log::debug!("bridge: connect {} failed (app not running?): {e}", path.display());
                return false;
            }
            Err(_) => {
                log::debug!("bridge: connect {} timed out", path.display());
                return false;
            }
        };

    if let Err(e) = stream.write_all(&payload).await {
        log::warn!("bridge: write to app failed after connect: {e}");
        return false;
    }
    if let Err(e) = stream.flush().await {
        log::warn!("bridge: flush to app failed after connect: {e}");
        return false;
    }
    log::info!(
        "bridge: delivered {} (tool={:?}, task={}) to app",
        env.kind,
        env.tool,
        env.task_id
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrip() {
        let env = Envelope::new("task-created", "abc-123")
            .with_tool("file_task")
            .with_summary("fix the intake webhook retry");
        let bytes = serde_json::to_vec(&env).unwrap();
        let parsed: Envelope = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(env, parsed);
    }

    #[test]
    fn envelope_minimal_skips_optional_fields() {
        let env = Envelope::new("task-updated", "id");
        let json = serde_json::to_string(&env).unwrap();
        assert!(!json.contains("\"tool\""), "{json}");
        assert!(!json.contains("run_id"), "{json}");
        assert!(!json.contains("summary"), "{json}");
        assert_eq!(serde_json::from_str::<Envelope>(&json).unwrap().v, 1);
    }

    #[test]
    fn envelope_tolerates_trailing_newline() {
        let env = Envelope::new("task-created", "a");
        let mut bytes = serde_json::to_vec(&env).unwrap();
        bytes.push(b'\n');
        assert_eq!(serde_json::from_slice::<Envelope>(&bytes).unwrap(), env);
    }

    #[test]
    fn envelope_ignores_unknown_fields_forward_compat() {
        let json = r#"{"v":2,"kind":"task-created","task_id":"a","future_field":true}"#;
        let parsed: Envelope = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.v, 2);
        assert_eq!(parsed.task_id, "a");
    }

    #[tokio::test]
    async fn notify_is_silent_when_no_app_is_listening() {
        // Must not panic or hang; the tool result is untouched by a dead socket.
        notify(&Envelope::new("task-created", "nobody-home")).await;
    }
}
