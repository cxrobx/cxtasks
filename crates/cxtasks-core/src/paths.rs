//! One place that resolves every shared on-disk location.
//!
//! Three processes share this app's state — the Tauri app, `cxtasks-mcp`, and
//! `cxtasks-runner`. cxmail resolved `dirs::data_dir()/com.cxmail.app/...`
//! independently in each binary, which works but drifts. Everything here is
//! derived from one constant.
//!
//! # The override, and why there is exactly one
//!
//! [`ENV_DATA_DIR`] moves the whole root. It exists because this crate is being
//! split so its non-UI half can run somewhere that has no
//! `~/Library/Application Support` — a Linux container, or a second machine —
//! and because a test that wants a throwaway data dir currently has nowhere to
//! put one.
//!
//! It overrides the **root**, never an individual path, and that is the whole
//! point. `CXTASKS_DB_PATH` + `CXTASKS_ATTACHMENTS_DIR` + `CXTASKS_SESSIONS_DIR`
//! would be exactly the drift this module exists to prevent: a container whose
//! database is on the persistent volume and whose attachments are in the
//! ephemeral layer, with nothing on screen saying so. One knob moves everything
//! together, or nothing moves.

use std::path::PathBuf;
use std::sync::OnceLock;

pub const BUNDLE_ID: &str = "com.cxtasks.app";

/// Moves the entire app-data root. See the module note.
pub const ENV_DATA_DIR: &str = "CXTASKS_DATA_DIR";

/// The root, as a pure function of the environment variable's value.
///
/// Pure on purpose. `app_data_dir` is memoized in a process-wide `OnceLock` and
/// the environment is process-global, so a test that set `CXTASKS_DATA_DIR` and
/// called the real resolver would race every other test in the binary — and
/// would race *intermittently*, which is the worst kind. Taking the value as an
/// argument means the override's behaviour is testable with no mutation at all.
///
/// An empty value reads as absent rather than as "the root is `""`", because an
/// unset variable and one set to nothing are the same intent, and the latter is
/// what a shell script produces when a substitution comes back empty.
fn root_from(env_value: Option<std::ffi::OsString>) -> PathBuf {
    match env_value {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(BUNDLE_ID),
    }
}

fn resolve_root() -> PathBuf {
    root_from(std::env::var_os(ENV_DATA_DIR))
}

/// Whether the root was overridden — the one thing [`log_dir`] needs to know.
fn root_is_overridden() -> bool {
    std::env::var_os(ENV_DATA_DIR).is_some_and(|v| !v.is_empty())
}

/// `~/Library/Application Support/com.cxtasks.app`, or [`ENV_DATA_DIR`].
///
/// **Memoized deliberately.** The environment is read once per process, so a
/// variable changed mid-flight cannot move the database out from under a
/// migration that is halfway through it.
pub fn app_data_dir() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(resolve_root).clone()
}

pub fn db_path() -> PathBuf {
    app_data_dir().join("cxtasks.db")
}

/// Host-local TypeSafe API credential used by the optional Jev integration.
///
/// A file rather than `app_meta`: settings may be synced deliberately, while a
/// bearer token must never enter a snapshot or an intent payload. The writer
/// in `jev` creates it owner-only on Unix.
pub fn jev_api_key_path() -> PathBuf {
    app_data_dir().join("typesafe-api-key")
}

/// This machine's identity, as a file.
///
/// Under the data root like everything else, but see [`crate::host`] for why it
/// is a FILE and emphatically not a row in `app_meta`.
pub fn host_id_path() -> PathBuf {
    app_data_dir().join("host_id")
}

/// Unix socket the MCP/runner write live-refresh envelopes to.
pub fn socket_path() -> PathBuf {
    app_data_dir().join("ipc.sock")
}

/// One of this app's other binaries, resolved beside the running one.
///
/// In production all three sit together in `CXTasks.app/Contents/MacOS`; under
/// `tauri dev` they are siblings in `target/debug`. Here rather than in each
/// caller for the reason the rest of this module exists (invariant 8): the
/// runner needs `cxtasks-mcp` and `launchd` needs `cxtasks-runner`, and two
/// hand-rolled copies of "parent of current_exe" is exactly the drift this file
/// prevents. Returns a path that may not exist — callers check, because
/// "the binary wasn't copied into the bundle" is a real and recurring state
/// (gotcha #5).
pub fn sibling_binary(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent().unwrap_or(std::path::Path::new(".")).join(name)
}

/// The same helper inside the INSTALLED app bundle, if CXTasks is installed.
///
/// The counterpart to [`sibling_binary`], and it exists for exactly one reason:
/// macOS attributes a notification to the bundle the POSTING PROCESS lives in.
/// A `tauri dev` build is a bare binary in `target/debug`, so it has no bundle
/// to be attributed to and its banners came out branded "Script Editor" — the
/// osascript host that posted them on its behalf. Borrowing the installed
/// bundle's identity for the half-second it takes to post is the only way an
/// unbundled build can put a banner on screen under our own name and icon.
///
/// `/Applications` first, then a per-user install. `None` means CXTasks has
/// never been installed, which is a real state on a fresh checkout — the caller
/// falls back rather than failing.
pub fn installed_binary(name: &str) -> Option<PathBuf> {
    let roots = [
        PathBuf::from("/Applications/CXTasks.app"),
        dirs::home_dir()?.join("Applications/CXTasks.app"),
    ];
    roots
        .into_iter()
        .map(|root| root.join("Contents/MacOS").join(name))
        .find(|p| p.exists())
}

/// Per-task scratch dirs for dispatched sessions.
pub fn sessions_dir() -> PathBuf {
    app_data_dir().join("claude-sessions")
}

/// Managed copies of images attached to task notes and comments. Keeping them
/// under app data means an attachment survives when the source file in
/// Downloads/Desktop is moved or deleted.
pub fn attachments_dir() -> PathBuf {
    app_data_dir().join("attachments")
}

/// Partial downloads, while they are still partial.
///
/// A subdirectory rather than a suffix, and that placement is the whole point:
/// [`crate::attachments::prune_orphaned_files`] walks `attachments_dir()` and
/// skips anything that is not a file, so a download in flight is structurally
/// invisible to it. A `.part` file sitting beside the finished ones would be
/// untracked — which is precisely what prune collects — and another process
/// starting up would delete it out from under the process writing it.
///
/// The leading dot keeps it out of the way of anything listing the directory
/// for images; the rename out of here into `attachments_dir()` is same-volume
/// and therefore atomic, which is what makes "the file exists" mean "the file
/// is complete".
pub fn attachments_incoming_dir() -> PathBuf {
    attachments_dir().join(ATTACHMENTS_INCOMING)
}

/// The subdirectory's name, so the two places that build the path agree.
pub const ATTACHMENTS_INCOMING: &str = ".incoming";

/// Which MCP servers THIS machine can provide, as `{ "name": "cmd args" }`.
///
/// A file rather than a compiled-in path, because where the vault MCP lives is
/// a fact about one machine — `~/Projects/vault-mcp/.venv/bin/python` on this
/// Mac, a mounted path in a container — and a default baked into shipped source
/// would be wrong everywhere but here.
///
/// Beside the database rather than in a dotfile, so it moves with
/// `CXTASKS_DATA_DIR` like everything else (invariant 8) and a throwaway root
/// gets a throwaway server list. Host-local by construction: it is never in
/// `sync::tables`, because "where a binary lives" must not travel to a machine
/// where that path means nothing.
///
/// Absent is the normal case and grants nothing. See [`crate::mcp_registry`]
/// for why an empty grant is the safe default rather than an unfinished one.
pub fn mcp_servers_path() -> PathBuf {
    app_data_dir().join("mcp-servers.json")
}

/// Where a task's `script:<name>` tripwire is resolved.
///
/// Beside `mcp-servers.json`, and for the same reason: which executables exist
/// on a machine is a fact about that machine. A task row carries only a NAME
/// (`tasks.tripwire = "script:pocketbuddy-first-user"`), never a path and never
/// a command line, so a row synced from the phone can name a script only the
/// Mac has — and the Mac's runner is the only thing that will ever look here.
/// Under the data root so `CXTASKS_DATA_DIR` moves it with everything else
/// (invariant 8), which is also what lets a test point it at a temp directory.
///
/// The directory is trusted: anything in it runs unattended. `tripwire::script_path`
/// refuses symlinks and anything that canonicalizes outside it, so a name can
/// never reach a file the user did not put here on purpose.
pub fn tripwires_dir() -> PathBuf {
    app_data_dir().join("tripwires")
}

/// Names an alternative mirror store, for a container that mounts one.
pub const ENV_MIRRORS_DIR: &str = "CXTASKS_MIRRORS_DIR";

/// Where bare mirrors of other machines' repos live.
///
/// Its own env var rather than a subdirectory of the data dir, because on a
/// remote host the two want opposite mount options: the data dir is written
/// constantly, and `/mirrors` is mounted **read-only** so that a run
/// materialized from it cannot corrupt the source everybody shares.
/// `git clone --shared` is what makes read-only workable — the clone borrows
/// the object store instead of copying it.
pub fn mirrors_dir() -> PathBuf {
    match std::env::var_os(ENV_MIRRORS_DIR) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => app_data_dir().join("mirrors"),
    }
}

/// The throwaway working tree for one run, materialized from a mirror.
///
/// Per RUN, not per task: two runs of one task must not share a directory, for
/// the same reason they must not share a checkpoint ref. Under the data dir
/// because that is the one place a container is guaranteed to have writable.
pub fn run_workspace(run_id: &str) -> PathBuf {
    let safe: String = run_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    app_data_dir().join("runs").join(safe)
}

/// **Global kill switch for background runs.** Presence of this file stops the
/// runner at its next tick. Deliberately a file, not a DB row or a UI toggle:
/// it has to be trippable from a terminal without launching the app, while a
/// run is in flight.
///
/// `touch ~/Library/Application\ Support/com.cxtasks.app/STOP_BACKGROUND`
pub fn kill_switch_path() -> PathBuf {
    app_data_dir().join("STOP_BACKGROUND")
}

pub fn background_paused() -> bool {
    kill_switch_path().exists()
}

/// **Per-run cancel flag.** Presence of this file stops ONE in-flight run at
/// the runner's next 250ms poll.
///
/// A file for the same reasons the global switch above is one, plus a third
/// that decides it here. The app and the runner are separate processes, so the
/// app cannot signal the child directly. The obvious alternative — a
/// `cancel_requested` column the poll loop reads — would put a SQLite read on
/// that loop every 250ms for every live run, and `process.rs` is deliberately
/// free of DB access: its own comment warns that a busy DB must never stall the
/// path that drains the child's pipes, because a full pipe deadlocks the child.
/// A `stat` cannot take a lock. It also means this needs no migration —
/// `cancelled` has been a valid `task_runs` status since the table existed and
/// simply had nothing writing it.
pub fn cancel_flag_path(run_id: &str) -> PathBuf {
    app_data_dir().join(format!("CANCEL_{run_id}"))
}

pub fn cancel_requested(run_id: &str) -> bool {
    cancel_flag_path(run_id).exists()
}

/// Ask for a run to stop. Writing the file IS the request — there is no
/// acknowledgement, because the runner may be mid-poll, on another machine's
/// idea of the clock, or already finished.
pub fn request_cancel(run_id: &str) -> std::io::Result<()> {
    if let Some(dir) = cancel_flag_path(run_id).parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(cancel_flag_path(run_id), b"")
}

/// Always called when a run ends, cancelled or not. A flag that outlives its
/// run is a landmine: run ids are unique, so it would never fire again, but it
/// would accumulate one file per cancellation forever.
pub fn clear_cancel(run_id: &str) {
    let _ = std::fs::remove_file(cancel_flag_path(run_id));
}

/// `~/Library/Logs/CXTasks` — the one path NOT derived from the root, because
/// macOS wants logs in a place of its own choosing and the LaunchAgent plist
/// names it literally.
///
/// When [`ENV_DATA_DIR`] is set there is no such place to want, so logs join
/// the rest of the state under the root. That keeps the override's promise:
/// everything moves together.
pub fn log_dir() -> PathBuf {
    if root_is_overridden() {
        return app_data_dir().join("logs");
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/Logs/CXTasks")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Invariant 8, as a relationship rather than a literal.
    ///
    /// This half holds whether or not the root was overridden, which is exactly
    /// what makes it the useful half: it says every shared path derives from
    /// one place, without asserting where that place is.
    #[test]
    fn paths_share_one_parent() {
        let root = app_data_dir();
        for p in [
            db_path(),
            socket_path(),
            sessions_dir(),
            attachments_dir(),
            attachments_incoming_dir(),
            tripwires_dir(),
            kill_switch_path(),
            cancel_flag_path("some-run-id"),
        ] {
            assert!(p.starts_with(&root), "{p:?} escaped {root:?}");
        }
    }

    /// The literal shape, which only holds when nothing overrode the root.
    ///
    /// Split out from the relationship test above rather than deleted: the
    /// default layout is still the one every documented `sqlite3` incantation
    /// in `.claude/rules/database.md` depends on, so it is worth pinning — just
    /// not worth failing a container's test run over.
    #[test]
    fn the_default_root_is_the_bundle_id_under_application_support() {
        if std::env::var_os(ENV_DATA_DIR).is_some() {
            return; // an overridden root has no literal shape to assert
        }
        assert!(db_path().ends_with("com.cxtasks.app/cxtasks.db"));
        assert!(socket_path().ends_with("com.cxtasks.app/ipc.sock"));
    }

    /// The override moves the root, and therefore moves everything.
    ///
    /// Pure — `root_from` takes the value instead of reading it, so this does
    /// not touch process env and cannot race the rest of the suite.
    #[test]
    fn the_override_replaces_the_root_outright() {
        let overridden = root_from(Some("/var/lib/cxtasks".into()));
        assert_eq!(overridden, PathBuf::from("/var/lib/cxtasks"));
        assert!(
            !overridden.ends_with(BUNDLE_ID),
            "the override is the root itself, not a parent to hang {BUNDLE_ID} off"
        );
    }

    /// Absent and empty are the same intent.
    ///
    /// A shell substitution that comes back empty (`CXTASKS_DATA_DIR=$UNSET`)
    /// must not resolve the root to `""`, which would put the database in the
    /// process's working directory — silently, and somewhere different for
    /// every caller.
    #[test]
    fn an_empty_override_reads_as_absent_not_as_the_current_directory() {
        let default = root_from(None);
        assert_eq!(root_from(Some("".into())), default);
        assert!(default.ends_with(BUNDLE_ID));
    }

    /// A name that isn't in the installed bundle must read as "no install",
    /// not as a path that fails later at spawn time.
    ///
    /// The notification router branches on exactly this: `Some` means run that
    /// executable to post the banner, `None` means fall back to osascript. A
    /// path that merely doesn't exist yet would send it to spawn a ghost and
    /// report a failure indistinguishable from a real one (gotcha #34).
    #[test]
    fn an_absent_bundled_binary_reads_as_no_install() {
        assert!(installed_binary("cxtasks-not-a-real-binary").is_none());
    }
}
