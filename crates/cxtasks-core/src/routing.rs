//! Which host may run which task.
//!
//! One Mac needed none of this: the machine draining the queue was the machine
//! holding the trees, so "can I run this" collapsed into "is it opted in".
//! With a second executor it stops collapsing, and the shape of the answer
//! decides how much a third host costs.
//!
//! # Why capability matching and not a locus
//!
//! The obvious design is `tasks.bg_locus` — an enum naming the host a task
//! should run on. It puts routing knowledge on every task row, so adding a Mac
//! mini means rewriting rows, and a host that goes away leaves tasks pointing
//! at nothing. Worse, it encodes the answer rather than the question: a task
//! does not want to run *on the NAS*, it wants a read-only workspace and a
//! Claude CLI, and the NAS happens to be somewhere that has both.
//!
//! So the task states a [`RunRequirement`] (derived, stored nowhere), each host
//! declares a [`HostCapability`] (a row in `hosts`), and [`satisfies`] is a
//! pure function between them. Adding a host is a registry row plus a
//! deployment. Removing one is a row delete — every task it could have run
//! simply routes elsewhere, with nothing to rewrite.
//!
//! # Declared versus observed
//!
//! A capability has two halves and the split is load-bearing. **Declared** is
//! policy — what Chris decided this host may do. **Observed** is what the host
//! reported about itself last time it woke up. Effective capability is the
//! *intersection*, which is also the preflight split (`admit` at queue time,
//! `ready` at execute time) landed in data rather than in control flow.
//!
//! `max_tier` is a **policy ceiling, not a probe**. The NAS is `ReadOnly`
//! because that is the decision, not because any code knows what a NAS is. A
//! Mac mini set to `Write` needs no code change — which is the property that
//! `if host == nas { tier = ReadOnly }` would have destroyed.
//!
//! # Fail closed, twice
//!
//! 1. **`tasks.bg_remote_allowed`** — the remote analogue of `bg_allowed`, in
//!    a column no pre-v23 binary reads, defaulting to 0. Nothing that exists
//!    today becomes remotely claimable by the act of deploying a remote host.
//! 2. **[`queue_sql`]** — a host's own capability generates the SQL it drains
//!    with, exactly as [`BackgroundExecutor::enabled_sql`] is generated from
//!    `ALL`. A read-only host's query carries `bg_writes_allowed = 0 AND
//!    bg_shell_allowed = 0`, so it cannot claim beyond what it declares
//!    **including task shapes invented after it shipped**. That is the half
//!    that keeps working when someone adds a fourth tier and forgets this file.
//!
//! [`BackgroundExecutor::enabled_sql`]: crate::executor::BackgroundExecutor::enabled_sql

use serde::{Deserialize, Serialize};

use crate::db::tasks::Task;
use crate::executor::BackgroundExecutor;
use crate::runner::ToolTier;

/// Where a run's files come from.
///
/// Not "which host" — a kind, so that two mirror hosts are the same case and
/// the routing code never learns a machine's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    /// The repo itself, on the machine that owns it. Always preferred: it is
    /// the only kind that is not, by construction, a snapshot of the past.
    Live,
    /// A git mirror of the repo, materialized from a checkpoint commit. The
    /// tree is real and complete as of `snapshot_at`, and stale after it.
    Mirror,
    /// No repository at all — a scratch directory inside the container's mount
    /// set.
    ///
    /// # Invariant 12 is satisfied differently, not weakened
    ///
    /// The rule was "background execution is refused outside a bounded
    /// workspace", and until now a repo was the only bounded thing there was.
    /// It now reads: **refused outside a DECLARED workspace.** A repo is one. A
    /// container mount set the app authored is another. `$HOME` is neither, and
    /// that is why the MacBook does not declare this kind — an unattended run
    /// at `$HOME` gets unsupervised `Read`/`Glob`/`Grep` over every client
    /// repo, `~/.ssh`, and the notes vault, with no mount boundary anywhere.
    ///
    /// So a detached run is not "a run without a guard". The filesystem guard
    /// is the container's mount set, and **what replaces the repo gate is the
    /// tool surface** — see `tasks.bg_mcp_servers`.
    Detached,
}

impl WorkspaceKind {
    pub const ALL: [WorkspaceKind; 3] = [Self::Live, Self::Mirror, Self::Detached];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Mirror => "mirror",
            Self::Detached => "detached",
        }
    }

    /// Strict parse, for anything a caller typed.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "live" => Ok(Self::Live),
            "mirror" => Ok(Self::Mirror),
            "detached" => Ok(Self::Detached),
            other => Err(format!(
                "unknown workspace kind '{other}' (expected one of {})",
                Self::ALL.iter().map(|w| w.as_str()).collect::<Vec<_>>().join(", ")
            )),
        }
    }

    /// Freshness order, low is better. `Live` beats `Mirror` **always** —
    /// freshness dominates every other consideration, because a mirror answers
    /// a question about the tree as it was and the user is asking about the
    /// tree as it is.
    ///
    /// `Detached` sorts last and never competes: it fits only tasks with no
    /// repo, which the other two refuse outright, so the order between them is
    /// decided by [`workspace_fits`] long before this matters.
    pub fn freshness_rank(self) -> i64 {
        match self {
            Self::Live => 0,
            Self::Mirror => 1,
            Self::Detached => 2,
        }
    }
}

impl std::fmt::Display for WorkspaceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a task needs from whatever runs it.
///
/// **Derived, stored nowhere.** Storing it would create a second source of
/// truth for facts that already live on the task row, and the two would drift
/// the first time somebody edited `bg_writes_allowed` without re-deriving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequirement {
    pub tier: ToolTier,
    pub executor: BackgroundExecutor,
    /// The repo this task is bound to, if any. A `None` here is not yet
    /// legal — [`crate::dispatch::queue_for_background`] refuses it — and is
    /// the seam Phase 4's detached runs open.
    pub repo_path: Option<String>,
    /// Whether a run at this tier has to be snapshotted first (invariant 7).
    pub needs_checkpoint: bool,
}

impl RunRequirement {
    pub fn for_task(task: &Task) -> Self {
        let tier = ToolTier::for_task(task);
        Self {
            tier,
            executor: task.bg_executor,
            repo_path: task
                .repo_path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            needs_checkpoint: tier.needs_checkpoint(),
        }
    }
}

/// What a host says it can do, and what it last reported about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCapability {
    // ── declared: policy, set by Chris, never probed ────────────────────────
    pub id: String,
    /// What to call it in a sentence. "the NAS", not a uuid — every refusal
    /// this module produces gets read by a human deciding what to fix.
    pub label: String,
    pub max_tier: ToolTier,
    pub executors: Vec<BackgroundExecutor>,
    pub workspaces: Vec<WorkspaceKind>,
    pub can_checkpoint: bool,
    pub max_concurrent: i64,
    /// Percent of the global daily budget this host must leave unspent.
    ///
    /// A **ceiling**, never a second budget. 50 means "stop admitting once the
    /// pool is half gone", which reserves headroom for the machine Chris is
    /// sitting at without ever creating any. Splitting the budget per host is
    /// what [`crate::runner::gate`]'s own doc forbids: two budgets that each
    /// look fine while together doubling the burn.
    pub headroom_pct: i64,
    /// Tie-break among hosts of equal workspace freshness. Lower wins.
    pub priority: i64,
    pub enabled: bool,

    // ── observed: what the host last said about itself ──────────────────────
    pub last_seen_at: Option<String>,
    pub paused: bool,
    /// The host's last [`crate::runner::Gate`], as its debug string. Advisory:
    /// a closed gate warns, it never refuses (see [`satisfies`]).
    pub gate: Option<String>,
    /// Which executors this host actually found installed and logged in.
    /// Intersected with `executors` to give effective capability.
    pub executors_ready: Vec<BackgroundExecutor>,
}

impl HostCapability {
    /// This machine, as it has always behaved: everything, locally.
    ///
    /// The default a Mac registers itself with on first run, and the input to
    /// `queue_sql_for_the_local_host_is_byte_identical_to_the_pre_v23_filter`.
    /// Nothing about the single-host case may change as a side effect of this
    /// module existing, and that test is how it stays true.
    pub fn local_default(id: String, label: String) -> Self {
        Self {
            id,
            label,
            max_tier: ToolTier::Write,
            executors: BackgroundExecutor::ALL.to_vec(),
            workspaces: vec![WorkspaceKind::Live],
            can_checkpoint: true,
            max_concurrent: 0, // 0 = defer to the global setting
            headroom_pct: 0,
            priority: 0,
            enabled: true,
            last_seen_at: None,
            paused: false,
            gate: None,
            executors_ready: BackgroundExecutor::ALL.to_vec(),
        }
    }

    /// What THIS process declares itself to be, before the registry is
    /// consulted.
    ///
    /// A Mac declares the local default and needs no configuration. A container
    /// declares itself entirely from its deployment, through `CXTASKS_HOST_*`,
    /// so standing up a second host is a compose file rather than a compose
    /// file plus a hand edit in the app afterwards. Either way this is only
    /// ever used to INSERT a row that does not exist — see
    /// [`crate::db::hosts::register_self`] for why a restart must never
    /// re-assert it.
    ///
    /// `can_checkpoint` is DERIVED, not declared, and that is load-bearing: a
    /// mirror host's tree is a throwaway clone, so a snapshot of it protects
    /// nothing and "restore" would restore something nobody can see. Deriving
    /// it means a mirror host is *structurally* incapable of write-tier work —
    /// [`satisfies`] refuses with [`Unmet::CannotCheckpoint`] — rather than
    /// relying on whoever writes the compose file to also set the tier
    /// correctly.
    pub fn declared_for_this_host() -> Self {
        let env = |k: &str| {
            std::env::var(k)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let id = crate::host::host_id();
        let label = env("CXTASKS_HOST_LABEL").unwrap_or_else(|| id.chars().take(8).collect());
        let mut cap = Self::local_default(id, label);

        if let Some(raw) = env("CXTASKS_HOST_WORKSPACES") {
            cap.workspaces = raw
                .split(',')
                .filter_map(|w| WorkspaceKind::parse(w).ok())
                .collect();
        }
        if let Some(raw) = env("CXTASKS_HOST_MAX_TIER") {
            if let Ok(t) = ToolTier::parse(&raw) {
                cap.max_tier = t;
            }
        }
        if let Some(raw) = env("CXTASKS_HOST_EXECUTORS") {
            cap.executors = raw
                .split(',')
                .filter_map(|e| BackgroundExecutor::parse(e).ok())
                .collect();
        }
        let num = |k: &str| env(k).and_then(|v| v.parse::<i64>().ok());
        if let Some(v) = num("CXTASKS_HOST_MAX_CONCURRENT") {
            cap.max_concurrent = v;
        }
        if let Some(v) = num("CXTASKS_HOST_HEADROOM_PCT") {
            cap.headroom_pct = v;
        }
        if let Some(v) = num("CXTASKS_HOST_PRIORITY") {
            cap.priority = v;
        }

        // Derived last, so it reflects whatever the workspace declaration
        // ended up being rather than what it started as.
        cap.can_checkpoint = cap.is_local();
        // Observed starts empty and is filled by the first heartbeat. Declaring
        // an executor READY before looking would defeat the whole intersection.
        cap.executors_ready = Vec::new();
        cap
    }

    /// Does this host hold the working trees itself?
    ///
    /// Derived rather than stored: "has a `Live` workspace" and "is the machine
    /// the repos are on" are the same fact, and storing it twice is how they
    /// come to disagree.
    pub fn is_local(&self) -> bool {
        self.workspaces.contains(&WorkspaceKind::Live)
    }

    /// The opt-in column this host's drain query tests.
    ///
    /// The skew guard, and the same trick as `bg_codex_allowed`: a task that
    /// may only run remotely is invisible to every binary that predates the
    /// column, because they all test `bg_allowed`.
    pub fn enabled_column(&self, executor: BackgroundExecutor) -> &'static str {
        if self.is_local() {
            executor.enabled_column()
        } else {
            "bg_remote_allowed"
        }
    }

    /// Declared ∩ observed — but only once there IS an observation.
    ///
    /// # Absence of an observation is not a negative observation
    ///
    /// A host that has never heartbeated has an empty `executors_ready`, and
    /// intersecting against it unconditionally means a freshly-installed
    /// machine can run nothing until its runner has ticked once. The app would
    /// refuse to queue on a new install, naming an executor that is in fact
    /// installed, and the only cure would be waiting for a process the user may
    /// not have started yet.
    ///
    /// So an unobserved host is taken at its word. It is the same rule
    /// [`Self::is_alive`] applies to `last_seen_at`, and it is safe for the
    /// same reason: the optimism lasts exactly until the first heartbeat, which
    /// is also the first moment anybody actually looked. If Claude really is
    /// missing there, the run fails preflight on that host and the task is
    /// parked with the real reason — one wasted dispatch, not a permanent
    /// refusal.
    pub fn effective_executors(&self) -> Vec<BackgroundExecutor> {
        if self.last_seen_at.is_none() {
            return self.executors.clone();
        }
        self.executors
            .iter()
            .copied()
            .filter(|e| self.executors_ready.contains(e))
            .collect()
    }

    /// Has this host reported in recently enough to be worth waiting for?
    ///
    /// A host that has never reported (`last_seen_at IS NULL`) is treated as
    /// alive, not dead: it is either brand new or predates heartbeating, and
    /// refusing a queue because a freshly-registered host has not ticked yet
    /// would make registering a host break queueing. A stamp that will not
    /// parse reads as alive for the same reason — a malformed timestamp is a
    /// bug in whoever wrote it, and it must not silently take a host out of
    /// rotation.
    pub fn is_alive(&self, stale_after_minutes: i64) -> bool {
        let Some(seen) = self.last_seen_at.as_deref() else {
            return true;
        };
        let Some(at) = parse_stamp(seen) else {
            return true;
        };
        chrono::Utc::now().signed_duration_since(at).num_minutes() < stale_after_minutes
    }
}

/// Read either timestamp format this database contains.
///
/// **Two formats coexist in one DB** and always have: Rust writes ISO-8601 UTC
/// with a trailing `Z` for instants the user set, and SQLite writes
/// `datetime('now')` — space-separated, no zone — for bookkeeping.
/// `last_seen_at` is bookkeeping, so it is the second form, but a host row
/// arriving over sync was serialized by Rust and may be the first. Accepting
/// only one and silently failing the other is how a host would look dead on one
/// machine and alive on another.
pub fn parse_stamp(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let raw = raw.trim();
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(d.with_timezone(&chrono::Utc));
    }
    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(n) = chrono::NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(n.and_utc());
        }
    }
    None
}

/// Why a host cannot run a task.
///
/// **Legibility is the requirement**, so each variant carries what the reader
/// needs to fix it. A queue refused with "no host can run this" and nothing
/// else is a dead end; refused with "the NAS is read-only and this task asks
/// for a shell" is an instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unmet {
    Disabled,
    TierExceeded { max: ToolTier, needs: ToolTier },
    ExecutorNotDeclared { executor: BackgroundExecutor },
    ExecutorNotReady { executor: BackgroundExecutor },
    NoWorkspaceKind,
    /// The host could run it, but has no mirror of this repo yet.
    NoMirror { repo: String },
    CannotCheckpoint,
}

impl Unmet {
    /// How much this refusal tells the reader, high is better.
    ///
    /// Used only to choose between refusals from different workspace kinds of
    /// ONE host — see [`satisfies`]. A refusal that names a repo beats one that
    /// says "nothing fits", because the first is an instruction.
    fn specificity(&self) -> u8 {
        match self {
            Self::NoWorkspaceKind => 0,
            Self::NoMirror { .. } => 1,
            Self::CannotCheckpoint => 2,
            Self::ExecutorNotReady { .. } => 3,
            Self::ExecutorNotDeclared { .. } => 3,
            Self::TierExceeded { .. } => 4,
            Self::Disabled => 5,
        }
    }
}

impl std::fmt::Display for Unmet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(f, "is switched off"),
            Self::TierExceeded { max, needs } => write!(
                f,
                "is capped at {max} and this task asks for {needs}",
                max = max.as_str(),
                needs = needs.as_str()
            ),
            Self::ExecutorNotDeclared { executor } => {
                write!(f, "is not set up to run {}", executor.label())
            }
            Self::ExecutorNotReady { executor } => write!(
                f,
                "has {} declared but last reported it missing or logged out",
                executor.label()
            ),
            Self::NoWorkspaceKind => write!(f, "has no workspace kind that fits this task"),
            Self::NoMirror { repo } => {
                write!(f, "has no mirror of {repo} yet")
            }
            Self::CannotCheckpoint => write!(
                f,
                "cannot take a checkpoint, and this task may write to the tree"
            ),
        }
    }
}

/// Can this host run this task, and if so with what workspace?
///
/// Pure, so the interesting cases are all reachable from a unit test —
/// which matters more here than almost anywhere else in the crate, because the
/// failure mode of getting it wrong is an unattended agent with more reach than
/// it was granted.
///
/// `mirrors` is the set of repo paths this host holds a mirror for. Empty for
/// a local host, where it is never consulted.
pub fn satisfies(
    cap: &HostCapability,
    req: &RunRequirement,
    mirrors: &[String],
) -> Result<WorkspaceKind, Unmet> {
    if !cap.enabled {
        return Err(Unmet::Disabled);
    }
    if req.tier.rank() > cap.max_tier.rank() {
        return Err(Unmet::TierExceeded {
            max: cap.max_tier,
            needs: req.tier,
        });
    }
    if !cap.executors.contains(&req.executor) {
        return Err(Unmet::ExecutorNotDeclared {
            executor: req.executor,
        });
    }
    if !cap.effective_executors().contains(&req.executor) {
        return Err(Unmet::ExecutorNotReady {
            executor: req.executor,
        });
    }
    if req.needs_checkpoint && !cap.can_checkpoint {
        return Err(Unmet::CannotCheckpoint);
    }

    // Freshest workspace this host declares that also fits the task, so a host
    // declaring both `Live` and `Mirror` uses the live tree. `Live` beating
    // `Mirror` here rather than at the call site is what makes the preference
    // a property of the type instead of a rule every caller has to remember.
    let mut kinds: Vec<WorkspaceKind> = cap.workspaces.clone();
    kinds.sort_by_key(|k| k.freshness_rank());

    // Keep the MOST SPECIFIC refusal, not the last one tried.
    //
    // A host declaring both `Mirror` and `Detached`, asked about a repo-bound
    // task it has no mirror for, produces `NoMirror` and then
    // `NoWorkspaceKind` — and taking the last would report "has no workspace
    // kind that fits", which is a dead end, in place of "has no mirror of
    // /repo yet", which names the thing to fix. Legibility is the requirement
    // here, so the ordering is explicit rather than incidental.
    let mut best: Option<Unmet> = None;
    for kind in kinds {
        match workspace_fits(kind, req, mirrors) {
            Ok(()) => return Ok(kind),
            Err(e) => {
                if best.as_ref().map(|b| e.specificity() > b.specificity()).unwrap_or(true) {
                    best = Some(e);
                }
            }
        }
    }
    Err(best.unwrap_or(Unmet::NoWorkspaceKind))
}

/// Whether one workspace kind can serve one requirement.
fn workspace_fits(
    kind: WorkspaceKind,
    req: &RunRequirement,
    mirrors: &[String],
) -> Result<(), Unmet> {
    match kind {
        // Invariant 12, unchanged: a background run is refused outside a
        // bounded workspace, and for a live host a repo is the only kind there
        // is.
        WorkspaceKind::Live => match req.repo_path.as_deref() {
            Some(_) => Ok(()),
            None => Err(Unmet::NoWorkspaceKind),
        },
        WorkspaceKind::Mirror => match req.repo_path.as_deref() {
            Some(repo) => {
                if mirrors.iter().any(|m| m == repo) {
                    Ok(())
                } else {
                    Err(Unmet::NoMirror { repo: repo.into() })
                }
            }
            None => Err(Unmet::NoWorkspaceKind),
        },
        // The exact complement of the other two, and deliberately so: a task
        // WITH a repo must never land detached, because it would then run
        // against an empty scratch directory and report, confidently, that the
        // code it was asked about does not exist.
        WorkspaceKind::Detached => match req.repo_path.as_deref() {
            None => Ok(()),
            Some(_) => Err(Unmet::NoWorkspaceKind),
        },
    }
}

/// The drain predicate a host with this capability may use.
///
/// Composed by [`crate::db::tasks::list_queued_for_background`] with `LIVE`,
/// the status test and the blocked-task exclusion — everything here is about
/// *capability*, so that the one thing this function can get wrong is the one
/// thing it is reviewed for.
///
/// # Why generated rather than written
///
/// Same argument as [`BackgroundExecutor::enabled_sql`], one level up. A
/// hand-written remote drain query is correct on the day it ships and wrong the
/// first time a tier is added, because nothing forces the author of the tier to
/// find it. Generated from the capability, a read-only host's query says
/// `bg_writes_allowed = 0 AND bg_shell_allowed = 0` forever, and a task shape
/// invented next year that sets some *new* flag is still refused — it simply
/// fails to match a tier the host declares.
///
/// [`BackgroundExecutor::enabled_sql`]: crate::executor::BackgroundExecutor::enabled_sql
pub fn queue_sql(cap: &HostCapability) -> String {
    let mut clauses: Vec<String> = Vec::new();

    // ── opt-in, per executor, in the column this host is entitled to read ───
    let enabled: Vec<String> = cap
        .effective_executors()
        .iter()
        .map(|e| {
            format!(
                "(bg_executor = '{}' AND {} = 1)",
                e.as_str(),
                cap.enabled_column(*e)
            )
        })
        .collect();
    clauses.push(if enabled.is_empty() {
        // A host with no usable executor drains nothing. Not an error: it is
        // exactly what a NAS whose Claude login expired should do, and it must
        // be expressible in SQL rather than as a special case above it.
        "0 = 1".to_string()
    } else {
        format!("({})", enabled.join(" OR "))
    });

    // ── workspace ──────────────────────────────────────────────────────────
    let mut workspace: Vec<String> = cap
        .workspaces
        .iter()
        .map(|k| workspace_sql(*k).to_string())
        .collect();
    workspace.dedup();
    if workspace.is_empty() {
        clauses.push("0 = 1".to_string());
    } else if workspace.len() == 1 {
        clauses.push(workspace.remove(0));
    } else {
        clauses.push(format!(
            "({})",
            workspace
                .iter()
                .map(|w| format!("({w})"))
                .collect::<Vec<_>>()
                .join(" OR ")
        ));
    }

    // ── tier ceiling, and ONLY when it constrains ──────────────────────────
    //
    // A `Write` host adds nothing, which is what keeps the local host's query
    // byte-identical to the one that predates this module.
    if let Some(tier) = tier_sql(cap.max_tier) {
        clauses.push(tier);
    }

    clauses.join(" AND ")
}

/// The task shapes one workspace kind can serve.
///
/// `Live` and `Mirror` agree today — both need a repo — and the duplication is
/// deliberate rather than collapsed: they will stop agreeing the moment a kind
/// exists that does not want one, and a shared constant would have to be split
/// at exactly the moment somebody is least likely to notice it should be.
fn workspace_sql(kind: WorkspaceKind) -> &'static str {
    match kind {
        WorkspaceKind::Live => "repo_path IS NOT NULL AND TRIM(repo_path) <> ''",
        WorkspaceKind::Mirror => "repo_path IS NOT NULL AND TRIM(repo_path) <> ''",
        // **The fail-closed encoding is free.** Every drain query that predates
        // Phase 4 — including this one for any host not declaring `Detached` —
        // already carries `repo_path IS NOT NULL`, so a detached task is
        // invisible to a leftover runner with NO new column. The existing guard
        // is the skew guard, which is worth saying out loud because the next
        // reader will go looking for a third flag and not find one.
        WorkspaceKind::Detached => "repo_path IS NULL OR TRIM(repo_path) = ''",
    }
}

/// SQL that is true only for tasks at or below a ceiling — `None` when the
/// ceiling is the top tier and therefore constrains nothing.
fn tier_sql(max: ToolTier) -> Option<String> {
    match max {
        ToolTier::Write => None,
        ToolTier::Shell => Some("bg_writes_allowed = 0".to_string()),
        ToolTier::ReadOnly => {
            Some("bg_writes_allowed = 0 AND bg_shell_allowed = 0".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nas() -> HostCapability {
        HostCapability {
            id: "nas".into(),
            label: "the NAS".into(),
            max_tier: ToolTier::ReadOnly,
            executors: vec![BackgroundExecutor::Claude],
            workspaces: vec![WorkspaceKind::Mirror],
            can_checkpoint: false,
            max_concurrent: 1,
            headroom_pct: 50,
            priority: 10,
            enabled: true,
            last_seen_at: None,
            paused: false,
            gate: None,
            executors_ready: vec![BackgroundExecutor::Claude],
        }
    }

    fn mac() -> HostCapability {
        HostCapability::local_default("mac".into(), "this Mac".into())
    }

    fn req(tier: ToolTier, repo: Option<&str>) -> RunRequirement {
        RunRequirement {
            tier,
            executor: BackgroundExecutor::Claude,
            repo_path: repo.map(str::to_string),
            needs_checkpoint: tier.needs_checkpoint(),
        }
    }

    /// **The test that protects the single-host case.**
    ///
    /// A blunt string comparison, and exactly right: it makes any future edit
    /// to the Mac's drain query a deliberate act rather than a side effect of
    /// editing this module. The clause it reproduces is the one that shipped
    /// before `hosts` existed, and if this fails the question is not "update
    /// the expectation" but "why did the local host's behaviour change".
    #[test]
    fn queue_sql_for_the_local_host_is_byte_identical_to_the_pre_v23_filter() {
        let pre_v23 = format!(
            "{} AND repo_path IS NOT NULL AND TRIM(repo_path) <> ''",
            BackgroundExecutor::enabled_sql()
        );
        assert_eq!(queue_sql(&mac()), pre_v23);
    }

    /// The generated half of failing closed: a read-only host's own SQL denies
    /// the two flags it is not allowed to honour, so it cannot claim a
    /// write-tier task even if one is somehow marked remote-eligible.
    #[test]
    fn a_read_only_host_denies_write_and_shell_in_its_own_drain_sql() {
        let sql = queue_sql(&nas());
        assert!(sql.contains("bg_writes_allowed = 0"), "{sql}");
        assert!(sql.contains("bg_shell_allowed = 0"), "{sql}");
        // And it reads the remote column, not the legacy one.
        assert!(sql.contains("bg_remote_allowed = 1"), "{sql}");
        assert!(
            !sql.contains("bg_allowed = 1"),
            "a remote host must never test the column a pre-v23 runner owns: {sql}"
        );
    }

    /// The skew guard, stated as the property rather than the mechanism: a
    /// task marked remote-only is invisible to the query the local host runs.
    #[test]
    fn the_local_hosts_query_cannot_see_the_remote_opt_in_column() {
        assert!(!queue_sql(&mac()).contains("bg_remote_allowed"));
    }

    #[test]
    fn a_host_that_looked_and_found_no_executor_drains_nothing() {
        let mut h = nas();
        h.executors_ready.clear();
        h.last_seen_at = Some(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
        assert!(queue_sql(&h).contains("0 = 1"));
    }

    /// **The fresh-install case.** A host that has never heartbeated is taken
    /// at its word, or the app refuses to queue on a new machine until a
    /// process the user may not have started yet has ticked once.
    #[test]
    fn a_host_that_has_never_looked_is_taken_at_its_word() {
        let mut h = nas();
        h.executors_ready.clear();
        assert!(h.last_seen_at.is_none());
        assert_eq!(h.effective_executors(), vec![BackgroundExecutor::Claude]);
        assert!(satisfies(&h, &req(ToolTier::ReadOnly, Some("/repo")), &["/repo".into()]).is_ok());
    }

    #[test]
    fn a_read_only_host_refuses_a_write_task_naming_both_tiers() {
        let mirrors = vec!["/repo".to_string()];
        let err = satisfies(&nas(), &req(ToolTier::Write, Some("/repo")), &mirrors).unwrap_err();
        assert_eq!(
            err,
            Unmet::TierExceeded {
                max: ToolTier::ReadOnly,
                needs: ToolTier::Write
            }
        );
        // The message is the product here — it is what the user reads when a
        // queue is refused, and "no host available" would be a dead end.
        let msg = err.to_string();
        assert!(msg.contains("read-only"), "{msg}");
        assert!(msg.contains("write"), "{msg}");
    }

    #[test]
    fn a_mirror_host_refuses_a_repo_it_has_not_mirrored_yet() {
        let err = satisfies(&nas(), &req(ToolTier::ReadOnly, Some("/repo")), &[]).unwrap_err();
        assert_eq!(
            err,
            Unmet::NoMirror {
                repo: "/repo".into()
            }
        );
    }

    #[test]
    fn a_mirror_host_accepts_a_read_only_task_it_has_a_mirror_for() {
        let mirrors = vec!["/repo".to_string()];
        assert_eq!(
            satisfies(&nas(), &req(ToolTier::ReadOnly, Some("/repo")), &mirrors).unwrap(),
            WorkspaceKind::Mirror
        );
    }

    /// Freshness dominates. A host that has both must use the live tree, or a
    /// local run silently starts answering questions about the past.
    #[test]
    fn live_beats_mirror_when_a_host_declares_both() {
        let mut h = mac();
        h.workspaces = vec![WorkspaceKind::Mirror, WorkspaceKind::Live];
        assert_eq!(
            satisfies(&h, &req(ToolTier::ReadOnly, Some("/repo")), &[]).unwrap(),
            WorkspaceKind::Live,
            "declaring a mirror must not demote a host that holds the real tree"
        );
    }

    /// Declared ∩ observed. A NAS whose Claude login expired is not a NAS that
    /// can run Claude tasks, however it is configured.
    #[test]
    fn a_declared_executor_that_is_not_ready_refuses_and_says_which_half_failed() {
        let mut h = nas();
        h.executors_ready.clear();
        // A heartbeat is what makes an empty ready-list mean "I looked and it
        // is not there" rather than "nobody has looked yet".
        h.last_seen_at = Some(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
        let err = satisfies(&h, &req(ToolTier::ReadOnly, Some("/repo")), &[]).unwrap_err();
        assert_eq!(
            err,
            Unmet::ExecutorNotReady {
                executor: BackgroundExecutor::Claude
            }
        );
        assert!(err.to_string().contains("logged out"), "{err}");
    }

    #[test]
    fn a_host_that_cannot_checkpoint_refuses_anything_that_may_write() {
        let mut h = mac();
        h.can_checkpoint = false;
        let err = satisfies(&h, &req(ToolTier::Write, Some("/repo")), &[]).unwrap_err();
        assert_eq!(err, Unmet::CannotCheckpoint);
        // ...but a read-only task is still fine, because it needs no snapshot.
        assert!(satisfies(&h, &req(ToolTier::ReadOnly, Some("/repo")), &[]).is_ok());
    }

    #[test]
    fn a_disabled_host_refuses_everything_before_looking_at_anything_else() {
        let mut h = mac();
        h.enabled = false;
        assert_eq!(
            satisfies(&h, &req(ToolTier::ReadOnly, Some("/repo")), &[]).unwrap_err(),
            Unmet::Disabled
        );
    }

    /// A host that has never checked in is new, not dead. Refusing here would
    /// mean registering a host breaks queueing until its first tick.
    #[test]
    fn a_host_that_has_never_reported_counts_as_alive() {
        assert!(mac().is_alive(10));
    }

    #[test]
    fn a_host_that_reported_long_ago_counts_as_dead() {
        let mut h = mac();
        h.last_seen_at = Some(
            (chrono::Utc::now() - chrono::Duration::hours(3))
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
        );
        assert!(!h.is_alive(10));
        h.last_seen_at = Some(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
        assert!(h.is_alive(10));
    }

    #[test]
    fn workspace_kind_round_trips_and_rejects_junk() {
        for k in WorkspaceKind::ALL {
            assert_eq!(WorkspaceKind::parse(k.as_str()).unwrap(), k);
        }
        assert!(WorkspaceKind::parse("ephemeral").is_err());
        assert!(WorkspaceKind::parse("").is_err());
    }

    // ── detached ───────────────────────────────────────────────────────────

    fn container() -> HostCapability {
        let mut c = nas();
        c.workspaces = vec![WorkspaceKind::Mirror, WorkspaceKind::Detached];
        c
    }

    /// **The reason the MacBook does not declare it.** A detached run gets
    /// unsupervised read over whatever its workspace can reach, and on the Mac
    /// that would be every client repo, ~/.ssh and the notes vault.
    #[test]
    fn the_local_host_refuses_a_detached_task() {
        let req = RunRequirement {
            tier: ToolTier::ReadOnly,
            executor: BackgroundExecutor::Claude,
            repo_path: None,
            needs_checkpoint: false,
        };
        assert_eq!(
            satisfies(&mac(), &req, &[]).unwrap_err(),
            Unmet::NoWorkspaceKind
        );
        assert_eq!(satisfies(&container(), &req, &[]).unwrap(), WorkspaceKind::Detached);
    }

    /// **The refusal survived repo-less runs arriving on the Mac.**
    ///
    /// `tripwire_on_trip = 'interpret'` means a repo-less task on this Mac now
    /// produces a real `claude -p`, which is exactly what this module spent
    /// Phase 4 refusing. It is still refused *here*, and that is the point: an
    /// interpretation does not ask `satisfies` anything, because it holds no
    /// tools and therefore has no workspace to be given (`ToolTier::
    /// denied_for_interpret` names `Read`/`Glob`/`Grep`). What must never change
    /// is that ordinary WORK with no repo keeps getting `NoWorkspaceKind` from
    /// the Mac, at every tier — if that ever starts succeeding, an unattended
    /// agent gets `$HOME`.
    #[test]
    fn the_local_host_still_refuses_repo_less_WORK_at_every_tier() {
        for tier in [ToolTier::ReadOnly, ToolTier::Shell, ToolTier::Write] {
            let r = req(tier, None);
            let err = satisfies(&mac(), &r, &[]).unwrap_err();
            assert!(
                matches!(err, Unmet::NoWorkspaceKind | Unmet::CannotCheckpoint),
                "a repo-less {tier:?} run was admitted on the Mac: {err:?}"
            );
        }
    }

    /// The skew guard, for the shape that did not exist before: a repo-less
    /// task is invisible to the drain query of **every** host that does not
    /// declare `Detached` — including this Mac, and including a leftover runner
    /// that has never heard of an on-trip policy. Nothing was added to make
    /// that true; the pre-Phase-4 `repo_path IS NOT NULL` is still the guard,
    /// which is why no new flag exists and the next reader should not add one.
    #[test]
    fn a_repo_less_task_stays_invisible_to_the_local_drain_however_its_tripwire_is_set() {
        let sql = queue_sql(&mac());
        assert!(sql.contains("repo_path IS NOT NULL"), "{sql}");
        assert!(!sql.contains("repo_path IS NULL"), "{sql}");
        // And the drain predicate knows nothing about tripwires at all — a
        // policy a leftover binary cannot read can therefore not change what it
        // claims.
        assert!(!sql.contains("tripwire"), "{sql}");
        assert!(!sql.contains("interpret"), "{sql}");
    }

    /// The complement, and it matters as much: a task WITH a repo must never
    /// land detached, or it runs against an empty scratch directory and
    /// reports — confidently — that the code does not exist.
    #[test]
    fn a_task_with_a_repo_never_lands_in_a_detached_workspace() {
        let mirrors = vec!["/repo".to_string()];
        assert_eq!(
            satisfies(&container(), &req(ToolTier::ReadOnly, Some("/repo")), &mirrors).unwrap(),
            WorkspaceKind::Mirror,
            "a repo-bound task took the detached branch"
        );
        // ...and with no mirror it refuses rather than falling through to
        // detached, which would look like success.
        assert_eq!(
            satisfies(&container(), &req(ToolTier::ReadOnly, Some("/repo")), &[]).unwrap_err(),
            Unmet::NoMirror { repo: "/repo".into() }
        );
    }

    /// The fail-closed encoding, in the SQL rather than in prose: a host that
    /// does not declare `Detached` generates a query carrying
    /// `repo_path IS NOT NULL`, so a detached task is invisible to it — and to
    /// every runner that predates the whole idea.
    #[test]
    fn a_host_without_a_detached_workspace_cannot_express_the_claim() {
        let sql = queue_sql(&mac());
        assert!(sql.contains("repo_path IS NOT NULL"), "{sql}");
        assert!(!sql.contains("repo_path IS NULL"), "{sql}");

        // The container's query admits BOTH shapes, because it declares both.
        let sql = queue_sql(&container());
        assert!(sql.contains("repo_path IS NOT NULL"), "{sql}");
        assert!(sql.contains("repo_path IS NULL"), "{sql}");
    }
}
