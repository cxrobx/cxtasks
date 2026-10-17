//! Background runner: drains queued tasks through a background agent and
//! writes the result back onto the task.
//!
//! Since v8 there are two agents — Claude Code and Codex — and the split is
//! deliberate about where it falls. **Everything that makes an unattended run
//! safe lives here and is written once**: claiming work, the budget gates, the
//! repo preflight, the checkpoint, the wall-clock kill, recording the run,
//! moving the task to `needs_review`. A provider ([`providers`]) only decides
//! how to phrase the invocation and how to read the output back. A provider
//! cannot skip a checkpoint, raise a budget, or start a run.
//!
//! # Why the guards are the design
//!
//! Per `feedback_subscription_not_api`, these runs burn Chris's Claude
//! **subscription** rate limit, not metered API — there is no bill to notice,
//! only a session that suddenly says he's out of usage. (Codex spends a
//! different allowance, his ChatGPT plan, but the shape of the problem is
//! identical: an invisible quota with no invoice.) Per
//! `reference_headless_claude_cost_overhead`, every `claude -p` reloads the
//! root + repo `CLAUDE.md` before doing any work (~41.5k tokens floor), so
//! even a trivial task is not cheap. Two prior agent-swarm experiments were
//! aborted for burn. So:
//!
//! * **Max 2 concurrent** (hard-clamped to 4 even if the DB says more), shared
//!   across both executors — the point is that no combination of tasks starts
//!   a swarm, and a per-executor limit would let two become four.
//! * **Per-run wall-clock timeout**, default 20 min, enforced by killing the
//!   child — not just by giving up on it.
//! * **Per-run token ceiling**, default 25M, polled while the child is alive
//!   and likewise enforced by killing it. This is the real per-run cost bound;
//!   the turn cap above it is only a loop detector, because turns correlate
//!   with spend far too loosely to be one (see
//!   [`crate::db::meta::DEFAULT_MAX_TURNS`]). It lives in the shared poll loop
//!   in [`process`] rather than per provider, so there is one implementation
//!   to keep correct — **but in practice it only bites Claude**, because
//!   `codex exec --json` reports usage exactly once, at the end. See
//!   [`providers::Provider::live_total`]; a Codex run is bounded by the
//!   wall-clock timeout and the daily cap alone.
//! * **Daily run cap**, counted from local midnight.
//! * **A kill switch that is a file**, checked every tick and again before
//!   each spawn, so it can be tripped from a terminal mid-run without opening
//!   the app.
//! * **Read-only tools by default**, enforced per provider — for Claude by
//!   `--disallowedTools` and not merely by omission from `--allowedTools` (see
//!   [`ToolTier`], where the difference is written up, because getting it wrong
//!   once made every "read-only" run write-capable); for Codex by the OS-level
//!   `--sandbox read-only`. Anything above `ReadOnly` first captures a
//!   [`crate::checkpoint`] — a snapshot of the whole working tree on a hidden
//!   git ref — so its changes can always be diffed and undone.
//! * **stderr is drained on its own thread** ([`process`]). Per
//!   `reference_claude_cli_subprocess`, not draining it deadlocks the child
//!   once the pipe buffer fills — the run just hangs forever, looking like a
//!   slow task rather than a bug.

pub mod completion;
pub mod process;
pub mod progress;
pub mod providers;
pub mod tripwire;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;

use crate::db::{self, meta::BudgetSettings, runs::RunOutcome, tasks::Task};
use crate::dispatch::{resolve_repo, write_scratch, PromptTarget};
use crate::error::AppError;
use crate::executor::BackgroundExecutor;
use crate::mcp::bridge::Envelope;

pub use providers::{Provider, RunKind, RunRequest, RunSummary};

/// How often the runner wakes up to look for queued work.
pub const TICK_SECONDS: u64 = 20;

pub fn open_db() -> Result<Connection, AppError> {
    let path = crate::paths::db_path();
    // Create the root if it isn't there. Under the default path it always is,
    // because `~/Library/Application Support` exists and the app made the rest.
    // Under `CXTASKS_DATA_DIR` — a container volume — it may be genuinely empty
    // on first start, and SQLite reports that as "unable to open database
    // file", which reads like a permissions problem rather than a missing
    // directory. `bin/mcp.rs` already did this for itself; the runner did not.
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!("could not create data dir {}: {e}", parent.display());
        }
    }
    let conn = Connection::open(&path)?;
    conn.busy_timeout(Duration::from_secs(10))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    Ok(conn)
}

/// Why a tick declined to start more work. Logged so a runner that looks idle
/// can always explain itself.
#[derive(Debug, PartialEq, Eq)]
pub enum Gate {
    Paused,
    /// This host in particular was paused, from the app, without touching the
    /// others. A separate variant from [`Gate::Paused`] because gotcha #31 —
    /// a stopped runner must say WHICH guard fired — and "paused" on a machine
    /// where the global switch is plainly off is otherwise unexplainable.
    HostPaused,
    /// The host is switched off in the registry, or declares no usable
    /// executor. Distinct from `HostPaused`: paused is a temporary decision,
    /// this is a configuration one.
    HostDisabled,
    AtConcurrencyLimit {
        running: i64,
        max: i64,
    },
    DailyCapReached {
        today: i64,
        cap: i64,
    },
    TokenCapReached {
        today: i64,
        cap: i64,
    },
    /// This host must leave a share of the daily pool for somebody else.
    /// **A ceiling, not a second budget** — see [`HostCapability::headroom_pct`].
    HostHeadroomReached {
        today: i64,
        ceiling: i64,
    },
    Open {
        slots: i64,
    },
}

impl Gate {
    /// The three SWITCHES — the kill-switch file, a host paused from the app,
    /// a host disabled in the registry. These stop everything the runner does,
    /// including the zero-cost script lane. The budget gates below them do
    /// not: a spent token budget is a reason not to start an agent, and no
    /// reason at all not to run a shell script (gotcha #42).
    pub fn is_switched_off(&self) -> bool {
        matches!(self, Gate::Paused | Gate::HostPaused | Gate::HostDisabled)
    }
}

/// What one tick may do, split by lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lanes {
    /// Run the script-tripwire pass at all.
    pub scripts: bool,
    /// Agent runs this tick may start.
    pub slots: i64,
}

/// The tick's ordering, as a pure function of the gate so it can be pinned by
/// a test rather than by reading `tick` carefully.
pub fn lanes(gate: &Gate) -> Lanes {
    if gate.is_switched_off() {
        return Lanes {
            scripts: false,
            slots: 0,
        };
    }
    Lanes {
        scripts: true,
        slots: match gate {
            Gate::Open { slots } => (*slots).max(0),
            _ => 0,
        },
    }
}

/// Decide how many runs (if any) this tick may start.
///
/// Pure so it can be tested without spawning anything — the interesting bugs
/// in a runner are all in this arithmetic.
///
/// **Executor-blind on purpose.** Every counter it reads spans both agents, so
/// a mixed Claude/Codex queue can never exceed the same ceilings a
/// single-agent queue would. Splitting the budget per executor is a later
/// decision if combined accounting turns out to be confusing in practice; the
/// unsafe direction — two budgets that each look fine while together doubling
/// the burn — is the one it must not start out in.
///
/// **Host-blind in the same way, and for the same reason.** `cap` narrows this
/// host's share; it never widens it. Every counter still spans every host, so
/// two machines draining one queue burn one budget. A per-host budget is the
/// same trap as a per-executor one, one level up: two pools that each look fine
/// while together spending twice what either was allowed.
///
/// The order matters and is deliberate — global guards first, host ceilings
/// after. A host asked to explain itself should say "the daily cap is gone",
/// not "I am at my share of a cap that is already gone", which sends the reader
/// to fix the wrong number.
pub fn gate(
    paused: bool,
    settings: &BudgetSettings,
    cap: &crate::routing::HostCapability,
    running: i64,
    runs_today: i64,
    tokens_today: i64,
) -> Gate {
    if paused {
        return Gate::Paused;
    }
    if !cap.enabled {
        return Gate::HostDisabled;
    }
    if cap.effective_executors().is_empty() {
        // Declared ∩ observed is empty: either nothing was declared, or
        // everything declared reported itself missing. Either way this host
        // cannot spawn anything, and saying so is better than an empty drain
        // nobody can account for.
        return Gate::HostDisabled;
    }
    if cap.paused {
        return Gate::HostPaused;
    }
    // Token cap first: it's the guard that actually tracks subscription burn.
    // A run cap can't, because measured runs span ~46k-234k tokens each.
    if settings.daily_token_cap > 0 && tokens_today >= settings.daily_token_cap {
        return Gate::TokenCapReached {
            today: tokens_today,
            cap: settings.daily_token_cap,
        };
    }
    if settings.daily_run_cap > 0 && runs_today >= settings.daily_run_cap {
        return Gate::DailyCapReached {
            today: runs_today,
            cap: settings.daily_run_cap,
        };
    }
    // The host's share of the token pool. `headroom_pct = 50` means "stop once
    // half the pool is gone", which RESERVES headroom for the machine Chris is
    // sitting at and never creates any: the ceiling is always at or below the
    // global cap, so a host can only ever spend less than it could before.
    if settings.daily_token_cap > 0 && cap.headroom_pct > 0 {
        let ceiling = settings.daily_token_cap * (100 - cap.headroom_pct.clamp(0, 100)) / 100;
        if tokens_today >= ceiling {
            return Gate::HostHeadroomReached {
                today: tokens_today,
                ceiling,
            };
        }
    }
    // 0 means "no host opinion", not "no runs" — a host that has never been
    // configured must behave exactly as it did before this column existed.
    let max_concurrent = if cap.max_concurrent > 0 {
        settings.max_concurrent.min(cap.max_concurrent)
    } else {
        settings.max_concurrent
    };
    if running >= max_concurrent {
        return Gate::AtConcurrencyLimit {
            running,
            max: max_concurrent,
        };
    }
    let by_concurrency = max_concurrent - running;
    let slots = if settings.daily_run_cap > 0 {
        by_concurrency.min(settings.daily_run_cap - runs_today)
    } else {
        by_concurrency
    };
    Gate::Open {
        slots: slots.max(0),
    }
}

/// Refuse a task the runner must not touch. Checked immediately before spawn,
/// because the world can change between queueing and running: the tree gets
/// dirty, a repo gets moved — or the agent's CLI gets uninstalled or logged out.
pub fn preflight(task: &Task) -> Result<std::path::PathBuf, String> {
    preflight_for(task, RunKind::Work)
}

/// [`preflight`] with the purpose stated.
///
/// A CHECK skips the `bg_allowed` opt-in — that flag says "this task's WORK
/// may run unattended", and arming an `agent:` tripwire is its own, separate
/// opt-in to a read-only run that does nothing to the tree. It also never
/// needs a git repo, because a check is pinned read-only and there is nothing
/// to checkpoint.
pub fn preflight_for(task: &Task, kind: RunKind) -> Result<std::path::PathBuf, String> {
    if kind == RunKind::Work && !task.bg_allowed {
        return Err("task is not opted into background runs".into());
    }
    let repo = resolve_repo(task).map_err(|e| e.to_string())?;
    if kind == RunKind::Check {
        // Claude only, and always: a check is the runner's run, not the
        // task's, and Codex reports usage once at the end (invariant 7), so a
        // Codex check could not be bounded by the ceiling it exists to respect.
        Provider::Claude.preflight()?;
        return Ok(repo);
    }
    // A mutation-capable run needs a git repo — not a CLEAN one. The checkpoint
    // (see `crate::checkpoint`) snapshots whatever state exists onto a hidden
    // ref, so dirty WIP is fine: the diff afterwards is still exactly the
    // agent's changes, and it's still undoable.
    //
    // Refuse rather than quietly downgrading to read-only: a task that asked
    // for a shell and silently didn't get one still "succeeds", and hands back
    // a confidently wrong answer built from whatever it could reach instead.
    if ToolTier::for_task(task).needs_checkpoint() && !crate::checkpoint::is_git_repo(&repo) {
        return Err(format!(
            "{} is not a git repository. A background run that can run a shell or write files \
             needs one so its changes can be diffed and undone.",
            repo.display()
        ));
    }
    // Last, because it can shell out. A missing or logged-out CLI is checked at
    // queue time too, where the user is looking; this is the same check at the
    // other end of the wait, for the case where it stopped being true in
    // between. Parking the task with the concrete reason beats burning a run
    // slot to discover the binary is gone.
    Provider::for_task(task).preflight()?;
    Ok(repo)
}

/// A resolved place for one run to work, and what it is.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub path: std::path::PathBuf,
    pub kind: crate::routing::WorkspaceKind,
    /// Set for a mirror: which snapshot, taken when, how much was uncommitted.
    /// Reported to the agent, because "executing against a snapshot from 14:20
    /// with 7 uncommitted files" is the difference between trusting the tree
    /// and wondering about it.
    pub snapshot: Option<crate::mirror::Snapshot>,
}

/// Resolve — or build — the tree this run will execute in.
///
/// The execute-time half of the preflight split. [`crate::dispatch::admit`]
/// answered "could any host do this" at queue time with no filesystem and no
/// subprocess; this answers "can I do it, right now, here", and it is allowed
/// to be expensive because it runs once per run on the host about to spawn.
///
/// It takes a **run id** because a mirror workspace is per-run and disposable,
/// which is also why it must be called after the claim.
pub fn ready(
    conn: &Connection,
    cap: &crate::routing::HostCapability,
    task: &Task,
    run_id: &str,
) -> Result<Workspace, String> {
    ready_for(conn, cap, task, run_id, RunKind::Work)
}

/// [`ready`] with the purpose stated. See [`preflight_for`] for what a check
/// skips; what it ADDS is the `read_only` gate on extra MCP servers, because a
/// check's tool surface is the whole of its bound and a server that can write
/// would widen it past what "read-only, unattended, on a timer" promised.
pub fn ready_for(
    conn: &Connection,
    cap: &crate::routing::HostCapability,
    task: &Task,
    run_id: &str,
    kind: RunKind,
) -> Result<Workspace, String> {
    let req = match kind {
        RunKind::Work => crate::routing::RunRequirement::for_task(task),
        // A check's requirement is derived from the KIND, never the task's
        // flags: read-only, Claude, no checkpoint, whatever the task's work
        // was granted.
        RunKind::Check => crate::routing::RunRequirement {
            tier: ToolTier::ReadOnly,
            executor: BackgroundExecutor::Claude,
            repo_path: task
                .repo_path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            needs_checkpoint: false,
        },
        // An interpretation asks no workspace question, so it must not be
        // routed as if it did. `satisfies` answers "where do this run's files
        // come from" and an interpretation has none — it holds zero tools
        // (`ToolTier::denied_for_interpret`), so there is nothing for a
        // workspace to serve and nothing for one to bound. Its cwd is built
        // directly by `runner::tripwire::interpret_one`, which is also the only
        // thing that starts one. Refused loudly rather than given a plausible
        // requirement, because a plausible one would route a repo-less
        // interpretation through the Mac's `Live`-only capability and report
        // "has no workspace kind that fits" for a run that needed no workspace.
        RunKind::Interpret => {
            return Err(
                "an interpretation has no workspace to resolve — `ready_for` is not its path"
                    .into(),
            )
        }
    };
    let mirrors = db::hosts::mirrors(conn, &cap.id).unwrap_or_default();

    if kind == RunKind::Check {
        let specs =
            crate::mcp_registry::parse_list(task.bg_mcp_servers.as_deref().unwrap_or_default())
                .map_err(|e| e.to_string())?;
        let writable = crate::mcp_registry::writable_servers(&specs);
        if !writable.is_empty() {
            return Err(format!(
                "a tripwire check may only reach read-only MCP servers, and this task grants {}",
                writable.join(", ")
            ));
        }
    }

    // Belt and braces. `admit` checked this at queue time and the drain query
    // checked it again at claim time, and this is the last moment before an
    // unattended agent starts — by which point the capability may have been
    // narrowed from the app, which neither of the other two would have seen.
    // EVERY run, not only a detached one. The grant is "extra MCP servers for
    // this run" — a repo-bound task can be told to search the notes vault too —
    // and it lives here rather than in the `Detached` arm because a run that
    // silently proceeds with a smaller tool surface than it was given reports
    // confidently on what it could not see, and the only evidence is an
    // absence. That is the same argument `mcp_registry::parse_list` makes for
    // refusing an unknown NAME; this is the other half, for a name this
    // particular host cannot resolve.
    let (_, missing) =
        crate::mcp_registry::resolve_all(task.bg_mcp_servers.as_deref().unwrap_or_default())
            .map_err(|e| e.to_string())?;
    if !missing.is_empty() {
        return Err(format!(
            "this host cannot provide the MCP server(s) this task asks for: {}",
            missing.join(", ")
        ));
    }

    let workspace_kind =
        crate::routing::satisfies(cap, &req, &mirrors).map_err(|u| format!("this host {u}"))?;
    match workspace_kind {
        crate::routing::WorkspaceKind::Live => {
            // Exactly today's behaviour, unchanged.
            let repo = preflight_for(task, kind)?;
            Ok(Workspace {
                path: repo,
                kind: workspace_kind,
                snapshot: None,
            })
        }
        crate::routing::WorkspaceKind::Mirror => {
            if kind == RunKind::Work && !task.bg_allowed {
                return Err("task is not opted into background runs".into());
            }
            let repo = task
                .repo_path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or("a mirror run needs a repo to be a mirror OF")?;
            let key = crate::mirror::repo_key(std::path::Path::new(repo));
            let bare = crate::paths::mirrors_dir().join(format!("{key}.git"));
            if !bare.join("HEAD").exists() {
                return Err(format!(
                    "no mirror of {repo} at {} — the host that owns it has not pushed one",
                    bare.display()
                ));
            }
            let sha = crate::mirror::snapshot_sha(&bare).map_err(|e| e.to_string())?;
            let into = crate::paths::run_workspace(run_id);
            let path = crate::mirror::materialize(&bare, &sha, &into).map_err(|e| e.to_string())?;

            // Last, because it can shell out — same ordering as `preflight`.
            provider_for(task, kind).preflight()?;

            Ok(Workspace {
                path,
                kind: workspace_kind,
                snapshot: Some(crate::mirror::describe(&bare, &sha)),
            })
        }
        crate::routing::WorkspaceKind::Detached => {
            if kind == RunKind::Work && !task.bg_allowed {
                return Err("task is not opted into background runs".into());
            }
            // A scratch directory under the DATA DIR, which keeps "the mount
            // set is the filesystem guard" true: it is inside a directory the
            // container was given, so nothing about a detached run reaches
            // outside the mounts the app authored. Not `$HOME` — that is the
            // whole thing this kind exists to avoid — and not a repo, because
            // there is not one.
            let path = crate::paths::run_workspace(run_id);
            std::fs::create_dir_all(&path)
                .map_err(|e| format!("could not create {}: {e}", path.display()))?;

            provider_for(task, kind).preflight()?;
            Ok(Workspace {
                path,
                kind: workspace_kind,
                snapshot: None,
            })
        }
    }
}

/// The provider a run of this kind spawns: the task's own for work, Claude
/// for a check (see [`preflight_for`]).
pub fn provider_for(task: &Task, kind: RunKind) -> Provider {
    match kind {
        RunKind::Work => Provider::for_task(task),
        RunKind::Check | RunKind::Interpret => Provider::Claude,
    }
}

/// Throw away a run's materialized workspace.
///
/// Best-effort and never fatal: a leftover directory costs disk, and refusing
/// to record a finished run because a `rm -rf` failed would cost the run.
/// Only ever called for a `Mirror` workspace — deleting a `Live` one would
/// delete the user's repo.
pub fn release(ws: &Workspace) {
    if ws.kind == crate::routing::WorkspaceKind::Live {
        return;
    }
    if let Err(e) = std::fs::remove_dir_all(&ws.path) {
        log::warn!(
            "could not remove the run workspace {}: {e}",
            ws.path.display()
        );
    }
}

/// Every tool that can execute code, mutate the tree, or reach the network.
///
/// A tier denies its own complement of this list, so adding an entry here
/// denies it in every tier that doesn't explicitly allow it. That direction —
/// new tool defaults to denied — is the only one it's safe to fail in.
///
/// **Claude-specific.** Codex has no equivalent list: its tiers are enforced by
/// the OS sandbox `codex --sandbox` selects, which is a stronger mechanism than
/// a name-matched allow/deny list and needs no complement to stay in sync.
///
/// **Every name must be a real tool.** The CLI answers an unknown one with
/// `Permission deny rule "X" matches no known tool — check for typos` on stderr
/// and carries on, so a typo here is a silently absent guard, not an error. It
/// costs one run to check: `strings $(zsh -lc 'command -v claude') | grep -x
/// SlashCommand` returned nothing, which is how that entry came back out.
const DANGEROUS_TOOLS: &[&str] = &[
    "Task",
    "Bash",
    "BashOutput",
    "KillShell",
    "Edit",
    "Write",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
    "Skill",
];

/// The read tools every tier above nothing grants.
///
/// Named here as a **deny** list, which is the only place in this module where
/// reading has to be forbidden: [`ToolTier::denied_for_interpret`] is the
/// complement of the empty grant, and these are not in `DANGEROUS_TOOLS`
/// because every tier that exists allows them. A run with no repo on this Mac
/// is the one case where they are the hazard — `Read` takes an absolute path,
/// so a workspace is no boundary for it, and `~/.ssh` is one `Read` away.
const READ_TOOLS: &[&str] = &["Read", "Glob", "Grep"];

/// What a background run is allowed to touch.
///
/// Shared across both executors as the **user-facing** grant — one choice, three
/// levels, same words in the detail pane whichever agent is selected — but
/// enforced by completely different mechanisms underneath (see
/// [`providers::codex::sandbox_for`] for Codex's side). The tier also decides
/// two things that are provider-independent and must stay that way: whether a
/// git repo is required, and whether the working tree is checkpointed first.
///
/// # Omission is NOT denial — this is the whole lesson (Claude side)
///
/// `--allowedTools` is **additive on top of the user's own
/// `~/.claude/settings.json`**, not a replacement for it. The runner spawns
/// `claude` as the user, with the user's `$HOME`, so every rule in that file's
/// `permissions.allow` applies to an unattended run too. Chris's holds 70 of
/// them, including `Bash(bash:*)`, `Bash(ssh:*)`, `Bash(curl:*)` and
/// `Bash(git push:*)`.
///
/// So the original design here — "read-only is an allowlist; in headless `-p`
/// mode an unlisted tool is simply denied" — was measurably false. Spawned
/// with `--allowedTools "Read,Glob,Grep"` and nothing else, a run executed
/// `echo` and then wrote a file to disk. The tier was decoration; invariant 7
/// rested on it, and `bg_writes_allowed` gated a checkpoint for runs that could
/// already write without one.
///
/// `--disallowedTools` *does* enforce, and beats the settings allowlist — that
/// is what makes the tiers real. So every tier states **both** halves, and
/// `denied()` is the load-bearing one: a tool that must not run has to be
/// **named** there. Leaving it out of `allowed()` accomplishes nothing.
/// `denied()` is computed as the complement of `allowed()` over
/// `DANGEROUS_TOOLS` precisely so the two can never drift apart by hand.
///
/// The `CXTASKS_BACKGROUND_RUN` env var set by both providers remains a
/// belt-and-braces marker for a `PreToolUse` hook. It was never the primary
/// gate, and after this it isn't the only real one either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolTier {
    /// Read the repo and nothing else. The only tier that provably cannot
    /// mutate anything, and so the only one that needs no checkpoint.
    ReadOnly,
    /// Read plus a shell. A shell *is* write-capable — `>` is a write and
    /// `git push` is a publish — so this tier is checkpointed and repo-gated
    /// exactly like `Write`. What it buys is a smaller accident surface, not
    /// safety: an agent given only `Bash` won't casually reach for `Edit`, and
    /// running a test suite that drops files in `target/` is expected rather
    /// than a violation.
    Shell,
    /// Read, a shell, and the file-editing tools.
    Write,
}

impl ToolTier {
    /// The tier a task's flags ask for.
    pub fn for_task(task: &Task) -> Self {
        if task.bg_writes_allowed {
            Self::Write
        } else if task.bg_shell_allowed {
            Self::Shell
        } else {
            Self::ReadOnly
        }
    }

    /// `--allowedTools`. Shapes what the agent reaches for first; on its own it
    /// enforces nothing (see the type docs). Claude only.
    pub fn allowed(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read,Glob,Grep",
            Self::Shell => "Read,Glob,Grep,Bash,BashOutput,KillShell",
            Self::Write => "Read,Glob,Grep,Bash,BashOutput,KillShell,Edit,Write",
        }
    }

    /// `--allowedTools` including the write-back tools, for the Claude side.
    ///
    /// **These have to be named or the run hangs.** `--allowedTools` is additive
    /// over the user's settings, but an MCP tool that appears in neither is a
    /// permission *prompt* — and there is nobody to answer it in an unattended
    /// run, so the tool silently never works. The tier grant above is about the
    /// repo; this is about the task's own record, which every tier may write:
    /// leaving a handoff is not a capability that should depend on whether the
    /// agent was allowed to edit files.
    ///
    /// Codex has no `--allowedTools` equivalent. Its provider puts the same
    /// three names in `mcp_servers.cxtasks.enabled_tools` and pre-approves that
    /// reduced server instead (see [`providers::codex`]).
    pub fn allowed_with_writeback(self) -> String {
        format!(
            "{},{}",
            self.allowed(),
            crate::mcp::server::BACKGROUND_TOOLS_CSV
        )
    }

    /// `--allowedTools` for an agent tripwire CHECK: the read tools plus ONE
    /// MCP tool. Not a tier — a check is not a level a task can be granted,
    /// it is what the runner's own lane hands out, so it is an associated
    /// function rather than a variant nobody may store on a row.
    ///
    /// Same reason `allowed_with_writeback` names the MCP tool: a tool in
    /// neither the grant nor the user's settings is a permission PROMPT, and a
    /// check has nobody to answer it.
    pub fn allowed_for_check() -> String {
        format!(
            "{},{}",
            Self::ReadOnly.allowed(),
            crate::mcp::server::CHECK_TOOLS_CSV
        )
    }

    /// `--disallowedTools` for a check: the whole dangerous list, because the
    /// check allows none of it. Spelled through `ReadOnly` so the two grants
    /// can never disagree about what "read-only" denies.
    pub fn denied_for_check() -> String {
        Self::ReadOnly.denied()
    }

    /// `--allowedTools` for an INTERPRETATION run: nothing at all.
    ///
    /// Empty is a real answer, not a missing one, and the caller omits the flag
    /// rather than passing `""`. An interpretation's whole input is the
    /// evidence already in its prompt, so a tool would only let it go looking
    /// for something nobody bounded.
    pub fn allowed_for_interpret() -> &'static str {
        ""
    }

    /// `--disallowedTools` for an interpretation: the whole dangerous list
    /// **plus the read tools**.
    ///
    /// This is the function the safety argument rests on. An interpretation is
    /// the one run that happens with **no repo on the Mac**, where invariant 12
    /// has no workspace to point at — so instead of bounding where it can
    /// reach, nothing is reachable. `--allowedTools` could never achieve that
    /// (it is additive over the user's `permissions.allow`, invariant 7), which
    /// is why `Read`/`Glob`/`Grep` are *named* here and not merely left out.
    pub fn denied_for_interpret() -> String {
        DANGEROUS_TOOLS
            .iter()
            .chain(READ_TOOLS.iter())
            .copied()
            .collect::<Vec<_>>()
            .join(",")
    }

    /// `--disallowedTools`: everything dangerous this tier does not allow.
    ///
    /// Note `WebFetch`/`WebSearch` are denied by **every** tier — an unattended
    /// run is told nobody is watching, and has no legitimate need for network
    /// egress. (It is not a complete egress guard: a `Shell`/`Write` run still
    /// has `curl`. Denying the two tools is worth doing; believing it seals the
    /// network is how the first version of this went wrong. Codex's sandbox
    /// gets closer, but see [`providers::codex::NETWORK_OFF`] for exactly how
    /// far that claim can honestly be pushed.)
    pub fn denied(self) -> String {
        let allowed: Vec<&str> = self.allowed().split(',').collect();
        DANGEROUS_TOOLS
            .iter()
            .filter(|t| !allowed.contains(*t))
            .copied()
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Whether a run at this tier must be snapshotted first (invariant 7).
    ///
    /// True for anything that can mutate the tree — which includes `Shell`,
    /// because a shell can write. Gating the checkpoint on "has Edit/Write"
    /// rather than "can mutate" is the bug this replaces. Provider-independent:
    /// Codex's `workspace-write` sandbox is exactly as mutation-capable.
    pub fn needs_checkpoint(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    /// The stored / wire name for a tier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Shell => "shell",
            Self::Write => "write",
        }
    }

    /// Strict parse, for a `hosts` row or anything a caller typed.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "read-only" | "readonly" | "read_only" => Ok(Self::ReadOnly),
            "shell" => Ok(Self::Shell),
            "write" => Ok(Self::Write),
            other => Err(format!(
                "unknown tool tier '{other}' (expected read-only, shell or write)"
            )),
        }
    }

    /// How much reach a tier grants, low to high.
    ///
    /// Written out rather than taken from `derive(Ord)`, which would key the
    /// ordering to *declaration order* — so reordering the variants for
    /// readability would silently invert every ceiling comparison in
    /// [`crate::routing`], with nothing failing to compile. A tier ordering is
    /// a security boundary; it should cost a deliberate edit to change.
    pub fn rank(self) -> u8 {
        match self {
            Self::ReadOnly => 0,
            Self::Shell => 1,
            Self::Write => 2,
        }
    }
}

/// The bounds on one run, resolved from the budget settings at claim time.
///
/// A struct rather than four positional arguments because the fourth was the
/// one that tipped `run_one` over clippy's argument limit — and answering that
/// with a second `#[allow(too_many_arguments)]` is how a call site ends up
/// passing `timeout` where `model` was meant, silently.
#[derive(Debug, Clone)]
pub struct RunLimits {
    /// The snapshot this run is executing against, when the workspace is a
    /// mirror. Carried here rather than re-derived inside `run_one_inner`
    /// because that function cannot see the host registry and must not learn
    /// to — see `build_prompt_full` for why the alternative (guessing from the
    /// path) is worse than a parameter.
    pub mirror: Option<crate::mirror::Snapshot>,
    /// MCP servers this run was granted, already resolved on this host.
    pub extra_mcp: Vec<crate::mcp_registry::ResolvedServer>,
    pub timeout: Duration,
    pub model: Option<String>,
    /// Claude's turn ceiling. A loop detector — see
    /// [`crate::db::meta::DEFAULT_MAX_TURNS`].
    pub max_turns: i64,
    /// The per-run token ceiling, enforced live by [`process`]. `0` disables.
    pub token_ceiling: i64,
}

/// How long the one-turn handoff salvage gets.
///
/// Short on purpose: it has one turn and no tools, so anything past a couple of
/// minutes is the CLI failing to resume rather than an agent thinking. The whole
/// point is to rescue value from a run that already ended, and it must not
/// become a second way to hold a concurrency slot.
const SALVAGE_TIMEOUT: Duration = Duration::from_secs(180);

/// Whether a stopped run should spend one more turn rescuing its handoff.
///
/// Pure, and split out because both of its `false` cases are **omissions** —
/// invisible to a reader, and each guarding a stated decision:
///
/// * A run **we killed** never salvages. After a wall-clock kill the transcript
///   can hold a `tool_use` with no matching `tool_result`, which makes
///   `--resume` a 400 — not worth discovering at the end of a twenty-minute
///   run. After the *token* ceiling it is worse than useless: spending another
///   turn immediately after a spend guard fires is incoherent.
/// * A run that ended on its own never salvages either, because it already said
///   whatever it had to say.
fn should_salvage(kill: Option<process::KillReason>, stop: Option<providers::StopReason>) -> bool {
    kill.is_none() && matches!(stop, Some(providers::StopReason::MaxTurns { .. }))
}

/// One agent invocation. Blocking; the caller runs it on its own thread.
///
/// Returns the outcome to record. Never panics on a malformed stream — a run
/// that produces garbage is a failed run, not a crashed runner.
///
/// The CLI path is passed in rather than resolved here, which is what lets a
/// test drive the whole spawn/drain/parse path against a fake executable
/// without touching the real one or spending a single token.
pub fn run_one(
    task: &Task,
    repo: &std::path::Path,
    provider: Provider,
    cli: &std::path::Path,
    limits: &RunLimits,
) -> RunOutcome {
    run_one_inner(task, repo, provider, cli, limits, None, RunKind::Work)
}

/// [`run_one`] for a tripwire CHECK — the un-recorded path, for tests that
/// drive the check prompt, grant and verdict against a fake CLI.
pub fn run_one_check(
    task: &Task,
    repo: &std::path::Path,
    cli: &std::path::Path,
    limits: &RunLimits,
) -> RunOutcome {
    run_one_inner(
        task,
        repo,
        Provider::Claude,
        cli,
        limits,
        None,
        RunKind::Check,
    )
}

/// [`run_one`] for an INTERPRETATION — the un-recorded path, for tests that
/// drive the tool-less grant and the prompt against a fake CLI.
///
/// The evidence must already be on disk (`dispatch::write_interpret_evidence`):
/// the prompt builder reads it from the scratch dir rather than taking it as a
/// parameter, so the same bytes an operator can `cat` afterwards are the bytes
/// the run was given.
pub fn run_one_interpret(
    task: &Task,
    repo: &std::path::Path,
    cli: &std::path::Path,
    limits: &RunLimits,
) -> RunOutcome {
    run_one_inner(
        task,
        repo,
        Provider::Claude,
        cli,
        limits,
        None,
        RunKind::Interpret,
    )
}

/// The production path: same execution contract as [`run_one`], plus a run id
/// whose trace can be committed while the process is still alive. Keeping the
/// unrecorded wrapper makes provider/process tests independent of the user's
/// on-disk application database.
pub(crate) fn run_one_recorded(
    task: &Task,
    repo: &std::path::Path,
    provider: Provider,
    cli: &std::path::Path,
    limits: &RunLimits,
    run_id: &str,
    kind: RunKind,
) -> RunOutcome {
    let outcome = run_one_inner(task, repo, provider, cli, limits, Some(run_id), kind);
    // ALWAYS, and on this side of the call rather than inside it — every early
    // return in `run_one_inner` is a path where the run ended, and a flag that
    // outlives its run is litter. Run ids are unique so it could never fire
    // again, but a cancelled run would leave one file behind forever.
    //
    // It also covers the race worth naming: a cancel arriving in the moment
    // between the last poll and the process exiting on its own. The run reports
    // whatever it actually did, and the request is cleared rather than left
    // armed for a run id that no longer exists.
    crate::paths::clear_cancel(run_id);
    outcome
}

fn run_one_inner(
    task: &Task,
    repo: &std::path::Path,
    provider: Provider,
    cli: &std::path::Path,
    limits: &RunLimits,
    run_id: Option<&str>,
    kind: RunKind,
) -> RunOutcome {
    let executor = provider.executor();
    // A check's tier is PINNED, whatever the task's flags say. The task's
    // `bg_writes_allowed` describes what its work may do; the check is a
    // different run with a different question and never inherits it.
    let tier = match kind {
        RunKind::Work => ToolTier::for_task(task),
        // `Interpret` is BELOW `ReadOnly` and there is no variant for it: a
        // tier is a level a task can be granted, and nothing may store "no
        // tools". The grant is pinned in `claude::command` off the KIND, and
        // this value only names the ceiling it is under.
        RunKind::Check | RunKind::Interpret => ToolTier::ReadOnly,
    };
    // Starts before scratch preparation so even a run that fails before spawn
    // produces its run-started hint. Drop closes and joins the writer on every
    // return path; trace persistence can never outlive final run recording.
    // Fatal if it cannot start, NOT best-effort. The writer's connection carries
    // the heartbeat, so a run that proceeds without one stops reporting
    // liveness and gets reclaimed as `abandoned` while it is still working —
    // and the reclaim would land mid-flight, with the work unreported. Refusing
    // up front costs nothing by comparison: a run that cannot reach the DB
    // could not have recorded its outcome anyway.
    let progress = match run_id {
        Some(run_id) => {
            match progress::ProgressReporter::start(run_id, &task.id, repo.to_path_buf()) {
                Ok(p) => Some(p),
                Err(e) => {
                    return failed(
                    executor,
                    format!("could not start progress reporting (the run would not be able to report that it is alive): {e}"),
                );
                }
            }
        }
        None => None,
    };

    let (scratch, prompt_file) = match kind {
        RunKind::Work => (
            write_scratch(
                task,
                repo,
                PromptTarget::Background(executor),
                limits.mirror.as_ref(),
            ),
            "prompt.txt",
        ),
        RunKind::Check => (
            crate::dispatch::write_check_scratch(task, repo),
            "check-prompt.txt",
        ),
        RunKind::Interpret => (
            crate::dispatch::write_interpret_scratch(task, repo),
            crate::dispatch::INTERPRET_PROMPT_FILE,
        ),
    };
    let scratch = match scratch {
        Ok(s) => s,
        Err(e) => {
            return failed(executor, format!("could not prepare the scratch dir: {e}"));
        }
    };
    let prompt = match std::fs::read_to_string(scratch.join(prompt_file)) {
        Ok(p) => p,
        Err(e) => {
            return failed(executor, format!("could not read the seed prompt: {e}"));
        }
    };
    // Codex writes its final message here and we read it back in `finalize`. A
    // stale file from a previous run of the SAME task would otherwise be
    // reported as this run's answer — the scratch dir is per-task, not per-run.
    let _ = std::fs::remove_file(scratch.join(providers::codex::LAST_MESSAGE_FILE));

    let req = RunRequest {
        task_id: task.id.clone(),
        repo: repo.to_path_buf(),
        tier,
        kind,
        prompt,
        scratch,
        model: limits.model.clone(),
        max_turns: limits.max_turns,
        timeout: limits.timeout,
        token_ceiling: limits.token_ceiling,
        extra_mcp: limits.extra_mcp.clone(),
    };

    // Built once rather than per poll: `cancel_flag_path` formats a string, and
    // the loop below asks four times a second for as long as the run lasts.
    let cancel_check: Option<Box<dyn Fn() -> bool>> = run_id.map(|id| {
        let path = crate::paths::cancel_flag_path(id);
        Box::new(move || path.exists()) as Box<dyn Fn() -> bool>
    });

    let summary = Arc::new(std::sync::Mutex::new(RunSummary::default()));
    let outcome = match process::run_to_completion(
        provider.command(cli, &req),
        provider,
        limits.timeout,
        limits.token_ceiling,
        &summary,
        progress.as_ref().map(progress::ProgressReporter::sender),
        // A run with no id cannot be cancelled, because there is nothing to
        // address the request to. That is only the un-recorded path
        // (`run_one` without a row), which nothing user-facing reaches.
        cancel_check.as_ref().map(|f| f as &dyn Fn() -> bool),
    ) {
        Ok(o) => o,
        Err(e) => {
            return failed(executor, format!("could not spawn {executor}: {e}"));
        }
    };

    let mut summary = process::take_summary(&summary);
    // On EVERY exit path, timeout included: finalize is where Codex reads its
    // result file and where its token buckets are de-duplicated. Skipping it on
    // the timeout branch would silently double-count the cached prefix into the
    // daily budget for exactly the runs that already cost the most.
    provider.finalize(&mut summary, &req);

    // The event states the turn it died on; the ceiling is ours, so fill it in
    // when the event didn't bother.
    let stop = summary.stop_reason.map(|s| s.resolved(limits.max_turns));

    match outcome.kill {
        Some(process::KillReason::Timeout) => {
            return RunOutcome {
                status: "timeout".into(),
                exit_code: None,
                result_text: Some(format!(
                    "Run exceeded its {} minute limit and was killed.\n\n{}",
                    limits.timeout.as_secs() / 60,
                    summary.best_text()
                )),
                ..from_summary(executor, summary)
            };
        }
        Some(process::KillReason::TokenCeiling { spent, cap }) => {
            let detail =
                format!("was stopped by its per-run token ceiling ({spent} of {cap} tokens)");
            return RunOutcome {
                status: "budget_stopped".into(),
                exit_code: None,
                result_text: Some(format!(
                    "Run {detail} and was killed.\n\n{}",
                    summary.best_text()
                )),
                stop_detail: Some(detail),
                ..from_summary(executor, summary)
            };
        }
        Some(process::KillReason::Cancelled) => {
            // NOT a failure, and the wording matters: this run did what it was
            // told. `best_text()` is still carried so whatever it had already
            // produced survives being stopped — a cancel that also discards the
            // work done so far is a worse answer than letting it finish.
            return RunOutcome {
                status: "cancelled".into(),
                exit_code: None,
                result_text: Some(format!("Run was cancelled.\n\n{}", summary.best_text())),
                stop_detail: Some("cancelled".into()),
                ..from_summary(executor, summary)
            };
        }
        None => {}
    }

    if let Some(stop) = stop {
        // 30 turns of genuine repo understanding, ~10M tokens, and a byte-
        // identical working tree is what a cutoff used to cost. One more turn
        // — no tools, so it can only write — turns that into a handoff the next
        // dispatch replays (invariant 18) and a review can audit (invariant 19).
        // Both of those already key on `finished_at` + a non-empty
        // `result_text` rather than on status, so nothing downstream needs to
        // learn about this state.
        // `outcome.kill` is already `None` here — the match above returned on
        // both of its arms. Asking anyway keeps the rule in one place, so
        // reordering these branches later cannot quietly enable a salvage after
        // a kill.
        // A check never salvages: its final message is a VERDICT line, and a
        // check that ran out of turns without reaching one is a failed check,
        // not a handoff worth another turn's tokens.
        let salvaged = kind == RunKind::Work
            && should_salvage(outcome.kill, Some(stop))
            && salvage_handoff(provider, cli, &req, task, &mut summary);
        let detail = format!(
            "{} — {}",
            stop.describe(),
            if salvaged {
                "a handoff was salvaged"
            } else {
                "no handoff could be salvaged"
            }
        );
        return RunOutcome {
            status: "turns_exhausted".into(),
            exit_code: outcome.exit_code.map(i64::from),
            // The notice leads, the same way the timeout branch's does: what
            // happened to the run has to be readable before what the run found,
            // and `handoff_summary` lifts this line into the comment log.
            result_text: Some(format!("Run {detail}.\n\n{}", summary.best_text())),
            stop_detail: Some(detail),
            ..from_summary(executor, summary)
        };
    }

    let ok = outcome.exit_code == Some(0) && summary.error_text.is_none();
    let mut text = summary.best_text();
    if !ok && !outcome.stderr.trim().is_empty() {
        text.push_str("\n\n--- stderr ---\n");
        text.push_str(outcome.stderr.trim());
    }

    let assessment =
        (ok && kind == RunKind::Work && task.bg_intent == crate::intent::BackgroundIntent::Work)
            .then(|| completion::assess(&text));
    let status = assessment
        .as_ref()
        .map(|result| result.status)
        .unwrap_or(if ok { "succeeded" } else { "failed" });
    let detail = assessment.and_then(|result| result.reason);
    if let Some(reason) = detail.as_deref() {
        text = format!("{}: {reason}\n\n{text}", completion::label(status));
    }
    RunOutcome {
        status: status.into(),
        stop_detail: detail,
        exit_code: outcome.exit_code.map(i64::from),
        result_text: Some(text),
        ..from_summary(executor, summary)
    }
}

/// Spend one more turn asking a cut-off run to write its handoff, and fold the
/// result back into `summary`. Returns whether it produced anything.
///
/// **Fails open.** Every failure here — no session id, no salvage command for
/// this provider, a spawn error, a nonzero exit, an empty answer — leaves the
/// summary exactly as it was, so the run still reports the narration it already
/// had. The salvage can only add.
///
/// Its tokens are folded in regardless of whether it produced text, because
/// they were spent either way and the budget has to say so.
fn salvage_handoff(
    provider: Provider,
    cli: &std::path::Path,
    req: &RunRequest,
    task: &Task,
    summary: &mut RunSummary,
) -> bool {
    let Some(session_id) = summary.session_id.clone() else {
        log::warn!("runner: a cut-off run left no session id, so its handoff cannot be salvaged");
        return false;
    };
    let Some(cmd) = provider.salvage_command(cli, req, task, &session_id) else {
        return false;
    };

    let salvaged = Arc::new(std::sync::Mutex::new(RunSummary::default()));
    // No token ceiling: the run being rescued has already tripped a guard, and
    // a second one firing inside a single tool-less turn could only cost the
    // handoff we came here for.
    // No cancel check either: this is one tool-less turn rescuing the handoff
    // from a run that has ALREADY stopped, so there is nothing left to cancel
    // and a stale flag must not kill the rescue.
    let outcome =
        process::run_to_completion(cmd, provider, SALVAGE_TIMEOUT, 0, &salvaged, None, None);
    let salvaged = process::take_summary(&salvaged);

    for (slot, spent) in [
        (&mut summary.output_tokens, salvaged.output_tokens),
        (&mut summary.input_tokens, salvaged.input_tokens),
        (&mut summary.cache_read_tokens, salvaged.cache_read_tokens),
        (&mut summary.cache_write_tokens, salvaged.cache_write_tokens),
    ] {
        if let Some(n) = spent {
            RunSummary::add(slot, n);
        }
    }

    match outcome {
        Ok(o) if o.kill.is_none() && o.exit_code == Some(0) => {}
        Ok(o) => {
            log::warn!(
                "runner: the handoff salvage exited {:?} ({:?})",
                o.exit_code,
                o.kill
            );
            return false;
        }
        Err(e) => {
            log::warn!("runner: could not spawn the handoff salvage: {e}");
            return false;
        }
    }

    let text = salvaged
        .result_text
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| salvaged.assistant_text.trim().to_string());
    if text.trim().is_empty() {
        return false;
    }
    summary.result_text = Some(text);
    true
}

/// A run that never got off the ground. Still a recorded run — a failure with a
/// reason on it is what stops the task looping through the queue forever.
fn failed(executor: BackgroundExecutor, reason: String) -> RunOutcome {
    RunOutcome {
        status: "failed".into(),
        result_text: Some(reason),
        executor,
        ..Default::default()
    }
}

fn from_summary(executor: BackgroundExecutor, s: RunSummary) -> RunOutcome {
    RunOutcome {
        output_tokens: s.output_tokens,
        session_id: s.session_id,
        input_tokens: s.input_tokens,
        cache_read_tokens: s.cache_read_tokens,
        cache_write_tokens: s.cache_write_tokens,
        model: s.model,
        executor,
        // Filled in by the caller — listed so the struct-update syntax above is
        // exhaustive and adding a field to RunOutcome is a compile error here
        // rather than a silently dropped value.
        status: String::new(),
        exit_code: None,
        result_text: None,
        stop_detail: None,
    }
}

/// The model for a run: the task's own override, else the executor's default.
///
/// Claude's default is a deliberate cost decision (Sonnet, not the session's
/// Opus), so it is always filled in. Codex gets `None` — meaning "use the CLI's
/// own default" — because hard-coding an OpenAI model name in this repo means
/// shipping a value that goes stale on someone else's release schedule.
pub fn model_for(task: &Task, settings: &BudgetSettings) -> Option<String> {
    let explicit = task.model.clone().filter(|m| !m.trim().is_empty());
    match task.bg_executor {
        BackgroundExecutor::Claude => explicit.or_else(|| Some(settings.model.clone())),
        BackgroundExecutor::Codex => explicit,
    }
}

/// The wall clock for a run: the task's own override, else the global setting.
///
/// Same shape as [`model_for`], and clamped to the same bounds the global
/// setting is, so a stored value can never mean one thing here and another in
/// `budget_settings`.
///
/// **Whatever this returns must be recorded on the run row**
/// (`db::runs::start`), because [`crate::db::runs::sweep_abandoned`] reclaims
/// stranded runs by comparing age against the clock the run was started with.
/// Resolve it here and forget to persist it and the sweep falls back to the
/// global cutoff, which reclaims a healthy long run while its process is still
/// executing — invariant 20's failure, inverted.
pub fn timeout_for(task: &Task, settings: &BudgetSettings) -> i64 {
    task.bg_timeout_minutes
        .unwrap_or(settings.timeout_minutes)
        .clamp(db::meta::MIN_TIMEOUT_MINUTES, db::meta::MAX_TIMEOUT_MINUTES)
}

/// The per-run token ceiling: the task's own override, else the global setting.
/// `0` disables the ceiling, which is why this is not filtered for non-zero.
///
/// Raised alongside the clock or it does nothing useful: a measured Opus run
/// touched 24.6M in 14 minutes, so a 60-minute task left on the 25M default
/// dies on tokens long before its new clock matters. Bounds ONE run — the
/// daily cap is a separate, deliberately un-overridable guard (invariant 6).
pub fn token_cap_for(task: &Task, settings: &BudgetSettings) -> i64 {
    task.bg_run_token_cap
        .unwrap_or(settings.run_token_cap)
        .clamp(db::meta::MIN_RUN_TOKEN_CAP, db::meta::MAX_RUN_TOKEN_CAP)
}

/// The concurrency ceiling THIS host runs under: the global setting, narrowed
/// by the host's own opinion when it has one (`0` = none).
pub fn host_max_concurrent(settings: &BudgetSettings, cap: &crate::routing::HostCapability) -> i64 {
    if cap.max_concurrent > 0 {
        settings.max_concurrent.min(cap.max_concurrent)
    } else {
        settings.max_concurrent
    }
}

/// Keep every remote host's mirrors warm.
///
/// # Why this runs on a timer at all
///
/// A run could push its own mirror at queue time and nothing else would be
/// needed — correctness does not depend on this loop. What it buys is that the
/// push has usually already happened when a run starts, so dispatch is fast
/// instead of stalling on a transfer. The tradeoff is stated in
/// [`crate::mirror::fingerprint`]: the trigger may miss a change, and missing
/// one only costs a refresh nobody was waiting on.
///
/// Only repos with a queued or in-flight remote-eligible task are mirrored.
/// Mirroring all 25 repos on a timer would be a lot of ssh for trees nothing is
/// going to ask about.
pub fn mirror_pass(conn: &Connection) -> Result<usize, AppError> {
    let mut pushed = 0usize;
    for host in db::hosts::list(conn)? {
        if !host.enabled || host.is_local() {
            continue;
        }
        let Some(remote_base) = db::hosts::mirror_remote(conn, &host.id)? else {
            continue;
        };
        for repo in repos_wanting_a_mirror(conn)? {
            match sync_one_mirror(conn, &host.id, &remote_base, &repo) {
                Ok(true) => pushed += 1,
                Ok(false) => {}
                // A failed mirror DEGRADES rather than refusing: the repo stays
                // absent from that host's mirror set, so `routing::satisfies`
                // simply does not offer it there. A loud failure here would
                // turn "the NAS is unreachable" into "queueing is broken".
                Err(e) => log::warn!("mirror: {} -> {}: {e}", repo, host.label),
            }
        }
    }
    Ok(pushed)
}

/// Repos that some remote-eligible task is waiting on.
fn repos_wanting_a_mirror(conn: &Connection) -> Result<Vec<String>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT repo_path FROM tasks
          WHERE deleted_at IS NULL AND archived_at IS NULL
            AND bg_remote_allowed = 1
            AND status IN ('queued', 'in_progress')
            AND repo_path IS NOT NULL AND TRIM(repo_path) <> ''",
    )?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Push one repo to one host if anything has changed since last time.
///
/// Returns whether it actually pushed. The fingerprint is cached in `app_meta`
/// keyed by (host, repo) rather than held in memory, so a runner restart does
/// not re-push every mirror — which on a cold morning would be every repo at
/// once, over ssh, before any work started.
pub fn sync_one_mirror(
    conn: &Connection,
    host_id: &str,
    remote_base: &str,
    repo: &str,
) -> Result<bool, AppError> {
    let path = std::path::Path::new(repo);
    if !crate::checkpoint::is_git_repo(path) {
        return Err(AppError::General(format!("{repo} is not a git repository")));
    }
    let key = crate::mirror::repo_key(path);
    let cache_key = format!("mirror_fp_{host_id}_{key}");
    let now = crate::mirror::fingerprint(path)?;
    if db::meta::get(conn, &cache_key)?.as_deref() == Some(now.as_str()) {
        return Ok(false);
    }

    let remote = format!("{}/{key}.git", remote_base.trim_end_matches('/'));
    crate::mirror::ensure_remote(&remote)?;
    let snap = crate::mirror::push(path, &remote)?;
    log::info!(
        "mirror: pushed {repo} to {host_id} ({} · {} dirty file(s))",
        &snap.sha[..8.min(snap.sha.len())],
        snap.dirty_files
    );

    // Record BOTH, and the order matters: the host's mirror set is what makes
    // routing offer this repo there, so it must not be claimed before the bytes
    // are actually across.
    db::hosts::record_mirror(conn, host_id, repo)?;
    db::meta::set(conn, &cache_key, &now)?;
    Ok(true)
}

/// How often the control loop looks for things it must react to promptly.
///
/// Much faster than [`TICK_SECONDS`] because it answers a different question.
/// The drain tick asks "is there work", which can wait; this asks "was I told
/// to stop", which cannot — a Stop button that takes 20 seconds to do anything
/// gets pressed again, and then a third time.
pub const CONTROL_TICK_SECONDS: u64 = 3;

/// How often this host writes down that it is alive.
///
/// Deliberately NOT the control tick. A heartbeat moves `hosts.updated_at`,
/// which puts the row on the wire (see [`crate::sync::sweep::SWEEP`]), so
/// beating every 3 seconds would push a row across the network twenty times
/// for every one thing it told anybody. Liveness is judged in minutes
/// ([`crate::db::hosts::HOST_STALE_MINUTES`]), so a minute is ample.
pub const HEARTBEAT_SECONDS: u64 = 60;

/// One pass of the control loop: react to anything addressed to this host, then
/// say that it is alive.
///
/// Returns whether a heartbeat was written, so the caller can pace them without
/// this function owning a clock.
///
/// Everything here is deliberately cheap and side-effect-light. It runs four
/// times a minute on a box that is also executing agents, and a control loop
/// that competes with the work it is supervising is its own outage.
pub fn control_pass(conn: &Connection, beat: bool) -> Result<bool, AppError> {
    let cap = db::hosts::local(
        conn,
        &crate::routing::HostCapability::declared_for_this_host(),
    )?;

    // Cancels addressed to me become the local file `process.rs` already polls.
    for run_id in db::runs::pending_cancels(conn)? {
        if let Err(e) = crate::paths::request_cancel(&run_id) {
            log::warn!("control: could not raise the cancel flag for {run_id}: {e}");
        }
    }

    // `hosts.paused` is a per-host switch, and it lands in the same file the
    // global kill switch uses — so there is ONE thing the drain loop consults,
    // and pausing a host from the app behaves identically to touching the file
    // by hand. The global switch stays a file too, which is why pausing works
    // even when this loop is not running.
    let switch = crate::paths::kill_switch_path();
    if cap.paused && !switch.exists() {
        if let Err(e) = std::fs::write(&switch, "paused from the host registry\n") {
            log::warn!("control: could not raise the kill switch: {e}");
        }
    }

    if !beat {
        return Ok(false);
    }

    // Observed capability: what this host can actually reach RIGHT NOW. The
    // probe shells out (a login shell to find the CLI, `codex login status`),
    // which is exactly why it is on the slow cadence.
    let ready: Vec<crate::executor::BackgroundExecutor> = crate::executor::BackgroundExecutor::ALL
        .into_iter()
        .filter(|e| cap.executors.contains(e) && Provider::for_executor(*e).preflight().is_ok())
        .collect();

    let settings = db::meta::budget_settings(conn);
    let gate_now = gate(
        crate::paths::background_paused(),
        &settings,
        &cap,
        db::runs::count_running_background(conn)?,
        db::runs::count_background_runs_today(conn)?,
        db::runs::total_tokens_today(conn)?,
    );

    db::hosts::heartbeat(
        conn,
        &cap.id,
        crate::paths::background_paused(),
        Some(&format!("{gate_now:?}")),
        &ready,
    )?;
    Ok(true)
}

/// One tick: claim what the gate allows and spawn a thread per claimed task.
/// Returns the number of runs started.
pub fn tick(in_flight: &Arc<AtomicUsize>) -> Result<usize, AppError> {
    let mut conn = open_db()?;
    let settings = db::meta::budget_settings(&conn);
    // Register before anything else reads the registry, so a machine appears in
    // it by virtue of running the software rather than by somebody remembering
    // to add it. Re-registering is a no-op on the declared half (see
    // `db::hosts`), so this is safe on every tick and not only at startup.
    let cap = db::hosts::local(
        &conn,
        &crate::routing::HostCapability::declared_for_this_host(),
    )?;
    // BEFORE the gate is computed, on every tick, and not only at startup.
    //
    // `initialize` already sweeps, but a runner that has been up for days never
    // calls it again — and the crash that strands a run is most likely to
    // happen to a *previous* runner process while this one is the replacement.
    // Sweeping here is what makes the recovery self-inflicted rather than
    // waiting on the user to relaunch the app: the very next tick after a
    // stranded row ages past the cutoff gives the slot back.
    if let Err(e) = db::runs::sweep_abandoned(&conn) {
        // Never fatal to a tick. A tick that refuses to run because the
        // cleanup failed is strictly worse than one that runs with a slot
        // still held.
        log::warn!("runner: abandoned-run sweep failed: {e}");
    }
    let running = db::runs::count_running_background(&conn)?;
    let runs_today = db::runs::count_background_runs_today(&conn)?;

    let tokens_today = db::runs::total_tokens_today(&conn)?;
    let decision = gate(
        crate::paths::background_paused(),
        &settings,
        &cap,
        running,
        runs_today,
        tokens_today,
    );
    let Lanes { scripts, slots } = lanes(&decision);
    if !scripts {
        // One of the switches. Nothing runs — not even a script.
        log::debug!("runner: switched off ({decision:?})");
        return Ok(0);
    }
    // The free lane, BEFORE the budget gate's early return below. A script
    // costs neither tokens nor a slot, so a day whose budget is spent must
    // still evaluate its tripwires — otherwise a spent budget silently stops
    // zero-token checks (gotcha #42).
    // A monitor completed somewhere that didn't re-arm it (a synced post-image
    // from an older build) is re-armed here, before `list_due` looks — so it
    // is checked on THIS tick, like one completed in the app.
    match db::tripwires::rearm_handled_monitors(&conn) {
        Ok(ids) if !ids.is_empty() => {
            log::info!(
                "runner: re-armed {} handled monitor(s): {}",
                ids.len(),
                ids.join(", ")
            )
        }
        Ok(_) => {}
        Err(e) => log::warn!("runner: could not re-arm handled monitors: {e}"),
    }
    match tripwire::script_pass(&conn, in_flight) {
        Ok(0) => {}
        Ok(n) => log::info!("runner: evaluating {n} tripwire script(s)"),
        Err(e) => log::warn!("runner: tripwire pass failed: {e}"),
    }
    if slots <= 0 {
        log::debug!("runner: not starting agent work ({decision:?})");
        return Ok(0);
    }

    // The AGENT-CHECK lane: at most ONE slot, taken before the drain so a
    // queue of work cannot starve the tripwires, and only one so the
    // tripwires cannot starve the work. It sits BELOW the budget return on
    // purpose — unlike a script, a check costs tokens and a slot.
    let checks = match tripwire::agent_pass(&mut conn, in_flight, &settings, &cap, slots.min(1)) {
        Ok(n) => n,
        Err(e) => {
            log::warn!("runner: agent tripwire pass failed: {e}");
            0
        }
    };
    let slots = slots - checks as i64;
    if slots <= 0 {
        return Ok(checks);
    }

    let queued = db::tasks::list_queued_for_background(&conn, &cap, slots as u32)?;
    let mut started = checks;

    for task in queued {
        // Re-check the switch between spawns: a long tick shouldn't start a
        // second run after the user has already hit stop.
        if crate::paths::background_paused() {
            log::info!("runner: kill switch tripped mid-tick; stopping");
            break;
        }

        let provider = Provider::for_task(&task);
        // A CHEAP pre-claim check, so an unrunnable task is parked without
        // burning a claim. On a mirror host it deliberately does not touch the
        // filesystem — the tree does not exist yet, and building it before the
        // claim would materialize a workspace for a run that may lose the race.
        let precheck = if cap.is_local() {
            preflight(&task).map(|_| ())
        } else {
            Provider::for_task(&task).preflight()
        };
        if let Err(reason) = precheck {
            log::warn!("runner: skipping task {}: {reason}", task.id);
            // Park it rather than looping on it every 20s forever: back to
            // `todo` with the explanation as its HOLD (`blocked_reason`), which
            // takes it out of the drain and into Waiting until someone reads
            // it. NOT appended to `body`: the body is the task's definition and
            // folding a run log into it leaves a body nobody dares edit (see
            // the comments module).
            let _ = db::tasks::update(
                &conn,
                &task.id,
                &db::tasks::TaskPatch {
                    status: Some("todo".into()),
                    blocked_reason: Some(Some(format!("Background run refused: {reason}"))),
                    ..Default::default()
                },
            );
            continue;
        }
        // Resolved before the claim so a missing binary parks the task rather
        // than claiming it into `in_progress` and immediately failing it.
        let Some(cli) = provider.find_cli() else {
            log::warn!(
                "runner: {} CLI not found; skipping {}",
                provider.executor(),
                task.id
            );
            continue;
        };

        // Resolved BEFORE the run row is written, because the row has to record
        // the clock this run is actually being given — `sweep_abandoned` reads
        // it back to decide whether a still-`running` row is late or merely
        // long (invariant 20).
        let timeout_minutes = timeout_for(&task, &settings);

        // Claim and record in ONE write transaction, which re-checks the
        // concurrency count inside the write lock. The gate above is still
        // what decides whether to bother; this is what makes it true.
        let host_max = host_max_concurrent(&settings, &cap);
        let Some(run) = db::runs::claim_and_start(
            &mut conn,
            &cap,
            &task.id,
            host_max,
            provider.executor(),
            task.bg_intent,
            timeout_minutes,
        )?
        else {
            // Lost the race, or filled the last slot between the gate and here.
            continue;
        };

        // The tree this run works in. On a local host that is the repo; on a
        // mirror host it is a fresh clone of a snapshot, which is why it needs
        // the run id and therefore has to happen after the claim.
        let workspace = match ready(&conn, &cap, &task, &run.id) {
            Ok(w) => w,
            Err(reason) => {
                log::warn!(
                    "runner: could not prepare a workspace for {}: {reason}",
                    task.id
                );
                let _ = db::runs::finish(
                    &conn,
                    &run.id,
                    &failed(
                        provider.executor(),
                        format!("Could not prepare a workspace: {reason}"),
                    ),
                );
                let _ = db::tasks::update(
                    &conn,
                    &task.id,
                    &db::tasks::TaskPatch {
                        status: Some("todo".into()),
                        blocked_reason: Some(Some(format!("Background run refused: {reason}"))),
                        ..Default::default()
                    },
                );
                continue;
            }
        };
        let repo = workspace.path.clone();

        // A mirror run's tree is a disposable clone, so there is nothing to
        // undo and `can_checkpoint` is false on such a host — which is what
        // makes this branch unreachable there rather than merely skipped.
        //
        // Snapshot BEFORE the agent touches anything, and AFTER the run row so
        // the ref can be named for the RUN. Keyed on `task.id` this used to
        // give two runs of one task the same ref name, so the second silently
        // overwrote the first's snapshot.
        if ToolTier::for_task(&task).needs_checkpoint() {
            match crate::checkpoint::capture(&repo, &run.id) {
                Ok(r) => {
                    db::runs::set_checkpoint(&conn, &run.id, &r)?;
                }
                Err(e) => {
                    log::warn!("runner: checkpoint failed for run {}: {e}", run.id);
                    // A RECORDED failure now, not a task parked with no run row.
                    // The old shape left the history showing nothing happened,
                    // which is the one place somebody would have looked.
                    let _ = db::runs::finish(
                        &conn,
                        &run.id,
                        &failed(
                            provider.executor(),
                            format!(
                                "Could not capture a checkpoint ({e}). Without one the \
                                 agent's writes would not be reviewable or undoable, so \
                                 the run was refused before it started."
                            ),
                        ),
                    );
                    let _ = db::tasks::update(
                        &conn,
                        &task.id,
                        &db::tasks::TaskPatch {
                            status: Some("todo".into()),
                            blocked_reason: Some(Some(format!(
                                "Background run refused: could not capture a checkpoint \
                                 ({e}). Without one the agent's writes would not be \
                                 reviewable or undoable."
                            ))),
                            ..Default::default()
                        },
                    );
                    continue;
                }
            }
        }
        started += 1;
        in_flight.fetch_add(1, Ordering::SeqCst);

        let limits = RunLimits {
            mirror: workspace.snapshot.clone(),
            // Resolved from the registry, and only ever what the task named.
            // `ready` already refused the run if this host could not provide
            // one of them, so an error here is unreachable rather than merely
            // unlikely — an empty grant is the right thing to fall back to.
            extra_mcp: crate::mcp_registry::resolve_all(
                task.bg_mcp_servers.as_deref().unwrap_or_default(),
            )
            .map(|(ready, _)| ready)
            .unwrap_or_default(),
            timeout: Duration::from_secs((timeout_minutes as u64) * 60),
            model: model_for(&task, &settings),
            max_turns: settings.max_turns,
            token_ceiling: token_cap_for(&task, &settings),
        };
        let in_flight = in_flight.clone();
        let task_for_thread = task.clone();
        let run_id = run.id.clone();
        let workspace = workspace.clone();

        std::thread::spawn(move || {
            // Decrement on EVERY exit path, including a panic inside run_one.
            // Without this an aborted run leaks a slot and the runner slowly
            // throttles itself to zero with nothing visibly wrong.
            struct SlotGuard(Arc<AtomicUsize>);
            impl Drop for SlotGuard {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _slot = SlotGuard(in_flight);

            log::info!(
                "runner: starting {} run {run_id} for task {} in {}",
                provider.executor(),
                task_for_thread.id,
                repo.display()
            );
            let outcome = run_one_recorded(
                &task_for_thread,
                &repo,
                provider,
                &cli,
                &limits,
                &run_id,
                RunKind::Work,
            );

            if let Err(e) = finish_run(&task_for_thread, &run_id, &outcome) {
                log::error!("runner: could not record run {run_id}: {e}");
            }
            // AFTER recording, and only for a mirror — `release` is a no-op on a
            // live workspace, which is the only thing standing between this
            // line and `rm -rf` on the user's repo. Recording first because a
            // failure to clean up must never cost the run's result.
            release(&workspace);
        });
    }

    Ok(started)
}

/// How much of a handoff the comment log carries.
const SUMMARY_CHARS: usize = 200;

/// Reduce a handoff to the one line a human scanning the task should see.
///
/// The full text lives on the run row and is replayed to the next agent by
/// [`crate::dispatch::history_block`]; this is purely the human-facing pointer,
/// so it optimises for "can I tell at a glance what came of this" rather than
/// completeness.
///
/// Markdown markers are stripped rather than rendered because the summary also
/// travels into a prompt, where a stray `##` is noise. A leading heading is kept
/// AND the following line appended — agents habitually open with a title
/// (`## Handoff: …`), which names the task back at you without saying what
/// happened, so a heading alone would make every summary useless.
pub fn handoff_summary(text: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("```") {
            continue;
        }
        // A rule (`---`, `===`, `***`) carries no words.
        if line.len() >= 3 && line.chars().all(|c| c == '-' || c == '=' || c == '*') {
            continue;
        }
        let was_heading = line.starts_with('#');
        let cleaned = strip_markdown(line);
        if cleaned.is_empty() {
            continue;
        }
        parts.push(cleaned);
        // A heading is a title, not an outcome — keep reading for one line more.
        if !was_heading || parts.len() >= 2 {
            break;
        }
    }
    providers::bounded(&parts.join(" — "), SUMMARY_CHARS)
}

/// Flatten one line of markdown to its words.
fn strip_markdown(line: &str) -> String {
    let mut s = line.trim_start_matches('#').trim().to_string();
    for prefix in ["- ", "* ", "+ ", "> "] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.to_string();
            break;
        }
    }
    s = s.replace("**", "").replace("__", "").replace('`', "");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What a finished run does to its task's row. `None` = leave it alone.
///
/// Pure, and split out of [`finish_run`] for the reason `gate` and
/// [`handoff_summary`] already are: `finish_run` opens the real database, so the
/// decision inside it is otherwise untestable — and two of the three rules
/// below look like omissions rather than choices.
///
/// **Only advance a task that's still mid-run.** If a human (or an agent that
/// ignored its instructions) already moved it, that decision wins — silently
/// reopening a task someone just closed is worse than a stale status.
///
/// **The intent reset lives INSIDE that guard.** `bg_intent` is the field a
/// human is most likely to have just changed, so a reset that fired regardless
/// would override exactly the mid-run decision the guard exists to protect.
///
/// **And only on success.** A review that dies at spawn must leave the arming
/// in place: consuming it would send the user back to re-queue a review they
/// never got, and the failure branch parks the task under a hold, so a
/// still-armed review cannot surprise anyone in the meantime.
///
/// `stop_detail` is the clause naming whichever of our own guards stopped the
/// run, when one did. It is what turns "Background run failed — see the latest
/// run for details" into something that answers the question, and the absence
/// of it is what once cost a dig through a session transcript to find a
/// `max_turns_reached` line.
fn after_run_patch(
    current_status: Option<&str>,
    outcome_status: &str,
    stop_detail: Option<&str>,
) -> Option<db::tasks::TaskPatch> {
    if current_status != Some("in_progress") {
        return None;
    }
    if outcome_status == "succeeded" || outcome_status == completion::READY {
        return Some(db::tasks::TaskPatch {
            status: Some("in_review".into()),
            // One-shot: the next dispatch is work again unless re-armed. A
            // sticky review would audit its own verdict, forever.
            bg_intent: Some(crate::intent::BackgroundIntent::Work),
            ..Default::default()
        });
    }
    // A failed run parks the task: back to `todo`, held by a written reason
    // (`blocked_reason` — a HOLD since v27, not a status). Held even for a run
    // we stopped ourselves: there IS no completed work to review, and the hold
    // is exactly "waiting on a human to look at that run". Re-queueing releases
    // it (`db::tasks::update`).
    //
    // The generic fallback is the run's own status vocabulary (`failed` /
    // `timeout` / `cancelled`), which at least names the shape. A guard that
    // knows more says so.
    Some(db::tasks::TaskPatch {
        status: Some("todo".into()),
        blocked_reason: Some(Some(match (outcome_status, stop_detail) {
            (completion::NEEDS_ACTION | completion::UNVERIFIED, Some(detail)) => {
                format!("{}: {detail}", completion::label(outcome_status))
            }
            (_, Some(detail)) => format!("Background run {detail}; see the latest run"),
            (_, None) => {
                format!("Background run {outcome_status} — see the latest run for details")
            }
        })),
        ..Default::default()
    })
}

/// Record a finished run and request review, or hold unfinished work with its reason.
///
/// The task never goes to `done` on its own: an unattended agent's own
/// assessment that it succeeded is exactly the thing a human should check.
fn finish_run(task: &Task, run_id: &str, outcome: &RunOutcome) -> Result<(), AppError> {
    let conn = open_db()?;
    finish_run_with_conn(&conn, task, run_id, outcome)
}

fn finish_run_with_conn(
    conn: &Connection,
    task: &Task,
    run_id: &str,
    outcome: &RunOutcome,
) -> Result<(), AppError> {
    db::runs::finish(conn, run_id, outcome)?;

    // Note the task's comment log that a run happened, and what came of it.
    //
    // **A summary, not the handoff.** Both used to hold the full text, which
    // meant the same several-thousand-character brief was rendered twice in one
    // pane. The two records answer different questions, so they now carry
    // different things: the run row is the artifact — machine bookkeeping
    // (tokens, exit code, session id, checkpoint ref) plus the complete handoff
    // — and the comment is the task's readable history, one line saying a run
    // happened and how it went.
    //
    // The comment must still exist. Without any entry a delegated run's
    // findings were reachable only by opening the run list, and the task's own
    // history read as though nothing had been done to it.
    //
    // What this does NOT do any more is carry the detail to the next agent:
    // `dispatch::history_block` replays the run's `result_text` directly for
    // that. Shortening this without that replay in place puts a second delegate
    // back to re-deriving everything the first one learned.
    //
    // Best-effort: a failure to record history must never turn a completed run
    // into a failed one. Logged, not propagated.
    if let Some(text) = outcome
        .result_text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        // A critique headed "Background run" is indistinguishable in the
        // chronology from the work it critiques — which is the whole thing this
        // feature exists to keep apart.
        let noun = if task.bg_intent.is_review() {
            "Background review"
        } else {
            "Background run"
        };
        let header = if outcome.status == "succeeded" {
            format!("{noun} · {}", outcome.executor.label())
        } else {
            format!(
                "{noun} · {} · {}",
                outcome.executor.label(),
                completion::label(&outcome.status)
            )
        };
        // Signed by the agent that ran — a Codex run's entry must not read as
        // Claude's in the log.
        if let Err(e) = db::comments::add(
            conn,
            &task.id,
            outcome.executor.as_str(),
            &format!(
                "{header}\n\n{}\n\n_Full handoff on the run._",
                handoff_summary(text)
            ),
        ) {
            log::warn!("runner: could not record the run result as a comment: {e}");
        }

        // A cheap second opinion over the handoff. It never changes status or
        // declares the work accepted; it only leaves a fixed-format flag in
        // the chronology when one of the bounded checks is concerning.
        match crate::jev::review_run(conn, task, text) {
            Ok(Some(review)) if review.needs_attention => {
                if let Err(e) = db::comments::add(
                    conn,
                    &task.id,
                    db::comments::AUTHOR_JEV,
                    &format!(
                        "Jev review · needs attention\n\n{}\n\n_Request match {:.0}% · verification {:.0}% · unresolved {:.0}%_",
                        review.note,
                        review.addresses_request * 100.0,
                        review.verification_present * 100.0,
                        review.unresolved_questions * 100.0,
                    ),
                ) {
                    log::warn!("runner: could not record Jev review: {e}");
                }
            }
            Ok(_) => {}
            Err(e) => log::warn!("runner: Jev post-run review skipped: {e}"),
        }
    }

    let current = db::tasks::get(conn, &task.id)?;
    match after_run_patch(
        current.as_ref().map(|t| t.status.as_str()),
        &outcome.status,
        outcome.stop_detail.as_deref(),
    ) {
        Some(patch) => {
            db::tasks::update(conn, &task.id, &patch)?;
        }
        None => log::info!(
            "runner: leaving task {} at status {:?} — it changed while the run was in flight",
            task.id,
            current.as_ref().map(|t| t.status.as_str())
        ),
    }

    let summary = outcome
        .result_text
        .as_deref()
        .unwrap_or("")
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("(no output)")
        .chars()
        .take(120)
        .collect::<String>();

    // Hand the app the envelope first. If it's running it both live-refreshes
    // AND posts the notification, so the banner is worded by the side that
    // knows what the user is looking at. Only when nothing is listening do we
    // post from here — which is equally well branded, since this binary also
    // lives inside the bundle (`CXTasks.app/Contents/MacOS/cxtasks-runner`);
    // what it loses is the app's phrasing, not the icon.
    let env = Envelope::new("run-finished", &task.id)
        .with_tool("runner")
        .with_run_id(run_id)
        .with_summary(&summary);

    let delivered = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| rt.block_on(crate::mcp::bridge::notify_delivered(&env)))
        .unwrap_or(false);

    if !delivered {
        // Through the hook, not `notify_macos` directly: this module has to
        // build for Linux, where there is no NSUserNotificationCenter and
        // nobody standing at the machine to see one. An uninstalled notifier
        // drops the banner and records the outcome regardless.
        crate::notify::post(
            &task.id,
            &format!(
                "{} {} {}",
                outcome.executor.label(),
                if task.bg_intent.is_review() {
                    "review"
                } else {
                    "run"
                },
                completion::label(&outcome.status)
            ),
            &format!("{}\n{}", task.title, summary),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
