//! Non-blocking persistence and notification for live run activity.
//!
//! The child stdout thread only parses and sends [`ProgressBatch`] values. This
//! dedicated writer owns SQLite and bridge I/O so a busy DB or stale socket can
//! never stop stdout from draining and deadlock the agent process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::db::run_events::NewRunEvent;
use crate::error::AppError;
use crate::mcp::bridge::Envelope;
use crate::runner::providers::TraceEvent;

const NOTIFY_INTERVAL: Duration = Duration::from_secs(1);
const SUMMARY_CAP: usize = 240;

#[derive(Debug)]
pub struct ProgressBatch {
    pub session_id: Option<String>,
    pub events: Vec<TraceEvent>,
}

/// Owns the sending half and joins the writer on every exit path.
pub struct ProgressReporter {
    sender: Option<Sender<ProgressBatch>>,
    writer: Option<JoinHandle<()>>,
}

impl ProgressReporter {
    /// Start the writer, or refuse the run.
    ///
    /// Returns `Result` because the connection now carries liveness as well as
    /// the trace — see the note in `writer_loop`. A run whose writer cannot
    /// reach the DB would stop heartbeating and be reclaimed while it worked,
    /// so failing here is strictly better than starting.
    pub fn start(run_id: &str, task_id: &str, repo: PathBuf) -> Result<Self, AppError> {
        let conn = crate::runner::open_db()?;
        let (sender, receiver) = mpsc::channel();
        let run_id = run_id.to_string();
        let task_id = task_id.to_string();
        let writer =
            std::thread::spawn(move || writer_loop(conn, receiver, &run_id, &task_id, &repo));
        Ok(Self {
            sender: Some(sender),
            writer: Some(writer),
        })
    }

    pub fn sender(&self) -> Sender<ProgressBatch> {
        self.sender.as_ref().expect("live progress sender").clone()
    }

    fn close(&mut self) {
        self.sender.take();
        if let Some(writer) = self.writer.take() {
            if writer.join().is_err() {
                log::warn!("runner progress writer panicked");
            }
        }
    }
}

impl Drop for ProgressReporter {
    fn drop(&mut self) {
        self.close();
    }
}

/// How often the writer says "still here".
///
/// Ten of these fit inside `runs::HEARTBEAT_STALE_MINUTES`, so nine can be lost
/// to a busy machine before anything concludes the run is dead.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// Write a heartbeat if one is due.
///
/// Failures are logged and swallowed on purpose. A transient `SQLITE_BUSY`
/// while another process records an outcome is not a reason to tear down a
/// working run, and the next beat is 30s away — well inside the staleness
/// window.
fn beat(conn: Option<&Connection>, run_id: &str, last_beat: &mut Instant) {
    if last_beat.elapsed() < HEARTBEAT_INTERVAL {
        return;
    }
    if let Some(conn) = conn {
        if let Err(e) = crate::db::runs::heartbeat(conn, run_id) {
            log::warn!("runner: heartbeat failed for run {run_id}: {e}");
        }
    }
    *last_beat = Instant::now();
}

fn writer_loop(
    conn: Connection,
    receiver: Receiver<ProgressBatch>,
    run_id: &str,
    task_id: &str,
    repo: &Path,
) {
    // Owned, not optional. This connection used to be best-effort — a failure
    // to open it downgraded the run to "no live trace" and carried on. Now that
    // it also carries the HEARTBEAT, that degradation is a run reclaimed out
    // from under itself: no beats means the sweep decides the process is gone
    // and marks a working run `abandoned`. So the open moved to the caller and
    // became fatal (`ProgressReporter::start` returns a Result). Nothing is
    // lost by refusing — a run that cannot reach the DB cannot record its
    // outcome either.
    let conn = Some(conn);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok();

    notify(&runtime, Envelope::new("run-started", task_id).with_run_id(run_id));

    let mut call_details: HashMap<String, (String, String)> = HashMap::new();
    let mut persisted_session: Option<String> = None;
    let mut dirty = false;
    let mut last_notified = Instant::now();
    let mut last_beat = Instant::now();

    loop {
        let notify_wait = if dirty {
            NOTIFY_INTERVAL.saturating_sub(last_notified.elapsed())
        } else {
            Duration::from_secs(60)
        };
        // Never sleep past the next beat. A run that is thinking rather than
        // calling tools produces no batches for minutes at a time, and the old
        // 60s idle wait would have been fine for notifications and fatal for
        // liveness.
        let wait = notify_wait.min(HEARTBEAT_INTERVAL.saturating_sub(last_beat.elapsed()));
        match receiver.recv_timeout(wait) {
            Ok(batch) => {
                if let Some(conn) = conn.as_ref() {
                    dirty |= persist_batch(
                        conn,
                        run_id,
                        repo,
                        &mut call_details,
                        &mut persisted_session,
                        batch,
                    );
                }

                if dirty && last_notified.elapsed() >= NOTIFY_INTERVAL {
                    notify_progress(&runtime, task_id, run_id);
                    dirty = false;
                    last_notified = Instant::now();
                }
                beat(conn.as_ref(), run_id, &mut last_beat);
            }
            Err(RecvTimeoutError::Timeout) => {
                if dirty {
                    notify_progress(&runtime, task_id, run_id);
                    dirty = false;
                    last_notified = Instant::now();
                }
                // UNGATED on `dirty`, deliberately. A quiet run is still a live
                // run; gating the beat on having something to report is how a
                // long single tool call gets swept mid-flight.
                beat(conn.as_ref(), run_id, &mut last_beat);
            }
            Err(RecvTimeoutError::Disconnected) => {
                if dirty {
                    notify_progress(&runtime, task_id, run_id);
                }
                break;
            }
        }
    }
}

fn persist_batch(
    conn: &rusqlite::Connection,
    run_id: &str,
    repo: &Path,
    call_details: &mut HashMap<String, (String, String)>,
    persisted_session: &mut Option<String>,
    batch: ProgressBatch,
) -> bool {
    if let Some(session_id) = batch.session_id.as_deref() {
        if persisted_session.as_deref() != Some(session_id) {
            if let Err(e) = crate::db::runs::set_session_id(conn, run_id, session_id) {
                log::warn!("runner: could not persist session id for {run_id}: {e}");
            } else {
                *persisted_session = Some(session_id.to_string());
            }
        }
    }

    let mut changed = false;
    for mut event in batch.events {
        if let Some(call_id) = event.call_id.as_deref() {
            if event.phase == "started" {
                call_details.insert(
                    call_id.to_string(),
                    (
                        event.tool_name.clone().unwrap_or_else(|| "Tool".into()),
                        event.summary.clone(),
                    ),
                );
            } else if let Some((tool_name, summary)) = call_details.get(call_id) {
                if event.tool_name.is_none() {
                    event.tool_name = Some(tool_name.clone());
                }
                if event.summary.is_empty() {
                    event.summary = summary.clone();
                }
            }
        }
        if event.summary.is_empty() {
            event.summary = event.tool_name.clone().unwrap_or_else(|| "Tool".into());
        }
        event.summary = normalize_summary(&event.summary, repo);

        let row = NewRunEvent {
            kind: event.kind,
            call_id: event.call_id,
            tool_name: event.tool_name,
            phase: event.phase,
            summary: event.summary,
            detail_json: None,
        };
        match crate::db::run_events::append(conn, run_id, &row) {
            Ok(Some(_)) => changed = true,
            Ok(None) => {}
            Err(e) => log::warn!("runner: could not append trace event for {run_id}: {e}"),
        }
    }
    changed
}

fn notify_progress(runtime: &Option<tokio::runtime::Runtime>, task_id: &str, run_id: &str) {
    notify(
        runtime,
        Envelope::new("run-progress", task_id)
            .with_tool("runner")
            .with_run_id(run_id),
    );
}

fn notify(runtime: &Option<tokio::runtime::Runtime>, envelope: Envelope) {
    if let Some(runtime) = runtime {
        runtime.block_on(crate::mcp::bridge::notify(&envelope));
    }
}

fn normalize_summary(summary: &str, repo: &Path) -> String {
    let mut out = summary.replace(&repo.to_string_lossy().to_string(), ".");
    if let Some(home) = dirs::home_dir() {
        out = out.replace(&home.to_string_lossy().to_string(), "~");
    }
    let normalized = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let mut capped = chars.by_ref().take(SUMMARY_CAP).collect::<String>();
    if chars.next().is_some() {
        capped.push('…');
    }
    capped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::{run_events, runs, tasks};
    use crate::executor::BackgroundExecutor;

    #[test]
    fn summaries_are_repo_relative_and_bounded() {
        let repo = PathBuf::from("/tmp/a-repo");
        let value = format!("Read /tmp/a-repo/src/main.rs {}", "x".repeat(400));
        let normalized = normalize_summary(&value, &repo);
        assert!(normalized.starts_with("Read ./src/main.rs"), "{normalized}");
        assert!(normalized.chars().count() <= SUMMARY_CAP + 1);
    }

    #[test]
    fn a_batch_persists_session_and_a_correlated_tool_lifecycle() {
        let conn = test_conn();
        let task = tasks::create(
            &conn,
            &tasks::NewTask {
                title: "trace".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let run = runs::start(
            &conn,
            &task.id,
            "background",
            None,
            BackgroundExecutor::Claude,
            crate::intent::BackgroundIntent::Work,
            None,
        )
        .unwrap();
        let mut calls = HashMap::new();
        let mut session = None;
        let repo = PathBuf::from("/tmp/a-repo");

        assert!(persist_batch(
            &conn,
            &run.id,
            &repo,
            &mut calls,
            &mut session,
            ProgressBatch {
                session_id: Some("session-1".into()),
                events: vec![
                    TraceEvent::tool_started(
                        Some("call-1".into()),
                        "Read".into(),
                        "Read /tmp/a-repo/src/main.rs".into(),
                    ),
                    TraceEvent::tool_finished(Some("call-1".into()), false),
                ],
            },
        ));

        assert_eq!(
            runs::get(&conn, &run.id)
                .unwrap()
                .unwrap()
                .session_id
                .as_deref(),
            Some("session-1")
        );
        let events = run_events::list_after(&conn, &run.id, 0).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].summary, "Read ./src/main.rs");
        assert_eq!(events[1].summary, "Read ./src/main.rs");
        assert_eq!(events[1].phase, "succeeded");
    }
}
