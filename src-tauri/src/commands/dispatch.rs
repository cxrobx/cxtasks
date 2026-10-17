//! Foreground dispatch: hand a task to Claude in its Herdr space when the
//! shared desktop router is installed, otherwise in a Ghostty window.

use std::process::{Command, Stdio};

use tauri::State;

use crate::db;
use crate::dispatch::{
    find_claude_cli, resolve_repo_or_home, write_launch_script, write_scratch,
    PromptTarget,
};
use crate::error::AppError;
use crate::executor::BackgroundExecutor;
use crate::runner::Provider;
use crate::{AppState, LockExt};

#[derive(serde::Serialize)]
pub struct DispatchResult {
    pub run_id: String,
    pub repo_path: String,
    pub scratch_dir: String,
}

fn herdr_router() -> Option<std::path::PathBuf> {
    let path = dirs::home_dir()?.join(".local/bin/cx-open-in-herdr");
    path.is_file().then_some(path)
}

/// Repo-less Herdr sessions have their own task directory. An explicit repo
/// still wins; the standalone Ghostty fallback retains its existing behavior.
fn foreground_repo(task: &db::tasks::Task) -> Result<std::path::PathBuf, AppError> {
    if herdr_router().is_some()
        && task
            .repo_path
            .as_deref()
            .map_or(true, |p| p.trim().is_empty())
    {
        let scratch = crate::dispatch::scratch_dir_for(task);
        std::fs::create_dir_all(&scratch)?;
        Ok(scratch)
    } else {
        resolve_repo_or_home(task)
    }
}

fn record_foreground_launch(
    conn: &rusqlite::Connection,
    task_id: &str,
    run_id: &str,
    result: Result<(), AppError>,
) -> Result<(), AppError> {
    if let Err(error) = result {
        db::runs::fail_foreground_dispatch(conn, run_id, &error.to_string())?;
        return Err(error);
    }
    db::tasks::update(
        conn,
        task_id,
        &db::tasks::TaskPatch {
            status: Some("in_progress".into()),
            ..Default::default()
        },
    )?;
    Ok(())
}

/// Open a fresh Claude tab in the task's Herdr space.
#[tauri::command]
pub async fn dispatch_foreground(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<DispatchResult, AppError> {
    let ghostty_app = crate::ghostty_macos::find_ghostty_app().ok_or_else(|| {
        AppError::Dispatch(
            "Ghostty is not installed. Install it from https://ghostty.org/download and try again."
                .into(),
        )
    })?;
    let claude_cli = find_claude_cli().ok_or_else(|| {
        AppError::Dispatch(
            "The Claude Code CLI (`claude`) wasn't found on PATH. Install it (e.g. \
             `npm install -g @anthropic-ai/claude-code`) and try again."
                .into(),
        )
    })?;

    let task = {
        let conn = state.db.safe_lock();
        db::tasks::get(&conn, &task_id)?
            .ok_or_else(|| AppError::NotFound(format!("task {task_id} not found")))?
    };

    let repo = foreground_repo(&task)?;
    // Always Claude: foreground opens a session the user drives, and the whole
    // write-back loop it's told to close runs through the CXTasks MCP.
    let scratch = write_scratch(&task, &repo, PromptTarget::Foreground, None)?;
    let launch_path = write_launch_script(&scratch, &repo, &claude_cli)?;

    // Record the run BEFORE spawning, so a session that's opened and then
    // abandoned still leaves a trace on the task.
    //
    // And close it immediately. A foreground dispatch hands a terminal session
    // to the user and the app never hears from it again — it cannot show progress,
    // cannot stop it, and cannot learn how it went. `running` was therefore a
    // state with no exit: nothing in this codebase ever called `runs::finish`
    // for a foreground run, so every one of them stayed "running" forever,
    // reported as live in Mission Control and on the task. What is actually
    // knowable is that a session was opened, and that is what gets recorded.
    //
    // The live signal is the TASK, which moves to `in_progress` below — that
    // one is real, survives the app quitting, and is the thing you close when
    // the work is done.
    let run = {
        let conn = state.db.safe_lock();
        let run = db::runs::start(
            &conn,
            &task.id,
            "foreground",
            Some(&scratch.to_string_lossy()),
            // Not `task.bg_executor`: this run IS Claude regardless of which
            // agent the task would use in the background, and a run row records
            // what happened rather than what was configured.
            BackgroundExecutor::Claude,
            // Intent, by the same rule, IS what happened: the prompt this
            // session was handed was framed by `task.bg_intent` a few lines up.
            task.bg_intent,
            // No wall clock: a foreground session runs in the terminal until the
            // user closes it, and the row is terminal at `dispatched` anyway
            // (invariant 20), so there is no timeout to record and nothing for
            // `sweep_abandoned` to judge it against.
            None,
        )?;
        db::runs::finish(
            &conn,
            &run.id,
            &db::runs::RunOutcome {
                status: db::runs::FOREGROUND_STATUS.into(),
                ..Default::default()
            },
        )?;
        run
    };

    let working_dir_arg = format!("--working-directory={}", repo.display());
    let launch_for_err = launch_path.clone();
    let repo_for_spawn = repo.clone();
    let router = herdr_router();
    let space = task.project.clone().unwrap_or_else(|| "CXTasks".into());
    let scope = task
        .repo_path
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .is_none()
        .then(|| format!("cxtasks:{}", task.project.as_deref().unwrap_or("inbox")));
    let launched = tokio::task::spawn_blocking(move || -> Result<(), AppError> {
        if let Some(router) = router {
            let mut command = Command::new(router);
            command
                .arg("--cwd")
                .arg(&repo_for_spawn)
                .arg("--launch")
                .arg(&launch_for_err)
                .arg("--space")
                .arg(&space);
            if let Some(scope) = scope {
                command.arg("--scope").arg(scope);
            }
            let output = command.stdin(Stdio::null()).output()?;
            return if output.status.success() {
                Ok(())
            } else {
                Err(AppError::Dispatch(format!(
                    "Could not open Claude in Herdr: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )))
            };
        }

        // Preferred: ask the Ghostty the user ALREADY has for a new window,
        // over AppleScript. Without this every dispatch spawns a second whole
        // Ghostty application — its own Dock icon, its own ⌘-Tab entry — and a
        // day of dispatching leaves a row of them.
        match crate::ghostty_macos::open_in_running_ghostty(&repo_for_spawn, &launch_for_err) {
            Ok(()) => return Ok(()),
            // Fail OPEN, always. Ghostty may be quit, `macos-applescript` may
            // be off, the automation consent may have been declined, or the
            // install may predate 1.3 — none of those are a reason to refuse to
            // dispatch, and each one leaves the fallback below working exactly
            // as it did before. Logged rather than surfaced because the user
            // gets their session either way; the line is what turns "why is it
            // spawning instances again" into a one-second answer.
            Err(reason) => log::info!(
                "dispatch: AppleScript route unavailable ({reason}); \
                 falling back to a new Ghostty instance"
            ),
        }

        // Fallback: `open -na` forces a NEW Ghostty instance, which is what
        // makes our `-e` command honored even when Ghostty is already running
        // (a plain `open -a … --args` is ignored by a running instance, and
        // there is no CLI to run a command in a tab of an existing one —
        // ghostty-org/ghostty#12136).
        //
        // But on macOS a fresh instance ALSO restores the previous session's
        // saved windows, so you get (restored windows) + our command window —
        // cxmail measured 3 windows from one launch (gotcha #24, Chris's own
        // upstream report ghostty-org/ghostty#9612). `--window-save-state=never`
        // as a per-launch CLI override makes this ephemeral instance neither
        // restore nor save state ⇒ exactly one window. It does not touch the
        // user's Ghostty config or their primary instance.
        Command::new("/usr/bin/open")
            .arg("-na")
            .arg(&ghostty_app)
            .arg("--args")
            .arg("--window-save-state=never")
            .arg(&working_dir_arg)
            .arg("-e")
            .arg(&launch_for_err)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                AppError::Dispatch(format!(
                    "Failed to launch Ghostty ({}): {e}",
                    launch_for_err.display()
                ))
            })?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::Dispatch(format!("spawn_blocking join error: {e}")))
    .and_then(|result| result);
    // A routing failure must not claim the user has started work. Keep the
    // dispatch attempt, with its failure, and leave the task's status alone.
    record_foreground_launch(&state.db.safe_lock(), &task.id, &run.id, launched)?;

    log::info!(
        "dispatch: opened Claude for task {} in {} (run {})",
        task.id,
        repo.display(),
        run.id
    );

    Ok(DispatchResult {
        run_id: run.id,
        repo_path: repo.to_string_lossy().to_string(),
        scratch_dir: scratch.to_string_lossy().to_string(),
    })
}

/// The seed prompt as text, for pasting into a session the user opens
/// themselves. Same builder as the real dispatch, so the two can't drift.
#[tauri::command]
pub async fn get_dispatch_prompt(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<String, AppError> {
    let task = {
        let conn = state.db.safe_lock();
        db::tasks::get(&conn, &task_id)?
            .ok_or_else(|| AppError::NotFound(format!("task {task_id} not found")))?
    };
    let repo = foreground_repo(&task)?;
    let conn = state.db.safe_lock();
    Ok(crate::dispatch::build_prompt_with_context(&conn, &task, &repo, PromptTarget::Foreground, None))
}

/// The serializable face of [`crate::dispatch::QueueOutcome`] — see there for
/// why the runner facts travel back with the task rather than being left for
/// the user to discover (gotcha #18).
pub use cxtasks_core::api::QueueResult;

/// Queue a task for the background runner. Validates the guards at the point
/// the user asks — rather than letting the runner silently skip it.
///
/// A thin wrapper: the guards, the preflight and the write all live in
/// [`crate::dispatch::queue_for_background`], shared with the MCP's
/// `update_task(status = "queued")` so the two entry points cannot drift
/// (gotcha #18's lesson — every route into queueing needs the same wiring).
/// The closure scopes the mutex per call, so the lock is not held across
/// preflight, which shells out.
#[tauri::command]
pub async fn queue_background(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<QueueResult, AppError> {
    // Through the table, like every other canonical verb.
    let mut conn = state.db.safe_lock();
    let value = cxtasks_core::api::dispatch(
        &mut conn,
        "queue_background",
        &serde_json::json!({ "task_id": task_id }),
    )?;
    serde_json::from_value(value)
        .map_err(|e| AppError::General(format!("could not decode queue result: {e}")))
}

/// Is a background agent installed and usable right now?
///
/// Read-only and cheap. The detail pane calls it when Codex is selected so the
/// warning appears **while you're configuring the task**, rather than as a
/// failure at queue time — the point of picking an executor is a decision, and
/// a decision needs to know its option is available.
#[tauri::command]
pub async fn executor_available(executor: String) -> Result<Option<String>, AppError> {
    let executor = BackgroundExecutor::parse(&executor).map_err(AppError::Validation)?;
    // `Some(reason)` = unavailable. An Option rather than a Result because
    // "Codex isn't installed" is information for a label, not a failed call.
    Ok(Provider::for_executor(executor).preflight().err())
}

/// Drain the queue once, right now, without installing anything.
///
/// The middle option between "queued forever because the runner is off" and
/// "installing a LaunchAgent that runs unattended agents at every login from
/// now on". Clicking a per-task menu item should not silently be a decision
/// about the second — but it also shouldn't do nothing, which is what it did
/// before this existed.
///
/// Spawns `cxtasks-runner --once` detached and returns immediately; the run
/// reports itself back through the same bridge + notification path as a
/// LaunchAgent-driven one, so from here on it is indistinguishable.
#[tauri::command]
pub async fn run_queue_once() -> Result<String, AppError> {
    // Resolve as a sibling of the running executable, exactly as `launchd.rs`
    // does — in the installed bundle both binaries sit in `Contents/MacOS/`.
    let exe = std::env::current_exe()
        .map_err(|e| AppError::Dispatch(format!("could not locate the running executable: {e}")))?;
    let runner = exe
        .parent()
        .map(|d| d.join("cxtasks-runner"))
        .filter(|p| p.exists())
        .ok_or_else(|| {
            AppError::Dispatch(
                "cxtasks-runner was not found next to the app. The build step that copies it into \
                 the bundle was probably skipped — see the ship recipe in CLAUDE.md."
                    .into(),
            )
        })?;

    tokio::task::spawn_blocking(move || -> Result<(), AppError> {
        Command::new(&runner)
            .arg("--once")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| AppError::Dispatch(format!("could not start the runner: {e}")))?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::Dispatch(format!("spawn_blocking join error: {e}")))??;

    Ok("Draining the queue once.".into())
}

// ── background runner LaunchAgent ─────────────────────────────────────────

#[tauri::command]
pub async fn is_runner_installed() -> bool {
    crate::launchd::is_installed()
}

#[tauri::command]
pub async fn install_runner() -> Result<bool, AppError> {
    crate::launchd::install_launch_agent()?;
    Ok(crate::launchd::is_installed())
}

#[tauri::command]
pub async fn uninstall_runner() -> Result<bool, AppError> {
    crate::launchd::uninstall_launch_agent()?;
    Ok(crate::launchd::is_installed())
}

/// Ask an in-flight background run to stop.
///
/// **A request, not an instruction, and the return value says which.** The
/// runner notices at its next 250ms poll, kills the child, and records the run
/// as `cancelled` — but it may equally be a second from finishing on its own,
/// mid-salvage, or already gone. So this reports whether the run was still
/// `running` when asked, and nothing here waits for or confirms the kill.
///
/// Refusing a run that is already terminal matters more than it looks: writing
/// the flag anyway would leave a file addressed to a run id that will never
/// poll again, and the caller would have been told the stop was accepted.
#[tauri::command]
pub async fn cancel_run(state: State<'_, AppState>, run_id: String) -> Result<bool, AppError> {
    let run = {
        let conn = state.db.safe_lock();
        db::runs::get(&conn, &run_id)?
            .ok_or_else(|| AppError::NotFound(format!("run {run_id} not found")))?
    };
    if db::runs::is_terminal(&run.status) {
        return Ok(false);
    }
    // Foreground runs are terminal at `dispatched` (invariant 20) — the session
    // is a Ghostty window this process does not own, so there is no child here
    // to kill and pretending otherwise would be a button that does nothing.
    if run.status == "dispatched" {
        return Ok(false);
    }
    crate::paths::request_cancel(&run_id)
        .map_err(|e| AppError::General(format!("could not request the cancel: {e}")))?;
    log::info!("cancel requested for run {run_id}");
    Ok(true)
}

// ── checkpoints ───────────────────────────────────────────────────────────

/// What a write-enabled background run changed, as a diff against the
/// checkpoint taken immediately before it started.
#[tauri::command]
pub async fn run_diff(state: State<'_, AppState>, run_id: String) -> Result<String, AppError> {
    let (run, task) = {
        let conn = state.db.safe_lock();
        let run = db::runs::get(&conn, &run_id)?
            .ok_or_else(|| AppError::NotFound(format!("run {run_id} not found")))?;
        let task = db::tasks::get(&conn, &run.task_id)?
            .ok_or_else(|| AppError::NotFound("task not found".into()))?;
        (run, task)
    };
    // Before touching the local repo at all: this ref only exists on the
    // machine that made it.
    db::runs::ensure_local(&run)?;
    let ref_name = run
        .scratch_dir
        .as_deref()
        .filter(|s| s.starts_with(crate::checkpoint::REF_PREFIX))
        .ok_or_else(|| AppError::General("this run has no checkpoint".into()))?;
    let repo = crate::dispatch::resolve_repo(&task)?;
    crate::checkpoint::diff_stat(&repo, ref_name)
}

/// Roll the repo back to the checkpoint taken before a run.
///
/// **Destructive**: discards everything written since the checkpoint, including
/// anything the user changed by hand in the meantime. Only ever invoked from an
/// explicit confirmation in the UI.
#[tauri::command]
pub async fn restore_checkpoint(
    state: State<'_, AppState>,
    run_id: String,
) -> Result<String, AppError> {
    let (run, task) = {
        let conn = state.db.safe_lock();
        let run = db::runs::get(&conn, &run_id)?
            .ok_or_else(|| AppError::NotFound(format!("run {run_id} not found")))?;
        let task = db::tasks::get(&conn, &run.task_id)?
            .ok_or_else(|| AppError::NotFound("task not found".into()))?;
        (run, task)
    };
    // This one RESETS A WORKING TREE. Refusing a foreign run is the difference
    // between "nothing happened" and "your local tree was reset on the strength
    // of a run that happened on another machine".
    db::runs::ensure_local(&run)?;
    let ref_name = run
        .scratch_dir
        .as_deref()
        .filter(|s| s.starts_with(crate::checkpoint::REF_PREFIX))
        .ok_or_else(|| AppError::General("this run has no checkpoint to restore".into()))?;
    let repo = crate::dispatch::resolve_repo(&task)?;
    crate::checkpoint::restore(&repo, ref_name)?;
    Ok(format!("Restored {} to its pre-run state.", repo.display()))
}

#[cfg(test)]
mod herdr_tests {
    use super::*;

    fn pending_dispatch() -> (rusqlite::Connection, db::tasks::Task, db::runs::TaskRun) {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        db::schema::initialize(&conn).unwrap();
        let task = db::tasks::create(
            &conn,
            &db::tasks::NewTask {
                title: "A foreground handoff".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let run = db::runs::start(
            &conn,
            &task.id,
            "foreground",
            None,
            BackgroundExecutor::Claude,
            task.bg_intent,
            None,
        )
        .unwrap();
        db::runs::finish(
            &conn,
            &run.id,
            &db::runs::RunOutcome {
                status: db::runs::FOREGROUND_STATUS.into(),
                ..Default::default()
            },
        )
        .unwrap();
        (conn, task, run)
    }

    #[test]
    fn a_failed_route_leaves_the_task_alone_and_records_the_failure() {
        let (conn, task, run) = pending_dispatch();
        let result = record_foreground_launch(
            &conn,
            &task.id,
            &run.id,
            Err(AppError::Dispatch("Herdr unavailable".into())),
        );
        assert!(result.is_err());
        assert_eq!(
            db::tasks::get(&conn, &task.id).unwrap().unwrap().status,
            task.status
        );
        let run = db::runs::get(&conn, &run.id).unwrap().unwrap();
        assert_eq!(run.status, "failed");
        assert!(run.result_text.unwrap().contains("Herdr unavailable"));
    }

    #[test]
    fn a_confirmed_route_starts_the_task_with_a_terminal_dispatch_receipt() {
        let (conn, task, run) = pending_dispatch();
        record_foreground_launch(&conn, &task.id, &run.id, Ok(())).unwrap();
        assert_eq!(
            db::tasks::get(&conn, &task.id).unwrap().unwrap().status,
            "in_progress"
        );
        assert_eq!(
            db::runs::get(&conn, &run.id).unwrap().unwrap().status,
            db::runs::FOREGROUND_STATUS
        );
    }

    #[test]
    fn an_unconfirmed_route_cannot_overwrite_a_handoff_that_already_arrived() {
        let (conn, task, run) = pending_dispatch();
        db::runs::append_result(&conn, &run.id, "The session already answered").unwrap();
        let _ = record_foreground_launch(&conn, &task.id, &run.id,
            Err(AppError::Dispatch("Reply lost".into())));
        let run = db::runs::get(&conn, &run.id).unwrap().unwrap();
        assert_eq!(run.status, db::runs::FOREGROUND_STATUS);
        assert_eq!(run.result_text.as_deref(), Some("The session already answered"));
    }
}
