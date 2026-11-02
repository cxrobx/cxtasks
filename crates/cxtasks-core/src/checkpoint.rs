//! Workspace checkpoints for write-enabled background runs.
//!
//! # The problem this replaces
//!
//! v1 refused a write-enabled background run unless `repo_path` had a clean git
//! tree. The reasoning was sound — you must be able to tell the agent's changes
//! from your own — but the policy is hostile: Chris always has work in progress
//! somewhere, so the feature was effectively unusable in the repos where it
//! would matter most.
//!
//! # The technique (from pingdotgg/t3code, `apps/server/src/vcs/GitVcsDriver.ts`)
//!
//! Point `GIT_INDEX_FILE` at a **throwaway index**, then:
//!
//! ```text
//! git read-tree HEAD        # seed the temp index from HEAD (skipped on an empty repo)
//! git add -A -- .           # stage the ENTIRE working tree into the temp index
//! git write-tree            # → tree oid
//! git commit-tree <tree>    # → commit oid
//! git update-ref refs/cxtasks/checkpoints/<id> <commit>
//! ```
//!
//! Every write lands in the temp index and the object database. The user's real
//! `.git/index`, working tree, `HEAD`, branches, and stash are **untouched** —
//! nothing is staged, nothing is committed to a branch, `git status` is
//! identical before and after. The snapshot lives on a ref under
//! `refs/cxtasks/`, which is outside `refs/heads` and `refs/tags`, so it doesn't
//! show up in `git branch`, `git log --all` by default, or get pushed.
//!
//! So a write-enabled run now: checkpoint whatever state exists → let the agent
//! work → `git diff <checkpoint>` is exactly what the agent did, and restoring
//! the checkpoint undoes it — dirty WIP and all.
//!
//! # Committing what a run finished
//!
//! The same snapshot is what lets the runner commit a run's work without
//! sweeping in anything else ([`commit_run`], invariant 31). The checkpoint's
//! parent is HEAD before the run and its tree is the working tree before the
//! run, so the checkpoint alone says which files were already dirty, and the
//! diff from it says which ones the run changed.

use std::path::Path;
use std::process::Command;

use crate::error::AppError;

/// Refs live here — deliberately outside `refs/heads` and `refs/tags` so they
/// stay invisible to normal git usage and are never pushed.
pub const REF_PREFIX: &str = "refs/cxtasks/checkpoints";

pub fn ref_for_run(run_id: &str) -> String {
    // Run ids are UUIDs, but sanitize anyway: a ref name can't contain `..`,
    // whitespace, `~^:?*[`, or a trailing `.lock`.
    let safe: String = run_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    format!("{REF_PREFIX}/{safe}")
}

fn git(repo: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Result<String, AppError> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    // Keep the caller's identity out of it and make the commit reproducible.
    // A checkpoint is never seen; `commit_run` passes the repo's own identity,
    // which is why `extra_env` is applied after this.
    cmd.env("GIT_AUTHOR_NAME", "CXTasks")
        .env("GIT_AUTHOR_EMAIL", "cxtasks@localhost")
        .env("GIT_COMMITTER_NAME", "CXTasks")
        .env("GIT_COMMITTER_EMAIL", "cxtasks@localhost");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }

    let out = cmd
        .output()
        .map_err(|e| AppError::General(format!("git {:?} failed to start: {e}", args)))?;
    if !out.status.success() {
        return Err(AppError::General(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn is_git_repo(repo: &Path) -> bool {
    git(repo, &["rev-parse", "--git-dir"], &[]).is_ok()
}

fn has_head_commit(repo: &Path) -> bool {
    git(repo, &["rev-parse", "--verify", "HEAD"], &[]).is_ok()
}

/// Snapshot the full working tree onto a hidden ref. Returns the ref name.
///
/// Does not touch the index, the working tree, HEAD, or any branch.
pub fn capture(repo: &Path, run_id: &str) -> Result<String, AppError> {
    let ref_name = ref_for_run(run_id);
    capture_to(repo, &ref_name)?;
    Ok(ref_name)
}

/// The same snapshot, onto a ref the caller names. Returns the COMMIT sha.
///
/// Split out for the mirror, which needs the sha rather than the ref name and
/// writes to a different namespace. That difference is the whole reason a
/// mirror can transport a working tree at all: **a checkpoint commit IS the
/// uncommitted tree, as a real commit object**, so the thing that travels is an
/// ordinary git object rather than a rsync of dirty files. Everything git
/// already knows about transferring commits — deltas, integrity, resumability —
/// comes along for free, and the receiving end needs no idea that the commit
/// represents work in progress.
pub fn capture_to(repo: &Path, ref_name: &str) -> Result<String, AppError> {
    if !is_git_repo(repo) {
        return Err(AppError::General(format!(
            "{} is not a git repository, so there's nothing to check point against. \
             A write-enabled background run needs one — otherwise there is no way to \
             see or undo what the agent changed.",
            repo.display()
        )));
    }

    // A temp index in the git dir (same filesystem as the object store). Keyed
    // on the REF rather than on a run id, because this function no longer knows
    // what a run is — and two concurrent captures onto different refs must not
    // share one index file.
    let git_dir = git(repo, &["rev-parse", "--absolute-git-dir"], &[])?;
    let token: String = ref_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let index_path = format!("{git_dir}/cxtasks-checkpoint-{token}.index");
    let env = [("GIT_INDEX_FILE", index_path.as_str())];

    // Best-effort cleanup of the temp index however we exit.
    struct TempIndex(String);
    impl Drop for TempIndex {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _guard = TempIndex(index_path.clone());

    if has_head_commit(repo) {
        git(repo, &["read-tree", "HEAD"], &env)?;
    }
    // `-A` includes untracked files; a new file the agent later edits is then
    // part of the checkpoint too. Honors .gitignore, which is what we want —
    // node_modules must not end up in the object store.
    git(repo, &["add", "-A", "--", "."], &env)?;

    let tree = git(repo, &["write-tree"], &env)?;
    if tree.is_empty() {
        return Err(AppError::General("git write-tree returned nothing".into()));
    }

    let message = format!("cxtasks checkpoint {ref_name}");
    let commit = if has_head_commit(repo) {
        let head = git(repo, &["rev-parse", "HEAD"], &[])?;
        git(repo, &["commit-tree", &tree, "-p", &head, "-m", &message], &env)?
    } else {
        git(repo, &["commit-tree", &tree, "-m", &message], &env)?
    };
    if commit.is_empty() {
        return Err(AppError::General("git commit-tree returned nothing".into()));
    }

    git(repo, &["update-ref", ref_name, &commit], &[])?;
    log::info!("checkpoint: captured {ref_name} ({commit}) in {}", repo.display());
    Ok(commit)
}

/// `git diff <checkpoint> --stat`-style summary of what changed since capture.
/// This is the answer to "what did the agent actually do?".
pub fn diff_stat(repo: &Path, ref_name: &str) -> Result<String, AppError> {
    // Snapshot the CURRENT working tree into a second throwaway index, then
    // diff tree-against-tree.
    //
    // The obvious `git diff <ref> --` is wrong here: the checkpoint tree
    // contains files that were *untracked* at capture time (because `add -A`
    // staged them), so diffing against the real index reports every one of
    // them as a DELETION. Observed live — the user's own untracked `notes.txt`
    // showed up as `notes.txt | 1 -`, which reads as "the agent deleted my
    // file". Tree-to-tree has no such asymmetry.
    let now_tree = snapshot_tree(repo, "diff")?;
    let stat = git(repo, &["diff", "--stat", ref_name, &now_tree, "--"], &[])?;
    Ok(stat)
}

/// Stage the whole working tree into a temp index and return the tree oid.
/// Shared by capture and diff; never touches the real index.
fn snapshot_tree(repo: &Path, tag: &str) -> Result<String, AppError> {
    let git_dir = git(repo, &["rev-parse", "--absolute-git-dir"], &[])?;
    let index_path = format!("{git_dir}/cxtasks-{tag}-{}.index", std::process::id());
    let env = [("GIT_INDEX_FILE", index_path.as_str())];

    struct TempIndex(String);
    impl Drop for TempIndex {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _guard = TempIndex(index_path.clone());

    if has_head_commit(repo) {
        git(repo, &["read-tree", "HEAD"], &env)?;
    }
    git(repo, &["add", "-A", "--", "."], &env)?;
    let tree = git(repo, &["write-tree"], &env)?;
    if tree.is_empty() {
        return Err(AppError::General("git write-tree returned nothing".into()));
    }
    Ok(tree)
}

/// Restore the working tree to a checkpoint, discarding everything since.
///
/// Destructive by definition — this is the undo button, and it throws away any
/// edits made after the checkpoint, including the user's own. Only ever called
/// from an explicit user action.
pub fn restore(repo: &Path, ref_name: &str) -> Result<(), AppError> {
    git(repo, &["rev-parse", "--verify", ref_name], &[]).map_err(|_| {
        AppError::General(format!("checkpoint {ref_name} no longer exists"))
    })?;
    // 1. Delete anything that exists now but wasn't in the checkpoint — files
    //    the agent CREATED. `git checkout <ref> -- .` alone does not remove
    //    them (verified live: the agent's new file survived a restore), so an
    //    undo without this step leaves the agent's droppings behind.
    let in_checkpoint: std::collections::HashSet<String> =
        git(repo, &["ls-tree", "-r", "--name-only", ref_name], &[])?
            .lines()
            .map(str::to_string)
            .collect();
    let present = git(
        repo,
        &["ls-files", "--cached", "--others", "--exclude-standard"],
        &[],
    )?;
    for f in present.lines().filter(|f| !in_checkpoint.contains(*f)) {
        let path = repo.join(f);
        if path.is_file() {
            if let Err(e) = std::fs::remove_file(&path) {
                log::warn!("checkpoint restore: could not remove {}: {e}", path.display());
            }
        }
    }

    // 2. Rewrite every checkpointed path from the snapshot. Does not move HEAD,
    //    so the branch and history are untouched.
    git(repo, &["checkout", ref_name, "--", "."], &[])?;

    // 3. `checkout <ref> -- .` STAGES what it writes, so without this the repo
    //    comes back with everything staged — `git status` reads `M  app.txt`
    //    instead of the ` M app.txt` it had before. A mixed reset puts the
    //    index back to HEAD, reproducing the natural modified-not-staged view.
    //    (Caveat: changes the user had deliberately staged before the run come
    //    back unstaged. Content is exact; only the staging split is lost.)
    git(repo, &["reset", "-q"], &[])?;

    log::info!("checkpoint: restored {ref_name} in {}", repo.display());
    Ok(())
}

/// What [`commit_run`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RunCommit {
    /// The new commit, or `None` when the run left nothing to commit.
    pub sha: Option<String>,
    pub committed: Vec<String>,
    /// Files the run changed that already had uncommitted changes when it
    /// started. Left alone, because the two sets of changes can't be told apart.
    pub left_out: Vec<String>,
}

/// NUL-separated names of the files that differ between two tree-ishes.
fn changed_paths(repo: &Path, from: &str, to: &str) -> Result<Vec<String>, AppError> {
    let out = git(repo, &["diff", "--name-only", "--no-renames", "-z", from, to, "--"], &[])?;
    Ok(out.split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect())
}

/// Commit the files a run changed onto the current branch, and nothing else.
///
/// Works in a throwaway index and moves the branch by compare-and-swap, so a
/// commit made elsewhere in the meantime fails this rather than being lost, and
/// the working tree is never written. Not pushed, and hooks don't run: the
/// run's own report is the verification, and the pre-push gate still stands.
pub fn commit_run(repo: &Path, ref_name: &str, message: &str) -> Result<RunCommit, AppError> {
    let head = git(repo, &["rev-parse", "--verify", "HEAD"], &[])
        .map_err(|_| AppError::General("the repo has no commits yet".into()))?;
    git(repo, &["symbolic-ref", "-q", "HEAD"], &[])
        .map_err(|_| AppError::General("HEAD is detached, so there is no branch to commit to".into()))?;
    for marker in ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "REBASE_HEAD"] {
        if git(repo, &["rev-parse", "-q", "--verify", marker], &[]).is_ok() {
            return Err(AppError::General(format!("{marker} is set: an operation is in progress")));
        }
    }
    let before = format!("{ref_name}^");
    git(repo, &["rev-parse", "--verify", &before], &[])
        .map_err(|_| AppError::General(format!("checkpoint {ref_name} has no parent to compare with")))?;

    let now = snapshot_tree(repo, "commit")?;
    // Against HEAD as well as the checkpoint: a file the agent committed itself
    // is already done, and a file nobody changed is not ours to touch.
    let uncommitted = changed_paths(repo, &head, &now)?;
    let by_run: std::collections::HashSet<String> =
        changed_paths(repo, ref_name, &now)?.into_iter().collect();
    let dirty_before: std::collections::HashSet<String> =
        changed_paths(repo, &before, ref_name)?.into_iter().collect();

    let (left_out, committed): (Vec<String>, Vec<String>) = uncommitted
        .into_iter()
        .filter(|p| by_run.contains(p))
        // A name git printed that isn't valid UTF-8 can't be handed back to it.
        .partition(|p| dirty_before.contains(p) || p.contains('\u{FFFD}'));
    if committed.is_empty() {
        return Ok(RunCommit { sha: None, committed, left_out });
    }

    let git_dir = git(repo, &["rev-parse", "--absolute-git-dir"], &[])?;
    let index_path = format!("{git_dir}/cxtasks-commit-{}.index", std::process::id());
    let list_path = format!("{git_dir}/cxtasks-commit-{}.paths", std::process::id());
    struct TempFiles<'a>(&'a str, &'a str);
    impl Drop for TempFiles<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0);
            let _ = std::fs::remove_file(self.1);
        }
    }
    let _guard = TempFiles(&index_path, &list_path);
    std::fs::write(&list_path, committed.join("\0"))
        .map_err(|e| AppError::General(format!("could not write the path list: {e}")))?;
    let from_file = format!("--pathspec-from-file={list_path}");
    // A file really can be called `:(glob)*`.
    let literal = ("GIT_LITERAL_PATHSPECS", "1");

    let name = git(repo, &["config", "user.name"], &[]).unwrap_or_default();
    let email = git(repo, &["config", "user.email"], &[]).unwrap_or_default();
    let mut env = vec![("GIT_INDEX_FILE", index_path.as_str()), literal];
    if !name.is_empty() && !email.is_empty() {
        env.extend([
            ("GIT_AUTHOR_NAME", name.as_str()),
            ("GIT_AUTHOR_EMAIL", email.as_str()),
            ("GIT_COMMITTER_NAME", name.as_str()),
            ("GIT_COMMITTER_EMAIL", email.as_str()),
        ]);
    }
    git(repo, &["read-tree", &head], &env)?;
    git(repo, &["add", "-A", &from_file, "--pathspec-file-nul"], &env)?;
    let tree = git(repo, &["write-tree"], &env)?;
    let sha = git(repo, &["commit-tree", &tree, "-p", &head, "-m", message], &env)?;
    git(repo, &["update-ref", "-m", "cxtasks: commit run", "HEAD", &sha, &head], &[])?;

    // The real index still holds the old versions of these files, so without
    // this they would read as staged reversals of the commit just made.
    if let Err(e) = git(repo, &["reset", "-q", &from_file, "--pathspec-file-nul"], &[literal]) {
        log::warn!("checkpoint: committed {sha} but could not refresh the index: {e}");
    }
    log::info!("checkpoint: committed {sha} in {} from {ref_name}", repo.display());
    Ok(RunCommit { sha: Some(sha), committed, left_out })
}

/// Delete a checkpoint ref. Called when its run row is pruned.
pub fn drop_ref(repo: &Path, ref_name: &str) -> Result<(), AppError> {
    let _ = git(repo, &["update-ref", "-d", ref_name], &[]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct TempRepo(PathBuf);
    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn new_repo(name: &str) -> TempRepo {
        let dir = std::env::temp_dir().join(format!("cxtasks-ckpt-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"], &[]).unwrap();
        git(&dir, &["config", "user.email", "t@t"], &[]).unwrap();
        git(&dir, &["config", "user.name", "t"], &[]).unwrap();
        TempRepo(dir)
    }

    fn tracked(repo: &Path) -> String {
        git(repo, &["show", "--name-only", "--format=", "HEAD"], &[]).unwrap()
    }

    #[test]
    fn a_run_commit_takes_only_what_the_run_changed() {
        let r = new_repo("commit-only-the-run");
        commit_file(&r.0, "seed.txt", "seed\n");
        commit_file(&r.0, "other.txt", "other\n");
        commit_file(&r.0, "gone.txt", "gone\n");
        // Chris's work in progress, before the run.
        fs::write(r.0.join("wip.txt"), "wip\n").unwrap();
        fs::write(r.0.join("other.txt"), "other, edited\n").unwrap();
        let cp = capture(&r.0, "run-1").unwrap();

        fs::write(r.0.join("seed.txt"), "seed, by the run\n").unwrap();
        fs::write(r.0.join("new file :(glob).txt"), "new\n").unwrap();
        fs::remove_file(r.0.join("gone.txt")).unwrap();

        let done = commit_run(&r.0, &cp, "feat: the run").unwrap();
        assert_eq!(done.committed.len(), 3, "{done:?}");
        assert!(done.left_out.is_empty());
        assert_eq!(git(&r.0, &["rev-parse", "HEAD"], &[]).unwrap(), done.sha.unwrap());
        let shown = tracked(&r.0);
        assert!(shown.contains("seed.txt") && shown.contains("gone.txt"), "{shown}");
        assert!(!shown.contains("wip.txt") && !shown.contains("other.txt"), "{shown}");

        // The WIP is exactly as it was, and the commit reads clean: nothing
        // staged, no reversal of the commit showing in status.
        let status = git(&r.0, &["status", "--porcelain"], &[]).unwrap();
        assert_eq!(status, "M other.txt\n?? wip.txt", "{status}");
        assert_eq!(fs::read_to_string(r.0.join("other.txt")).unwrap(), "other, edited\n");
    }

    #[test]
    fn a_file_that_was_already_dirty_is_left_out_and_named() {
        let r = new_repo("commit-dirty-overlap");
        commit_file(&r.0, "both.txt", "base\n");
        commit_file(&r.0, "seed.txt", "seed\n");
        fs::write(r.0.join("both.txt"), "mine\n").unwrap();
        let cp = capture(&r.0, "run-2").unwrap();

        fs::write(r.0.join("both.txt"), "mine and the run's\n").unwrap();
        fs::write(r.0.join("seed.txt"), "by the run\n").unwrap();

        let done = commit_run(&r.0, &cp, "fix: partial").unwrap();
        assert_eq!(done.committed, vec!["seed.txt".to_string()]);
        assert_eq!(done.left_out, vec!["both.txt".to_string()]);
        assert_eq!(fs::read_to_string(r.0.join("both.txt")).unwrap(), "mine and the run's\n");
        assert_eq!(git(&r.0, &["status", "--porcelain"], &[]).unwrap(), "M both.txt");
    }

    #[test]
    fn a_run_that_changed_nothing_commits_nothing() {
        let r = new_repo("commit-nothing");
        commit_file(&r.0, "seed.txt", "seed\n");
        fs::write(r.0.join("wip.txt"), "wip\n").unwrap();
        let head = git(&r.0, &["rev-parse", "HEAD"], &[]).unwrap();
        let cp = capture(&r.0, "run-3").unwrap();

        assert_eq!(commit_run(&r.0, &cp, "nope").unwrap(), RunCommit::default());
        assert_eq!(git(&r.0, &["rev-parse", "HEAD"], &[]).unwrap(), head);
    }

    #[test]
    fn work_the_agent_committed_itself_is_not_committed_twice() {
        let r = new_repo("commit-agent-did");
        commit_file(&r.0, "seed.txt", "seed\n");
        let cp = capture(&r.0, "run-4").unwrap();
        commit_file(&r.0, "seed.txt", "the agent committed this\n");
        let head = git(&r.0, &["rev-parse", "HEAD"], &[]).unwrap();

        assert_eq!(commit_run(&r.0, &cp, "nope").unwrap().sha, None);
        assert_eq!(git(&r.0, &["rev-parse", "HEAD"], &[]).unwrap(), head);
    }

    #[test]
    fn a_detached_head_or_an_open_merge_commits_nothing() {
        let r = new_repo("commit-refused");
        commit_file(&r.0, "seed.txt", "seed\n");
        let cp = capture(&r.0, "run-5").unwrap();
        fs::write(r.0.join("seed.txt"), "by the run\n").unwrap();
        let head = git(&r.0, &["rev-parse", "HEAD"], &[]).unwrap();

        fs::write(r.0.join(".git/MERGE_HEAD"), format!("{head}\n")).unwrap();
        let err = commit_run(&r.0, &cp, "nope").unwrap_err().to_string();
        assert!(err.contains("MERGE_HEAD"), "{err}");
        fs::remove_file(r.0.join(".git/MERGE_HEAD")).unwrap();

        git(&r.0, &["checkout", "-q", "--detach"], &[]).unwrap();
        let err = commit_run(&r.0, &cp, "nope").unwrap_err().to_string();
        assert!(err.contains("detached"), "{err}");
        assert_eq!(git(&r.0, &["rev-parse", "HEAD"], &[]).unwrap(), head);
    }

    #[test]
    fn a_run_commit_carries_the_repos_own_identity() {
        let r = new_repo("commit-identity");
        commit_file(&r.0, "seed.txt", "seed\n");
        let cp = capture(&r.0, "run-6").unwrap();
        fs::write(r.0.join("seed.txt"), "by the run\n").unwrap();
        commit_run(&r.0, &cp, "feat: x").unwrap();
        let who = git(&r.0, &["log", "-1", "--format=%an <%ae>"], &[]).unwrap();
        assert_eq!(who, "t <t@t>");
    }

    fn commit_file(repo: &Path, name: &str, body: &str) {
        fs::write(repo.join(name), body).unwrap();
        git(repo, &["add", "-A"], &[]).unwrap();
        git(repo, &["commit", "-q", "-m", "seed"], &[]).unwrap();
    }

    fn status(repo: &Path) -> String {
        git(repo, &["status", "--porcelain"], &[]).unwrap()
    }

    #[test]
    fn capture_leaves_the_working_tree_and_index_untouched() {
        let r = new_repo("clean-side-effects");
        let repo = &r.0;
        commit_file(repo, "a.txt", "one\n");

        // Dirty WIP: a modified tracked file, a staged change, and an untracked file.
        fs::write(repo.join("a.txt"), "one modified\n").unwrap();
        fs::write(repo.join("staged.txt"), "staged\n").unwrap();
        git(repo, &["add", "staged.txt"], &[]).unwrap();
        fs::write(repo.join("untracked.txt"), "untracked\n").unwrap();

        let before_status = status(repo);
        let before_head = git(repo, &["rev-parse", "HEAD"], &[]).unwrap();
        let before_branches = git(repo, &["branch", "--list"], &[]).unwrap();

        capture(repo, "run-1").unwrap();

        assert_eq!(status(repo), before_status, "git status changed — capture is not side-effect free");
        assert_eq!(git(repo, &["rev-parse", "HEAD"], &[]).unwrap(), before_head, "HEAD moved");
        assert_eq!(git(repo, &["branch", "--list"], &[]).unwrap(), before_branches, "a branch appeared");
        assert_eq!(
            fs::read_to_string(repo.join("a.txt")).unwrap(),
            "one modified\n",
            "working tree content changed"
        );
    }

    #[test]
    fn capture_works_on_a_dirty_tree_which_is_the_whole_point() {
        let r = new_repo("dirty");
        let repo = &r.0;
        commit_file(repo, "a.txt", "one\n");
        fs::write(repo.join("a.txt"), "dirty\n").unwrap();
        assert!(!status(repo).is_empty(), "fixture should be dirty");

        // v1 would have refused here.
        let ref_name = capture(repo, "run-dirty").unwrap();
        assert!(ref_name.starts_with(REF_PREFIX));
    }

    #[test]
    fn the_checkpoint_ref_is_hidden_from_normal_git() {
        let r = new_repo("hidden");
        let repo = &r.0;
        commit_file(repo, "a.txt", "one\n");
        capture(repo, "run-hidden").unwrap();

        let branches = git(repo, &["branch", "--list"], &[]).unwrap();
        assert!(branches.is_empty() || !branches.contains("cxtasks"), "{branches}");
        let tags = git(repo, &["tag", "--list"], &[]).unwrap();
        assert!(tags.is_empty(), "{tags}");
        // But it IS resolvable when we ask for it by name.
        assert!(git(repo, &["rev-parse", "--verify", &ref_for_run("run-hidden")], &[]).is_ok());
    }

    #[test]
    fn restore_deletes_files_the_agent_created() {
        let r = new_repo("restore-created");
        let repo = &r.0;
        commit_file(repo, "a.txt", "one\n");
        capture(repo, "run-created").unwrap();

        fs::write(repo.join("agent-made-this.txt"), "junk\n").unwrap();
        restore(repo, &ref_for_run("run-created")).unwrap();

        assert!(
            !repo.join("agent-made-this.txt").exists(),
            "a file the agent created must not survive the undo"
        );
    }

    #[test]
    fn restore_reproduces_the_original_git_status_not_a_staged_one() {
        let r = new_repo("restore-status");
        let repo = &r.0;
        commit_file(repo, "a.txt", "one\n");
        fs::write(repo.join("a.txt"), "wip\n").unwrap();
        fs::write(repo.join("untracked.txt"), "mine\n").unwrap();
        let before = status(repo);

        capture(repo, "run-status").unwrap();
        fs::write(repo.join("a.txt"), "agent\n").unwrap();
        restore(repo, &ref_for_run("run-status")).unwrap();

        assert_eq!(
            status(repo),
            before,
            "restore must leave git status as it was, not stage everything"
        );
    }

    #[test]
    fn diff_does_not_report_untracked_files_as_deletions() {
        let r = new_repo("diff-untracked");
        let repo = &r.0;
        commit_file(repo, "a.txt", "one\n");
        // The USER's untracked file, present at capture time.
        fs::write(repo.join("notes.txt"), "my notes\n").unwrap();
        capture(repo, "run-untracked").unwrap();

        // The agent touches only a.txt.
        fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();

        let stat = diff_stat(repo, &ref_for_run("run-untracked")).unwrap();
        assert!(stat.contains("a.txt"), "{stat}");
        assert!(
            !stat.contains("notes.txt"),
            "an untracked file present at capture must not show as changed: {stat}"
        );
    }

    #[test]
    fn capture_then_restore_undoes_agent_edits_including_on_dirty_wip() {
        let r = new_repo("restore");
        let repo = &r.0;
        commit_file(repo, "a.txt", "committed\n");
        // The user's own uncommitted work, present BEFORE the run.
        fs::write(repo.join("a.txt"), "user wip\n").unwrap();

        capture(repo, "run-restore").unwrap();

        // The "agent" edits the same file and adds another.
        fs::write(repo.join("a.txt"), "agent overwrote it\n").unwrap();
        fs::write(repo.join("b.txt"), "agent made this\n").unwrap();

        restore(repo, &ref_for_run("run-restore")).unwrap();

        assert_eq!(
            fs::read_to_string(repo.join("a.txt")).unwrap(),
            "user wip\n",
            "restore must bring back the USER's WIP, not just the last commit"
        );
    }

    #[test]
    fn diff_stat_reports_what_changed_after_the_checkpoint() {
        let r = new_repo("diff");
        let repo = &r.0;
        commit_file(repo, "a.txt", "one\n");
        capture(repo, "run-diff").unwrap();

        fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        fs::write(repo.join("new.txt"), "brand new\n").unwrap();

        let stat = diff_stat(repo, &ref_for_run("run-diff")).unwrap();
        assert!(stat.contains("a.txt"), "modified file missing from diff: {stat}");
        assert!(stat.contains("new.txt"), "untracked file missing from diff: {stat}");
    }

    #[test]
    fn diff_is_empty_when_the_agent_changed_nothing() {
        let r = new_repo("nodiff");
        let repo = &r.0;
        commit_file(repo, "a.txt", "one\n");
        fs::write(repo.join("a.txt"), "wip\n").unwrap();
        capture(repo, "run-nodiff").unwrap();

        let stat = diff_stat(repo, &ref_for_run("run-nodiff")).unwrap();
        assert!(stat.trim().is_empty(), "expected no diff, got: {stat}");
    }

    #[test]
    fn capture_succeeds_on_a_repo_with_no_commits_yet() {
        let r = new_repo("empty");
        let repo = &r.0;
        fs::write(repo.join("only.txt"), "x\n").unwrap();
        assert!(capture(repo, "run-empty").is_ok());
    }

    #[test]
    fn capture_refuses_a_non_git_directory_with_a_useful_message() {
        let dir = std::env::temp_dir().join("cxtasks-ckpt-notgit");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let err = capture(&dir, "run-x").unwrap_err();
        assert!(format!("{err}").contains("not a git repository"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_temp_index_is_cleaned_up() {
        let r = new_repo("tempidx");
        let repo = &r.0;
        commit_file(repo, "a.txt", "one\n");
        capture(repo, "run-tmp").unwrap();
        let git_dir = git(repo, &["rev-parse", "--absolute-git-dir"], &[]).unwrap();
        let leftovers: Vec<_> = fs::read_dir(&git_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("cxtasks-checkpoint"))
            .collect();
        assert!(leftovers.is_empty(), "temp index left behind: {leftovers:?}");
    }

    #[test]
    fn drop_ref_removes_the_checkpoint() {
        let r = new_repo("drop");
        let repo = &r.0;
        commit_file(repo, "a.txt", "one\n");
        let name = capture(repo, "run-drop").unwrap();
        drop_ref(repo, &name).unwrap();
        assert!(git(repo, &["rev-parse", "--verify", &name], &[]).is_err());
    }

    #[test]
    fn ref_names_are_sanitized() {
        assert_eq!(ref_for_run("abc-123"), format!("{REF_PREFIX}/abc-123"));
        let bad = ref_for_run("../../evil ref~1");
        assert!(!bad.contains(".."), "{bad}");
        assert!(!bad.contains(' '), "{bad}");
        assert!(!bad.contains('~'), "{bad}");
    }
}
