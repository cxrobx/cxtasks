//! Spawn a provider's command, drain it, and enforce the per-run ceilings.
//!
//! Provider-neutral on purpose: the deadlock and the kill are properties of
//! running *any* long-lived CLI over pipes, and writing them twice is how the
//! two paths end up with only one of them fixed. The **token ceiling belongs
//! here for the same reason** — Codex has no turn cap of its own, so a
//! per-provider copy of this guard is exactly how one of the two ends up
//! unbounded. The only per-provider part is reading the numbers, which is
//! [`Provider::live_total`].

use std::io::{BufRead, BufReader};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::runner::providers::{Provider, RunSummary};
use crate::runner::progress::ProgressBatch;

/// Which of our guards killed the child. Mutually exclusive — the caller has to
/// tell them apart, because one is "this run hung" and the other is "this run
/// was expensive", and they map to different run statuses and different advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillReason {
    Timeout,
    TokenCeiling { spent: i64, cap: i64 },
    /// Someone asked. The only kill reason that is a decision rather than a
    /// guard, which is why it reports as `cancelled` and not as a failure.
    Cancelled,
}

/// How the child ended, independent of what it said.
#[derive(Debug, Clone)]
pub struct ProcessOutcome {
    pub exit_code: Option<i32>,
    /// `Some` when **we** stopped it; `None` when it exited on its own.
    pub kill: Option<KillReason>,
    /// stderr, capped. Only surfaced on failure — on a healthy run it is
    /// progress chatter and would bury the answer.
    pub stderr: String,
}

/// Max stderr we keep. Enough for a stack trace, not enough to blow up a row.
const STDERR_CAP: usize = 8_000;

/// Run `cmd` until it exits, until `timeout`, or until it has spent
/// `token_ceiling` tokens — folding stdout into `summary` throughout.
///
/// The caller supplies an already-built [`Command`] with stdout/stderr piped.
/// `token_ceiling` of `0` disables that guard.
pub fn run_to_completion(
    mut cmd: Command,
    provider: Provider,
    timeout: Duration,
    token_ceiling: i64,
    summary: &Arc<std::sync::Mutex<RunSummary>>,
    progress: Option<std::sync::mpsc::Sender<ProgressBatch>>,
    // Polled alongside the two ceilings. A closure rather than anything
    // concrete so this module keeps knowing nothing about run ids or the DB —
    // see the note on `paths::cancel_flag_path` for why the caller checks a
    // file and not a column.
    cancelled: Option<&dyn Fn() -> bool>,
) -> std::io::Result<ProcessOutcome> {
    let mut child = cmd.spawn()?;

    // CRITICAL: drain stderr on its own thread. If we don't read it, the pipe
    // buffer fills and the child blocks writing to it — forever. The run then
    // looks like a slow task rather than a deadlock
    // (`reference_claude_cli_subprocess`). Codex is chattier than Claude on
    // stderr, so this matters more now, not less.
    let stderr = child.stderr.take();
    let stderr_handle = std::thread::spawn(move || {
        let mut collected = String::new();
        if let Some(err) = stderr {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                if collected.len() < STDERR_CAP {
                    collected.push_str(&line);
                    collected.push('\n');
                }
            }
        }
        collected
    });

    let stdout = child.stdout.take();
    let summary_for_thread = summary.clone();
    let stdout_handle = std::thread::spawn(move || {
        if let Some(out) = stdout {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let mut guard = match summary_for_thread.lock() {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
                let prior_session = guard.session_id.clone();
                let events = provider.absorb(&mut guard, &line);
                let session_id = if guard.session_id != prior_session {
                    guard.session_id.clone()
                } else {
                    None
                };
                // Never perform SQLite or socket I/O while holding this lock or
                // on the stdout-draining path. A busy DB must not let the pipe
                // fill and deadlock the child process.
                drop(guard);
                if (session_id.is_some() || !events.is_empty())
                    && progress
                        .as_ref()
                        .is_some_and(|tx| tx.send(ProgressBatch { session_id, events }).is_err())
                {
                    log::debug!("runner progress writer closed before stdout ended");
                }
            }
        }
    });

    // Both ceilings are enforced by killing the child. Polling `try_wait`
    // rather than blocking on `wait` is what makes either reachable at all.
    let started = Instant::now();
    let mut kill = None;
    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    kill = Some(KillReason::Timeout);
                } else if cancelled.is_some_and(|f| f()) {
                    // Checked BEFORE the token read: cancelling is the cheaper
                    // question and the one the user is waiting on, and there is
                    // no reason to take the summary lock for a run that is about
                    // to be killed anyway.
                    log::info!("runner: stopping a run at the user's request");
                    kill = Some(KillReason::Cancelled);
                } else if token_ceiling > 0 {
                    // Read the counters, then DROP the lock before touching the
                    // process — same rule as the stdout thread's: nothing slow
                    // happens while the parse path is blocked, or the pipe fills
                    // and the child deadlocks.
                    let spent = {
                        let guard = match summary.lock() {
                            Ok(g) => g,
                            Err(p) => p.into_inner(),
                        };
                        provider.live_total(&guard)
                    };
                    if spent >= token_ceiling {
                        log::warn!(
                            "runner: stopping a run at the per-run token ceiling \
                             ({spent} >= {token_ceiling})"
                        );
                        kill = Some(KillReason::TokenCeiling {
                            spent,
                            cap: token_ceiling,
                        });
                    }
                }
                if kill.is_some() {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => {
                log::warn!("runner: try_wait failed: {e}");
                break None;
            }
        }
    };

    let _ = stdout_handle.join();
    let stderr = stderr_handle.join().unwrap_or_default();

    Ok(ProcessOutcome {
        exit_code,
        kill,
        stderr,
    })
}

/// Take the summary back out of the mutex, poisoning and all.
///
/// A panic in a parsing thread must not take the run's result with it: whatever
/// was absorbed before the panic is still the best answer we have.
pub fn take_summary(summary: &Arc<std::sync::Mutex<RunSummary>>) -> RunSummary {
    match summary.lock() {
        Ok(g) => g.clone(),
        Err(p) => p.into_inner().clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    fn sleeper(seconds: &str) -> Command {
        let mut cmd = Command::new("/bin/sleep");
        cmd.arg(seconds)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// The cancel has to actually reach a live child, and quickly.
    ///
    /// A 30-second sleep against a 10-minute timeout and no token ceiling means
    /// neither existing guard can be the thing that stops it — so if this
    /// returns at all, the cancel closure is what did it. The elapsed assert is
    /// what pins the poll: the loop sleeps 250ms between checks, and a
    /// regression that moved the cancel test after a blocking read would show
    /// up here as seconds rather than as a failure to kill.
    #[test]
    fn a_cancel_kills_a_live_child() {
        let summary = Arc::new(std::sync::Mutex::new(RunSummary::default()));
        let started = Instant::now();
        let outcome = run_to_completion(
            sleeper("30"),
            Provider::Claude,
            Duration::from_secs(600),
            0,
            &summary,
            None,
            Some(&|| true),
        )
        .expect("spawn");

        assert_eq!(outcome.kill, Some(KillReason::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancel took {:?} — the poll should catch it within a tick",
            started.elapsed()
        );
    }

    /// The mirror, and the one that would catch a cancel check wired to fire on
    /// every poll: a run nobody cancelled must exit on its own terms, reporting
    /// no kill at all.
    #[test]
    fn an_uncancelled_run_is_left_alone() {
        let summary = Arc::new(std::sync::Mutex::new(RunSummary::default()));
        let outcome = run_to_completion(
            sleeper("0"),
            Provider::Claude,
            Duration::from_secs(600),
            0,
            &summary,
            None,
            Some(&|| false),
        )
        .expect("spawn");

        assert_eq!(outcome.kill, None);
        assert_eq!(outcome.exit_code, Some(0));
    }
}
