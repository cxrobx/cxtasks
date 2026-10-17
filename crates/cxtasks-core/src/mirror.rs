//! Getting a repo to a machine that does not have it.
//!
//! # A mirror transports a COMMIT, never a dirty tree
//!
//! The obvious approach is to copy the working directory — rsync, a tar, a
//! bind mount. Every one of them has to answer "what about the files git is
//! ignoring", "what about the ones half-written right now", and "how do I know
//! it arrived intact", and every answer is a thing to maintain.
//!
//! A checkpoint commit already *is* the uncommitted tree, as a real git commit
//! object (see [`crate::checkpoint::capture_to`]). So the mirror pushes that
//! commit, and everything git knows about moving commits — deltas, integrity,
//! resumability, deduplication against what the far end already has — applies
//! without being reimplemented.
//!
//! # Three properties that produce confidently wrong findings
//!
//! An agent working in a materialized mirror is in a tree that differs from the
//! Mac's in ways it cannot see, and each one reads as a *finding* rather than
//! as an artefact:
//!
//! 1. **`.gitignore`d files are absent.** `.env`, `node_modules`, build output.
//!    An agent will report "the project has no environment configuration" as a
//!    discovery.
//! 2. **Files untracked on the Mac are TRACKED here**, because `add -A` staged
//!    them into the snapshot. So `git status` reads clean on work in progress,
//!    and an agent concludes the tree is committed when it is not.
//! 3. **Submodules are absent.**
//!
//! None of this can be fixed by being cleverer about the transport — it is what
//! a snapshot IS. So it goes in the prompt, via [`MIRROR_CONTEXT`], and that is
//! the load-bearing part of this module rather than an afterthought to it.
//!
//! # Never `--mirror`
//!
//! `git push --mirror` makes the far end match the near end exactly, which
//! includes **deleting refs the near end does not have** — and the sync refs
//! are exactly that. One `--mirror` push would delete every snapshot the remote
//! is holding, including the one a run is executing against right now. The
//! explicit refspec below is not a stylistic preference.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::AppError;

/// Where a mirror's snapshots live. Deliberately NOT
/// [`crate::checkpoint::REF_PREFIX`]: a checkpoint is "undo this run" and a
/// snapshot is "here is the tree to work in". Sharing a namespace would mean a
/// checkpoint cleanup could delete a tree a remote run is standing in.
pub const SYNC_REF: &str = "refs/cxtasks/sync/snapshot";

/// The paragraph a remote run's prompt must carry.
///
/// Kept here, next to the transport that creates the condition, rather than in
/// the prompt builder — the three facts are properties of how a mirror is made,
/// and the person editing that is the person who needs to update this.
pub const MIRROR_CONTEXT: &str = "\
You are working in a MIRROR of this repository, not the original checkout. The \
tree is a faithful snapshot of the developer's working directory, taken at the \
time noted above, with three differences you must account for:

1. Files matched by .gitignore are ABSENT — .env files, node_modules, build \
   output, local caches. Their absence is an artefact of how you received this \
   tree. It is not a finding, and you must not report the project as lacking \
   configuration it simply did not ship to you.
2. Files that are untracked in the developer's checkout appear TRACKED here, \
   because the snapshot staged them. `git status` will therefore read clean \
   even where work is genuinely in progress. Do not conclude from a clean \
   status that everything is committed upstream.
3. Submodules are ABSENT.

Do not run `git push`, and do not treat this checkout's branch state as \
authoritative for the developer's.";

/// A stable directory name for a repo path.
///
/// Basename plus a short hash of the ABSOLUTE path, because basenames collide
/// constantly in a projects folder — `~/Projects/artisttracker/web` and
/// `~/clients/initech/web` are both `web`, and one mirror serving both would hand
/// an agent the wrong codebase while looking entirely healthy.
pub fn repo_key(repo: &Path) -> String {
    let abs = repo
        .canonicalize()
        .unwrap_or_else(|_| repo.to_path_buf())
        .to_string_lossy()
        .to_string();
    let base = repo
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "repo".to_string());
    let safe: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();

    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(abs.as_bytes());
    format!("{safe}-{}", &hex::encode(digest)[..8])
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, AppError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        // A push must never stop to ask. With no terminal there is nobody to
        // answer, so it would hang until the run's wall clock killed it and
        // report as a timeout rather than as a credential problem.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        .env("GIT_AUTHOR_NAME", "CXTasks")
        .env("GIT_AUTHOR_EMAIL", "cxtasks@localhost")
        .env("GIT_COMMITTER_NAME", "CXTasks")
        .env("GIT_COMMITTER_EMAIL", "cxtasks@localhost")
        .output()
        .map_err(|e| AppError::General(format!("git {args:?} failed to start: {e}")))?;
    if !out.status.success() {
        return Err(AppError::General(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// What a mirror push produced, so the run row can say what it is standing in.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub repo_key: String,
    pub sha: String,
    /// When it was taken, ISO-8601 UTC.
    pub taken_at: String,
    /// How many files were dirty at the time. Reported to the agent because
    /// "7 uncommitted files included" is the difference between trusting the
    /// tree and wondering about it.
    pub dirty_files: usize,
}

/// Count what `git status --porcelain` considers changed.
///
/// Also the cheap change detector: hashing this output plus HEAD is how the
/// runner decides whether a mirror is worth re-pushing, without walking the
/// tree itself.
pub fn dirty_count(repo: &Path) -> Result<usize, AppError> {
    let out = git(repo, &["status", "--porcelain"])?;
    Ok(out.lines().filter(|l| !l.trim().is_empty()).count())
}

/// A cheap fingerprint of "has anything changed since last time".
///
/// # What this is FOR, which decides how exact it has to be
///
/// It gates the **background** refresh — the 20-second tick that keeps a mirror
/// warm so a run does not have to wait for a push. It is not what makes a run
/// correct: [`crate::dispatch`] pushes at queue time regardless, so a
/// fingerprint that misses a change delays a refresh that nothing was waiting
/// on. That asymmetry is why an approximation is acceptable here and would not
/// be one line further down.
///
/// # Why the obvious version was wrong
///
/// HEAD plus `git status --porcelain` was the first attempt, and a test caught
/// it in about a minute: porcelain reports the *names* of changed files, not
/// their contents, so editing a file that was already dirty produced an
/// identical fingerprint. In a repo where somebody is actively working — which
/// is every repo this runs against — that is the common case, not the edge one.
///
/// So it also hashes `git diff HEAD`, which carries the content of every
/// tracked modification, and stats each untracked file.
///
/// **What it can still miss:** an untracked file rewritten to exactly the same
/// length within the same second. `git status` can't see it either. The
/// queue-time push closes it.
///
/// Not a tree hash, which would be exact: `write-tree` against a cold index
/// re-hashes every file in the repo, and this runs every 20 seconds on every
/// mirrored repo whether or not anything has changed.
pub fn fingerprint(repo: &Path) -> Result<String, AppError> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();

    h.update(git(repo, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "no-head".into()).as_bytes());
    h.update(b"\n");

    let status = git(repo, &["status", "--porcelain"])?;
    h.update(status.as_bytes());
    h.update(b"\n");

    // Content of every tracked modification, staged or not.
    h.update(git(repo, &["diff", "HEAD"]).unwrap_or_default().as_bytes());
    h.update(b"\n");

    // Untracked files have no diff to read, so stat them. `??` is porcelain's
    // untracked marker; the paths are relative to the repo root.
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("?? ") else {
            continue;
        };
        let path = repo.join(rest.trim().trim_matches('"'));
        h.update(rest.as_bytes());
        if let Ok(meta) = std::fs::metadata(&path) {
            h.update(meta.len().to_le_bytes());
            if let Ok(modified) = meta.modified() {
                if let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH) {
                    h.update(since.as_nanos().to_le_bytes());
                }
            }
        }
    }
    Ok(hex::encode(h.finalize()))
}

/// Snapshot the tree and push it to a bare mirror.
///
/// `remote` is anything git can push to — an ssh URL for the NAS, a local path
/// in a test. That is what lets every test here run with **zero network**.
pub fn push(repo: &Path, remote: &str) -> Result<Snapshot, AppError> {
    let dirty_files = dirty_count(repo)?;
    let sha = crate::checkpoint::capture_to(repo, SYNC_REF)?;

    // NEVER `--mirror`. See the module docs: it would delete the snapshot refs
    // the far end is holding, including one a run may be executing against.
    //
    // The leading `+` on each side forces a non-fast-forward update, which is
    // required: a snapshot's parent is HEAD, so an amended commit or a rebase
    // on the Mac makes the next snapshot unrelated to the last one.
    git(
        repo,
        &[
            "push",
            remote,
            "+refs/heads/*:refs/heads/*",
            &format!("+{SYNC_REF}:{SYNC_REF}"),
        ],
    )?;

    Ok(Snapshot {
        repo_key: repo_key(repo),
        sha,
        taken_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        dirty_files,
    })
}

/// Which snapshot a mirror is currently holding.
pub fn snapshot_sha(bare: &Path) -> Result<String, AppError> {
    let sha = git(bare, &["rev-parse", SYNC_REF])?;
    if sha.is_empty() {
        return Err(AppError::General(format!(
            "{} has no snapshot ref yet",
            bare.display()
        )));
    }
    Ok(sha)
}

/// What a mirror can say about a snapshot it is holding.
///
/// Read from the commit rather than carried across from the pusher, because the
/// side that materializes it is a different process on a different machine and
/// the only thing it is certain to have is the object itself. `dirty_files` is
/// unknowable here — it was a fact about the source tree at capture time — so
/// it reads 0 and the caller may fill it in if it knows better.
pub fn describe(bare: &Path, sha: &str) -> Snapshot {
    let taken_at = git(bare, &["show", "-s", "--format=%cI", sha]).unwrap_or_default();
    Snapshot {
        repo_key: bare
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        sha: sha.to_string(),
        taken_at,
        dirty_files: 0,
    }
}

/// Make a working tree from a mirror, for one run.
///
/// `--shared` rather than a full copy, which is what lets `/mirrors` be mounted
/// **read-only** in the container: the clone borrows the mirror's object store
/// instead of duplicating it, so materializing a 300 MB repo costs a checkout
/// rather than a second 300 MB. It also means the run's directory is disposable
/// — nothing in it is the only copy of anything.
///
/// `--no-checkout` then `checkout --detach <sha>` rather than cloning a branch:
/// the snapshot is not on any branch, and detaching says so. An agent that runs
/// `git branch` sees the truth rather than a fiction about which branch it is
/// on.
pub fn materialize(mirror: &Path, sha: &str, into: &Path) -> Result<PathBuf, AppError> {
    if into.exists() {
        return Err(AppError::General(format!(
            "{} already exists; a run's workspace must be fresh",
            into.display()
        )));
    }
    if let Some(parent) = into.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AppError::General(format!("could not create {}: {e}", parent.display()))
        })?;
    }
    let out = Command::new("git")
        .arg("clone")
        .arg("--shared")
        .arg("--no-checkout")
        .arg(mirror)
        .arg(into)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| AppError::General(format!("git clone failed to start: {e}")))?;
    if !out.status.success() {
        return Err(AppError::General(format!(
            "git clone {} failed: {}",
            mirror.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    git(into, &["checkout", "--detach", sha])?;
    Ok(into.to_path_buf())
}

/// Create the bare repo a mirror pushes into. Idempotent.
///
/// `gc.auto 0` because a mirror is written by pushes and read by clones, never
/// worked in — an automatic gc firing mid-clone on a 4-core box is pure
/// contention for a repack nobody asked for. It is also the reason a mirror
/// needs an occasional deliberate `git gc`, which is a cron problem and not
/// this function's.
pub fn init_bare(path: &Path) -> Result<(), AppError> {
    if path.join("HEAD").exists() {
        return Ok(());
    }
    std::fs::create_dir_all(path)
        .map_err(|e| AppError::General(format!("could not create {}: {e}", path.display())))?;
    let out = Command::new("git")
        .arg("init")
        .arg("--bare")
        .arg(path)
        .output()
        .map_err(|e| AppError::General(format!("git init --bare failed to start: {e}")))?;
    if !out.status.success() {
        return Err(AppError::General(format!(
            "git init --bare {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    git(path, &["config", "gc.auto", "0"])?;
    Ok(())
}

/// Create the bare repo on the far side of an ssh destination, if it is not
/// already there.
///
/// `remote` is either a local path (tests, and a NAS mount) or
/// `host:/path/to/x.git`. The ssh form runs `git init --bare` remotely, which
/// is idempotent — git refuses to clobber an existing repo and re-running is
/// how this stays a one-liner instead of a probe followed by a create.
///
/// `-o BatchMode=yes` so a host that is not in `known_hosts`, or whose key
/// changed, FAILS rather than hanging on a prompt nobody can answer. A mirror
/// push that hangs would hold the runner's tick until the wall clock killed it,
/// and report as a timeout.
pub fn ensure_remote(remote: &str) -> Result<(), AppError> {
    let Some((host, path)) = split_ssh_remote(remote) else {
        return init_bare(Path::new(remote));
    };
    let out = Command::new("ssh")
        .args(["-o", "BatchMode=yes", host])
        .arg(format!(
            "mkdir -p {p} && git init --bare -q {p} >/dev/null 2>&1;              git -C {p} config gc.auto 0",
            p = shell_quote(path)
        ))
        .output()
        .map_err(|e| AppError::General(format!("ssh {host} failed to start: {e}")))?;
    if !out.status.success() {
        return Err(AppError::General(format!(
            "could not prepare the mirror at {remote}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// `user@host:/path` → `("user@host", "/path")`. `None` for a local path.
///
/// Windows drive letters are not a case here — this is a macOS and Linux
/// codebase — but a bare `:` with no path is, and it is not an ssh remote.
fn split_ssh_remote(remote: &str) -> Option<(&str, &str)> {
    let (host, path) = remote.split_once(':')?;
    if host.is_empty() || path.is_empty() || host.contains('/') {
        return None;
    }
    Some((host, path))
}

/// Single-quote a path for a remote shell.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One directory per test, removed as one directory.
    ///
    /// **Never a path whose parent is the temp root.** The first version of
    /// these tests cleaned up with `remove_dir_all(dir.parent())`, and for a
    /// directory created directly under `$TMPDIR` that parent is *the whole
    /// temp directory* — every other test's scratch space, and every other
    /// process's. It surfaced as one flaky test (parallel tests deleting each
    /// other's fixtures) and could have been much worse than that. A `Scratch`
    /// that owns exactly one root and drops it makes the mistake unavailable
    /// rather than merely corrected.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let d = std::env::temp_dir().join(format!("cxtasks-mirror-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        /// A path INSIDE this scratch. Nothing here can name a sibling.
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
        fn dir(&self, name: &str) -> PathBuf {
            let d = self.path(name);
            std::fs::create_dir_all(&d).unwrap();
            d
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            // Bounded to this test's own root by construction, and best-effort
            // because failing to clean up must not fail a passing test.
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn run(cwd: &Path, args: &[&str]) -> String {
        git(cwd, args).unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", cwd.display()))
    }

    /// A repo with a commit, a gitignored file, an untracked file and a dirty
    /// tracked file — one of each of the three properties that mislead an agent.
    fn source_repo(scratch: &Scratch) -> PathBuf {
        let dir = scratch.dir("src");
        run(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join(".gitignore"), "secret.env\n").unwrap();
        std::fs::write(dir.join("tracked.txt"), "original\n").unwrap();
        run(&dir, &["add", "-A"]);
        run(&dir, &["commit", "-qm", "first"]);

        std::fs::write(dir.join("secret.env"), "TOKEN=hunter2\n").unwrap();
        std::fs::write(dir.join("untracked.txt"), "wip\n").unwrap();
        std::fs::write(dir.join("tracked.txt"), "edited but not committed\n").unwrap();
        dir
    }

    #[test]
    fn a_mirror_carries_uncommitted_work_and_leaves_gitignored_files_behind() {
        let scratch = Scratch::new();
        let src = source_repo(&scratch);
        let bare = scratch.path("m.git");
        init_bare(&bare).unwrap();

        let snap = push(&src, bare.to_str().unwrap()).unwrap();
        assert_eq!(snap.dirty_files, 2, "one edited tracked file + one untracked");

        let out = scratch.path("work");
        materialize(&bare, &snap.sha, &out).unwrap();

        assert_eq!(
            std::fs::read_to_string(out.join("tracked.txt")).unwrap(),
            "edited but not committed\n",
            "the whole point: uncommitted work travelled"
        );
        assert!(
            out.join("untracked.txt").exists(),
            "an untracked file is part of the working tree and must travel"
        );
        assert!(
            !out.join("secret.env").exists(),
            "a gitignored file must NOT travel — that is a secret leaving the machine"
        );
    }

    /// Property 2, asserted rather than merely described: the file that was
    /// untracked on the Mac reads as tracked and clean here. This is the one an
    /// agent misreads as "everything is committed".
    #[test]
    fn an_untracked_file_arrives_tracked_which_is_why_status_reads_clean() {
        let scratch = Scratch::new();
        let src = source_repo(&scratch);
        let bare = scratch.path("m.git");
        init_bare(&bare).unwrap();
        let snap = push(&src, bare.to_str().unwrap()).unwrap();
        let out = scratch.path("work");
        materialize(&bare, &snap.sha, &out).unwrap();

        assert_eq!(
            run(&out, &["status", "--porcelain"]),
            "",
            "the snapshot must present as a clean tree"
        );
        // ...and the prompt has to say so, or the agent concludes the work is
        // committed upstream. Belt and braces on the one that misleads silently.
        assert!(MIRROR_CONTEXT.contains("read clean"));
        assert!(MIRROR_CONTEXT.contains(".gitignore"));
        assert!(MIRROR_CONTEXT.contains("Submodules"));
    }

    /// **A push must not disturb the repo it is reading.** The developer is
    /// working in this tree while the mirror runs every 20 seconds; anything
    /// that touched the index or a branch would be a background process editing
    /// their checkout.
    #[test]
    fn pushing_a_mirror_leaves_the_source_repo_byte_identical() {
        let scratch = Scratch::new();
        let src = source_repo(&scratch);
        let bare = scratch.path("m.git");
        init_bare(&bare).unwrap();

        let before_status = run(&src, &["status", "--porcelain"]);
        let before_head = run(&src, &["rev-parse", "HEAD"]);
        let before_branches = run(&src, &["for-each-ref", "refs/heads"]);

        push(&src, bare.to_str().unwrap()).unwrap();

        assert_eq!(run(&src, &["status", "--porcelain"]), before_status);
        assert_eq!(run(&src, &["rev-parse", "HEAD"]), before_head);
        assert_eq!(run(&src, &["for-each-ref", "refs/heads"]), before_branches);
    }

    /// A second push replaces the snapshot rather than failing on a
    /// non-fast-forward, which is what the `+` refspec buys — a snapshot's
    /// parent is HEAD, so any amend or rebase makes the next one unrelated.
    #[test]
    fn a_later_snapshot_replaces_an_earlier_one_even_after_a_rebase() {
        let scratch = Scratch::new();
        let src = source_repo(&scratch);
        let bare = scratch.path("m.git");
        init_bare(&bare).unwrap();

        let first = push(&src, bare.to_str().unwrap()).unwrap();

        // Rewrite history the way an amend does, so the next snapshot's parent
        // is a commit the mirror has never seen.
        std::fs::write(src.join("tracked.txt"), "rewritten\n").unwrap();
        run(&src, &["add", "-A"]);
        run(&src, &["commit", "-q", "--amend", "-m", "amended"]);

        let second = push(&src, bare.to_str().unwrap()).unwrap();
        assert_ne!(first.sha, second.sha);
        assert_eq!(
            run(&bare, &["rev-parse", SYNC_REF]),
            second.sha,
            "the mirror is still holding the old snapshot"
        );
    }

    /// Two repos whose basenames collide must not share a mirror. This happens
    /// constantly in a projects folder — several `web`s, several `api`s — and
    /// the failure is an agent handed the wrong codebase while everything looks
    /// healthy.
    #[test]
    fn repos_with_the_same_basename_get_different_keys() {
        let scratch = Scratch::new();
        let a = scratch.dir("alpha/web");
        let b = scratch.dir("beta/web");

        let (ka, kb) = (repo_key(&a), repo_key(&b));
        assert_ne!(ka, kb);
        assert!(ka.starts_with("web-"), "{ka} should stay readable");
        assert_eq!(ka, repo_key(&a), "the key must be stable across calls");
    }

    /// The trigger for the background refresh. Every case here is one the first
    /// implementation got wrong: it hashed only HEAD and the porcelain status,
    /// which names changed files without their contents — so editing an
    /// already-dirty file, which is what working in a repo looks like, was
    /// completely invisible.
    #[test]
    fn the_fingerprint_notices_an_edit_to_a_file_that_was_already_dirty() {
        let scratch = Scratch::new();
        let src = source_repo(&scratch);
        let a = fingerprint(&src).unwrap();
        assert_eq!(a, fingerprint(&src).unwrap(), "nothing changed");

        // `tracked.txt` is ALREADY modified in `source_repo`, so porcelain's
        // output does not move — only the content does.
        std::fs::write(src.join("tracked.txt"), "different again\n").unwrap();
        let b = fingerprint(&src).unwrap();
        assert_ne!(a, b, "an edit to an already-dirty file must be visible");

        // A new untracked file: caught by the porcelain listing.
        std::fs::write(src.join("brand-new.txt"), "hello\n").unwrap();
        let c = fingerprint(&src).unwrap();
        assert_ne!(b, c);

        // An edit to an untracked file, changing its length: caught by the stat.
        std::fs::write(src.join("brand-new.txt"), "hello, at greater length\n").unwrap();
        let d = fingerprint(&src).unwrap();
        assert_ne!(c, d);

        // A commit moves HEAD, which must also move the fingerprint even though
        // it makes the tree CLEANER rather than dirtier.
        run(&src, &["add", "tracked.txt"]);
        run(&src, &["commit", "-qm", "second"]);
        assert_ne!(d, fingerprint(&src).unwrap(), "a commit must be visible");
    }

    #[test]
    fn materializing_into_an_existing_directory_is_refused() {
        let scratch = Scratch::new();
        let src = source_repo(&scratch);
        let bare = scratch.path("m.git");
        init_bare(&bare).unwrap();
        let snap = push(&src, bare.to_str().unwrap()).unwrap();

        let out = scratch.path("work");
        materialize(&bare, &snap.sha, &out).unwrap();
        assert!(
            materialize(&bare, &snap.sha, &out).is_err(),
            "reusing a run directory would mix two runs' output"
        );
    }

    /// A local path is not an ssh destination, and a path containing a colon
    /// must not be mistaken for one — `ensure_remote` would then try to ssh to
    /// a directory name.
    #[test]
    fn a_local_path_is_never_mistaken_for_an_ssh_remote() {
        assert_eq!(
            split_ssh_remote("nas-ts:/volume1/cxtasks/mirrors/x.git"),
            Some(("nas-ts", "/volume1/cxtasks/mirrors/x.git"))
        );
        assert_eq!(
            split_ssh_remote("cx@nas:/srv/m.git"),
            Some(("cx@nas", "/srv/m.git"))
        );
        assert_eq!(split_ssh_remote("/tmp/local/m.git"), None);
        assert_eq!(split_ssh_remote("/tmp/weird:name/m.git"), None);
        assert_eq!(split_ssh_remote("nas-ts:"), None);
    }

    /// A path with a quote in it must not break out of the remote shell
    /// command. Asserted against a REAL shell rather than by inspecting the
    /// string, because the question is what `sh` does with it — the first
    /// version of this test checked for a substring that legitimately appears
    /// in correct output and failed a correct implementation.
    #[test]
    fn a_quoted_path_survives_a_real_shell_unchanged() {
        for raw in [
            "/srv/it's here/m.git",
            "/srv/plain/m.git",
            "/srv/$(touch pwned)/m.git",
            "/srv/a;rm -rf b/m.git",
            "/srv/back\\slash/m.git",
        ] {
            let out = Command::new("sh")
                .arg("-c")
                .arg(format!("printf %s {}", shell_quote(raw)))
                .output()
                .unwrap();
            assert!(out.status.success(), "sh rejected {raw}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                raw,
                "the shell saw something other than the path"
            );
        }
    }

    /// `ensure_remote` on a local path is just `init_bare`, which is what lets
    /// every test in this module run without a network or an ssh key.
    #[test]
    fn ensure_remote_creates_a_local_bare_repo() {
        let scratch = Scratch::new();
        let bare = scratch.path("m.git");
        ensure_remote(bare.to_str().unwrap()).unwrap();
        assert!(bare.join("HEAD").exists());
    }

    #[test]
    fn init_bare_is_idempotent_and_disables_auto_gc() {
        let scratch = Scratch::new();
        let bare = scratch.path("m.git");
        init_bare(&bare).unwrap();
        init_bare(&bare).unwrap();
        assert_eq!(run(&bare, &["config", "gc.auto"]), "0");
    }
}
