//! Tripwires — a task that waits for a CONDITION rather than a date.
//!
//! `defer_until` hides a task until a moment on the clock. A tripwire hides it
//! until a check comes back true. While armed the task is hidden exactly where
//! a deferred one is hidden — Open, Inbox, Today, `next_task`, the background
//! queue — and shows in Upcoming with a badge. When the check trips, the task
//! surfaces with the evidence, a banner is posted, and (opt-in, per task) it
//! completes itself so `blocked_by` releases whatever waits on it.
//!
//! # What lives here
//!
//! The VOCABULARY, and nothing that touches a database or a process:
//!
//! * the spec grammar (`script:<name> [arg]`, `jev:<condition>`,
//!   `agent:<condition>`) and its round-tripping [`Tripwire`];
//! * the three stable states and the two on-trip policies;
//! * the cadence grammar (`manual | 5m | 6h | 7d | <minutes>`) and its floors;
//! * the pure scheduling arithmetic — [`is_due`], [`backoff_minutes`],
//!   [`failure_threshold`];
//! * the ONE filesystem question, [`script_path`], which turns a name into an
//!   executable this machine is willing to run unattended.
//!
//! The storage half is `db::tripwires` (the leased attempt), and the process
//! half is `runner::tripwire` (spawning the script). Keeping the arithmetic
//! here means it is tested as arithmetic — a backoff table, not a runner
//! harness.
//!
//! # Scripts are NAMES plus one hostile argument
//!
//! A task row never carries an executable or a shell command. It carries the
//! name of a file under [`crate::paths::tripwires_dir`] and, optionally, one
//! argv value passed as `$1` through `Command::arg`, never through a shell.
//! The directory is trusted because the user put things in it; the argument is
//! not, because it is a column on a synced row and could be written by any
//! client. So the script is told, in its contract, to treat `$1` as data.
//!
//! # Agent checks are BOUNDED, and the bounds live here
//!
//! `agent:<condition>` (Release 2) is evaluated by a background run — a real
//! `claude -p` with read-only tools — so unlike a script it costs tokens and
//! a concurrency slot. Three constants pin what one check may spend
//! ([`CHECK_MAX_TURNS`], [`CHECK_TOKEN_CEILING`], [`CHECK_TIMEOUT_MINUTES`])
//! and [`min_nonzero`] is how they compose with a task's own overrides: a
//! task's `bg_run_token_cap = 0` means "no ceiling" for its WORK and must never
//! mean that for its CHECK. The cadence floor ([`AGENT_MIN_MINUTES`]) and the
//! per-day sub-cap (`app_meta['bg_tripwire_daily_token_cap']`) bound the
//! other axis — how often — and [`cadence_floor_message`] says the arithmetic
//! out loud rather than just "too low".
//!
//! The DB half is unchanged: a check is the same leased, CAS-recorded attempt
//! a script is, with the RUN ID as the lease id. The process half is
//! `runner::tripwire::agent_pass`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

/// A script tripwire may be re-checked this often at most.
pub const SCRIPT_MIN_MINUTES: i64 = 1;
/// An agent tripwire may be re-checked this often at most. Each check is a
/// background run — see [`cadence_floor_message`] for the arithmetic.
pub const AGENT_MIN_MINUTES: i64 = 60;
/// Jev checks are a single small structured request. Five minutes keeps an
/// accidentally aggressive cadence bounded while still supporting monitors.
pub const JEV_MIN_MINUTES: i64 = 5;
/// One year. A longer cadence is a typo.
pub const MAX_MINUTES: i64 = 525_600;
/// Backoff after consecutive failures never waits longer than this.
pub const BACKOFF_CAP_MINUTES: i64 = 1440;
/// A script that is still running after this is killed and recorded as an
/// error.
pub const SCRIPT_TIMEOUT_SECS: u64 = 60;
/// How long a script attempt's lease lives. Longer than the timeout by enough
/// that a healthy attempt always records before its lease can be reclaimed.
pub const SCRIPT_LEASE_MINUTES: i64 = 5;
/// At most this many scripts are claimed per runner tick.
pub const SCRIPTS_PER_TICK: usize = 10;
/// `last_note` and the comment body are bounded to this many characters.
pub const NOTE_CHARS: usize = 500;
/// Bytes kept from each of stdout and stderr. The rest is drained and dropped.
pub const CAPTURE_BYTES: usize = 4096;
/// The one argv value a script tripwire may carry.
pub const ARG_MAX_CHARS: usize = 256;
/// A condition is prose, but bounded: it is written into a prompt and a row.
pub const CONDITION_MAX_CHARS: usize = 1000;

// ── agent checks: what ONE check may spend ────────────────────────────────

/// Turn ceiling for one agent check. A check that needs more turns than this
/// is not a check; the floor of ~100k tokens a run already assumes a handful
/// of tool calls, and twelve is generous for "look at the thing and say".
pub const CHECK_MAX_TURNS: i64 = 12;
/// Token ceiling for one agent check. Reserved in full at admission so the
/// daily sub-cap cannot be overshot by a check already in flight.
pub const CHECK_TOKEN_CEILING: i64 = 3_000_000;
/// Wall clock for one agent check, in minutes.
pub const CHECK_TIMEOUT_MINUTES: i64 = 10;
// ── the interpretation run: what ONE may spend ────────────────────────────

/// Turn ceiling for an interpretation. It has NO tools — not even `Read` — so
/// it cannot do anything on a second turn but keep talking. Two, so a model
/// that opens with a preamble still gets to finish.
pub const INTERPRET_MAX_TURNS: i64 = 2;
/// Token ceiling for one interpretation. A single tool-less turn measures
/// ~46k (invariant 6's floor), almost all of it the reloaded `CLAUDE.md`, so a
/// million is a runaway guard rather than a budget.
pub const INTERPRET_TOKEN_CEILING: i64 = 1_000_000;
/// Wall clock for one interpretation, in minutes. Short because there is
/// nothing to wait for: no tool call can be in flight.
pub const INTERPRET_TIMEOUT_MINUTES: i64 = 5;

/// The lease a check attempt holds: its wall clock plus the same grace the
/// abandoned-run sweep allows, so a healthy check always records before its
/// lease can be reclaimed — and a crashed one is reclaimed soon after the
/// sweep would have judged its run row dead.
pub fn check_lease_minutes(timeout_minutes: i64) -> i64 {
    timeout_minutes + crate::db::runs::ABANDON_GRACE_MINUTES
}

/// The smaller of a configured bound and a constant, where `0` on the
/// configured side means "unbounded" — and is therefore NOT the answer.
///
/// `bg_run_token_cap = 0` disables the per-run ceiling for a task's WORK
/// (invariant 7's per-task override). Composed naively, that zero would also
/// disable the ceiling on the task's CHECK, which is the one run that must
/// always be bounded because nobody asked for it today. So a non-positive
/// configured value yields the constant, and a positive one is capped by it.
pub fn min_nonzero(configured: i64, constant: i64) -> i64 {
    if configured <= 0 {
        constant
    } else {
        configured.min(constant)
    }
}

/// The kind of check a spec names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TripwireKind {
    Script,
    Agent,
    Jev,
}

impl TripwireKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Script => "script",
            Self::Agent => "agent",
            Self::Jev => "jev",
        }
    }

    /// The shortest cadence this kind may be given. A script costs nothing; an
    /// agent check is a background run, and the floor is what stops a 5-minute
    /// cadence quietly spending ~29M tokens a day on one task.
    pub fn min_interval_minutes(self) -> i64 {
        match self {
            Self::Script => SCRIPT_MIN_MINUTES,
            Self::Agent => AGENT_MIN_MINUTES,
            Self::Jev => JEV_MIN_MINUTES,
        }
    }
}

/// A parsed `tasks.tripwire` spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tripwire {
    /// `script:<name>[ <arg>]` — an executable under the tripwires directory,
    /// handed `arg` as `$1`.
    Script { name: String, arg: Option<String> },
    /// `agent:<condition>` — a bounded, read-only background run that answers
    /// whether the condition holds. See the module docs for the bounds.
    Agent { condition: String },
    /// `jev:<condition>` — a fast typed judgment over the task and its recent
    /// evidence. It cannot fetch evidence; use a script or agent for that.
    Jev { condition: String },
}

impl Tripwire {
    /// Strict parse of the stored spec.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("a tripwire spec must not be empty".into());
        }
        if let Some(rest) = raw.strip_prefix("script:") {
            let rest = rest.trim();
            let (name, arg) = match rest.split_once(char::is_whitespace) {
                Some((n, a)) => (n.trim(), Some(a.trim())),
                None => (rest, None),
            };
            validate_name(name)?;
            let arg = arg.filter(|a| !a.is_empty()).map(str::to_string);
            if let Some(a) = arg.as_deref() {
                validate_arg(a)?;
            }
            return Ok(Self::Script { name: name.to_string(), arg });
        }
        if let Some(rest) = raw.strip_prefix("agent:") {
            let condition = rest.trim();
            if condition.is_empty() {
                return Err("an agent tripwire needs a condition after 'agent:'".into());
            }
            validate_condition(condition)?;
            return Ok(Self::Agent { condition: condition.to_string() });
        }
        if let Some(rest) = raw.strip_prefix("jev:") {
            let condition = rest.trim();
            if condition.is_empty() {
                return Err("a Jev tripwire needs a condition after 'jev:'".into());
            }
            validate_condition(condition)?;
            return Ok(Self::Jev { condition: condition.to_string() });
        }
        Err(format!(
            "unknown tripwire spec '{raw}': expected 'script:<name> [arg]', 'jev:<condition>', or 'agent:<condition>'"
        ))
    }

    pub fn kind(&self) -> TripwireKind {
        match self {
            Self::Script { .. } => TripwireKind::Script,
            Self::Agent { .. } => TripwireKind::Agent,
            Self::Jev { .. } => TripwireKind::Jev,
        }
    }

    /// The script's name, when this is a script tripwire.
    pub fn script_name(&self) -> Option<&str> {
        match self {
            Self::Script { name, .. } => Some(name),
            Self::Agent { .. } | Self::Jev { .. } => None,
        }
    }

    /// The condition, when this is an agent tripwire.
    pub fn condition(&self) -> Option<&str> {
        match self {
            Self::Agent { condition } => Some(condition),
            Self::Jev { condition } => Some(condition),
            Self::Script { .. } => None,
        }
    }
}

impl std::fmt::Display for Tripwire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Script { name, arg: None } => write!(f, "script:{name}"),
            Self::Script { name, arg: Some(a) } => write!(f, "script:{name} {a}"),
            Self::Agent { condition } => write!(f, "agent:{condition}"),
            Self::Jev { condition } => write!(f, "jev:{condition}"),
        }
    }
}

/// `^[a-z0-9][a-z0-9._-]{0,63}$` — a file name with no directory in it.
///
/// Hand-rolled rather than a regex for the same reason `parse_short_ref` is:
/// this IS the whole grammar. No `/`, no leading `.`, nothing that could be
/// read as a path component, so the name can only ever address a file that
/// sits directly in the tripwires directory.
pub fn validate_name(name: &str) -> Result<(), String> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return Err(format!(
            "tripwire script name '{name}' must be 1-64 characters of [a-z0-9._-] and start with a letter or digit"
        ));
    }
    let ok_first = bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit();
    let ok_rest = bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'));
    if !ok_first || !ok_rest {
        return Err(format!(
            "tripwire script name '{name}' must be 1-64 characters of [a-z0-9._-] and start with a letter or digit"
        ));
    }
    Ok(())
}

/// The one argument: bounded, printable, no control characters. It is DATA
/// to the script, and the script is told so; this only keeps it from being
/// something a log line or a comment cannot carry.
pub fn validate_arg(arg: &str) -> Result<(), String> {
    if arg.chars().count() > ARG_MAX_CHARS {
        return Err(format!(
            "tripwire argument is too long ({} chars; max {ARG_MAX_CHARS})",
            arg.chars().count()
        ));
    }
    if arg.chars().any(char::is_control) {
        return Err("tripwire argument must not contain control characters".into());
    }
    Ok(())
}

/// A condition is prose for a prompt: bounded, and no control characters
/// other than a newline — it is written into `check-prompt.txt` verbatim and
/// echoed in a comment, and a tab or an escape in either is noise at best.
pub fn validate_condition(condition: &str) -> Result<(), String> {
    if condition.chars().count() > CONDITION_MAX_CHARS {
        return Err(format!(
            "tripwire condition is too long ({} chars; max {CONDITION_MAX_CHARS}) — a check \
             needs a question, not a brief",
            condition.chars().count()
        ));
    }
    if condition.chars().any(|c| c.is_control() && c != '\n') {
        return Err("tripwire condition must not contain control characters".into());
    }
    Ok(())
}

/// Resolve a script name to an executable this machine may run.
///
/// The name has already been checked for shape; this checks the FILE:
/// * it must exist directly under `dir` — `symlink_metadata`, so a symlink is
///   seen as a symlink and refused rather than followed;
/// * it must canonicalize to a path beneath the canonicalized `dir`, so even a
///   directory that is itself a symlink cannot be used to reach outside it;
/// * it must be a regular file with an execute bit.
///
/// Every refusal names the path, because the remedy is always a file
/// operation the user has to do by hand.
pub fn script_path_in(dir: &Path, name: &str) -> Result<PathBuf, String> {
    validate_name(name)?;
    let root = dir.canonicalize().map_err(|e| {
        format!(
            "the tripwire scripts directory {} is not available: {e} (create it and put \
             '{name}' there)",
            dir.display()
        )
    })?;
    let candidate = root.join(name);
    let meta = std::fs::symlink_metadata(&candidate).map_err(|_| {
        format!(
            "no tripwire script named '{name}' — expected an executable at {}",
            candidate.display()
        )
    })?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "tripwire script '{name}' is a symlink, which is refused; put the real file at {}",
            candidate.display()
        ));
    }
    if !meta.is_file() {
        return Err(format!(
            "tripwire script '{name}' at {} is not a regular file",
            candidate.display()
        ));
    }
    let resolved = candidate
        .canonicalize()
        .map_err(|e| format!("could not resolve {}: {e}", candidate.display()))?;
    if !resolved.starts_with(&root) {
        return Err(format!(
            "tripwire script '{name}' resolves outside the tripwires directory ({})",
            resolved.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "tripwire script '{name}' is not executable — run: chmod +x {}",
                candidate.display()
            ));
        }
    }
    Ok(resolved)
}

/// [`script_path_in`] against the app's own tripwires directory.
pub fn script_path(name: &str) -> Result<PathBuf, String> {
    script_path_in(&crate::paths::tripwires_dir(), name)
}

/// Every runnable script name in `dir`, sorted — what a picker offers.
///
/// A file that fails [`script_path_in`] (no exec bit, a symlink) is simply not
/// listed: the picker offers what will work, and the MCP names the reason when
/// somebody types a name that will not.
pub fn list_scripts_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| script_path_in(dir, n).is_ok())
        .collect();
    names.sort();
    names
}

// ── cadence ───────────────────────────────────────────────────────────────

/// `manual | 5m | 2h | 7d | 1w | daily | weekly | hourly | <minutes>` → minutes.
/// `None` is manual: never evaluated on its own.
pub fn parse_every(raw: &str) -> Result<Option<i64>, String> {
    let s = raw.trim().to_ascii_lowercase();
    match s.as_str() {
        "" | "manual" | "never" | "none" => return Ok(None),
        "hourly" => return Ok(Some(60)),
        "daily" => return Ok(Some(1440)),
        "weekly" => return Ok(Some(10_080)),
        _ => {}
    }
    let (digits, unit) = match s.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((i, _)) => (&s[..i], s[i..].trim()),
        None => (s.as_str(), ""),
    };
    let n: i64 = digits
        .parse()
        .map_err(|_| format!("unknown cadence '{raw}': expected manual, 5m, 6h, 7d or a number of minutes"))?;
    let mult = match unit {
        "" | "m" | "min" | "mins" | "minute" | "minutes" => 1,
        "h" | "hr" | "hrs" | "hour" | "hours" => 60,
        "d" | "day" | "days" => 1440,
        "w" | "wk" | "week" | "weeks" => 10_080,
        other => {
            return Err(format!(
                "unknown cadence unit '{other}' in '{raw}': use m, h, d or w"
            ))
        }
    };
    let minutes = n
        .checked_mul(mult)
        .ok_or_else(|| format!("cadence '{raw}' is out of range"))?;
    if minutes <= 0 {
        return Err(format!("cadence '{raw}' must be positive (or 'manual')"));
    }
    Ok(Some(minutes))
}

/// The inverse of [`parse_every`], in the largest unit that divides evenly.
pub fn format_every(every: Option<i64>) -> String {
    let Some(m) = every else {
        return "manual".into();
    };
    if m > 0 && m % 10_080 == 0 {
        format!("{}w", m / 10_080)
    } else if m > 0 && m % 1440 == 0 {
        format!("{}d", m / 1440)
    } else if m > 0 && m % 60 == 0 {
        format!("{}h", m / 60)
    } else {
        format!("{m}m")
    }
}

/// The floor refusal, with the arithmetic in it — so an agent that asked for a
/// 5-minute agent check learns what that would have cost rather than just
/// "too low".
pub fn cadence_floor_message(kind: TripwireKind, every: i64) -> String {
    let floor = kind.min_interval_minutes();
    match kind {
        TripwireKind::Script => format!(
            "a script tripwire may be checked at most every {floor} minute(s); got {every}"
        ),
        TripwireKind::Agent => {
            let per_day = 1440 / every.max(1);
            format!(
                "an agent tripwire may be checked at most every {floor} minutes; got {every}. \
                 Every check is a background run of ~100k tokens, so {every}-minute checks \
                 would be ~{per_day} runs ≈ {}M tokens a day for ONE task.",
                (per_day * 100_000) / 1_000_000
            )
        }
        TripwireKind::Jev => format!(
            "a Jev tripwire may be checked at most every {floor} minutes; got {every}"
        ),
    }
}

/// Floors and the ceiling, keyed on the kind. `None` (manual) is always fine.
pub fn validate_cadence(kind: TripwireKind, every: Option<i64>) -> Result<(), String> {
    let Some(every) = every else {
        return Ok(());
    };
    if every < kind.min_interval_minutes() {
        return Err(cadence_floor_message(kind, every));
    }
    if every > MAX_MINUTES {
        return Err(format!(
            "tripwire cadence {every} minutes is longer than a year ({MAX_MINUTES}); use 'manual' instead"
        ));
    }
    Ok(())
}

// ── states ────────────────────────────────────────────────────────────────

/// The stable state on `tasks.tripwire_state`. Only `Armed` hides the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TripwireState {
    Armed,
    Tripped,
    Error,
}

impl TripwireState {
    pub const ALL: [TripwireState; 3] = [Self::Armed, Self::Tripped, Self::Error];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Tripped => "tripped",
            Self::Error => "error",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "armed" => Ok(Self::Armed),
            "tripped" => Ok(Self::Tripped),
            "error" => Ok(Self::Error),
            other => Err(format!("unknown tripwire state '{other}'")),
        }
    }

    /// Lenient, for a value already in the database. An unknown value reads as
    /// `Error` — the state that SURFACES the task — because a row this build
    /// cannot interpret must never keep a task hidden.
    pub fn from_db(raw: &str) -> Self {
        Self::parse(raw).unwrap_or_else(|_| {
            log::warn!("unknown tripwire_state '{raw}' in the database; treating it as error");
            Self::Error
        })
    }
}

/// What happens when the check comes back true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnTrip {
    /// Surface the task with the evidence and stop. The default.
    #[default]
    Surface,
    /// Surface AND complete it, so `blocked_by` releases its dependents.
    Complete,
    /// A STANDING check rather than a one-time gate. It surfaces like
    /// `Surface`, but the tripwire outlives the work: completing the task
    /// means "handled" and re-arms it (`db::tasks::update`), the runner keeps
    /// checking it while it is done (`db::tripwires::list_due`), and a trip
    /// on a done monitor reopens it. Only archiving it, or removing the
    /// tripwire, stops the check. An older build reads it as `Surface`, and
    /// the worst it can then do is mark it done, which is now harmless.
    Monitor,
    /// Surface it AND hand the evidence to a bounded run that says what the
    /// evidence MEANS, as a comment on the task.
    ///
    /// # Why this is where a repo-less shell run lives
    ///
    /// A script tripwire already runs a shell command with no repo, safely:
    /// the name is an allowlisted executable, the one argument is data, and
    /// the RUNNER spawns it, not an agent. What it could not do is read its
    /// own output — `io-write-report` emits ~5,300 characters and
    /// [`NOTE_CHARS`] stores 500, so the task got the first ninth of its own
    /// report and nothing to tell it whether 426 GB/day was alarming.
    ///
    /// This closes that without giving an unattended agent a shell or a
    /// filesystem. The interpretation run is pinned by the lane to **zero
    /// tools** (`runner::ToolTier::denied_for_interpret`) — not even `Read` —
    /// so there is no workspace for a boundary to bound and the Mac keeps
    /// refusing [`crate::routing::WorkspaceKind::Detached`] exactly as before.
    /// Its whole input is the evidence in its prompt; its whole output is its
    /// final message.
    ///
    /// **An older or leftover runner reads this as `Surface`** (see
    /// [`Self::from_db`]) and does precisely what it does today: trip,
    /// surface, post the raw evidence. That lenient read IS the skew guard,
    /// which is why this is a fourth policy rather than a new column or a new
    /// spec grammar.
    ///
    /// Exclusive with `Monitor` and `Complete`, because they are one enum. A
    /// standing check that also interprets is a second axis and would need a
    /// column.
    Interpret,
}

impl OnTrip {
    pub const ALL: [OnTrip; 4] = [Self::Surface, Self::Complete, Self::Monitor, Self::Interpret];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Surface => "surface",
            Self::Complete => "complete",
            Self::Monitor => "monitor",
            Self::Interpret => "interpret",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "surface" => Ok(Self::Surface),
            "complete" => Ok(Self::Complete),
            "monitor" => Ok(Self::Monitor),
            "interpret" => Ok(Self::Interpret),
            other => Err(format!(
                "unknown tripwire_on_trip '{other}' (expected surface, complete, monitor or \
                 interpret)"
            )),
        }
    }

    /// Does a trip under this policy ask for an interpretation run?
    pub fn wants_interpretation(self) -> bool {
        self == Self::Interpret
    }

    /// Lenient. Unknown reads as `Surface` — the value that DOES less.
    pub fn from_db(raw: Option<&str>) -> Self {
        raw.map(Self::parse)
            .unwrap_or(Ok(Self::Surface))
            .unwrap_or_else(|_| {
                log::warn!("unknown tripwire_on_trip {raw:?} in the database; treating it as surface");
                Self::Surface
            })
    }
}

/// How one attempt ended. The note is the script's stdout (tripped / not yet)
/// or its stderr and exit status (error), bounded to [`NOTE_CHARS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Tripped(String),
    NotYet(String),
    Error(String),
}

impl Outcome {
    pub fn kind(&self) -> OutcomeKind {
        match self {
            Self::Tripped(_) => OutcomeKind::Tripped,
            Self::NotYet(_) => OutcomeKind::NotYet,
            Self::Error(_) => OutcomeKind::Error,
        }
    }

    pub fn note(&self) -> &str {
        match self {
            Self::Tripped(n) | Self::NotYet(n) | Self::Error(n) => n,
        }
    }
}

/// The value stored in `tripwire_runtime.last_outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    Tripped,
    NotYet,
    Error,
}

impl OutcomeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tripped => "tripped",
            Self::NotYet => "not_yet",
            Self::Error => "error",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "tripped" => Ok(Self::Tripped),
            "not_yet" => Ok(Self::NotYet),
            "error" => Ok(Self::Error),
            other => Err(format!("unknown tripwire outcome '{other}'")),
        }
    }

    pub fn from_db(raw: &str) -> Self {
        Self::parse(raw).unwrap_or_else(|_| {
            log::warn!("unknown tripwire outcome '{raw}' in the database; treating it as error");
            Self::Error
        })
    }
}

// ── the verdict ───────────────────────────────────────────────────────────

/// What an agent check's final message says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Tripped,
    NotYet,
    /// The check could not tell. Recorded as an ERROR, never as not-yet: a
    /// task must not stay hidden on the strength of a shrug.
    Unsure,
}

/// The line a check must end with. Spelled once here and quoted verbatim by
/// the prompt tail, so the grammar the model is asked for and the grammar the
/// parser reads cannot drift.
pub const VERDICT_MARKER: &str = "VERDICT:";
pub const EVIDENCE_MARKER: &str = "EVIDENCE:";

/// Read a check's final message.
///
/// The LAST `VERDICT:` line wins — a model that reasons in its answer may
/// write "if X then VERDICT: TRIPPED" on the way to its conclusion, and the
/// conclusion is the one at the end. The evidence is everything after the
/// last `EVIDENCE:` marker (it may run several lines), else, when there is
/// none, the message itself — a verdict with no stated reason is still a
/// verdict, and the note is bounded downstream anyway.
///
/// `None` is "no verdict line at all", which the caller records as an error.
pub fn parse_verdict(text: &str) -> (Option<Verdict>, String) {
    let mut verdict: Option<Verdict> = None;
    let mut evidence_start: Option<usize> = None;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        let bare = trimmed.trim_start_matches(|c: char| c == '*' || c == '#' || c == '`' || c == '-' || c.is_whitespace());
        if let Some(rest) = strip_marker(bare, VERDICT_MARKER) {
            let word: String = rest
                .trim_start_matches(|c: char| {
                    c == '*' || c == '`' || c == '\'' || c == '"' || c.is_whitespace()
                })
                .chars()
                .take_while(|c| c.is_ascii_alphabetic() || *c == '_')
                .collect::<String>()
                .to_ascii_uppercase();
            match word.as_str() {
                "TRIPPED" => verdict = Some(Verdict::Tripped),
                "NOT_YET" | "NOTYET" => verdict = Some(Verdict::NotYet),
                "UNSURE" => verdict = Some(Verdict::Unsure),
                _ => {}
            }
        } else if let Some(rest) = strip_marker(bare, EVIDENCE_MARKER) {
            // Byte offset of the text after the marker, in the ORIGINAL string.
            let rest_start = offset + (line.len() - line.trim_start().len())
                + (trimmed.len() - bare.len())
                + (bare.len() - rest.len());
            evidence_start = Some(rest_start);
        }
        offset += line.len();
    }
    let dressing = |c: char| c == '*' || c == '#' || c == '`' || c == '-' || c.is_whitespace();
    let evidence = match evidence_start {
        Some(i) => text[i..]
            .lines()
            .take_while(|l| strip_marker(l.trim().trim_start_matches(dressing), VERDICT_MARKER).is_none())
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            // `**Evidence:** …` leaves the closing stars in front of the text.
            .trim_start_matches(|c: char| c == '*' || c == '`' || c.is_whitespace())
            .trim_end_matches(|c: char| c == '*' || c == '`' || c.is_whitespace())
            .to_string(),
        None => text.trim().to_string(),
    };
    (verdict, evidence)
}

fn strip_marker<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    if line.len() >= marker.len() && line[..marker.len()].eq_ignore_ascii_case(marker) {
        Some(&line[marker.len()..])
    } else {
        None
    }
}

// ── scheduling arithmetic ─────────────────────────────────────────────────

/// Clip a note to [`NOTE_CHARS`], marking the cut.
pub fn bound_note(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.chars().count() <= NOTE_CHARS {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(NOTE_CHARS - 1).collect();
    format!("{head}…")
}

/// How long to wait after `failures` consecutive errors before re-checking.
///
/// `every × 2^failures`, capped at [`BACKOFF_CAP_MINUTES`] — but never SHORTER
/// than the cadence itself. A weekly check that fails must not start checking
/// daily because a day is the cap; the cap bounds the growth, not the floor.
pub fn backoff_minutes(every: i64, failures: i64) -> i64 {
    let exp = failures.clamp(0, 10) as u32;
    let grown = every.saturating_mul(1i64 << exp).min(BACKOFF_CAP_MINUTES);
    grown.max(every)
}

/// Should an armed tripwire be evaluated now?
///
/// A pending request always wins. Otherwise a manual tripwire is never due,
/// a never-checked cadenced one is due at once, and a checked one is due once
/// its (backed-off) interval has elapsed. The defer gate and the lease are
/// SQL's business — this is only the arithmetic.
pub fn is_due(
    every: Option<i64>,
    failures: i64,
    checked_at: Option<DateTime<Utc>>,
    requested: bool,
    now: DateTime<Utc>,
) -> bool {
    if requested {
        return true;
    }
    let Some(every) = every else {
        return false;
    };
    let Some(checked) = checked_at else {
        return true;
    };
    checked + Duration::minutes(backoff_minutes(every, failures)) <= now
}

/// How many consecutive errors move a tripwire to `error`.
///
/// A manual tripwire's first failure surfaces the task: the user asked for a
/// check and deserves the answer now, and there is no next attempt coming. A
/// cadenced one is allowed a transient — a NAS that is asleep, a rate limit —
/// and surfaces after five.
pub fn failure_threshold(every: Option<i64>) -> i64 {
    if every.is_some() {
        5
    } else {
        1
    }
}

/// Parse either of the two timestamp formats this database holds — bookkeeping
/// `YYYY-MM-DD HH:MM:SS[.fff]` UTC, or ISO-8601 with a `Z`.
pub fn parse_stamp(raw: &str) -> Option<DateTime<Utc>> {
    let s = raw.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(DateTime::from_naive_utc_and_offset(naive, Utc));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn utc(s: &str) -> DateTime<Utc> {
        parse_stamp(s).unwrap()
    }

    // ── the spec ───────────────────────────────────────────────────────────

    #[test]
    fn a_script_spec_round_trips_with_and_without_its_argument() {
        let bare = Tripwire::parse("script:pocketbuddy-first-user").unwrap();
        assert_eq!(
            bare,
            Tripwire::Script { name: "pocketbuddy-first-user".into(), arg: None }
        );
        assert_eq!(bare.to_string(), "script:pocketbuddy-first-user");

        let with_arg = Tripwire::parse("  script:check-users   prod eu  ").unwrap();
        assert_eq!(
            with_arg,
            Tripwire::Script { name: "check-users".into(), arg: Some("prod eu".into()) }
        );
        assert_eq!(with_arg.to_string(), "script:check-users prod eu");
        assert_eq!(Tripwire::parse(&with_arg.to_string()).unwrap(), with_arg);
        assert_eq!(with_arg.kind(), TripwireKind::Script);
    }

    #[test]
    fn an_agent_spec_round_trips_and_is_bounded() {
        let a = Tripwire::parse("  agent:  PocketBuddy has an outside user ").unwrap();
        assert_eq!(a, Tripwire::Agent { condition: "PocketBuddy has an outside user".into() });
        assert_eq!(a.to_string(), "agent:PocketBuddy has an outside user");
        assert_eq!(Tripwire::parse(&a.to_string()).unwrap(), a);
        assert_eq!(a.kind(), TripwireKind::Agent);
        assert_eq!(a.condition(), Some("PocketBuddy has an outside user"));
        assert!(a.script_name().is_none());

        let err = Tripwire::parse("agent:   ").unwrap_err();
        assert!(err.contains("condition"), "{err}");
        let err = Tripwire::parse(&format!("agent:{}", "x".repeat(CONDITION_MAX_CHARS + 1))).unwrap_err();
        assert!(err.contains("too long"), "{err}");
        assert!(Tripwire::parse("agent:a\tb").is_err());
    }

    #[test]
    fn a_jev_spec_round_trips_and_uses_the_fast_check_floor() {
        let j = Tripwire::parse("  jev:  the customer has replied ").unwrap();
        assert_eq!(
            j,
            Tripwire::Jev { condition: "the customer has replied".into() }
        );
        assert_eq!(j.to_string(), "jev:the customer has replied");
        assert_eq!(j.kind(), TripwireKind::Jev);
        assert!(validate_cadence(TripwireKind::Jev, Some(5)).is_ok());
        assert!(validate_cadence(TripwireKind::Jev, Some(1)).is_err());
    }

    /// The composition rule for a check's bounds: a task's "no ceiling" must
    /// never become a check's "no ceiling".
    #[test]
    fn min_nonzero_never_lets_a_zero_disable_the_constant() {
        assert_eq!(min_nonzero(0, CHECK_TOKEN_CEILING), CHECK_TOKEN_CEILING);
        assert_eq!(min_nonzero(-1, CHECK_TOKEN_CEILING), CHECK_TOKEN_CEILING);
        assert_eq!(min_nonzero(1_000_000, CHECK_TOKEN_CEILING), 1_000_000);
        assert_eq!(min_nonzero(25_000_000, CHECK_TOKEN_CEILING), CHECK_TOKEN_CEILING);
        assert_eq!(min_nonzero(150, CHECK_MAX_TURNS), CHECK_MAX_TURNS);
        assert_eq!(min_nonzero(20, CHECK_TIMEOUT_MINUTES), CHECK_TIMEOUT_MINUTES);
        assert_eq!(min_nonzero(5, CHECK_TIMEOUT_MINUTES), 5);
        assert!(check_lease_minutes(CHECK_TIMEOUT_MINUTES) > CHECK_TIMEOUT_MINUTES);
    }

    #[test]
    fn a_name_is_a_filename_and_never_a_path() {
        for bad in ["../etc/passwd", "a/b", ".hidden", "-dash", "Upper", "sp ace", ""] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be refused");
        }
        for good in ["a", "pocketbuddy-first-user", "v2.check_users", "0x"] {
            assert!(validate_name(good).is_ok(), "{good:?} should be fine");
        }
        assert!(validate_name(&"a".repeat(64)).is_ok());
        assert!(validate_name(&"a".repeat(65)).is_err());
        assert!(Tripwire::parse("script:../oops").is_err());
        assert!(Tripwire::parse("run this thing").is_err());
    }

    #[test]
    fn the_argument_is_bounded_and_printable() {
        assert!(validate_arg("prod").is_ok());
        assert!(validate_arg(&"x".repeat(ARG_MAX_CHARS)).is_ok());
        assert!(validate_arg(&"x".repeat(ARG_MAX_CHARS + 1)).is_err());
        assert!(validate_arg("a\nb").is_err());
        assert!(validate_arg("a\u{7}b").is_err());
        assert!(Tripwire::parse("script:x a\tb").is_err());
    }

    // ── the file ───────────────────────────────────────────────────────────

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cxtasks-tripwire-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn executable(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn script_path_accepts_a_plain_executable_and_refuses_everything_else() {
        let dir = scratch("path");
        executable(&dir, "good");
        assert!(script_path_in(&dir, "good").unwrap().ends_with("good"));

        // No exec bit.
        std::fs::write(dir.join("noexec"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(dir.join("noexec"), std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = script_path_in(&dir, "noexec").unwrap_err();
        assert!(err.contains("chmod +x"), "{err}");

        // A symlink, even to a fine executable, is refused as a symlink.
        std::os::unix::fs::symlink(dir.join("good"), dir.join("link")).unwrap();
        let err = script_path_in(&dir, "link").unwrap_err();
        assert!(err.contains("symlink"), "{err}");

        // A directory is not a regular file.
        std::fs::create_dir_all(dir.join("subdir")).unwrap();
        let err = script_path_in(&dir, "subdir").unwrap_err();
        assert!(err.contains("not a regular file"), "{err}");

        // Missing names the expected location.
        let err = script_path_in(&dir, "absent").unwrap_err();
        assert!(err.contains("absent") && err.contains(dir.file_name().unwrap().to_str().unwrap()), "{err}");

        // Traversal cannot even be spelled.
        assert!(script_path_in(&dir, "../good").is_err());

        // And a directory that does not exist says so, naming the name.
        let err = script_path_in(&dir.join("nope"), "good").unwrap_err();
        assert!(err.contains("not available") && err.contains("good"), "{err}");

        assert_eq!(list_scripts_in(&dir), vec!["good".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── cadence ────────────────────────────────────────────────────────────

    #[test]
    fn cadence_parses_the_documented_forms_and_prints_the_shortest_one() {
        assert_eq!(parse_every("manual").unwrap(), None);
        assert_eq!(parse_every("").unwrap(), None);
        assert_eq!(parse_every("5m").unwrap(), Some(5));
        assert_eq!(parse_every("5").unwrap(), Some(5));
        assert_eq!(parse_every("2h").unwrap(), Some(120));
        assert_eq!(parse_every("6 h").unwrap(), Some(360));
        assert_eq!(parse_every("7d").unwrap(), Some(10_080));
        assert_eq!(parse_every("1w").unwrap(), Some(10_080));
        assert_eq!(parse_every("daily").unwrap(), Some(1440));
        assert!(parse_every("soon").is_err());
        assert!(parse_every("0m").is_err());
        assert!(parse_every("3y").is_err());

        assert_eq!(format_every(None), "manual");
        assert_eq!(format_every(Some(5)), "5m");
        assert_eq!(format_every(Some(90)), "90m");
        assert_eq!(format_every(Some(360)), "6h");
        assert_eq!(format_every(Some(1440)), "1d");
        assert_eq!(format_every(Some(10_080)), "1w");
        for v in [None, Some(1), Some(45), Some(120), Some(2880), Some(20_160)] {
            assert_eq!(parse_every(&format_every(v)).unwrap(), v, "{v:?} did not round-trip");
        }
    }

    #[test]
    fn the_floors_refuse_with_the_arithmetic_in_the_message() {
        assert!(validate_cadence(TripwireKind::Script, Some(1)).is_ok());
        assert!(validate_cadence(TripwireKind::Script, None).is_ok());
        assert!(validate_cadence(TripwireKind::Agent, Some(60)).is_ok());
        let err = validate_cadence(TripwireKind::Agent, Some(5)).unwrap_err();
        assert!(err.contains("288") && err.contains("28M"), "{err}");
        assert!(validate_cadence(TripwireKind::Script, Some(0)).is_err());
        assert!(validate_cadence(TripwireKind::Script, Some(MAX_MINUTES + 1)).is_err());
    }

    // ── scheduling ─────────────────────────────────────────────────────────

    #[test]
    fn backoff_doubles_caps_at_a_day_and_never_undercuts_the_cadence() {
        assert_eq!(backoff_minutes(5, 0), 5);
        assert_eq!(backoff_minutes(5, 1), 10);
        assert_eq!(backoff_minutes(5, 3), 40);
        assert_eq!(backoff_minutes(5, 9), 1440);
        assert_eq!(backoff_minutes(5, 50), 1440, "the exponent is clamped");
        // A weekly check that fails must not start checking daily.
        assert_eq!(backoff_minutes(10_080, 4), 10_080);
    }

    #[test]
    fn is_due_follows_the_table() {
        let now = utc("2026-08-24 12:00:00");
        let five_ago = Some(utc("2026-08-24 11:55:00"));
        let four_ago = Some(utc("2026-08-24 11:56:00"));
        // A request always wins, even on a manual tripwire checked a second ago.
        assert!(is_due(None, 0, Some(now), true, now));
        // Manual and unrequested: never.
        assert!(!is_due(None, 0, None, false, now));
        // Cadenced and never checked: now.
        assert!(is_due(Some(5), 0, None, false, now));
        // Cadenced, checked exactly one interval ago: due (<=).
        assert!(is_due(Some(5), 0, five_ago, false, now));
        assert!(!is_due(Some(5), 0, four_ago, false, now));
        // One failure doubles the wait.
        assert!(!is_due(Some(5), 1, five_ago, false, now));
        assert!(is_due(Some(5), 1, Some(utc("2026-08-24 11:50:00")), false, now));
    }

    #[test]
    fn a_manual_tripwire_surfaces_on_its_first_error_and_a_cadenced_one_on_its_fifth() {
        assert_eq!(failure_threshold(None), 1);
        assert_eq!(failure_threshold(Some(360)), 5);
    }

    // ── the verdict ────────────────────────────────────────────────────────

    #[test]
    fn the_last_verdict_line_wins_and_the_evidence_follows_its_marker() {
        let text = "I looked at the changelog.\nIf 2.0 were listed the answer would be VERDICT: TRIPPED, but it is not.\n\nVERDICT: NOT_YET\nEVIDENCE: CHANGELOG.md tops out at 1.9.3 (line 4);\nno 2.0 heading anywhere.\n";
        let (v, e) = parse_verdict(text);
        assert_eq!(v, Some(Verdict::NotYet));
        assert_eq!(e, "CHANGELOG.md tops out at 1.9.3 (line 4);\nno 2.0 heading anywhere.");

        // Markdown dressing and case are tolerated; the grammar is the words.
        let (v, e) = parse_verdict("**Verdict:** tripped\n**Evidence:** 51 users with a linked account.");
        assert_eq!(v, Some(Verdict::Tripped));
        assert_eq!(e, "51 users with a linked account.");

        // Evidence before the verdict still stops at the verdict line.
        let (v, e) = parse_verdict("EVIDENCE: nothing changed\nVERDICT: NOT_YET\n");
        assert_eq!(v, Some(Verdict::NotYet));
        assert_eq!(e, "nothing changed");

        let (v, e) = parse_verdict("VERDICT: UNSURE\nEVIDENCE: the repo has no changelog.");
        assert_eq!(v, Some(Verdict::Unsure));
        assert_eq!(e, "the repo has no changelog.");

        // No verdict at all: None, and the whole text as the note.
        let (v, e) = parse_verdict("I could not find it.");
        assert_eq!(v, None);
        assert_eq!(e, "I could not find it.");

        // A verdict word nobody defined is not a verdict.
        assert_eq!(parse_verdict("VERDICT: MAYBE").0, None);
    }

    #[test]
    fn both_timestamp_formats_parse() {
        assert_eq!(utc("2026-08-24 12:00:00"), utc("2026-08-24T12:00:00.000Z"));
        assert_eq!(utc("2026-08-24 12:00:00.250").timestamp_subsec_millis(), 250);
        assert!(parse_stamp("yesterday").is_none());
    }

    #[test]
    fn states_round_trip_and_unknown_state_surfaces() {
        for s in TripwireState::ALL {
            assert_eq!(TripwireState::parse(s.as_str()).unwrap(), s);
            assert_eq!(TripwireState::from_db(s.as_str()), s);
        }
        assert_eq!(TripwireState::from_db("sleeping"), TripwireState::Error);
        assert_eq!(OnTrip::parse("").unwrap(), OnTrip::Surface);
        assert_eq!(OnTrip::parse("COMPLETE").unwrap(), OnTrip::Complete);
        assert_eq!(OnTrip::parse(" monitor ").unwrap(), OnTrip::Monitor);
        assert_eq!(OnTrip::from_db(Some("monitor")), OnTrip::Monitor);
        assert!(OnTrip::parse("delete").is_err());
        assert_eq!(OnTrip::from_db(None), OnTrip::Surface);
        assert_eq!(OnTrip::from_db(Some("garbage")), OnTrip::Surface);
        for k in [OutcomeKind::Tripped, OutcomeKind::NotYet, OutcomeKind::Error] {
            assert_eq!(OutcomeKind::parse(k.as_str()).unwrap(), k);
        }
    }

    #[test]
    fn a_note_is_bounded_and_marked() {
        assert_eq!(bound_note("  hi  "), "hi");
        let long = "x".repeat(NOTE_CHARS + 50);
        let bounded = bound_note(&long);
        assert_eq!(bounded.chars().count(), NOTE_CHARS);
        assert!(bounded.ends_with('…'));
    }
}
