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
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    // Keep the caller's identity out of it and make the commit reproducible.
    cmd.env("GIT_AUTHOR_NAME", "CXTasks")
        .env("GIT_AUTHOR_EMAIL", "cxtasks@localhost")
        .env("GIT_COMMITTER_NAME", "CXTasks")
        .env("GIT_COMMITTER_EMAIL", "cxtasks@localhost");

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
