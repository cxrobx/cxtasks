//! Interactive Claude sessions in Herdr: the pieces the Mac app's
//! `dispatch_foreground` and the runner's session lane share, and the request
//! a phone makes for one ("Open on Mac mini").
//!
//! The app opens a session on the Mac it runs on, or on its Herdr host. A phone
//! can't open anything: it talks to a server (usually the NAS hub) that has no
//! Herdr and no checkouts. So [`request`] records a `requested` foreground run
//! addressed to the machine that runs the task, sync carries it there, and that
//! machine's runner opens it with [`open_here`] (`runner::session`).
//! `.claude/rules/execution.md` § Sessions requested from another device.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::db;
use crate::db::tasks::Task;
use crate::dispatch::{
    find_claude_cli, repo_on_host, resolve_repo_or_home, scratch_dir_for, write_launch_script,
    write_scratch, PromptTarget, ENV_HERDR_HOST, HOST_CLAUDE_CLI,
};
use crate::error::AppError;
use crate::routing::HostCapability;

/// The shared desktop router, when it is installed for this user.
pub fn herdr_router() -> Option<PathBuf> {
    let path = dirs::home_dir()?.join(".local/bin/cx-open-in-herdr");
    path.is_file().then_some(path)
}

/// Repo-less Herdr sessions have their own task directory. An explicit repo
/// still wins; the standalone Ghostty fallback retains its existing behavior.
///
/// With a Herdr host the repo is the host's business, so nothing here checks it
/// exists on this Mac. The scratch directory is also where the launch script
/// lives, so the helper's copy of that folder creates this cwd on the host.
pub fn foreground_repo(task: &Task, routed: bool, host: Option<&str>) -> Result<PathBuf, AppError> {
    let on_host = host.and(repo_on_host(task));
    if let Some(repo) = on_host {
        return Ok(repo);
    }
    if (routed || host.is_some())
        && task
            .repo_path
            .as_deref()
            .map_or(true, |p| p.trim().is_empty())
    {
        let scratch = scratch_dir_for(task);
        std::fs::create_dir_all(&scratch)?;
        Ok(scratch)
    } else {
        resolve_repo_or_home(task)
    }
}

/// The Herdr space a task's session opens in: its project, else CXTasks.
pub fn session_space(task: &Task) -> String {
    task.project.clone().unwrap_or_else(|| "CXTasks".into())
}

/// A repo-less task's explicit identity, so its sessions group by project (or
/// the Inbox) rather than by a scratch folder nobody recognises.
pub fn session_scope(task: &Task) -> Option<String> {
    task.repo_path
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .is_none()
        .then(|| format!("cxtasks:{}", task.project.as_deref().unwrap_or("inbox")))
}

/// The `cx-open-in-herdr` invocation. The environment is touched only for a
/// host, so a local handoff starts the helper exactly as it always has.
pub fn router_command(
    router: &Path,
    repo: &Path,
    launch: &Path,
    space: &str,
    scope: Option<&str>,
    host: Option<&str>,
) -> Command {
    let mut command = Command::new(router);
    command
        .arg("--cwd")
        .arg(repo)
        .arg("--launch")
        .arg(launch)
        .arg("--space")
        .arg(space);
    if let Some(scope) = scope {
        command.arg("--scope").arg(scope);
    }
    if let Some(host) = host {
        command.env(ENV_HERDR_HOST, host);
    }
    command
}

/// The same invocation for a machine nobody is sitting at.
///
/// `--headless` opens the tab through Herdr's socket and shows no terminal:
/// it fails rather than starting Herdr or a Ghostty window, and it ignores an
/// inherited `CX_HERDR_HOST`, so the session can only ever open HERE. That's
/// what the helper's remote half already runs as when a laptop's app sends a
/// session to the mini.
pub fn headless_router_command(
    router: &Path,
    repo: &Path,
    launch: &Path,
    space: &str,
    scope: Option<&str>,
) -> Command {
    let mut command = Command::new(router);
    command.arg("--headless");
    let built = router_command(router, repo, launch, space, scope, None);
    command.args(built.get_args());
    command
}

/// What [`request`] hands back: the row, and the sentence to show.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionRequest {
    /// The `task_runs` row, at status `requested`.
    pub run: db::runs::TaskRun,
    pub host_id: String,
    pub host_label: String,
    /// "Requested on Mac mini. …" — always present.
    pub message: String,
    /// Present when the machine looks asleep, so the request will probably
    /// lapse. The request is recorded anyway.
    pub warning: Option<String>,
}

/// The machine whose Herdr a session for `task` belongs in: the one it names
/// in Run on, else the one that takes unassigned work (invariant 30). The same
/// machine its background runs would use, so the session opens on the
/// checkout that work happens in.
pub fn target_host(conn: &rusqlite::Connection, task: &Task) -> Result<HostCapability, AppError> {
    let hosts = db::hosts::list(conn)?;
    let host = hosts
        .into_iter()
        .find(|h| h.is_mine(task.run_on.as_deref()))
        .ok_or_else(|| {
            AppError::Dispatch(match task.run_on.as_deref() {
                Some(_) => "This task is assigned to a machine that no longer exists. Pick \
                            another one in Run on."
                    .into(),
                None => "No machine takes unassigned work, so there's nowhere to open a \
                         session. Choose one in Settings, or pick one in this task's Run on."
                    .into(),
            })
        })?;
    if !host.is_local() {
        return Err(AppError::Dispatch(format!(
            "{} has no checkouts of its own, so it can't open an interactive session. \
             Pick a Mac in this task's Run on.",
            host.label
        )));
    }
    if !host.enabled {
        return Err(AppError::Dispatch(format!(
            "{} is switched off in CXTasks, so it won't open a session.",
            host.label
        )));
    }
    if host.paused {
        return Err(AppError::Dispatch(format!(
            "Background runs are paused on {}, so it won't open a session until they resume.",
            host.label
        )));
    }
    Ok(host)
}

/// The `request_session` verb: record that `task_ref` (a uuid or T-number)
/// should open in Herdr on the machine that runs it.
///
/// Refuses a task that is done, archived or in the Trash, and a target that
/// could never open one. A second request while one is waiting returns the
/// waiting one.
pub fn request(conn: &rusqlite::Connection, task_ref: &str) -> Result<SessionRequest, AppError> {
    let task = db::tasks::resolve_ref(conn, task_ref)?
        .ok_or_else(|| AppError::NotFound(format!("task {task_ref} not found")))?;
    let name = task
        .short_id
        .map(|n| format!("T{n}"))
        .unwrap_or_else(|| "This task".into());
    if task.deleted_at.is_some() {
        return Err(AppError::Validation(format!(
            "{name} is in the Trash. Restore it before opening a session on it."
        )));
    }
    if task.archived_at.is_some() {
        return Err(AppError::Validation(format!(
            "{name} is archived. Unarchive it before opening a session on it."
        )));
    }
    if db::tasks::CLOSED_STATUSES.contains(&task.status.as_str()) {
        return Err(AppError::Validation(format!(
            "{name} is done. Reopen it before opening a session on it."
        )));
    }
    let host = target_host(conn, &task)?;
    let run = db::runs::request_session(conn, &task.id, &host.id, task.bg_intent)?;
    let warning = (!host.is_alive(db::hosts::HOST_STALE_MINUTES)).then(|| {
        format!(
            "{} hasn't checked in recently, so its runner may be off. The request lapses \
             after {} minutes.",
            host.label,
            db::runs::SESSION_REQUEST_MINUTES
        )
    });
    Ok(SessionRequest {
        message: format!(
            "Requested on {}. It opens in Herdr there within a few minutes.",
            host.label
        ),
        warning,
        host_id: host.id,
        host_label: host.label,
        run,
    })
}

/// The longest the runner waits on the helper. A headless open is a few local
/// socket calls; a helper that hangs must not stop the runner's tick.
const ROUTER_TIMEOUT: Duration = Duration::from_secs(60);

/// Open `task` in a fresh Claude tab in THIS machine's Herdr, with no terminal.
/// Returns the scratch folder, or the reason nothing opened.
///
/// The same scratch folder, launch script and repo choice as the app's
/// dispatch, so a session reads the same however it was asked for.
pub fn open_here(task: &Task, label: &str) -> Result<PathBuf, String> {
    let router = herdr_router().ok_or_else(|| {
        format!("cx-open-in-herdr isn't installed on {label}, so nothing was opened.")
    })?;
    // launchd's PATH can't see the CLI, so find it through the login shell.
    // Failing that, the launch script is itself a login shell and resolves a
    // bare `claude` the way a terminal would.
    let cli = find_claude_cli().unwrap_or_else(|| PathBuf::from(HOST_CLAUDE_CLI));
    let repo = foreground_repo(task, true, None).map_err(|e| e.to_string())?;
    let scratch =
        write_scratch(task, &repo, PromptTarget::Foreground, None).map_err(|e| e.to_string())?;
    let launch = write_launch_script(&scratch, &repo, &cli).map_err(|e| e.to_string())?;
    let mut command = headless_router_command(
        &router,
        &repo,
        &launch,
        &session_space(task),
        session_scope(task).as_deref(),
    );
    command
        .env("PATH", crate::runner::providers::agent_path())
        .stdin(Stdio::null());
    run_bounded(command, ROUTER_TIMEOUT)?;
    Ok(scratch)
}

/// Run the helper, keeping its stderr as the reason when it fails.
fn run_bounded(mut command: Command, limit: Duration) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(command.output());
    });
    match rx.recv_timeout(limit) {
        Ok(Ok(output)) if output.status.success() => Ok(()),
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(format!(
                "Could not open Claude in Herdr: {}",
                if stderr.is_empty() { "the helper failed without saying why" } else { &stderr }
            ))
        }
        Ok(Err(e)) => Err(format!("Could not start cx-open-in-herdr: {e}")),
        // Not retried: a helper that went quiet may still have opened the tab.
        Err(_) => Err(format!(
            "cx-open-in-herdr didn't answer within {}s. Check Herdr before asking again.",
            limit.as_secs()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tasks::{create, NewTask};

    fn conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        db::schema::initialize(&conn).unwrap();
        conn
    }

    fn register(conn: &rusqlite::Connection, id: &str, label: &str, live: bool, default: bool) {
        let mut cap = HostCapability::local_default(id.into(), label.into());
        cap.takes_unassigned = default;
        if !live {
            cap.workspaces = vec![crate::routing::WorkspaceKind::Mirror];
        }
        db::hosts::register_self(conn, &cap).unwrap();
        if default {
            db::hosts::set_default(conn, id).unwrap();
        }
    }

    fn task(conn: &rusqlite::Connection, run_on: Option<&str>) -> Task {
        let t = create(conn, &NewTask { title: "Look at the flaky test".into(), ..Default::default() })
            .unwrap();
        if let Some(host) = run_on {
            conn.execute("UPDATE tasks SET run_on = ?2 WHERE id = ?1", rusqlite::params![t.id, host])
                .unwrap();
        }
        db::tasks::get(conn, &t.id).unwrap().unwrap()
    }

    fn args_of(command: &Command) -> Vec<String> {
        command.get_args().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_request_goes_to_the_default_machine_and_says_where() {
        let c = conn();
        register(&c, "mini", "Mac mini", true, true);
        register(&c, "laptop", "MacBook", true, false);
        let t = task(&c, None);
        let req = request(&c, &t.id).unwrap();
        assert_eq!(req.host_id, "mini");
        assert_eq!(req.run.status, db::runs::STATUS_REQUESTED);
        assert_eq!(req.run.mode, "foreground");
        assert_eq!(req.run.host.as_deref(), Some("mini"));
        assert!(req.run.finished_at.is_none());
        assert!(req.message.starts_with("Requested on Mac mini."), "{}", req.message);
    }

    #[test]
    fn run_on_beats_the_default_and_a_t_number_resolves() {
        let c = conn();
        register(&c, "mini", "Mac mini", true, true);
        register(&c, "laptop", "MacBook", true, false);
        let t = task(&c, Some("laptop"));
        let req = request(&c, &format!("T{}", t.short_id.unwrap())).unwrap();
        assert_eq!(req.host_id, "laptop");
    }

    #[test]
    fn a_double_tap_returns_the_waiting_request() {
        let c = conn();
        register(&c, "mini", "Mac mini", true, true);
        let t = task(&c, None);
        let a = request(&c, &t.id).unwrap();
        let b = request(&c, &t.id).unwrap();
        assert_eq!(a.run.id, b.run.id);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM task_runs", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn closed_archived_and_trashed_tasks_are_refused() {
        let c = conn();
        register(&c, "mini", "Mac mini", true, true);
        let done = task(&c, None);
        crate::recurrence::complete_with_recurrence(&c, &done.id, None).unwrap();
        assert!(request(&c, &done.id).unwrap_err().to_string().contains("is done"));

        let archived = task(&c, None);
        db::tasks::archive(&c, &archived.id).unwrap();
        assert!(request(&c, &archived.id).unwrap_err().to_string().contains("archived"));

        let trashed = task(&c, None);
        db::tasks::trash(&c, &trashed.id).unwrap();
        assert!(request(&c, &trashed.id).unwrap_err().to_string().contains("Trash"));

        let n: i64 = c.query_row("SELECT COUNT(*) FROM task_runs", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "a refusal records nothing");
    }

    #[test]
    fn a_machine_that_could_never_open_one_is_refused() {
        let c = conn();
        let t = task(&c, None);
        assert!(
            request(&c, &t.id).unwrap_err().to_string().contains("No machine takes unassigned"),
            "an empty registry"
        );

        register(&c, "mini", "Mac mini", true, true);
        register(&c, "nas", "the NAS", false, false);
        let on_nas = task(&c, Some("nas"));
        assert!(request(&c, &on_nas.id).unwrap_err().to_string().contains("no checkouts"));

        let gone = task(&c, Some("retired-host"));
        assert!(request(&c, &gone.id).unwrap_err().to_string().contains("no longer exists"));

        c.execute("UPDATE hosts SET paused = 1 WHERE id = 'mini'", []).unwrap();
        assert!(request(&c, &t.id).unwrap_err().to_string().contains("paused on Mac mini"));
    }

    #[test]
    fn a_machine_that_has_gone_quiet_gets_the_request_and_a_warning() {
        let c = conn();
        register(&c, "mini", "Mac mini", true, true);
        c.execute("UPDATE hosts SET last_seen_at = datetime('now', '-2 hours')", []).unwrap();
        let t = task(&c, None);
        let req = request(&c, &t.id).unwrap();
        assert!(req.warning.unwrap().contains("hasn't checked in"));
    }

    #[test]
    fn the_headless_command_is_the_app_command_plus_one_flag_and_no_host() {
        let local = router_command(
            Path::new("/bin/helper"),
            Path::new("/repo"),
            Path::new("/scratch/launch.sh"),
            "CXTasks",
            Some("cxtasks:inbox"),
            None,
        );
        let headless = headless_router_command(
            Path::new("/bin/helper"),
            Path::new("/repo"),
            Path::new("/scratch/launch.sh"),
            "CXTasks",
            Some("cxtasks:inbox"),
        );
        let mut expected = vec!["--headless".to_string()];
        expected.extend(args_of(&local));
        assert_eq!(args_of(&headless), expected);
        assert_eq!(headless.get_program(), "/bin/helper");
        assert_eq!(headless.get_envs().count(), 0, "never a Herdr host: it opens HERE");
    }

    #[test]
    fn space_and_scope_follow_the_project() {
        let c = conn();
        let mut t = task(&c, None);
        assert_eq!(session_space(&t), "CXTasks");
        assert_eq!(session_scope(&t).as_deref(), Some("cxtasks:inbox"));
        t.project = Some("Acme".into());
        assert_eq!(session_space(&t), "Acme");
        assert_eq!(session_scope(&t).as_deref(), Some("cxtasks:Acme"));
        t.repo_path = Some("/r/acme".into());
        assert_eq!(session_scope(&t), None, "a repo is its own identity");
    }

    #[test]
    fn a_failing_helper_reports_its_stderr_and_a_hung_one_is_cut_off() {
        let mut fails = Command::new("/bin/sh");
        fails.args(["-c", "echo 'Herdr is not running' >&2; exit 1"]);
        assert_eq!(
            run_bounded(fails, Duration::from_secs(10)).unwrap_err(),
            "Could not open Claude in Herdr: Herdr is not running"
        );
        let mut hangs = Command::new("/bin/sh");
        hangs.args(["-c", "sleep 5"]);
        assert!(run_bounded(hangs, Duration::from_millis(200)).unwrap_err().contains("didn't answer"));
        assert!(run_bounded(Command::new("/usr/bin/true"), Duration::from_secs(10)).is_ok());
    }
}
