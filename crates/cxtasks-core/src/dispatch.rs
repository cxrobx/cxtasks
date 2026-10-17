//! Shared dispatch plumbing: finding the tools, and building the per-task
//! scratch directory that a dispatched session reads from.
//!
//! Used by the foreground path (`commands/dispatch.rs`, spawns Ghostty) and
//! the background runner, so a task's context block is byte-identical however
//! it is dispatched — only the closing instructions differ, by
//! [`PromptTarget`].

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::db::tasks::Task;
use crate::error::AppError;
use crate::executor::BackgroundExecutor;
use crate::intent::BackgroundIntent;

/// Who the seed prompt is being written for.
///
/// Foreground is always Claude and carries no executor: it opens a Ghostty
/// window the user watches, and a second interactive agent there is something
/// they can already get by typing `codex` themselves. Adding one would also
/// break the write-back loop — `complete_task` is a CXTasks MCP tool, and the
/// foreground prompt's whole closing instruction is to call it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTarget {
    Foreground,
    Background(BackgroundExecutor),
}

impl PromptTarget {
    /// The `mode` recorded on the run row and in `task.json`.
    pub fn mode(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background(_) => "background",
        }
    }
}

/// Points `find_claude_cli` at a specific binary, skipping discovery.
///
/// Two callers need this and neither can use the search chain below: a Linux
/// container, where none of those paths exist, and a test that wants to drive
/// the runner against a fake CLI without spending a token.
pub const ENV_CLAUDE_CLI: &str = "CXTASKS_CLAUDE_CLI";

/// Locate the `claude` CLI.
///
/// [`ENV_CLAUDE_CLI`] wins outright when set. **A set-but-missing override
/// returns `None` rather than falling through to discovery**, because falling
/// through means "I ignored the binary you named and ran a different one" —
/// and for the binary that executes unattended agents against your repos, that
/// is the wrong failure. `preflight` then refuses the run and says the CLI
/// wasn't found, which is a question whoever set the variable can answer.
///
/// Otherwise: GUI apps on macOS don't inherit the shell's PATH, so
/// `/usr/bin/which` runs against a bare-bones PATH and usually misses user
/// installs. Ask a login shell to resolve it instead — that picks up whatever
/// is configured in .zshrc / .zprofile / .bash_profile. (Straight from cxmail;
/// this is the part that makes the difference between "works in dev" and
/// "works in the bundle".)
/// What [`ENV_CLAUDE_CLI`] had to say. Separated from the env read so the
/// three-way decision is testable without mutating process env, which would
/// race every other test in the binary.
#[derive(Debug, PartialEq, Eq)]
enum CliOverride {
    /// Unset, or set to nothing — fall through to discovery.
    NotSet,
    Use(PathBuf),
    /// Named a path that is not a file. Deliberately NOT `NotSet`.
    Missing(PathBuf),
}

fn cli_override(raw: Option<std::ffi::OsString>) -> CliOverride {
    match raw {
        Some(v) if !v.is_empty() => {
            let p = PathBuf::from(v);
            if p.is_file() {
                CliOverride::Use(p)
            } else {
                CliOverride::Missing(p)
            }
        }
        _ => CliOverride::NotSet,
    }
}

pub fn find_claude_cli() -> Option<PathBuf> {
    match cli_override(std::env::var_os(ENV_CLAUDE_CLI)) {
        CliOverride::Use(p) => return Some(p),
        CliOverride::Missing(p) => {
            log::warn!(
                "{ENV_CLAUDE_CLI} names {p:?}, which is not a file — refusing \
                 to fall back to a different claude binary"
            );
            return None;
        }
        CliOverride::NotSet => {}
    }

    if let Ok(output) = Command::new("/bin/zsh")
        .arg("-lc")
        .arg("command -v claude")
        .output()
    {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !s.is_empty() {
                let p = PathBuf::from(&s);
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }

    // Fallback: probe common install locations directly.
    for candidate in &["/opt/homebrew/bin/claude", "/usr/local/bin/claude"] {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return Some(p);
        }
    }
    if let Some(home) = dirs::home_dir() {
        for rel in &[
            ".local/bin/claude",
            ".claude/local/claude",
            ".volta/bin/claude",
            ".bun/bin/claude",
        ] {
            let p = home.join(rel);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

pub fn sanitize_id(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Single-quote for a POSIX shell. Safe for any bytes: wraps in '...' and
/// escapes embedded single quotes as '\''.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Validate a task's `repo_path` at dispatch time. A path can go stale between
/// filing and running (repo moved, external drive unmounted), and a bad `cd`
/// inside the launch script would strand the user in a terminal with a cryptic
/// error instead of a clear one here.
///
/// STRICT: errors when `repo_path` is unset. Used by every background/
/// checkpoint path (`queue_background`, `preflight`, `run_diff`,
/// `restore_checkpoint`) — those must keep failing loudly on a missing repo
/// rather than silently operating on `$HOME`. Foreground dispatch uses
/// `resolve_repo_or_home` instead.
pub fn resolve_repo(task: &Task) -> Result<PathBuf, AppError> {
    let raw = non_empty_repo_path(task).ok_or_else(|| {
        AppError::Dispatch(
            "This task has no repo path, so there's nothing to dispatch. \
             Add one in the detail pane."
                .into(),
        )
    })?;
    validate_exists(raw)
}

/// Same validation as `resolve_repo`, but a task with no `repo_path` resolves
/// to the user's home directory instead of erroring — "run at root level"
/// rather than refusing to dispatch. Foreground-only (`dispatch_foreground`,
/// `get_dispatch_prompt`): a human is present, so Claude's normal cwd-scoped
/// permission prompts still gate anything outside `$HOME`. Background runs
/// must NOT use this — see `resolve_repo`'s doc comment.
pub fn resolve_repo_or_home(task: &Task) -> Result<PathBuf, AppError> {
    let Some(raw) = non_empty_repo_path(task) else {
        return dirs::home_dir().ok_or_else(|| {
            AppError::Dispatch("no repo path set and could not resolve a home directory".into())
        });
    };
    validate_exists(raw)
}

fn non_empty_repo_path(task: &Task) -> Option<&str> {
    task.repo_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn validate_exists(raw: &str) -> Result<PathBuf, AppError> {
    let path = PathBuf::from(raw);
    if !path.is_dir() {
        return Err(AppError::Dispatch(format!(
            "repo path '{raw}' no longer exists (moved or unmounted?)"
        )));
    }
    Ok(path)
}

/// Per-task scratch directory holding `task.json` + `prompt.txt`.
pub fn scratch_dir_for(task: &Task) -> PathBuf {
    crate::paths::sessions_dir().join(sanitize_id(&task.id))
}

/// Write the scratch dir. Returns its path.
pub fn write_scratch(
    task: &Task,
    repo: &Path,
    target: PromptTarget,
    mirror: Option<&crate::mirror::Snapshot>,
) -> Result<PathBuf, AppError> {
    let scratch = scratch_dir_for(task);
    fs::create_dir_all(&scratch)?;

    let mode = target.mode();
    let task_json = serde_json::json!({
        "task_id": task.id,
        "short_id": task.short_id,
        "title": task.title,
        "body": task.body,
        "project": task.project,
        "priority": task.priority,
        "due_at": task.due_at,
        "repo_path": repo.to_string_lossy(),
        "mode": mode,
        "executor": match target {
            PromptTarget::Foreground => BackgroundExecutor::Claude,
            PromptTarget::Background(e) => e,
        }
        .as_str(),
        // Without this, two runs of one task produce a byte-identical
        // `task.json` and a completely different `prompt.txt` — a debugging
        // dead end when you are trying to work out what a run was handed.
        "intent": task.bg_intent.as_str(),
        "bg_writes_allowed": task.bg_writes_allowed,
        "bg_shell_allowed": task.bg_shell_allowed,
    });
    fs::write(
        scratch.join("task.json"),
        serde_json::to_string_pretty(&task_json).unwrap_or_default(),
    )?;
    // Best-effort history: a task that has been worked before carries what was
    // learned into the next session. If the DB can't be read the dispatch still
    // happens — losing context is a worse prompt, refusing to dispatch is a
    // broken feature.
    let prompt = rusqlite::Connection::open(crate::paths::db_path())
        .map(|conn| build_prompt_with_context(&conn, task, repo, target, mirror))
        .unwrap_or_else(|_| build_prompt_full(task, repo, target, "", mirror));
    fs::write(
        scratch.join("prompt.txt"),
        prompt,
    )?;
    Ok(scratch)
}

/// The seed prompt handed to the dispatched session.
///
/// Three pieces, in order: the **task context** (identical everywhere, so a
/// task reads the same to whoever picks it up), the **work** itself, and a
/// **tail** that depends entirely on who is reading. The tail is where the two
/// halves of the system disagree, and they disagree completely — foreground is
/// told to call `complete_task`, background is told never to.
///
/// The foreground closing instruction is load-bearing: calling `complete_task`
/// is what closes the loop back to the app (via the MCP → bridge →
/// `mcp-activity` path), so the result lands on the task instead of dying with
/// the session.
pub fn build_prompt(task: &Task, repo: &Path, target: PromptTarget) -> String {
    build_prompt_with_history(task, repo, target, "")
}

/// How much of a task's history to carry into the next dispatch.
///
/// Bounded, and the bound is the point. Cost scales with TURNS, not context
/// size (invariant 5) — this lands in the one-time `cache_write`, so a few
/// hundred tokens of history that saves a single tool-using turn has already
/// paid for itself several times over. What it must not become is a task whose
/// prompt grows without limit as it accumulates comments, because that cost is
/// then re-read on every turn of every future run.
const HISTORY_COMMENTS: usize = 6;
const HISTORY_COMMENT_CHARS: usize = 1200;

/// The most recent finished run's FULL handoff, replayed verbatim up to this
/// bound.
///
/// This is the load-bearing half of the split `runner::finish_run` performs: the
/// comment log gets a one-line summary a human can scan, and the detail stays on
/// the run row. Nothing else replays a run's `result_text`, so without this the
/// next delegated agent would receive only that summary and re-derive
/// everything the previous run learned — which is the exact failure the handoff
/// was written to prevent, one level down.
///
/// ~4k chars is roughly a thousand tokens. Per invariant 5 it lands in the
/// one-time `cache_write` rather than being re-read every turn, so it is
/// comfortably cheaper than the single tool-using turn it saves.
const HISTORY_RUN_CHARS: usize = 4000;

/// How much of a run's handoff to look for in the comment log before deciding
/// it is already there.
///
/// Tasks worked before the split have the WHOLE handoff mirrored into a
/// comment, so replaying both would send it twice in one prompt. New-style
/// comments carry a summary with markdown stripped and so never match this
/// probe, which is what keeps the guard from suppressing the replay it exists
/// to protect.
const HISTORY_DEDUP_PROBE: usize = 200;

/// The "what happened before" block, assembled from the task's own record.
///
/// **This is what makes a handoff worth writing.** Before it, a background run
/// ended with its result on the run row and the task in `in_review`, and a
/// second dispatch against that task was handed the original seed prompt again
/// — so it re-derived everything the first run had learned, at full cost, and
/// the careful summary the first agent wrote was read by nobody. Prior comments,
/// the outcomes of recent runs, and the most recent run's full handoff turn that
/// into a conversation.
///
/// **The run's handoff is replayed here and nowhere else.** `runner::finish_run`
/// deliberately puts only a one-line summary in the comment log — the detail
/// belongs on the run row, where a human reads it — so removing the
/// `HISTORY_RUN_CHARS` block below silently returns this to the pre-handoff
/// behaviour it was built to fix.
///
/// Takes a connection rather than opening one so it stays testable and so the
/// caller decides what a failure means — here, nothing: dispatch proceeds
/// without history rather than refusing to dispatch.
pub fn history_block(conn: &rusqlite::Connection, task: &Task) -> String {
    let comments = crate::db::comments::for_task(conn, &task.id).unwrap_or_default();
    // A run the TRIPWIRE started is not history. A check's final message is a
    // verdict about a condition and an interpretation's is a reading of a
    // script's output; neither is a handoff about the work, and replaying
    // either as "the most recent run's handoff" would send the next agent off
    // after a question nobody asked it. Both already live on the task as
    // comments under their own author. Filtered here and in `reviewable_run`,
    // which must name the same runs.
    let runs: Vec<_> = crate::db::runs::list_for_task(conn, &task.id)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| !r.intent.is_tripwire_run())
        .collect();
    let finished: Vec<_> = runs.iter().filter(|r| r.finished_at.is_some()).collect();
    if comments.is_empty() && finished.is_empty() {
        return String::new();
    }

    let mut s = String::from(
        "What happened before (this task has been worked on already — read this \
         before repeating any of it):\n",
    );
    if !finished.is_empty() {
        // `list_for_task` is `ORDER BY started_at DESC`, so this list is ALREADY
        // newest-first. It used to be `.rev()`ed here, which took the three
        // OLDEST runs and labelled them "most recent" — on a task worked more
        // than three times the next agent was told about the wrong runs.
        //
        // One line per run, and the INTENT leads. `"{status} ({mode})"` rendered
        // a critique and the work it critiques identically — the one distinction
        // a continuation cannot afford to lose, since "which of these was a
        // review" is the question you answer before reading anything else.
        // ~60 chars each against `HISTORY_RUN_CHARS` = 4000; the cost is noise.
        s.push_str(&format!(
            "  {} previous run(s), most recent first:\n",
            finished.len()
        ));
        for r in finished.iter().take(3) {
            s.push_str(&format!(
                "    - {} · {} · {} ({})\n",
                r.intent,
                r.executor.label(),
                r.status,
                r.mode
            ));
        }
    }
    // Oldest-first within the window, so the block reads chronologically while
    // still keeping the MOST RECENT entries when it has to drop some.
    let skip = comments.len().saturating_sub(HISTORY_COMMENTS);
    for c in comments.iter().skip(skip) {
        let body = if c.body.chars().count() > HISTORY_COMMENT_CHARS {
            let head: String = c.body.chars().take(HISTORY_COMMENT_CHARS).collect();
            format!("{head}\n    […truncated — the full text is on the task…]")
        } else {
            c.body.clone()
        };
        s.push_str(&format!("\n  [{} · {}]\n  {}\n", c.author, c.created_at, body));
    }
    if skip > 0 {
        s.push_str(&format!(
            "\n  ({skip} older comment(s) omitted.)\n"
        ));
    }

    // Last, and deliberately so: it is the most recent thing that happened and
    // the most detailed, so it sits closest to the instructions that follow.
    let newest = finished.first().copied();
    if let Some(run) = newest {
        push_handoff(&mut s, run, &comments[skip..], "the most recent run");
        // **A reply is an instruction, and it reads like a note.** A run leaves a
        // summary comment when it finishes, so when the newest comment is
        // Chris's it was written AFTER the last run — it is what this run is
        // being asked to do. Without this line it sits among the history as one
        // more thing that happened, and a continuation "picks up where the
        // previous run left off" without noticing it was asked something.
        if comments.last().is_some_and(|c| c.author == crate::db::comments::AUTHOR_ME) {
            s.push_str(
                "\n  The newest comment above is from Chris and came AFTER that run finished. \
                 It is what you are being asked to do now: answer it, using the handoff as \
                 where things stood.\n",
            );
        }
    }

    // **A review must not displace the work handoff it is about.** Once a review
    // is the newest finished run, the block above replays a VERDICT — and the
    // next continuation would read a critique of an artifact it was never shown.
    // So when the newest run was a review, the most recent WORK handoff is
    // replayed too, under its own label. `finished` is newest-first already, so
    // this is a `find`, never a `rev().find()`.
    if newest.is_some_and(|r| r.intent.is_review()) {
        if let Some(work) = finished.iter().find(|r| !r.intent.is_review()) {
            push_handoff(
                &mut s,
                work,
                &comments[skip..],
                "the most recent WORK run — this is what the review above is about",
            );
        }
    }

    s.push('\n');
    s
}

/// Append one run's full handoff, bounded and deduplicated against the comments
/// already replayed.
///
/// The dedup guard exists for tasks worked before `finish_run` split the summary
/// from the artifact, whose comment holds the ENTIRE handoff — replaying both
/// would send it twice in one prompt. New-style summaries have their markdown
/// stripped, so they never match the probe, which is what stops the guard
/// suppressing the replay it exists to protect.
fn push_handoff(
    s: &mut String,
    run: &crate::db::runs::TaskRun,
    replayed_comments: &[crate::db::comments::Comment],
    label: &str,
) {
    let Some(text) = run.result_text.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
        return;
    };
    let probe: String = text.chars().take(HISTORY_DEDUP_PROBE).collect();
    if replayed_comments.iter().any(|c| c.body.contains(probe.as_str())) {
        return;
    }
    let body = if text.chars().count() > HISTORY_RUN_CHARS {
        let head: String = text.chars().take(HISTORY_RUN_CHARS).collect();
        format!("{head}\n    […truncated — the full handoff is on the run…]")
    } else {
        text.to_string()
    };
    s.push_str(&format!("\n  [full handoff from {label}]\n  {body}\n"));
}

/// The line introducing the seed prompt.
///
/// **This label is the fix, not a decoration on it.** The bug is an agent that
/// reads `What to do:` and does it — handed a finished plan and a fresh
/// assignment in the same prompt, the assignment wins, because it is the only
/// line in the region that survives skimming. A paragraph forty lines below
/// saying "that was the original brief" loses to it every time.
///
/// One function so the three strings cannot drift apart.
fn brief_label(intent: BackgroundIntent, has_history: bool) -> &'static str {
    match (intent, has_history) {
        // Byte-identical to the pre-continuations prompt. A FIRST dispatch —
        // the majority path — must not change at all.
        //
        // `Check` and `Interpret` are unreachable here in practice — a task row
        // never carries either (`db::tasks::update` refuses them) and their
        // prompts are built by `check_prompt` / `interpret_prompt`, not this —
        // so an impossible row degrades to a plain dispatch rather than to a
        // panic.
        (
            BackgroundIntent::Work | BackgroundIntent::Check | BackgroundIntent::Interpret,
            false,
        ) => "What to do:\n",
        (
            BackgroundIntent::Work | BackgroundIntent::Check | BackgroundIntent::Interpret,
            true,
        ) => {
            "The ORIGINAL BRIEF — what this task was filed to do. Work on it has already \
             happened, so this is not a fresh assignment:\n"
        }
        (BackgroundIntent::Review, _) => {
            "The ORIGINAL BRIEF — what the run you are reviewing was asked to do. You are not \
             being asked to do it:\n"
        }
    }
}

/// How to read everything above: continue it, audit it, or stop.
///
/// Sits between the history and the tail so "the handoff above" refers to
/// something already read. Returns `""` for a first dispatch — the whole point
/// of deriving continuation from the history block is that a task nobody has
/// worked yet is dispatched exactly as it was before this existed.
fn framing_block(intent: BackgroundIntent, has_history: bool) -> String {
    match (intent, has_history) {
        // `Check` and `Interpret` fold into `Work` for `brief_label`'s reason.
        (
            BackgroundIntent::Work | BackgroundIntent::Check | BackgroundIntent::Interpret,
            false,
        ) => String::new(),

        (
            BackgroundIntent::Work | BackgroundIntent::Check | BackgroundIntent::Interpret,
            true,
        ) => "This is a CONTINUATION, not a fresh start.\n\
             Pick up where the previous run left off. Do not begin the task over.\n\
             - The handoff above is where things STOOD, not where they necessarily stand now. \
             Check its claims against the tree as it is today; where they disagree, the repo \
             wins.\n\
             - Do not re-derive a finding you were just handed. If the handoff names a file, a \
             symbol or a cause, start from it.\n\
             - Your deliverable is the DELTA — what you did on top of that, not a restatement of \
             it.\n\
             - You may disagree with the previous run and say so. You may not ignore it.\n\n"
            .to_string(),

        (BackgroundIntent::Review, true) => "This run is a REVIEW, not a continuation.\n\
             You are auditing the handoff above. Two questions, and only these two: is it \
             TRUE, and is it ENOUGH?\n\
             Do not finish the work. Do not improve it. Do not do the task — a review that \
             quietly starts implementing is a failed review, however good the code.\n\
             Go after what handoffs are most often wrong about:\n\
             - Claims it never checked. If it names a path, a symbol, a line number or an \
             error, OPEN IT and confirm the thing is there and says what it says.\n\
             - Work reported as done that isn't, or is done differently than described.\n\
             - Scope the brief asked for that got waved off, silently narrowed, or declared \
             out of scope without a reason.\n\
             - Whether the next step it proposes is the right next step — not merely a \
             plausible one.\n\
             - Assumptions it made without flagging them, and anything load-bearing it did \
             not think to mention.\n\
             Say plainly and briefly if it is sound. Manufacturing objections to look useful \
             is worse than a review that says \"this is right\" — a verdict nobody trusts is \
             worth less than no verdict.\n\n"
            .to_string(),

        // Reachable, not hypothetical: `write_scratch` swallows a DB read
        // failure into an empty history, and `history_block` `unwrap_or_default`s
        // both of its queries. The queue-time guard ran minutes earlier and
        // cannot cover this. Without this branch the run silently degrades into
        // doing the task from scratch — read-only, unattended, and nobody asked.
        (BackgroundIntent::Review, false) => "This run was dispatched as a REVIEW, but the handoff \
             it was meant to audit could not be read — nothing from the previous run reached \
             this prompt.\n\
             STOP. Report exactly that: there was nothing to review. Do NOT do the task \
             instead, and do not reconstruct what the previous run might have said. Either is \
             work nobody asked for, and it would be filed as though it were a verdict.\n\n"
            .to_string(),
    }
}

/// The run a review would audit: the most recent finished run that left a
/// non-empty handoff.
///
/// **One function, called from both the queue guard and the replay**, because
/// they must name the SAME run. If `queue_background` accepted a review of
/// something `history_block` won't show, the agent is told to audit an artifact
/// it was never handed — the `list_queued_for_background` /
/// `claim_for_background` lesson, one level up.
///
/// Deliberately NOT filtered to `succeeded`: a `timeout` run's partial handoff
/// is genuinely reviewable, and often the one most worth a second pair of eyes.
/// Deliberately NOT filtered to `intent == Work` either — reviewing a review is
/// a second opinion, which is the feature.
///
/// `list_for_task` is already `ORDER BY started_at DESC`, so this is a `find`,
/// never a `rev().find()`.
pub fn reviewable_run(
    conn: &rusqlite::Connection,
    task_id: &str,
) -> Option<crate::db::runs::TaskRun> {
    crate::db::runs::list_for_task(conn, task_id)
        .unwrap_or_default()
        .into_iter()
        .find(|r| {
            !r.intent.is_tripwire_run()
                && r.finished_at.is_some()
                && r.result_text.as_deref().map(str::trim).is_some_and(|t| !t.is_empty())
        })
}

/// [`build_prompt`] plus the task's prior history, which only a caller holding a
/// database connection can supply.
pub fn build_prompt_with_history(
    task: &Task,
    repo: &Path,
    target: PromptTarget,
    history: &str,
) -> String {
    build_prompt_full(task, repo, target, history, None)
}

/// The task context every prompt opens with — byte-identical whoever reads
/// it, which is the module's whole contract.
fn context_block(task: &Task, repo: &Path, agent: &str, doing: &str) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "You are in a CXTasks-dispatched {agent} session, {doing}.\n\n\
         Task: {}\n  id:      {}\n  repo:    {}\n",
        task.title,
        task.id,
        repo.display()
    ));
    // The speakable reference, so a dispatched session can cite the task in a
    // commit message or a comment without pasting a UUID.
    if let Some(n) = task.short_id {
        s.push_str(&format!("  ref:     T{n}\n"));
    }
    if let Some(project) = task.project.as_deref() {
        s.push_str(&format!("  project: {project}\n"));
    }
    if let Some(due) = task.due_at.as_deref() {
        s.push_str(&format!("  due:     {due}\n"));
    }
    s.push('\n');
    s
}

/// How much of a task's notes a check is shown. The notes are where a person
/// wrote what the condition MEANS ("outside users — not Chris, not admins")
/// and where to look; a check without them answers a different question.
const CHECK_NOTES_CHARS: usize = 3000;

/// The prompt for an `agent:` tripwire check: task context, the notes, the
/// condition, and the verdict tail. **No history and no seed prompt** — the
/// seed prompt is the WORK, and a check that reads it starts doing the task.
///
/// Built here rather than in a provider because a check is Claude-only and
/// the verdict grammar it asks for is `tripwire::parse_verdict`'s, quoted
/// from the same constants so the two cannot drift.
pub fn check_prompt(task: &Task, repo: &Path) -> Result<String, AppError> {
    let spec = task.tripwire.as_deref().ok_or_else(|| {
        AppError::Dispatch("this task has no tripwire, so there is nothing to check".into())
    })?;
    let condition = match crate::tripwire::Tripwire::parse(spec) {
        Ok(crate::tripwire::Tripwire::Agent { condition }) => condition,
        Ok(other) => {
            return Err(AppError::Dispatch(format!(
                "this task's tripwire is a {} check, not an agent one",
                other.kind().as_str()
            )))
        }
        Err(e) => return Err(AppError::Dispatch(format!("unreadable tripwire spec: {e}"))),
    };

    let mut s = context_block(task, repo, "Claude", "evaluating ONE condition this task is waiting on");
    if let Some(body) = task.body.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        let shown: String = body.chars().take(CHECK_NOTES_CHARS).collect();
        s.push_str("The task's notes — read these for what the condition means and where to look:\n");
        s.push_str(&shown);
        if body.chars().count() > CHECK_NOTES_CHARS {
            s.push_str("\n[…notes truncated…]");
        }
        s.push_str("\n\n");
    }
    s.push_str(&format!(
        "This is a TRIPWIRE CHECK, not the task. Do NOT do the task, plan it, or improve \
         anything. Answer one question and stop:\n\n\
         CONDITION: {condition}\n\n\
         Does the condition HOLD right now? You have read-only tools (Read, Glob, Grep) in the \
         workspace above and `mcp__cxtasks__get_task` to re-read this task; nothing else, and \
         nothing that writes. Nobody is watching, so do not ask questions. This check runs on \
         a schedule and every turn costs — look at the thing the condition names, decide, and \
         say so. If the workspace cannot answer the question, say UNSURE rather than guessing; \
         UNSURE is recorded as a failed check (the task surfaces after repeated ones), never \
         as not-yet, so use it honestly.\n\n\
         Your FINAL MESSAGE must END with exactly these two lines, in this order:\n\
         {verdict} TRIPPED | NOT_YET | UNSURE\n\
         {evidence} one or two sentences naming the fact that decided it — the file and line, \
         the value, the count.\n\n\
         TRIPPED means the condition holds now. NOT_YET means you confirmed it does not. \
         The last {verdict} line in your message is the one that counts.\n",
        verdict = crate::tripwire::VERDICT_MARKER,
        evidence = crate::tripwire::EVIDENCE_MARKER,
    ));
    Ok(s)
}

/// Write `check-prompt.txt` into the task's scratch dir. Returns the dir.
///
/// Beside `prompt.txt`, never over it: the two are different questions about
/// one task, and a check must not leave the work's prompt reading as a
/// verdict request the next time the task is dispatched.
pub fn write_check_scratch(task: &Task, repo: &Path) -> Result<PathBuf, AppError> {
    let scratch = scratch_dir_for(task);
    fs::create_dir_all(&scratch)?;
    fs::write(scratch.join("check-prompt.txt"), check_prompt(task, repo)?)?;
    Ok(scratch)
}

/// The file `run_one_inner` reads an interpretation's prompt from.
pub const INTERPRET_PROMPT_FILE: &str = "interpret-prompt.txt";
/// The file the evidence is staged in, beside the prompt built from it.
pub const INTERPRET_EVIDENCE_FILE: &str = "interpret-evidence.txt";

/// Stage the evidence an interpretation will read, and return the scratch dir.
///
/// **The evidence is a FILE, not a parameter**, for three reasons. The run is
/// spawned through the same `run_one_inner` every other run goes through, and
/// that function reads its prompt off disk — threading a payload through
/// `RunLimits` to reach it would put a prompt fragment in a struct about
/// bounds. The bytes the agent was given stay inspectable afterwards, next to
/// `prompt.txt` and `check-prompt.txt`, which is how anyone reading a wrong
/// interpretation finds out whether the script or the model was wrong. And it
/// is host-local, which is correct: the evidence was produced here, by a
/// script that only exists here.
pub fn write_interpret_evidence(task: &Task, evidence: &str) -> Result<PathBuf, AppError> {
    let scratch = scratch_dir_for(task);
    fs::create_dir_all(&scratch)?;
    fs::write(scratch.join(INTERPRET_EVIDENCE_FILE), evidence)?;
    Ok(scratch)
}

/// The prompt for an interpretation: task context, the notes, the evidence, and
/// what to say about it. **No history, no seed prompt and no condition** — the
/// seed prompt is the work, and an interpretation that reads it starts doing
/// the task (`check_prompt`'s reason, unchanged).
///
/// It tells the agent it has no tools, which is not decoration: a model that
/// believes it can look will spend its two turns trying, and the run is over
/// before it says anything. The honest framing — *everything you can know is
/// below* — is what makes a tool-less run produce an answer instead of an
/// apology.
pub fn interpret_prompt(task: &Task, repo: &Path) -> Result<String, AppError> {
    let evidence = fs::read_to_string(scratch_dir_for(task).join(INTERPRET_EVIDENCE_FILE))
        .map_err(|e| {
            AppError::Dispatch(format!(
                "no staged evidence to interpret for this task ({e}) —                  `write_interpret_evidence` must run first"
            ))
        })?;
    let evidence = evidence.trim();
    if evidence.is_empty() {
        return Err(AppError::Dispatch(
            "the staged evidence is empty, so there is nothing to interpret".into(),
        ));
    }
    let spec = task.tripwire.as_deref().unwrap_or("a tripwire");
    let mut s = context_block(
        task,
        repo,
        "Claude",
        "reading ONE piece of evidence this task's tripwire just produced",
    );
    if let Some(body) = task.body.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        let shown: String = body.chars().take(CHECK_NOTES_CHARS).collect();
        s.push_str(
            "The task's notes — read these for what the numbers mean and what counts as \
             normal:\n",
        );
        s.push_str(&shown);
        if body.chars().count() > CHECK_NOTES_CHARS {
            s.push_str("\n[…notes truncated…]");
        }
        s.push_str("\n\n");
    }
    s.push_str(&format!(
        "The tripwire `{spec}` just tripped. This is its output, in full:\n\n\
         --- evidence ---\n{evidence}\n--- end evidence ---\n\n"
    ));
    s.push_str(
        "Say what it MEANS. You have NO tools — no shell, no Read, no Grep, nothing that \
         reaches the filesystem or the network — so the evidence above and the notes are \
         everything you can know. Do not plan to look something up, do not ask for more, \
         and do not do the task; nobody is watching and there is no second turn worth \
         spending.\n\n\
         Your whole reply is written onto the task as a comment, so write it for somebody \
         who will read it without the evidence in front of them. START WITH ONE SENTENCE \
         saying whether anything needs doing. Then, briefly:\n\
         - the numbers that decided it, quoted from the evidence;\n\
         - what changed, if the notes give you something to compare against;\n\
         - the one next step, or that none is needed.\n\n\
         If the evidence does not actually answer the question the notes are asking, say \
         THAT plainly and say what would. A confident reading of the wrong number is the \
         one outcome worse than saying you cannot tell. Keep it short — a few hundred words \
         at most, markdown, with numbers and paths in backticks.\n",
    );
    Ok(s)
}

/// Write `interpret-prompt.txt` into the task's scratch dir. Returns the dir.
///
/// Beside `prompt.txt` and `check-prompt.txt`, never over either: three
/// different questions about one task.
pub fn write_interpret_scratch(task: &Task, repo: &Path) -> Result<PathBuf, AppError> {
    let scratch = scratch_dir_for(task);
    fs::create_dir_all(&scratch)?;
    fs::write(scratch.join(INTERPRET_PROMPT_FILE), interpret_prompt(task, repo)?)?;
    Ok(scratch)
}

/// The full builder, including what to say when the tree is a mirror.
///
/// `mirror` is `Some` only on a host executing against a materialized
/// snapshot. It is a parameter rather than something inferred from `repo`
/// because inferring it means a path-shape heuristic deciding whether an
/// unattended agent is told the truth about its own filesystem — and the day
/// that heuristic is wrong, the agent reports the absence of `.env` as a
/// finding and nobody can see why.
pub fn build_prompt_full(
    task: &Task,
    repo: &Path,
    target: PromptTarget,
    history: &str,
    mirror: Option<&crate::mirror::Snapshot>,
) -> String {
    build_prompt_scoped(task, repo, target, history, mirror, "")
}

/// The actual dispatch and the copyable continuation use the same live scope.
pub fn build_prompt_with_context(
    conn: &rusqlite::Connection,
    task: &Task,
    repo: &Path,
    target: PromptTarget,
    mirror: Option<&crate::mirror::Snapshot>,
) -> String {
    let scope = match crate::db::tasks::list_children(conn, &task.id) {
        Ok(children) if children.is_empty() => String::new(),
        Ok(children) => {
            let mut scope = String::from("Task subtasks — read these alongside the parent request and account for relevant requirements:\n");
            for child in children {
                let reference = child.short_id.map(|n| format!("T{n}")).unwrap_or(child.id.clone());
                scope.push_str(&format!("\n- {reference} [{}]: {}\n", child.status, child.title));
                if let Some(body) = child.body.as_deref().filter(|body| !body.trim().is_empty()) {
                    scope.push_str(&format!("  Notes: {body}\n"));
                }
                if let Some(prompt) = child.prompt.as_deref().filter(|prompt| !prompt.trim().is_empty()) {
                    scope.push_str(&format!("  Instruction: {prompt}\n"));
                }
            }
            scope.push('\n');
            scope
        }
        Err(_) => String::from("Task subtasks could not be loaded. Re-read the task before claiming the whole request is ready for review.\n\n"),
    };
    build_prompt_scoped(task, repo, target, &history_block(conn, task), mirror, &scope)
}

fn build_prompt_scoped(
    task: &Task,
    repo: &Path,
    target: PromptTarget,
    history: &str,
    mirror: Option<&crate::mirror::Snapshot>,
    scope: &str,
) -> String {
    let agent = match target {
        PromptTarget::Foreground => "Claude",
        PromptTarget::Background(e) => e.label(),
    };
    let mut s = context_block(task, repo, agent, "working one specific task");

    // `history` ends in '\n' (see `history_block`), so `is_empty()` alone would
    // be true only for the literal `""` that `build_prompt` passes — every
    // OTHER caller would look like a continuation. `trim()` is load-bearing.
    let has_history = !history.trim().is_empty();
    s.push_str(brief_label(task.bg_intent, has_history));
    s.push_str(&task.effective_prompt());
    s.push_str("\n\n");
    if let Some(body) = task.body.as_deref().filter(|body| !body.trim().is_empty()) {
        if body.trim() != task.effective_prompt().trim() {
            s.push_str("Task notes — part of the requested scope:\n");
            s.push_str(body);
            s.push_str("\n\n");
        }
    }
    s.push_str(scope);

    // AFTER the instruction, before the tail. The seed prompt is the definition
    // of the work and has to be read as such; history is context that qualifies
    // it. Putting history first would bury the actual instruction under a
    // transcript that grows every run.
    s.push_str(history);

    // Between the history and the tail, so "the handoff above" refers to
    // something the agent has just read. Emitted here rather than from a
    // provider tail for the same reason `claude::handoff_tail` is shared
    // verbatim by Codex: what makes a continuation a continuation has nothing
    // to do with which agent is reading it.
    s.push_str(&framing_block(task.bg_intent, has_history));

    // BEFORE the provider tail, because everything after this assumes the tree
    // is the real one, and this is the paragraph that says it is not.
    if let Some(snap) = mirror {
        s.push_str("\n");
        s.push_str(&format!(
            "WORKSPACE: mirror snapshot {} taken {}{}.\n\n",
            &snap.sha[..8.min(snap.sha.len())],
            if snap.taken_at.is_empty() { "recently" } else { &snap.taken_at },
            if snap.dirty_files > 0 {
                format!(" · {} uncommitted file(s) included", snap.dirty_files)
            } else {
                String::new()
            }
        ));
        s.push_str(crate::mirror::MIRROR_CONTEXT);
        s.push_str("\n");
    }

    match target {
        // The tail belongs to the provider: what "don't write files" means
        // depends on whether the guard is a tool deny-list or an OS sandbox,
        // and a prompt that describes the wrong one is worse than a vague one.
        PromptTarget::Background(executor) => {
            s.push_str(&crate::runner::Provider::for_executor(executor).prompt_tail(task));
        }
        // Foreground honours the intent too. The Review control sits next to
        // BOTH "Delegate to X" and "Open in Claude", so a foreground dispatch
        // that silently ignored an armed review is exactly the half-wired
        // divergence gotcha #18 is about.
        PromptTarget::Foreground if task.bg_intent.is_review() => {
            s.push_str(
                "Leave your verdict by calling `mcp__cxtasks__comment_task` with this task's id. \
                 That write-back is the only thing that outlives this session.\n\
                 Keep it under 2,000 characters — a verdict that needs more than that is a \
                 handoff, so pass `handoff: true` on the same call and the full text is kept on \
                 the run while the log gets a one-line summary.\n\
                 Do NOT call `mcp__cxtasks__complete_task`: the review is not the work, and \
                 closing the task would claim work finished that nobody has done yet.\n",
            );
        }
        PromptTarget::Foreground => {
            s.push_str(
                "When the work is genuinely done, call `mcp__cxtasks__complete_task` with this \
                 task's id and a `result`. That write-back is the only thing that outlives this \
                 session — without it the work is invisible to CXTasks.\n\
                 Write the `result` as a HANDOFF, not a one-liner, for a reader with none of \
                 your context: the requested outcome, what you did and found (name relevant \
                 sources, artifacts, or observed results), what remains and why, and the next concrete step. \
                 The full text is kept on the run and replayed to whoever picks this task up \
                 next; a short summary is derived from your opening line for the task's log, so \
                 write that first line as the outcome and don't write the summary yourself.\n\
                 Mid-way, `mcp__cxtasks__comment_task` records anything worth keeping that isn't \
                 the final answer — a dead end you ruled out, a decision and its reason. Keep \
                 those under 2,000 characters; if you have a full handoff to leave without \
                 finishing the task, pass `handoff: true` and it takes the same split.\n\
                 If you get blocked, call `mcp__cxtasks__update_task` with status \"blocked\" and \
                 a `blocked_reason` saying what it is waiting on — the status is refused without \
                 one.\n",
            );
            s.push_str(crate::runner::completion::EXPECTATIONS);
        }
    }
    s
}

/// Write the `launch.sh` Ghostty executes. `cd`s to the **repo**, not the
/// scratch dir — that's the whole point of carrying a repo_path, and it's the
/// one place this differs from cxmail (which has no repo to land in).
///
/// It passes `--dangerously-skip-permissions` deliberately, and this is the
/// **opposite** of the rule background runs follow. Two reasons it's safe here
/// and would not be there. A human is watching: foreground dispatch exists to
/// put a session in front of you, so the approval prompts are being answered by
/// someone who can see what is being asked, and answering "yes" to each one is
/// the only thing they were buying. And it matches what Chris gets by typing
/// `claude` himself — his interactive shell defines a function that injects
/// this exact flag (`~/.zshrc`, gotcha #7), while `find_claude_cli` resolves
/// through a *non-interactive* login shell and therefore returns the bare
/// binary. Without the flag, a dispatched session was the one Claude on this
/// machine that stopped to ask.
///
/// Do NOT carry this into `runner::process` — an unattended run has nobody to
/// answer a prompt, and the tool grant there is the whole guard (invariant 7).
pub fn write_launch_script(
    scratch: &Path,
    repo: &Path,
    claude_cli: &Path,
) -> Result<PathBuf, AppError> {
    let launch_path = scratch.join("launch.sh");
    let script = format!(
        "#!/bin/zsh -l\nset -e\ncd {repo}\nexec {claude} --dangerously-skip-permissions \"$(cat {prompt})\"\n",
        repo = shell_quote(&repo.to_string_lossy()),
        claude = shell_quote(&claude_cli.to_string_lossy()),
        prompt = shell_quote(&scratch.join("prompt.txt").to_string_lossy()),
    );
    fs::write(&launch_path, script)?;
    let mut perms = fs::metadata(&launch_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&launch_path, perms)?;
    Ok(launch_path)
}

/// The outcome of queueing, including whether anything will actually pick it
/// up.
///
/// Queueing succeeds whether or not the runner is installed — the status is
/// still the right one to record. But "Queued" with nothing draining the queue
/// is indistinguishable from "Queued, running shortly", and it stays that way
/// forever (gotcha #18). So the two facts that decide whether work will move
/// travel back with the task, and every entry point says which case it is in.
#[derive(Debug)]
pub struct QueueOutcome {
    pub task: Task,
    /// Is the LaunchAgent installed — i.e. is there a runner process at all?
    pub runner_installed: bool,
    /// Is the `STOP_BACKGROUND` kill switch tripped?
    pub paused: bool,
    /// Every registered host, and what it made of this task.
    ///
    /// **Every entry point renders this.** A queue that succeeded but whose
    /// only capable host has been asleep for a day is indistinguishable from a
    /// healthy one otherwise, and it stays that way forever — the same failure
    /// gotcha #18 names for a missing runner, one host up.
    pub hosts: Vec<HostReadiness>,
}

/// What one host made of one task.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HostReadiness {
    pub host_id: String,
    pub label: String,
    /// `Some` when this host can run the task, naming the workspace it would
    /// use. Freshness is visible here: `Live` is the real tree, `Mirror` is a
    /// snapshot.
    pub workspace: Option<crate::routing::WorkspaceKind>,
    /// `None` when it can. The rendered reason when it cannot — rendered rather
    /// than structured because it is read by a person deciding what to fix, and
    /// every caller would otherwise re-render the same sentence differently.
    pub unmet: Option<String>,
    /// Has it reported in recently enough to be worth waiting for?
    pub alive: bool,
    pub paused: bool,
}

impl HostReadiness {
    /// Can this host run the task at all, ignoring whether it is awake?
    pub fn capable(&self) -> bool {
        self.workspace.is_some()
    }
    /// Capable AND worth waiting for.
    pub fn ready(&self) -> bool {
        self.capable() && self.alive && !self.paused
    }
}

/// Does any registered host declare a detached workspace?
///
/// # There is no "this task is detached" flag, and that is deliberate
///
/// A detached task IS one with no `repo_path`. Every drain query that predates
/// Phase 4 — including the one generated for any host that does not declare
/// `Detached` — already carries `repo_path IS NOT NULL`, so such a task is
/// invisible to a leftover runner with **no new column at all**. The existing
/// guard is the skew guard. Worth saying out loud, because the next reader will
/// go looking for a third flag and not find one.
///
/// What that leaves is a message problem, not a safety one: "somebody forgot
/// the repo" and "this is deliberately repo-less" look identical on the row.
/// [`admit`] refuses both unless a host can actually run it, so nothing unsafe
/// turns on this — it only decides whether the user is told to add a repo path
/// (right nineteen times in twenty) or given a capability refusal naming hosts.
fn any_host_offers_detached(conn: &rusqlite::Connection) -> bool {
    crate::db::hosts::list(conn)
        .map(|hosts| {
            hosts
                .iter()
                .any(|h| h.enabled && h.workspaces.contains(&crate::routing::WorkspaceKind::Detached))
        })
        .unwrap_or(false)
}

/// Which hosts could run this task, and why the others could not.
///
/// # The distinction that makes this readable
///
/// **A capability miss refuses. A liveness miss warns.** They feel similar and
/// are not: a task no host is *configured* to run will never move however long
/// you wait, so accepting it into the queue is a lie that only resolves when
/// somebody goes looking. A task whose capable host is merely asleep will run
/// the moment it wakes, and refusing it would mean closing the laptop broke
/// queueing.
///
/// Errors name the **best near-miss** rather than listing every host, because
/// "the NAS is read-only and this asks for a shell" is an instruction and "no
/// host can run this" is a dead end.
///
/// No filesystem, no subprocess — that is [`crate::runner::preflight`]'s job at
/// the other end of the wait, on the host about to spawn.
pub fn admit(conn: &rusqlite::Connection, task: &Task) -> Result<Vec<HostReadiness>, AppError> {
    let req = crate::routing::RunRequirement::for_task(task);

    // An EMPTY registry falls back to this machine as declared, and does not
    // write.
    //
    // Both halves matter. Without the fallback, the first queue on a new
    // install refuses for want of a registry nobody has populated — the runner
    // populates it on its first tick, which may not have happened yet.
    //
    // Without "does not write", every process that ever calls this creates a
    // host row, and `cxtasks-server` is a process that calls this. It cannot
    // execute anything, so it would appear in the registry as a machine that
    // can do everything and drains nothing — and a task only IT could run would
    // be admitted and then sit forever. A host row means "a machine that runs
    // tasks", and the only process entitled to write one is the runner, which
    // does so on every tick.
    let registered = crate::db::hosts::list(conn)?;
    let hosts = if registered.is_empty() {
        vec![crate::routing::HostCapability::declared_for_this_host()]
    } else {
        registered
    };

    let mut out = Vec::new();
    for cap in hosts {
        let mirrors = crate::db::hosts::mirrors(conn, &cap.id)?;
        let verdict = crate::routing::satisfies(&cap, &req, &mirrors);
        out.push(HostReadiness {
            alive: cap.is_alive(crate::db::hosts::HOST_STALE_MINUTES),
            paused: cap.paused,
            host_id: cap.id.clone(),
            label: cap.label.clone(),
            workspace: verdict.as_ref().ok().copied(),
            unmet: verdict.as_ref().err().map(|u| u.to_string()),
        });
    }

    if out.iter().any(HostReadiness::capable) {
        return Ok(out);
    }

    // Nothing can run it. Say which host came closest and what it lacked — the
    // most specific refusal available is the one that gets acted on.
    let near = out
        .iter()
        .find(|h| h.unmet.is_some())
        .map(|h| format!("{} {}", h.label, h.unmet.as_deref().unwrap_or("cannot run this")));
    Err(AppError::Dispatch(match near {
        Some(reason) => format!(
            "No host can run this task: {reason}. Change the task's tool grant, \
             or give a host the capability in Settings."
        ),
        None => "No hosts are registered, so there is nothing to run this task."
            .to_string(),
    }))
}

/// The ONE path into `status = 'queued'` — guards, preflight, then the write.
///
/// Shared by the app's queue command and the MCP's `update_task(status =
/// "queued")`, because `queued` is the runner's claim key and the guards here
/// are the only thing standing between a hand-set status and an unattended
/// agent (invariant 16). The MCP used to be a third entry point that ran none
/// of them: a task queued without `bg_allowed` never matches the drain query
/// and sits at "Queued" forever, and a review armed with nothing to review
/// dispatches against a handoff the prompt can't show. Gotcha #18's lesson —
/// every route into queueing needs the same wiring — as one implementation
/// rather than three kept in step by hand.
///
/// `with_conn` is called once to read and once to write, and is deliberately
/// NOT held across the middle: `runner::preflight` shells out (a login shell
/// to find the CLI; `codex login status` for Codex), and the app passes a
/// mutex-guarded connection that must not be locked for the duration of a
/// subprocess. If any guard or preflight fails, the second call never happens
/// and the task is left exactly as it was — not silently enabled.
pub fn queue_for_background<F>(mut with_conn: F, task_ref: &str) -> Result<QueueOutcome, AppError>
where
    F: FnMut(
        &mut dyn FnMut(&rusqlite::Connection) -> Result<Task, AppError>,
    ) -> Result<Task, AppError>,
{
    // Filled by the guard closure below. Declared out here because the caller
    // renders it and `with_conn` can only hand back a `Task`.
    let mut hosts: Vec<HostReadiness> = Vec::new();

    let mut task = with_conn(&mut |conn| {
        let task = crate::db::tasks::get(conn, task_ref)?
            .ok_or_else(|| AppError::NotFound(format!("task {task_ref} not found")))?;

        // The ONE queue-time repo refusal in the workspace, and it grows an
        // explicit `Detached` branch rather than losing the check.
        //
        // Invariant 12 is satisfied differently, not weakened: background
        // execution is refused outside a DECLARED workspace. A repo is one. A
        // container mount set the app authored is another. `$HOME` is neither,
        // which is exactly why the MacBook does not declare `Detached` — an
        // unattended run there would get unsupervised Read/Glob/Grep over every
        // client repo, ~/.ssh and the notes vault.
        //
        // `admit` below is what actually enforces that, by asking every
        // registered host. This check stays because its message is better: "add
        // a repo path" is the remedy nineteen times in twenty, and a capability
        // refusal naming three hosts would bury it.
        if task.repo_path.is_none() && !any_host_offers_detached(conn) {
            return Err(AppError::Dispatch(
                "Add a repo path first — unattended runs need a bounded workspace. \
                 (A task with no repo can still run if it is marked detached and a host \
                 declares a detached workspace.)"
                    .into(),
            ));
        }
        if let Err(e) = crate::mcp_registry::parse_list(
            task.bg_mcp_servers.as_deref().unwrap_or_default(),
        ) {
            return Err(AppError::Dispatch(e.to_string()));
        }
        // An armed tripwire is a task that is WAITING, and the drain query
        // skips it for the same reason it skips a deferred one — so queueing
        // it would park it at `queued` where nothing will ever claim it.
        // Named, rather than left to sit: the remedy is on the task.
        if task.tripwire_armed() {
            return Err(AppError::Dispatch(format!(
                "This task is waiting on its tripwire ({}) and is hidden until it trips, so \
                 the runner would never claim it. Remove or re-check the tripwire first.",
                task.tripwire.as_deref().unwrap_or("?")
            )));
        }
        // A decision is a question for Chris, not work for an agent — the
        // drain queries skip it for the same reason next_task does, so queueing
        // it would park it at `queued` where nothing will ever claim it. Same
        // shape as the tripwire refusal above: name the remedy, on the task.
        if crate::db::tags::has(conn, &task.id, crate::db::tasks::DECISION_TAG)? {
            return Err(AppError::Dispatch(
                "This task is tagged `decision` — a call only Chris can make, which no agent \
                 will be handed. Answer it in the app (Today → Decide), or remove the tag if \
                 it was mis-filed."
                    .into(),
            ));
        }

        // Before `bg_allowed = true` and before `preflight`, deliberately:
        // running preflight first would surface "Codex isn't logged in" for a
        // task that could never have been reviewed anyway, sending the user
        // off to fix an unrelated thing.
        //
        // `reviewable_run` is shared with `history_block`'s replay so the
        // guard names the SAME run the prompt will show — accepting a review
        // of something the prompt won't carry is the
        // `list_queued_for_background` / `claim_for_background` failure, one
        // level up. One error string, not two: "no runs at all" and "no run
        // left a handoff" are indistinguishable to the user and have the
        // identical remedy.
        if task.bg_intent.is_review() && reviewable_run(conn, &task.id).is_none() {
            return Err(AppError::Dispatch(
                "Nothing to review yet — no finished run has left a handoff on this task. \
                 Queue the work first; a review reads what the work leaves behind."
                    .into(),
            ));
        }

        // Last of the queue-time guards, and the only one that can refuse for a
        // reason outside the task itself. It reads `bg_writes_allowed`,
        // `bg_shell_allowed`, `bg_executor` and `repo_path` — all of which are
        // already persisted — so running it here rather than after the
        // in-memory `bg_allowed = true` below changes nothing about its answer.
        hosts = admit(conn, &task)?;
        Ok(task)
    })?;

    // Queueing is the opt-in. The stored flag stays because the runner still
    // needs a durable permission gate, but it is set in the same operation
    // that records the explicit queue action — asking for the same decision
    // twice is the old UI this replaced.
    //
    // Mutate the in-memory copy before preflight so it sees the state that
    // will be persisted. The SAME check the runner makes immediately before
    // spawning — called rather than re-implemented — which also covers "is
    // the agent's CLI installed and logged in": a missing Codex install
    // becomes an error the caller sees now, not a task that silently sits at
    // `queued`.
    task.bg_allowed = true;
    crate::runner::preflight(&task).map_err(AppError::Dispatch)?;

    let task = with_conn(&mut |conn| {
        crate::db::tasks::update(
            conn,
            &task.id,
            &crate::db::tasks::TaskPatch {
                status: Some("queued".into()),
                bg_allowed: Some(true),
                ..Default::default()
            },
        )
    })?;

    Ok(QueueOutcome {
        task,
        runner_installed: crate::platform::drain_is_installed(),
        paused: crate::paths::background_paused(),
        hosts,
    })
}

/// Is `repo` a git worktree with no uncommitted changes?
///
/// The gate for a write-capable background run: an unattended agent editing
/// files on top of work in progress makes the damage impossible to separate
/// from the user's own edits. Clean tree ⇒ `git diff` after the run is exactly
/// what the agent did.
pub fn git_tree_is_clean(repo: &Path) -> Result<bool, AppError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|e| AppError::Dispatch(format!("could not run git in {}: {e}", repo.display())))?;

    if !output.status.success() {
        // Not a git repo at all — treat as "not clean" so writes stay blocked.
        return Ok(false);
    }
    Ok(output.stdout.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tasks::{create, NewTask};

    // ── the CLI override ──────────────────────────────────────────────────

    /// A set-but-missing override must NOT read as "unset".
    ///
    /// The two outcomes look alike from the outside — both end with
    /// `find_claude_cli` not returning that path — but they mean opposite
    /// things. `NotSet` says "go and look"; `Missing` says "the binary you
    /// named isn't there, and I am not going to quietly run a different one."
    /// Collapsing them is how a container pointed at a bad path ends up
    /// executing whatever `claude` happens to be on the image instead.
    #[test]
    fn a_missing_override_is_not_the_same_as_an_absent_one() {
        let ghost = std::path::PathBuf::from("/nonexistent/claude-that-is-not-there");
        assert_eq!(
            cli_override(Some(ghost.clone().into_os_string())),
            CliOverride::Missing(ghost)
        );
        assert_eq!(cli_override(None), CliOverride::NotSet);
    }

    /// Empty reads as absent, matching `paths::root_from`. A shell
    /// substitution that came back empty is not a request to run `""`.
    #[test]
    fn an_empty_cli_override_falls_through_to_discovery() {
        assert_eq!(cli_override(Some("".into())), CliOverride::NotSet);
    }

    /// A real file is taken at its word — no PATH probe, no login shell.
    #[test]
    fn an_override_naming_a_real_file_wins_outright() {
        // /bin/sh is a file on every machine this will ever build on.
        let real = std::path::PathBuf::from("/bin/sh");
        assert_eq!(
            cli_override(Some(real.clone().into_os_string())),
            CliOverride::Use(real)
        );
    }

    // ── the one queue path ────────────────────────────────────────────────

    /// The guards fire BEFORE anything is written. A task queued without the
    /// opt-in never matches the drain query (`enabled_sql` filters on it), so
    /// the failure this prevents is a task sitting at "Queued" forever with
    /// nothing on screen to explain why.
    #[test]
    fn queue_for_background_refuses_a_task_without_a_repo_and_writes_nothing() {
        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "no repo".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let err = queue_for_background(|f| f(&conn), &t.id).unwrap_err();
        assert!(format!("{err}").contains("repo path"), "{err}");

        let after = crate::db::tasks::get(&conn, &t.id).unwrap().unwrap();
        assert_eq!(after.status, t.status, "a refused queue must not move the status");
        assert!(!after.bg_allowed, "a refused queue must not opt the task in");
    }

    /// An armed review with no finished handoff is refused — the queue guard
    /// and `history_block`'s replay must name the same run (invariant 19), and
    /// this guard exists ONLY at queue time: the runner's claim-time preflight
    /// never re-checks it, so an entry point that skips it dispatches a review
    /// of an artifact the prompt can't show.
    #[test]
    fn queue_for_background_refuses_a_review_with_nothing_to_review() {
        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "review me".into(),
                repo_path: Some("/tmp".into()),
                ..Default::default()
            },
        )
        .unwrap();
        crate::db::tasks::update(
            &conn,
            &t.id,
            &crate::db::tasks::TaskPatch {
                bg_intent: Some(BackgroundIntent::Review),
                ..Default::default()
            },
        )
        .unwrap();

        let err = queue_for_background(|f| f(&conn), &t.id).unwrap_err();
        assert!(format!("{err}").contains("Nothing to review"), "{err}");
    }

    /// Status and the opt-in land in ONE write, only after every guard and
    /// preflight pass. Tolerant of a machine without the claude CLI (the
    /// runner tests' pattern): there the preflight refusal is legitimate, and
    /// the assertion becomes "nothing was written".
    #[test]
    fn queue_for_background_writes_status_and_opt_in_together_or_not_at_all() {
        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "queue me".into(),
                repo_path: Some(std::env::temp_dir().to_string_lossy().to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        match queue_for_background(|f| f(&conn), &t.id) {
            Ok(outcome) => {
                assert_eq!(outcome.task.status, "queued");
                assert!(outcome.task.bg_allowed, "queueing IS the opt-in");
            }
            Err(e) => {
                let msg = format!("{e}");
                assert!(msg.contains("claude"), "only a missing CLI may refuse here: {msg}");
                let after = crate::db::tasks::get(&conn, &t.id).unwrap().unwrap();
                assert_eq!(after.status, t.status);
                assert!(!after.bg_allowed);
            }
        }
    }

    fn task(title: &str, prompt: Option<&str>, body: Option<&str>) -> Task {
        let conn = crate::db::schema::test_conn();
        create(
            &conn,
            &NewTask {
                title: title.into(),
                body: body.map(str::to_string),
                prompt: prompt.map(str::to_string),
                repo_path: Some("/tmp".into()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn every_dispatch_includes_notes_and_subtasks_alongside_the_seed_instruction() {
        let conn = crate::db::schema::test_conn();
        let parent = create(&conn, &NewTask {
            title: "Prepare the event plan".into(),
            prompt: Some("Review the subtasks and complete the plan".into()),
            body: Some("Use the supplied budget and include accessibility requirements.".into()),
            ..Default::default()
        }).unwrap();
        for (title, body, prompt) in [
            ("Compare venues", "Cite the source for each estimate", None),
            ("Check travel dates", "Account for holidays", Some("Include the alternate date")),
        ] {
            create(&conn, &NewTask {
                title: title.into(), body: Some(body.into()), prompt: prompt.map(str::to_string),
                parent_id: Some(parent.id.clone()), ..Default::default()
            }).unwrap();
        }
        for target in [PromptTarget::Foreground, BG_CLAUDE, PromptTarget::Background(BackgroundExecutor::Codex)] {
            let p = build_prompt_with_context(&conn, &parent, Path::new("/tmp"), target, None);
            for requirement in ["Review the subtasks", "accessibility requirements", "Compare venues", "Cite the source", "Account for holidays", "Include the alternate date"] {
                assert!(p.contains(requirement), "{requirement} absent: {p}");
            }
            assert!(p.contains("What to do:"));
            assert!(!p.contains("This is a CONTINUATION"), "scope is not a previous run");
            assert!(p.contains("evidence appropriate to this task") || p.contains("evidence \nappropriate to this task"));
        }
    }

    // ── prior context (the handoff being READ, not written) ───────────────

    /// The half that makes a handoff worth writing. Before this, a delegated
    /// run's findings went onto the run row, the task went to `in_review`, and
    /// a second delegate was handed the ORIGINAL seed prompt — so it re-derived
    /// everything at full cost and the summary was read by nobody.
    #[test]
    fn a_second_dispatch_is_handed_what_the_first_one_learned() {
        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "fix the webhook".into(),
                repo_path: Some("/tmp".into()),
                ..Default::default()
            },
        )
        .unwrap();

        // A fresh task has no history, and must not carry an empty scaffold —
        // a "what happened before" heading with nothing under it reads as a
        // prior run that produced nothing.
        assert_eq!(history_block(&conn, &t), "");

        crate::db::comments::add(
            &conn,
            &t.id,
            crate::db::comments::AUTHOR_CLAUDE,
            "**Background run (Claude Code)**\n\nThe 429 comes from lib/webhook.ts:88.",
        )
        .unwrap();

        let block = history_block(&conn, &t);
        assert!(block.contains("What happened before"), "{block}");
        assert!(block.contains("lib/webhook.ts:88"), "{block}");
        assert!(block.contains("claude"), "the author is part of the record: {block}");

        // And it reaches the prompt — after the instruction, never before it.
        // The label is the CONTINUATION one here, not `What to do:`: prior work
        // exists, so the seed prompt is being introduced as the original brief.
        let p = build_prompt_with_history(&t, Path::new("/tmp"), BG_CLAUDE, &block);
        let brief = p.find("ORIGINAL BRIEF").expect("the instruction");
        let history = p.find("What happened before").expect("the history");
        assert!(
            brief < history,
            "the seed prompt is the definition of the work; history qualifies it"
        );
    }

    /// Bounded, because this lands in the prompt of every FUTURE run of the
    /// task. Unbounded history is a task whose cost grows with how often it has
    /// been worked — and per invariant 5 that cost is then re-read on every
    /// turn of every subsequent run.
    #[test]
    fn history_keeps_the_newest_comments_and_says_what_it_dropped() {
        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "long-running".into(),
                repo_path: Some("/tmp".into()),
                ..Default::default()
            },
        )
        .unwrap();
        for i in 0..(HISTORY_COMMENTS + 3) {
            crate::db::comments::add(&conn, &t.id, "claude", &format!("note number {i}")).unwrap();
        }

        let block = history_block(&conn, &t);
        assert!(
            block.contains(&format!("note number {}", HISTORY_COMMENTS + 2)),
            "the most recent note must survive: {block}"
        );
        assert!(
            !block.contains("note number 0"),
            "the oldest must be the one dropped: {block}"
        );
        assert!(
            block.contains("3 older comment(s) omitted"),
            "silent truncation reads as 'that's all there is': {block}"
        );

        // A single enormous comment is truncated rather than dropped — losing
        // the whole entry loses that it happened at all.
        let conn2 = crate::db::schema::test_conn();
        let t2 = create(
            &conn2,
            &NewTask { title: "one big note".into(), ..Default::default() },
        )
        .unwrap();
        let huge = "x".repeat(HISTORY_COMMENT_CHARS * 3);
        crate::db::comments::add(&conn2, &t2.id, "claude", &huge).unwrap();
        let block2 = history_block(&conn2, &t2);
        assert!(block2.contains("truncated"), "{block2}");
        assert!(block2.len() < huge.len(), "it has to actually shrink");
    }

    #[test]
    fn shell_quote_survives_embedded_quotes_and_spaces() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("with space"), "'with space'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    /// Round-trip through a REAL shell rather than asserting on the quoted
    /// string's shape. Correct escaping produces `'a'\''; rm …'`, which still
    /// *contains* the substring `'; rm` — so a substring assertion here reports
    /// a failure on working code and would tempt someone into "fixing" the
    /// quoting. Asking `sh` to echo it back is the only assertion that means
    /// what it says.
    #[test]
    fn shell_quote_is_inert_when_a_real_shell_expands_it() {
        for evil in [
            "/tmp/a'; rm -rf /; echo '",
            "/tmp/$(whoami)",
            "/tmp/`id`",
            "/tmp/a b\"c$d",
        ] {
            let out = Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("printf %s {}", shell_quote(evil)))
                .output()
                .expect("run sh");
            assert!(out.status.success(), "sh rejected {evil:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                evil,
                "shell expansion altered {evil:?} — quoting is not inert"
            );
        }
    }

    #[test]
    fn sanitize_id_strips_path_separators() {
        assert_eq!(sanitize_id("abc-123_x"), "abc-123_x");
        // Six leading non-alphanumerics ("../../") each become one underscore,
        // so no `/` survives to escape the sessions directory.
        let sanitized = sanitize_id("../../etc/passwd");
        assert_eq!(sanitized, "______etc_passwd");
        assert!(!sanitized.contains('/') && !sanitized.contains('.'));
        assert_eq!(sanitize_id(""), "_");
    }

    #[test]
    fn scratch_dir_stays_inside_the_sessions_dir_for_a_hostile_id() {
        let conn = crate::db::schema::test_conn();
        let mut t = create(
            &conn,
            &NewTask {
                title: "x".into(),
                ..Default::default()
            },
        )
        .unwrap();
        t.id = "../../../../tmp/pwned".into();
        let dir = scratch_dir_for(&t);
        assert!(
            dir.starts_with(crate::paths::sessions_dir()),
            "scratch dir escaped: {dir:?}"
        );
    }

    #[test]
    fn resolve_repo_rejects_missing_and_absent_paths() {
        let conn = crate::db::schema::test_conn();
        let plain = create(
            &conn,
            &NewTask {
                title: "plain".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let err = resolve_repo(&plain).unwrap_err();
        assert!(format!("{err}").contains("no repo path"));

        let stale = create(
            &conn,
            &NewTask {
                title: "stale".into(),
                repo_path: Some("/definitely/not/here/98765".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let err = resolve_repo(&stale).unwrap_err();
        assert!(format!("{err}").contains("no longer exists"), "{err}");

        let ok = task("ok", None, None);
        assert_eq!(resolve_repo(&ok).unwrap(), PathBuf::from("/tmp"));
    }

    #[test]
    fn resolve_repo_or_home_falls_back_when_unset_but_still_rejects_stale_paths() {
        let conn = crate::db::schema::test_conn();
        let plain = create(
            &conn,
            &NewTask {
                title: "plain".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            resolve_repo_or_home(&plain).unwrap(),
            dirs::home_dir().unwrap(),
            "no repo_path must fall back to $HOME, not error"
        );

        let stale = create(
            &conn,
            &NewTask {
                title: "stale".into(),
                repo_path: Some("/definitely/not/here/98765".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let err = resolve_repo_or_home(&stale).unwrap_err();
        assert!(
            format!("{err}").contains("no longer exists"),
            "an EXPLICIT but stale repo_path must still error, not silently fall back: {err}"
        );

        let ok = task("ok", None, None);
        assert_eq!(resolve_repo_or_home(&ok).unwrap(), PathBuf::from("/tmp"));
    }

    const FG: PromptTarget = PromptTarget::Foreground;
    const BG_CLAUDE: PromptTarget = PromptTarget::Background(BackgroundExecutor::Claude);
    const BG_CODEX: PromptTarget = PromptTarget::Background(BackgroundExecutor::Codex);

    #[test]
    fn prompt_uses_explicit_prompt_when_present() {
        let t = task("title", Some("do the specific thing"), Some("some notes"));
        let p = build_prompt(&t, Path::new("/tmp"), FG);
        assert!(p.contains("do the specific thing"));
        assert!(p.contains("some notes"), "the instruction must not erase the task's requirements");
    }

    #[test]
    fn prompt_falls_back_to_title_and_body() {
        let t = task("fix the retry", None, Some("it drops on 429"));
        let p = build_prompt(&t, Path::new("/tmp"), FG);
        assert!(p.contains("fix the retry"));
        assert!(p.contains("it drops on 429"));
    }

    #[test]
    fn foreground_prompt_asks_the_agent_to_complete_the_task() {
        let t = task("x", None, None);
        let p = build_prompt(&t, Path::new("/tmp"), FG);
        assert!(p.contains("call `mcp__cxtasks__complete_task`"));
        assert!(p.contains(&t.id), "prompt must carry the task id");
        // The `result` is the handoff channel, not a caption on one — it is
        // kept in full on the run row and replayed to the next session.
        assert!(p.contains("Write the `result` as a HANDOFF"), "{p}");
        // **A confirmed stale instruction, fixed.** The tail used to say to put
        // a blocker's reason "in the body", and `db::tasks::update` REFUSES
        // `blocked` without `blocked_reason` (invariant 16) — so following the
        // prompt produced a rejected call with nothing explaining it.
        assert!(p.contains("blocked_reason"), "{p}");
        assert!(
            !p.contains("put the reason in the body"),
            "the body is the definition; `blocked` is refused without a reason: {p}"
        );
    }

    /// The inverse of the foreground rule, and the fix for a bug seen live: a
    /// background agent that calls `complete_task` races the runner's status
    /// write AND skips the human review that `needs_review` exists for.
    #[test]
    fn background_prompt_forbids_self_completion() {
        let t = task("x", None, None);
        let p = build_prompt(&t, Path::new("/tmp"), BG_CLAUDE);
        assert!(
            !p.contains("When the work is genuinely done, call"),
            "background prompt must not carry the foreground instruction"
        );
        assert!(p.contains("FINAL MESSAGE is the result"));
        assert!(p.contains(&t.id));

        // Self-completion is now prevented STRUCTURALLY — `complete_task` is
        // removed from the router a background run talks to
        // (`mcp::server::BACKGROUND_TOOLS`), so there is no tool to forbid. What
        // the prompt must still never do is *name* it, in either branch: with
        // tools, naming a removed tool sends the agent chasing something that
        // doesn't exist; without tools, it implies a channel that isn't there.
        for has_tools in [true, false] {
            let tail = crate::runner::providers::claude::handoff_tail(&t, has_tools);
            assert!(
                !tail.contains("complete_task"),
                "has_tools={has_tools}: the tail must not name a tool the run doesn't have: {tail}"
            );
            assert!(
                tail.contains("HANDOFF"),
                "has_tools={has_tools}: every background tail asks for a handoff"
            );
        }
        assert!(
            crate::runner::providers::claude::handoff_tail(&t, true)
                .contains("no way to mark this task done"),
            "when it HAS tools, say plainly that finishing is not one of them"
        );
    }

    #[test]
    fn background_read_only_prompt_forbids_edits() {
        let t = task("x", None, None);
        let p = build_prompt(&t, Path::new("/tmp"), BG_CLAUDE);
        assert!(p.contains("Do not edit, create or delete files"));

        // Foreground is interactive — no such restriction.
        let fg = build_prompt(&t, Path::new("/tmp"), FG);
        assert!(!fg.contains("Do not edit"));
    }

    #[test]
    fn background_write_enabled_prompt_says_dont_ask_questions() {
        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "x".into(),
                repo_path: Some("/tmp".into()),
                bg_allowed: Some(true),
                bg_writes_allowed: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        let p = build_prompt(&t, Path::new("/tmp"), BG_CLAUDE);
        assert!(p.contains("do not ask"));
        assert!(!p.contains("Do not edit, create or delete files"));
    }

    /// The context block is what makes a task mean the same thing to whoever
    /// picks it up. Only the tail may differ between executors — if the two
    /// prompts diverged above the tail, a task would quietly behave differently
    /// depending on which agent happened to be selected.
    #[test]
    fn both_executors_get_an_identical_task_context_block() {
        let t = task("fix the retry", Some("look at backoff.rs"), None);
        let claude = build_prompt(&t, Path::new("/tmp"), BG_CLAUDE);
        let codex = build_prompt(&t, Path::new("/tmp"), BG_CODEX);

        // Everything up to "What to do:" plus the work itself is shared.
        let shared = |p: &str| p.split("This is an UNATTENDED").next().unwrap().to_string();
        assert_eq!(
            shared(&claude).replace("Claude Code session", "AGENT session"),
            shared(&codex).replace("Codex session", "AGENT session"),
            "the two prompts diverge above the provider tail"
        );
        for p in [&claude, &codex] {
            assert!(p.contains("look at backoff.rs"));
            assert!(p.contains(&t.id));
        }
    }

    /// Provider-specific guards stay distinct, while the shared write-back tail
    /// must describe the same MCP availability for both agents.
    #[test]
    fn each_executor_gets_its_own_guard_and_the_same_writeback_availability() {
        let t = task("x", None, None);
        let claude = build_prompt(&t, Path::new("/tmp"), BG_CLAUDE);
        let codex = build_prompt(&t, Path::new("/tmp"), BG_CODEX);

        let has_tools = crate::runner::providers::writeback_binary().is_some();
        for p in [&claude, &codex] {
            assert!(!p.contains("complete_task"), "background self-completion leaked: {p}");
            if has_tools {
                assert!(p.contains("mcp__cxtasks__file_followup"), "{p}");
                assert!(p.contains("three CXTasks tools and no others"), "{p}");
            } else {
                assert!(
                    !p.contains("mcp__cxtasks__"),
                    "a missing MCP binary must produce no tool names: {p}"
                );
                assert!(
                    p.contains("no CXTasks tools in this run"),
                    "say so explicitly, so the final message is understood as the only channel"
                );
            }
        }
        assert!(codex.contains("sandbox"), "Codex's guard is a sandbox; say so");
        assert!(!claude.contains("READ-ONLY sandbox"));
        // The handoff ask is the SHARED half — same words whoever is reading,
        // because what makes a good handoff has nothing to do with the agent.
        for p in [&claude, &codex] {
            assert!(p.contains("Write it as a HANDOFF, not a summary"), "{p}");
            assert!(p.contains("The next concrete step"), "{p}");
        }
    }

    /// Read-only grants limit actions, but must not redefine the task as planning.
    #[test]
    fn an_investigation_preserves_the_requested_deliverable() {
        let t = task("x", None, None);
        let inspect = crate::runner::providers::claude::handoff_tail(&t, true);
        assert!(inspect.contains("Complete research, analysis, or planning"), "{inspect}");
        assert!(inspect.contains("do not replace the requested deliverable with a plan"), "{inspect}");

        let mut writing = task("x", None, None);
        writing.bg_writes_allowed = true;
        let tail = crate::runner::providers::claude::handoff_tail(&writing, true);
        assert!(
            !tail.contains("the plan IS the deliverable"),
            "a run that can implement should implement, not plan: {tail}"
        );
    }

    // ── run intent: continuations and reviews ──────────────────────────

    /// **The majority path, and the one nobody writes an absence assertion
    /// for.** `history` ends in `'\n'` (see `history_block`), so deriving
    /// continuation from `history.is_empty()` instead of `trim().is_empty()`
    /// would tell EVERY first dispatch it had prior work — reframing the brief
    /// as "already worked on" on a task nobody has touched.
    #[test]
    fn a_fresh_task_is_dispatched_exactly_as_it_was_before_continuations_existed() {
        let t = task("x", None, None);
        for history in ["", "\n", "   \n  "] {
            let p = build_prompt_with_history(&t, Path::new("/tmp"), BG_CLAUDE, history);
            assert!(p.contains("What to do:\n"), "{p}");
            assert!(!p.contains("ORIGINAL BRIEF"), "a first dispatch has no prior brief: {p}");
            assert!(!p.contains("CONTINUATION"), "{p}");
            assert!(!p.contains("REVIEW"), "{p}");
        }
    }

    /// **The label is the fix, not decoration on it.** The cheap version adds
    /// the continuation block and leaves `What to do:` alone — and a test that
    /// only checks for the new block is green against exactly that bug, while
    /// the agent goes on reading the one line that survives skimming and
    /// starting the task over.
    #[test]
    fn prior_work_reframes_the_seed_prompt_as_the_original_brief_rather_than_a_fresh_assignment() {
        let t = task("x", None, None);
        let p = build_prompt_with_history(&t, Path::new("/tmp"), BG_CLAUDE, "prior stuff\n");
        assert!(!p.contains("What to do:"), "the fresh-assignment label must be gone: {p}");
        assert!(p.contains("ORIGINAL BRIEF"), "{p}");
        assert!(p.contains("not a fresh assignment"), "{p}");
        assert!(p.contains("This is a CONTINUATION"), "{p}");
    }

    /// Ordering, by `find` index. Emitted right after the seed prompt — the
    /// obvious spot — "the handoff above" refers to nothing at all.
    #[test]
    fn a_continuation_is_told_to_reconcile_the_handoff_before_the_handoff_it_is_about() {
        let t = task("x", None, None);
        let p = build_prompt_with_history(
            &t,
            Path::new("/tmp"),
            BG_CLAUDE,
            "What happened before: the 429 is in lib/webhook.ts\n",
        );
        let history = p.find("What happened before").expect("the history");
        let framing = p.find("This is a CONTINUATION").expect("the framing");
        assert!(history < framing, "the framing must follow what it refers to: {p}");
        assert!(p.contains("where they disagree, the repo wins"), "{p}");
        assert!(p.contains("deliverable is the DELTA"), "{p}");
    }

    /// **Only the absence assertion catches the cheap version**, which emits
    /// BOTH blocks and hands the agent two contradictory jobs in one prompt.
    #[test]
    fn a_review_run_is_told_not_to_do_the_work() {
        let mut t = task("x", None, None);
        t.bg_intent = BackgroundIntent::Review;
        let p = build_prompt_with_history(&t, Path::new("/tmp"), BG_CLAUDE, "a prior handoff\n");
        assert!(p.contains("This run is a REVIEW"), "{p}");
        assert!(p.contains("is it TRUE, and is it ENOUGH"), "{p}");
        assert!(p.contains("Do not do the task"), "{p}");
        assert!(p.contains("You are not being asked to do it"), "{p}");
        assert!(
            !p.contains("This is a CONTINUATION"),
            "a review is not a continuation; emitting both is two contradictory jobs: {p}"
        );
    }

    /// A real path, not a hypothetical: `write_scratch` swallows a DB read
    /// failure into an empty history and `history_block` `unwrap_or_default`s
    /// both queries. The queue guard ran minutes earlier and cannot cover it.
    #[test]
    fn a_review_dispatched_with_no_readable_history_says_so_instead_of_doing_the_work() {
        let mut t = task("x", None, None);
        t.bg_intent = BackgroundIntent::Review;
        let p = build_prompt_with_history(&t, Path::new("/tmp"), BG_CLAUDE, "");
        assert!(p.contains("could not be read"), "{p}");
        assert!(p.contains("STOP"), "{p}");
        assert!(p.contains("Do NOT do the task"), "{p}");
        assert!(!p.contains("This is a CONTINUATION"), "{p}");
    }

    /// The old `"{status} ({mode})"` rendering made a critique and the work it
    /// critiques identical on the page — the one distinction a continuation
    /// cannot afford to lose.
    #[test]
    fn the_history_block_names_which_prior_runs_were_reviews() {
        let conn = crate::db::schema::test_conn();
        let t = create(&conn, &NewTask { title: "worked".into(), ..Default::default() }).unwrap();
        finish_a_run(&conn, &t.id, "Did the work: the 429 is a missing retry.");
        finish_a_run_as(&conn, &t.id, "Verdict: step 3 is wrong.", BackgroundIntent::Review);

        let block = history_block(&conn, &t);
        assert!(block.contains("review · Claude Code · succeeded"), "{block}");
        assert!(block.contains("work · Claude Code · succeeded"), "{block}");
    }

    /// **Green today — the regression arrives with this feature**, which is why
    /// it has to be written now. Once a review is the newest finished run,
    /// `finished.first()` replays a VERDICT and the work handoff it is about
    /// silently stops reaching the next continuation.
    #[test]
    fn a_review_verdict_never_replaces_the_work_handoff_it_is_about() {
        let conn = crate::db::schema::test_conn();
        let t = create(&conn, &NewTask { title: "worked".into(), ..Default::default() }).unwrap();
        finish_a_run(&conn, &t.id, "WORK HANDOFF: the 429 is a missing retry on webhook.ts:88.");
        finish_a_run_as(
            &conn,
            &t.id,
            "REVIEW VERDICT: sound, except the line number is 91.",
            BackgroundIntent::Review,
        );

        let block = history_block(&conn, &t);
        assert!(block.contains("REVIEW VERDICT"), "the newest handoff still replays: {block}");
        assert!(
            block.contains("WORK HANDOFF"),
            "the work the review is ABOUT must still reach the next agent: {block}"
        );
        assert!(
            block.contains("this is what the review above is about"),
            "and it must be labelled, or the two read as two attempts at the work: {block}"
        );
    }

    /// One function backs both the queue guard and the replay, so this covers
    /// both. The two ways it drifts are a `succeeded` filter (which refuses a
    /// genuinely reviewable partial) and an intent filter (which refuses a
    /// second opinion).
    #[test]
    fn a_review_is_refused_until_a_run_has_left_something_to_review() {
        let conn = crate::db::schema::test_conn();
        let t = create(&conn, &NewTask { title: "fresh".into(), ..Default::default() }).unwrap();
        assert!(reviewable_run(&conn, &t.id).is_none(), "no runs at all");

        // A run that finished but left nothing is not reviewable either — the
        // guard and the replay agree that an empty handoff is no handoff.
        let empty = crate::db::runs::start(
            &conn,
            &t.id,
            "background",
            None,
            BackgroundExecutor::Claude,
            BackgroundIntent::Work,
            None,
        )
        .unwrap();
        crate::db::runs::finish(
            &conn,
            &empty.id,
            &crate::db::runs::RunOutcome {
                status: "succeeded".into(),
                result_text: Some("   ".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(reviewable_run(&conn, &t.id).is_none(), "a blank handoff is nothing to review");

        // A TIMEOUT that left a partial handoff IS reviewable — filtering to
        // `succeeded` would refuse the case most worth a second opinion.
        let partial = crate::db::runs::start(
            &conn,
            &t.id,
            "background",
            None,
            BackgroundExecutor::Claude,
            BackgroundIntent::Work,
            None,
        )
        .unwrap();
        crate::db::runs::finish(
            &conn,
            &partial.id,
            &crate::db::runs::RunOutcome {
                status: "timeout".into(),
                result_text: Some("got halfway".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(reviewable_run(&conn, &t.id).map(|r| r.id), Some(partial.id));

        // And a REVIEW is itself reviewable: a second opinion is the feature.
        finish_a_run_as(&conn, &t.id, "Verdict: sound.", BackgroundIntent::Review);
        let newest = reviewable_run(&conn, &t.id).expect("the review is reviewable");
        assert!(newest.intent.is_review(), "reviewing a review is a second opinion");
    }

    #[test]
    fn the_prompt_names_the_agent_that_will_read_it() {
        let t = task("x", None, None);
        assert!(build_prompt(&t, Path::new("/tmp"), BG_CODEX).contains("dispatched Codex session"));
        assert!(build_prompt(&t, Path::new("/tmp"), FG).contains("dispatched Claude session"));
    }

    #[test]
    fn launch_script_cds_to_the_repo_not_the_scratch_dir() {
        let scratch = std::env::temp_dir().join("cxtasks-test-scratch");
        fs::create_dir_all(&scratch).unwrap();
        let repo = Path::new("/tmp/some repo");
        let path =
            write_launch_script(&scratch, repo, Path::new("/opt/homebrew/bin/claude")).unwrap();
        let script = fs::read_to_string(&path).unwrap();

        assert!(script.contains("cd '/tmp/some repo'"), "{script}");
        assert!(
            !script.contains(&format!("cd {}", scratch.display())),
            "must land in the repo, not the scratch dir: {script}"
        );
        assert!(script.contains("prompt.txt"));
        // A watched session skips the approval prompts — that is what the human
        // sitting in front of it was buying. Ordered before the prompt so it is
        // read as a flag rather than as part of the positional argument.
        let flag = script
            .find("--dangerously-skip-permissions")
            .unwrap_or_else(|| panic!("{script}"));
        assert!(flag < script.find("$(cat").unwrap(), "{script}");
        // Executable, or Ghostty's -e can't run it.
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "launch.sh must be executable");
        fs::remove_dir_all(&scratch).ok();
    }

    #[test]
    fn git_tree_is_clean_says_false_for_a_non_repo() {
        let dir = std::env::temp_dir().join("cxtasks-not-a-repo");
        fs::create_dir_all(&dir).unwrap();
        assert!(
            !git_tree_is_clean(&dir).unwrap(),
            "a non-repo must not count as clean — that would let writes through"
        );
        fs::remove_dir_all(&dir).ok();
    }

    // ── replaying the run's own handoff ────────────────────────────────

    fn finish_a_run(conn: &rusqlite::Connection, task_id: &str, text: &str) {
        finish_a_run_as(conn, task_id, text, BackgroundIntent::Work);
    }

    fn finish_a_run_as(
        conn: &rusqlite::Connection,
        task_id: &str,
        text: &str,
        intent: BackgroundIntent,
    ) {
        let run = crate::db::runs::start(
            conn,
            task_id,
            "background",
            None,
            crate::executor::BackgroundExecutor::Claude,
            intent,
            None,
        )
        .unwrap();
        crate::db::runs::finish(
            conn,
            &run.id,
            &crate::db::runs::RunOutcome {
                executor: crate::executor::BackgroundExecutor::Claude,
                status: "succeeded".into(),
                exit_code: Some(0),
                result_text: Some(text.into()),
                output_tokens: None,
                session_id: None,
                input_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                model: None,
                stop_detail: None,
            },
        )
        .unwrap();
    }

    /// **The half that makes shortening the comment safe.** `finish_run` puts
    /// only a one-line summary in the comment log, so if this block stops
    /// replaying the run's own `result_text` the next delegate is handed a
    /// sentence and re-derives everything — the exact regression the handoff
    /// exists to prevent.
    #[test]
    fn the_most_recent_runs_full_handoff_is_replayed_not_just_its_summary() {
        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask { title: "worked before".into(), repo_path: Some("/tmp".into()), ..Default::default() },
        )
        .unwrap();
        crate::db::comments::add(
            &conn,
            &t.id,
            crate::db::comments::AUTHOR_CLAUDE,
            "Background run · Claude Code\n\nFound the 429.\n\n_Full handoff on the run._",
        )
        .unwrap();
        finish_a_run(&conn, &t.id, "Found the 429.\n\nIt is a missing retry in lib/webhook.ts:88.");

        let block = history_block(&conn, &t);
        assert!(
            block.contains("lib/webhook.ts:88"),
            "the detail lives only on the run row: {block}"
        );
        assert!(block.contains("full handoff from the most recent run"), "{block}");
    }

    /// A reply written after a run is an instruction, and the prompt says so.
    ///
    /// The run's own summary comment is the last comment when nobody has said
    /// anything since, so "the newest comment is Chris's" means it came after.
    #[test]
    fn a_reply_after_the_last_run_is_flagged_as_the_instruction() {
        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask { title: "asked before".into(), repo_path: Some("/tmp".into()), ..Default::default() },
        )
        .unwrap();
        crate::db::comments::add(
            &conn,
            &t.id,
            crate::db::comments::AUTHOR_CLAUDE,
            "Background run · Claude Code\n\nThe cause is the retry.\n",
        )
        .unwrap();
        finish_a_run(&conn, &t.id, "The cause is the retry, in lib/webhook.ts:88.");

        // Nobody has replied: nothing to flag.
        assert!(
            !history_block(&conn, &t).contains("came AFTER that run finished"),
            "a run's own summary must not read as a reply"
        );

        crate::db::comments::add(&conn, &t.id, crate::db::comments::AUTHOR_ME, "and the iPad?").unwrap();
        let block = history_block(&conn, &t);
        assert!(block.contains("came AFTER that run finished"), "{block}");
        assert!(block.contains("and the iPad?"), "{block}");
    }

    /// Runs worked before the split have the WHOLE handoff mirrored into a
    /// comment. Replaying both would send it twice in one prompt.
    #[test]
    fn a_handoff_already_mirrored_into_a_comment_is_not_replayed_twice() {
        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask { title: "legacy".into(), repo_path: Some("/tmp".into()), ..Default::default() },
        )
        .unwrap();
        let handoff = "Found the 429. ".repeat(40);
        // The pre-split shape: header + the entire text.
        crate::db::comments::add(
            &conn,
            &t.id,
            crate::db::comments::AUTHOR_CLAUDE,
            &format!("Background run · Claude Code\n\n{handoff}"),
        )
        .unwrap();
        finish_a_run(&conn, &t.id, &handoff);

        let block = history_block(&conn, &t);
        assert!(
            !block.contains("full handoff from the most recent run"),
            "already in the comment log; sending it twice wastes the prompt: {block}"
        );
        assert_eq!(
            block.matches("Found the 429").count(),
            40,
            "exactly one copy survives: {block}"
        );
    }

    /// Bounded for the same reason the comment window is: this lands in every
    /// FUTURE run's prompt.
    #[test]
    fn a_replayed_handoff_is_truncated_rather_than_dropped() {
        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask { title: "enormous".into(), repo_path: Some("/tmp".into()), ..Default::default() },
        )
        .unwrap();
        let huge = "y".repeat(HISTORY_RUN_CHARS * 3);
        finish_a_run(&conn, &t.id, &huge);

        let block = history_block(&conn, &t);
        assert!(block.contains("the full handoff is on the run"), "{block}");
        assert!(block.len() < huge.len(), "it has to actually shrink");
    }

    /// **The foreground half of the same loop.** A watched session's handoff now
    /// takes the run-row split too (`db::runs::record_foreground_handoff`), and
    /// the whole point of moving it there is that this block replays a run's
    /// `result_text` in FULL while truncating a comment at 1200 chars.
    ///
    /// Written with the production-shaped summary comment beside it, because
    /// that is what could break it: `push_handoff` suppresses a replay whose
    /// first 200 chars already appear in a replayed comment, so a summary that
    /// happened to match would silently drop the artifact it points at. It
    /// cannot, since `handoff_summary` strips markdown and joins whitespace —
    /// and this asserts that rather than assuming it.
    #[test]
    fn a_foreground_handoff_is_replayed_into_the_next_dispatch() {
        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask { title: "worked by hand".into(), repo_path: Some("/tmp".into()), ..Default::default() },
        )
        .unwrap();

        // Multi-line and past the dedup probe, which is the shape a real
        // handoff has and the shape a naive summary would collide with.
        let handoff = format!(
            "## Handoff: worked by hand\n\n\
             Found the 429: a missing retry on lib/webhook.ts:88.\n\n\
             {}\n\nNext: add the backoff and a test.",
            "Checked every call site in turn. ".repeat(20)
        );
        assert!(handoff.chars().count() > HISTORY_DEDUP_PROBE, "the fixture must exercise the probe");

        let run = crate::db::runs::record_foreground_handoff(
            &conn,
            &t.id,
            BackgroundIntent::Work,
            &handoff,
        )
        .unwrap();
        crate::db::comments::add(
            &conn,
            &t.id,
            crate::db::comments::AUTHOR_CLAUDE,
            &format!(
                "Result · foreground session\n\n{}\n\n_Full handoff on the run._",
                crate::runner::handoff_summary(&handoff)
            ),
        )
        .unwrap();

        let block = history_block(&conn, &t);
        assert!(
            block.contains("full handoff from the most recent run"),
            "the summary comment must not suppress the artifact it points at: {block}"
        );
        assert!(
            block.contains("Checked every call site in turn."),
            "the DETAIL only exists on the run row — that is why it goes there: {block}"
        );
        assert!(block.contains("Next: add the backoff and a test."), "{block}");

        // And a review can be armed against it: `reviewable_run` keys on
        // `finished_at` + a non-empty handoff, never on status (gotcha #31), so
        // a `dispatched` foreground row qualifies without any change there.
        assert_eq!(
            reviewable_run(&conn, &t.id).map(|r| r.id),
            Some(run.id),
            "a foreground handoff is reviewable like any other"
        );
    }

    /// `list_for_task` is `ORDER BY started_at DESC`. This was `.rev()`ed once,
    /// which reported the three OLDEST runs as "most recent" — and would now
    /// also replay the wrong run's handoff.
    #[test]
    fn the_run_history_reports_the_newest_runs_not_the_oldest() {
        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask { title: "many runs".into(), repo_path: Some("/tmp".into()), ..Default::default() },
        )
        .unwrap();
        for i in 0..4 {
            finish_a_run(&conn, &t.id, &format!("run number {i}"));
            // `started_at` is second-resolution, so force distinct stamps
            // rather than racing them (gotcha #12's lesson, applied to runs).
            conn.execute(
                "UPDATE task_runs SET started_at = datetime('now', ?1) WHERE result_text = ?2",
                rusqlite::params![format!("+{i} seconds"), format!("run number {i}")],
            )
            .unwrap();
        }
        let block = history_block(&conn, &t);
        assert!(
            block.contains("run number 3"),
            "the newest run's handoff is the one worth replaying: {block}"
        );
        assert!(!block.contains("run number 0"), "{block}");
    }

    // ── admit: which hosts could run this ──────────────────────────────────

    mod admit_tests {
        use super::super::*;
        use crate::db::schema::test_conn;
        use crate::routing::{HostCapability, WorkspaceKind};
        use crate::runner::ToolTier;

        fn task_needing(conn: &rusqlite::Connection, writes: bool, shell: bool) -> Task {
            crate::db::tasks::create(
                conn,
                &crate::db::tasks::NewTask {
                    title: "work".into(),
                    repo_path: Some("/repo".into()),
                    bg_writes_allowed: Some(writes),
                    bg_shell_allowed: Some(shell),
                    ..Default::default()
                },
            )
            .unwrap()
        }

        fn register(conn: &rusqlite::Connection, id: &str, f: impl FnOnce(&mut HostCapability)) {
            let mut c = HostCapability::local_default(id.into(), id.into());
            f(&mut c);
            crate::db::hosts::register_self(conn, &c).unwrap();
        }

        /// A machine that has never registered must still be able to queue —
        /// the runner writes the row on its first tick, which may not have
        /// happened yet. **And it must not write one on the way.**
        ///
        /// Both halves are load-bearing. `cxtasks-server` calls `admit` too,
        /// and it cannot execute anything: a row from it would claim to do
        /// everything and drain nothing, so a task only IT could run would be
        /// admitted and then sit forever.
        #[test]
        fn an_empty_registry_falls_back_to_this_machine_without_writing_a_row() {
            let conn = test_conn();
            assert!(crate::db::hosts::list(&conn).unwrap().is_empty());
            let t = task_needing(&conn, false, false);

            let hosts = admit(&conn, &t).unwrap();
            assert_eq!(hosts.len(), 1);
            assert!(hosts[0].ready());

            assert!(
                crate::db::hosts::list(&conn).unwrap().is_empty(),
                "admit created a host row; only the runner may do that"
            );
        }

        /// ...and once ANY host is registered, the fallback stops. Otherwise a
        /// server would keep adding itself to every verdict.
        #[test]
        fn the_fallback_stops_as_soon_as_a_real_host_exists() {
            let conn = test_conn();
            register(&conn, "nas", |c| {
                c.label = "the NAS".into();
                c.max_tier = ToolTier::ReadOnly;
                c.workspaces = vec![WorkspaceKind::Mirror];
                c.can_checkpoint = false;
            });
            let t = task_needing(&conn, false, false);
            // No host can run it — the NAS has no mirror — and the machine
            // running the test must NOT be conjured in to rescue it.
            let err = admit(&conn, &t).unwrap_err().to_string();
            assert!(err.contains("the NAS"), "{err}");
            assert!(err.contains("mirror"), "{err}");
        }

        /// Switch off the machine actually running the tests.
        ///
        /// `admit` registers this host by design — it is what makes queueing
        /// work on a fresh install — so a test about "no host can do it" has to
        /// take the real one out of the running, and cannot do it by choosing a
        /// different id: `host::host_id` is memoized per process.
        fn silence_this_machine(conn: &rusqlite::Connection) {
            let _ = crate::db::hosts::local(
                conn,
                &crate::routing::HostCapability::declared_for_this_host(),
            );
            crate::db::hosts::set_policy(
                conn,
                &crate::host::host_id(),
                &crate::db::hosts::HostPolicy {
                    enabled: Some(false),
                    ..Default::default()
                },
            )
            .unwrap();
        }

        /// Register the machine running the tests as a normal, capable host —
        /// which is what the runner would have done on its first tick, and
        /// what stops `admit`'s empty-registry fallback from firing.
        fn this_machine_is_registered(conn: &rusqlite::Connection) {
            let _ = crate::db::hosts::local(
                conn,
                &crate::routing::HostCapability::declared_for_this_host(),
            );
        }

        /// **Capability misses refuse.** A task no host is configured to run
        /// will never move however long you wait, so accepting it into the
        /// queue is a lie that only resolves when somebody goes looking.
        #[test]
        fn a_task_no_host_can_run_is_refused_naming_the_nearest_miss() {
            let conn = test_conn();
            silence_this_machine(&conn);
            register(&conn, "nas", |c| {
                c.label = "the NAS".into();
                c.max_tier = ToolTier::ReadOnly;
                c.workspaces = vec![WorkspaceKind::Mirror];
                c.can_checkpoint = false;
            });
            let t = task_needing(&conn, false, true); // wants a shell

            let err = admit(&conn, &t).unwrap_err().to_string();
            assert!(err.contains("the NAS"), "{err}");
            assert!(err.contains("read-only"), "{err}");
            assert!(err.contains("shell"), "{err}");
        }

        /// **Liveness misses warn.** A capable host that is merely asleep will
        /// run it on waking; refusing here would mean closing the laptop broke
        /// queueing.
        #[test]
        fn a_capable_but_sleeping_host_is_admitted_with_a_warning() {
            let conn = test_conn();
            this_machine_is_registered(&conn);
            register(&conn, "mac", |c| c.label = "the Mac".into());
            crate::db::hosts::heartbeat(
                &conn,
                "mac",
                false,
                None,
                &crate::executor::BackgroundExecutor::ALL,
            )
            .unwrap();
            conn.execute(
                "UPDATE hosts SET last_seen_at = datetime('now','-3 hours') WHERE id = 'mac'",
                [],
            )
            .unwrap();

            let t = task_needing(&conn, false, false);
            let hosts = admit(&conn, &t).expect("a sleeping host must not refuse the queue");
            let mac = hosts.iter().find(|h| h.host_id == "mac").unwrap();
            assert!(mac.capable(), "it can run it");
            assert!(!mac.alive, "it just is not awake");
            assert!(!mac.ready(), "so nothing will happen yet, and the UI must say so");
        }

        /// The queue outcome names every host, not only the one that will run
        /// it — a queue whose only capable host is asleep is otherwise
        /// indistinguishable from a healthy one.
        #[test]
        fn every_registered_host_appears_in_the_verdict() {
            let conn = test_conn();
            this_machine_is_registered(&conn);
            register(&conn, "mac", |c| c.label = "the Mac".into());
            register(&conn, "nas", |c| {
                c.label = "the NAS".into();
                c.max_tier = ToolTier::ReadOnly;
                c.workspaces = vec![WorkspaceKind::Mirror];
                c.can_checkpoint = false;
            });

            let t = task_needing(&conn, false, false);
            let hosts = admit(&conn, &t).unwrap();
            // Three, not two: `admit` also registers the machine running the
            // tests, which is the behaviour that makes a fresh install work.
            assert_eq!(hosts.len(), 3);
            assert!(hosts.iter().any(|h| h.host_id == crate::host::host_id()));

            let nas = hosts.iter().find(|h| h.host_id == "nas").unwrap();
            assert!(!nas.capable(), "the NAS has no mirror of /repo yet");
            assert!(nas.unmet.as_deref().unwrap().contains("mirror"), "{nas:?}");

            let mac = hosts.iter().find(|h| h.host_id == "mac").unwrap();
            assert_eq!(mac.workspace, Some(WorkspaceKind::Live));
        }

        /// Once the NAS holds a mirror it becomes an option — but the Mac still
        /// says `Live`, because freshness dominates.
        #[test]
        fn a_mirrored_repo_makes_a_remote_host_an_option_without_demoting_the_local_one() {
            let conn = test_conn();
            this_machine_is_registered(&conn);
            register(&conn, "mac", |c| c.label = "the Mac".into());
            register(&conn, "nas", |c| {
                c.label = "the NAS".into();
                c.max_tier = ToolTier::ReadOnly;
                c.workspaces = vec![WorkspaceKind::Mirror];
                c.can_checkpoint = false;
            });
            crate::db::hosts::heartbeat(
                &conn,
                "nas",
                false,
                None,
                &[crate::executor::BackgroundExecutor::Claude],
            )
            .unwrap();
            crate::db::hosts::record_mirror(&conn, "nas", "/repo").unwrap();

            let t = task_needing(&conn, false, false);
            let hosts = admit(&conn, &t).unwrap();
            assert_eq!(
                hosts.iter().find(|h| h.host_id == "nas").unwrap().workspace,
                Some(WorkspaceKind::Mirror)
            );
            assert_eq!(
                hosts.iter().find(|h| h.host_id == "mac").unwrap().workspace,
                Some(WorkspaceKind::Live)
            );
        }

        /// A disabled host is not a reason to refuse if another host can do it.
        #[test]
        fn a_disabled_host_does_not_block_a_capable_one() {
            let conn = test_conn();
            this_machine_is_registered(&conn);
            register(&conn, "mac", |c| c.label = "the Mac".into());
            register(&conn, "old", |c| {
                c.label = "the old mini".into();
                c.enabled = false;
            });
            let t = task_needing(&conn, false, false);
            let hosts = admit(&conn, &t).unwrap();
            assert!(hosts.iter().any(HostReadiness::ready));
            assert!(hosts
                .iter()
                .find(|h| h.host_id == "old")
                .unwrap()
                .unmet
                .as_deref()
                .unwrap()
                .contains("switched off"));
        }
    }
}
