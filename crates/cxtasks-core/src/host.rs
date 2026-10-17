//! Which machine is this?
//!
//! Nothing needs this yet with one Mac. It exists because the moment there are
//! two, several things that are currently correct become actively destructive —
//! `sweep_abandoned` reclaiming another host's live run, the concurrency gate
//! counting rows instead of local processes — and all of them need a stable
//! answer to "is that row mine".
//!
//! # Why a file and not `app_meta`
//!
//! This is the whole design decision, and getting it wrong is unrecoverable.
//!
//! `app_meta` is per-DATABASE. The database gets copied between machines: that
//! is what `sqlite3 .backup` before the sync cutover produces, what restoring
//! from Time Machine produces, and what seeding a second host from the first
//! produces. Every one of those copies would carry the original machine's
//! identity, and the moment the copy is opened both machines claim to be the
//! same host.
//!
//! What follows from that is not a cosmetic mix-up. Two hosts sharing an id
//! means each one's sweep treats the other's live runs as its own to reclaim,
//! and the concurrency gate double-counts. A file beside the database, created
//! once per machine, cannot be copied by any of those operations, because none
//! of them copy anything but the database.
//!
//! [`ENV_HOST_ID`] overrides it, for the container — whose data dir may be a
//! fresh volume on every start, and which should keep its identity across that.

use std::io::Write;
use std::path::Path;
use std::sync::OnceLock;

/// Names this host explicitly, skipping the file.
pub const ENV_HOST_ID: &str = "CXTASKS_HOST_ID";

/// This machine's stable id, created once and never changed.
///
/// Memoized: read once per process, so a file deleted mid-flight cannot make
/// two halves of the same run disagree about who ran it.
pub fn host_id() -> String {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let from_env = std::env::var_os(ENV_HOST_ID)
            .map(|v| v.to_string_lossy().trim().to_string())
            .filter(|v| !v.is_empty());
        resolve(&crate::paths::host_id_path(), from_env)
    })
    .clone()
}

/// The resolution, as a function of its inputs, so it is testable without
/// touching process env or the real data dir.
pub fn resolve(path: &Path, from_env: Option<String>) -> String {
    if let Some(id) = from_env {
        return id;
    }
    if let Ok(existing) = std::fs::read_to_string(path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    create(path)
}

/// Mint a new id and publish it, tolerating a race with another process.
///
/// Three binaries can start at once — the app, the runner tick, and an MCP
/// spawned by a Claude session — and on a fresh machine all three find no file.
/// Exactly one must win, and the losers must read what the winner wrote.
///
/// # Why write-then-link rather than `create_new`
///
/// `create_new` looks like the whole answer and is not. It creates an **empty**
/// file and returns a handle; the id is written a moment later. A loser that
/// hits `AlreadyExists` inside that window reads an empty file, falls through
/// its own "nothing there" branch, and returns the id it had minted for itself.
/// Two processes then hold different ids for one machine — which is precisely
/// the failure this module exists to prevent, arriving through the mechanism
/// meant to prevent it.
///
/// It is not theoretical: `tests::racing_processes_agree_on_one_id` reproduced
/// it about once in sixty runs, and on a real machine the window is widest at
/// exactly the moment three processes start together on a fresh install.
///
/// So the id is written to a private temp file FIRST and only then linked into
/// place. `hard_link` fails with `AlreadyExists` if the name is taken, so it is
/// still exactly-one-winner — but the winning name is complete from the instant
/// it exists, so there is no window for a loser to read half of it.
fn create(path: &Path) -> String {
    let fresh = new_id();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Same directory as the target, so the link is within one filesystem — and
    // unique per CALL, not per process. Keyed on the pid, eight threads of one
    // process share a staging path and clobber each other's, which the racing
    // test caught immediately: one thread removes the file another is about to
    // link. Reusing the id being published costs nothing and cannot collide.
    let staging = path.with_extension(format!("tmp-{fresh}"));
    if let Err(e) = std::fs::write(&staging, fresh.as_bytes()) {
        // A read-only data dir, most likely. Staying up with a per-process id
        // is worse than useless — every run would look like it came from a
        // different host — but refusing to start is worse still, so log loudly
        // and carry on unstably.
        log::warn!(
            "could not stage a host id at {} ({e}); this process's host id is not persistent",
            staging.display()
        );
        return fresh;
    }

    let result = match std::fs::hard_link(&staging, path) {
        Ok(()) => fresh,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Somebody beat us by microseconds. Theirs is the real one, and it
            // is whole — that is what the link bought.
            std::fs::read_to_string(path)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or(fresh)
        }
        Err(e) => {
            log::warn!(
                "could not publish a host id to {} ({e}); this process's host id is not persistent",
                path.display()
            );
            fresh
        }
    };
    let _ = std::fs::remove_file(&staging);
    result
}

/// A UUID v4, without taking a uuid dependency into this module's API.
fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cxtasks-host-{}", new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn an_id_is_created_once_and_then_reused() {
        let dir = tmpdir();
        let path = dir.join("host_id");
        let first = resolve(&path, None);
        let second = resolve(&path, None);
        assert_eq!(first, second, "the second call must read, not re-mint");
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), first);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The container case: a fresh volume every start must not mean a fresh
    /// identity every start.
    #[test]
    fn the_env_override_wins_and_writes_nothing() {
        let dir = tmpdir();
        let path = dir.join("host_id");
        assert_eq!(resolve(&path, Some("nas-1".into())), "nas-1");
        assert!(!path.exists(), "an explicit id needs no file");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A half-written or truncated file must not hand back an empty id, which
    /// would compare equal across every host and defeat the entire point.
    #[test]
    fn an_empty_file_is_replaced_rather_than_returned() {
        let dir = tmpdir();
        let path = dir.join("host_id");
        std::fs::write(&path, "   \n").unwrap();
        let id = resolve(&path, None);
        assert!(!id.trim().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Concurrent first-start: exactly one id, however many processes race.
    ///
    /// This is the failure the `create_new` is for. On a fresh machine the app,
    /// the runner and an MCP can all find no file at the same moment; if each
    /// minted its own, their run rows would disagree about who owns them.
    /// The race that `create_new` did NOT close — see [`create`].
    ///
    /// Eight threads is enough to hit it: against the old implementation this
    /// failed about once in sixty runs, which is exactly the frequency that
    /// gets a failure dismissed as flaky. Against the current one it is
    /// structurally impossible, because the file is complete before it has a
    /// name.
    #[test]
    fn racing_processes_agree_on_one_id() {
        let dir = tmpdir();
        let path = dir.join("host_id");
        let ids: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| s.spawn(|| resolve(&path, None)))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            1,
            "eight racing starts produced more than one host id: {ids:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
