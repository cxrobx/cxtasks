//! `app_meta` key/value store: schema version (owned by `schema.rs`) plus the
//! background-runner budget settings.
//!
//! The defaults here are the Phase-4 guards. They are conservative on purpose:
//! per `feedback_subscription_not_api`, background runs burn Chris's Claude
//! **subscription** rate limit rather than metered API, and per
//! `reference_headless_claude_cost_overhead` every `claude -p` reloads the root
//! + repo `CLAUDE.md` (~41.5k tokens floor) before it does any work. Two prior
//! agent-swarm runs were aborted for burn — these counters are what stop a third.

use crate::error::AppError;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub const KEY_MAX_CONCURRENT: &str = "bg_max_concurrent";
pub const KEY_TIMEOUT_MINUTES: &str = "bg_timeout_minutes";
pub const KEY_DAILY_RUN_CAP: &str = "bg_daily_run_cap";
pub const KEY_MODEL: &str = "bg_model";
pub const KEY_DAILY_TOKEN_CAP: &str = "bg_daily_token_cap";
pub const KEY_MAX_TURNS: &str = "bg_max_turns";
pub const KEY_RUN_TOKEN_CAP: &str = "bg_run_token_cap";
/// The daily sub-cap on agent tripwire CHECKS, in total tokens.
///
/// A sub-cap, not a second budget: check tokens land on `task_runs` like any
/// other background run and so count against `bg_daily_token_cap` too. This
/// key only bounds how much of that day a set of hourly checks may quietly
/// eat before the runner stops admitting them — the failure it exists for is
/// twenty armed `agent:` tripwires spending the whole budget before the one
/// real task of the day gets a slot.
pub const KEY_TRIPWIRE_DAILY_TOKEN_CAP: &str = "bg_tripwire_daily_token_cap";

/// When the daily counters were last manually zeroed — the **manual reset**.
///
/// Holds a `datetime('now')` stamp (UTC, space-separated) or is absent. Read
/// only through [`WINDOW_START_SQL`] / [`window_start`], never on its own: what
/// it means is "the window starts later than midnight today", and a stamp from
/// yesterday means nothing at all.
pub const KEY_BUDGET_RESET_AT: &str = "bg_budget_reset_at";

/// The T-number counter. Monotonic and **never** rewound — see [`claim_short_id`].
pub const KEY_NEXT_SHORT_ID: &str = "next_short_id";

/// Background runs default to **Sonnet 5**, not the session default.
///
/// The token floor per run is model-independent (~40-55k, dominated by the
/// reloaded `CLAUDE.md`), but subscription rate limits are weighted by model —
/// Opus drains the allowance far faster for the same work. Unattended runs are
/// exactly the case where you want the cheaper model: nobody is watching, the
/// task is usually mechanical, and the result gets human review anyway.
/// Override per task, or globally here.
pub const DEFAULT_MODEL: &str = "claude-sonnet-5";

pub const DEFAULT_MAX_CONCURRENT: i64 = 2;
pub const DEFAULT_TIMEOUT_MINUTES: i64 = 20;
pub const DEFAULT_DAILY_RUN_CAP: i64 = 20;

/// The legal range for a wall clock, global or per-task.
///
/// Named rather than repeated as literals because since v18 there are now
/// THREE places that clamp a timeout — `budget_settings`, `save_budget_settings`
/// and a task's own `bg_timeout_minutes` — and a per-task bound that drifted
/// above the global one would let a task store a number the sweep's own clamp
/// then disagrees with, which is invariant 20's failure with extra steps.
pub const MIN_TIMEOUT_MINUTES: i64 = 1;
pub const MAX_TIMEOUT_MINUTES: i64 = 120;

/// The legal range for a per-run token ceiling, global or per-task. `0`
/// disables the ceiling, which is why the floor is 0 rather than 1.
pub const MIN_RUN_TOKEN_CAP: i64 = 0;
pub const MAX_RUN_TOKEN_CAP: i64 = 100_000_000;

/// Daily ceiling on tokens touched by background runs.
///
/// A run *count* is a poor proxy for cost: measured runs range from ~46k tokens
/// (single turn, no tools) to ~234k (a few tool-using turns, because each turn
/// re-sends the reloaded `CLAUDE.md` context and the cache reads accumulate).
/// This cap is the one that actually bounds subscription burn; the run cap
/// stays as a secondary guard against runaway loops.
///
/// **The first number here was an order of magnitude too small, and the way it
/// failed is the reason it's worth writing down.** It was set from those
/// early measurements, whose top end was ~234k. A real agentic run on a real
/// repo — 4 minutes, Sonnet 5, one task — touched **14.6M**: 13.3M of it cache
/// *reads*, because every turn re-reads the whole accumulated conversation
/// (invariant 5) and a run that works for minutes accumulates a lot of it. So a
/// 2M cap could not fit a single honest run, and the symptom was the cap
/// tripping on the very first task of the day with the meter reading 14.6M/2.0M
/// — a guard that only ever fires *after* the spend it was meant to prevent
/// says nothing about the day's remaining budget.
///
/// Sized in units of that measurement instead: ~3 substantial runs. Note the
/// gate is checked **before** a run starts and never during one, so this is the
/// point where the day *stops*, not a ceiling on where it lands — one run can
/// begin at 49M and finish the day north of 60M.
pub const DEFAULT_DAILY_TOKEN_CAP: i64 = 50_000_000;

/// The pre-v15 default, kept solely so the v15 migration can recognise a stored
/// cap nobody chose. **Not** a fallback — see `schema::create_tables_v15`.
pub const LEGACY_DAILY_TOKEN_CAP: i64 = 2_000_000;

/// Per-run turn ceiling — a **loop detector**, no longer the cost guard.
///
/// It was 30, and 30 was chosen as a proxy for spend. Two things were wrong
/// with that, and both showed up on the same day: cost per turn is not flat
/// (every turn re-sends the whole accumulated conversation, so one run's
/// context grew 93k → 149k *within* the run and another's 75k → 260k), and 30
/// sat **below** the cost of an honest run — both of that day's background runs
/// died at turn 31 with zero output and ~10M tokens each already spent. A guard
/// that fires on healthy runs is a tax, not a guard.
///
/// So the real per-run bound is [`DEFAULT_RUN_TOKEN_CAP`], measured in the unit
/// that actually matters, and this rises to a number you only reach by
/// genuinely looping.
pub const DEFAULT_MAX_TURNS: i64 = 150;

/// The pre-v17 default, kept solely so the v17 migration can recognise a stored
/// turn cap nobody chose. **Not** a fallback — see `schema::create_tables_v17`.
pub const LEGACY_MAX_TURNS: i64 = 30;

/// Ceiling on what ONE background run may touch, enforced live.
///
/// Unlike [`DEFAULT_DAILY_TOKEN_CAP`], which is checked before a run starts and
/// never during one, this is polled while the child is alive and kills it —
/// which is what makes it a ceiling rather than a place the day stops.
///
/// **Chosen the way the turn cap should have been.** Gotcha #30 records a
/// genuine agentic run at **14.6M**; a per-run ceiling that fires on the
/// largest known-good run would be the exact tax this replaced, one level down.
/// 25M leaves ~70% headroom over that measurement, is a quarter of the day, and
/// bounds overshoot past the daily cap at `25M × max_concurrent` = 50M — the
/// daily gate is a start-of-run check, so concurrent runs can each begin just
/// under it.
///
/// It is deliberately **not** clamped to the day's *remaining* budget: that
/// would start runs with an allowance too small to finish, wasting a checkpoint
/// and a spawn to produce nothing.
///
/// **Model-blind on purpose — this was decided, not overlooked.** Two runs the
/// same day spent 9.89M on Sonnet and 9.29M on Opus: near-identical token
/// counts that cost very different amounts of the subscription, which a token
/// ceiling cannot see. Selecting Opus is an intentional decision to spend more
/// per unit of work, so the ceiling does not second-guess it.
///
/// The obvious fix is also measurably **wrong**, which is the part worth
/// keeping. Dividing this by Opus's ~5× price weight gives an effective 5M —
/// *below* the 9.29M an ordinary Opus run already reached — so it would kill
/// healthy work, which is precisely the tax this constant replaced (gotcha
/// #31). Opus and Sonnet runs land in the same token range; the 5× shows up in
/// subscription burn per *day*, not in the size of one run. Weighting belongs
/// on the daily budget or nowhere, and "nowhere" is the standing choice.
///
/// **Claude-only in practice, which is the sharper limit.** `codex exec --json`
/// reports usage in a single event at the end of the run, so there is nothing
/// for the poll loop to read while a Codex run is alive and this ceiling never
/// fires for one — measured, not assumed. See
/// [`crate::runner::providers::Provider::live_total`].
pub const DEFAULT_RUN_TOKEN_CAP: i64 = 25_000_000;

/// 5M a day for agent checks: at the ~100k a bounded check is expected to
/// touch, that is ~50 checks — a dozen hourly tripwires with room to spare —
/// and a tenth of the default daily budget, so checks can never be what
/// spends the day.
pub const DEFAULT_TRIPWIRE_DAILY_TOKEN_CAP: i64 = 5_000_000;

pub fn get(conn: &Connection, key: &str) -> Result<Option<String>, AppError> {
    let v = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            params![key],
            |r| r.get::<_, String>(0),
        )
        .ok();
    Ok(v)
}

pub fn set(conn: &Connection, key: &str, value: &str) -> Result<(), AppError> {
    conn.execute(
        "INSERT INTO app_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Take the next T-number and advance the counter, atomically.
///
/// **Never reuse is the invariant; gaps are fine.** A number claimed here and
/// then lost to a failed INSERT simply never appears, which is invisible. The
/// opposite — two tasks sharing "T42" — makes every reference to a T-number
/// ambiguous, including references already written into commit messages and
/// comments, and is unrepairable after the fact.
///
/// That is why the claim is a single `UPDATE … RETURNING` rather than a read
/// followed by a write: two processes (the app, the MCP, the runner) hold their
/// own connections to this file, and a read-then-write pair has a window between
/// the two halves where both see the same value. `RETURNING` reports the value
/// *after* the update, so subtracting one gives the number this caller owns.
/// # The lease ceiling
///
/// Returns `None` when this host holds a lease and has used all of it. That is
/// a real, expected state — a laptop that has been off the tailnet long enough
/// to burn 500 numbers — and the caller stores `short_id = NULL`, which
/// [`crate::db::tasks::backfill_short_ids`] heals once a lease arrives. Issuing
/// a number outside the lease instead would produce the duplicate T-number that
/// leases exist to make impossible.
///
/// **With no lease there is no ceiling**, which is the standalone Mac and every
/// database that has never seen sync. The `NOT EXISTS` arm below is what keeps
/// that path byte-identical to the pre-sync statement.
pub fn claim_short_id(conn: &Connection) -> Result<Option<i64>, AppError> {
    // Idempotent seed. The v6 migration seeds this key too, but a fresh
    // in-memory test connection and any DB whose migration predates the key
    // both arrive here without it.
    conn.execute(
        "INSERT OR IGNORE INTO app_meta (key, value)
         SELECT ?1, CAST(COALESCE(MAX(short_id), 0) + 1 AS TEXT) FROM tasks",
        params![KEY_NEXT_SHORT_ID],
    )?;
    // The ceiling is part of the SAME statement, deliberately. Checking it
    // first and updating second reopens the exact read-then-write window this
    // function's `RETURNING` form was written to close — two processes would
    // both see one remaining number and both take it.
    let claimed: Option<i64> = conn
        .query_row(
            "UPDATE app_meta SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT)
              WHERE key = ?1
                AND (NOT EXISTS (SELECT 1 FROM app_meta WHERE key = ?2)
                     OR CAST(value AS INTEGER)
                        <= (SELECT CAST(value AS INTEGER) FROM app_meta WHERE key = ?2))
             RETURNING CAST(value AS INTEGER) - 1",
            params![KEY_NEXT_SHORT_ID, crate::sync::leases::KEY_LEASE_HI],
            |r| r.get(0),
        )
        .optional()?;
    if claimed.is_none() {
        log::warn!(
            "out of T-number lease — new tasks will have no reference until one is renewed"
        );
    }
    Ok(claimed)
}

/// The lower bound of the current budget window, as a SQL expression.
///
/// Local midnight — unless [`KEY_BUDGET_RESET_AT`] holds a *later* stamp, which
/// is the manual reset. `MAX` with two arguments is SQLite's **scalar** max, and
/// both operands are fixed-width `YYYY-MM-DD HH:MM:SS` UTC strings, so the
/// lexical comparison it performs is also the chronological one. The `COALESCE`
/// to `''` is what makes an absent key sort below midnight rather than
/// poisoning the comparison with NULL.
///
/// **Interpolated into every counter rather than retyped in each**, for the same
/// reason [`crate::db::tasks::LIVE`] is one constant: three near-identical WHERE
/// clauses are three chances for the token counter and the run counter to
/// disagree about what "today" means, and that disagreement would show up as a
/// cap that reopened for one of them and not the other.
///
/// A stale stamp needs no cleanup job: tomorrow's midnight is later than
/// today's reset, so the window heals itself at the next local midnight.
pub const WINDOW_START_SQL: &str = "datetime(MAX(
        datetime(date('now','localtime'), 'utc'),
        COALESCE((SELECT value FROM app_meta WHERE key = 'bg_budget_reset_at'), '')
    ))";

/// Which timezone decides when "today" starts, for the budget only.
///
/// # Why this is a property of the DATABASE and not of each process
///
/// `date('now','localtime')` asks the C library, which asks `TZ`. That is
/// exactly right on a Mac and exactly wrong the moment a second host exists: a
/// container defaults to UTC, so its day rolls over at 6 or 7pm Central and its
/// counters reset while the Mac's are still counting. Two hosts then enforce
/// one shared daily cap against two different days, and the failure is
/// *silent* — the budget simply admits more work than it should, on a schedule
/// nobody would connect to a timezone.
///
/// Setting `TZ` in the container fixes it too, and is one forgotten line in a
/// compose file away from being wrong again with no symptom. Storing the answer
/// where both hosts read it makes there be one answer.
///
/// Unset falls back to this process's own local zone, which is what every
/// existing database does and is correct on the machine that has always been
/// the only host. [`crate::api`]'s settings write pins it the first time
/// somebody looks at the budget.
pub const KEY_BUDGET_TZ: &str = "bg_budget_tz";

/// Local midnight in the budget's timezone, expressed as a UTC stamp in
/// SQLite's own `YYYY-MM-DD HH:MM:SS` form.
///
/// DST-safe: on a spring-forward day midnight may not exist in a given zone, so
/// this walks forward to the first hour that does rather than unwrapping a
/// `LocalResult::None` and panicking one morning a year in one timezone.
fn local_midnight_utc(conn: &Connection) -> Result<String, AppError> {
    use chrono::TimeZone;

    let named = get(conn, KEY_BUDGET_TZ)?
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .and_then(|v| v.parse::<chrono_tz::Tz>().ok());

    let now = chrono::Utc::now();
    let day = match named {
        Some(tz) => now.with_timezone(&tz).date_naive(),
        None => now.with_timezone(&chrono::Local).date_naive(),
    };

    for hour in 0..=3u32 {
        let candidate = match day.and_hms_opt(hour, 0, 0) {
            Some(c) => c,
            None => continue,
        };
        // Both arms collapse to a UTC instant here rather than being carried
        // as two different timezone types, which is the only reason the named
        // and fallback paths can share one loop.
        let resolved: Option<chrono::DateTime<chrono::Utc>> = match named {
            Some(tz) => tz
                .from_local_datetime(&candidate)
                .earliest()
                .map(|d| d.with_timezone(&chrono::Utc)),
            None => chrono::Local
                .from_local_datetime(&candidate)
                .earliest()
                .map(|d| d.with_timezone(&chrono::Utc)),
        };
        if let Some(instant) = resolved {
            return Ok(instant.format("%Y-%m-%d %H:%M:%S").to_string());
        }
    }
    // Every hour of the day is ambiguous, which no real zone does. Falling back
    // to UTC midnight is wrong by a few hours; failing is wrong by the whole
    // feature.
    Ok(now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap_or_else(|| now.naive_utc())
        .format("%Y-%m-%d %H:%M:%S")
        .to_string())
}

/// The current budget window's start, resolved in Rust, plus whether a manual
/// reset is what moved it.
///
/// Bound as a PARAMETER by the three counters rather than interpolated, which
/// is what makes the timezone a single computed value shared by all of them —
/// see [`KEY_BUDGET_TZ`] for why SQLite's `localtime` cannot be that value once
/// there are two hosts.
pub fn window_start_utc(conn: &Connection) -> Result<(String, bool), AppError> {
    let midnight = local_midnight_utc(conn)?;
    let reset = get(conn, KEY_BUDGET_RESET_AT)?.filter(|v| !v.trim().is_empty());
    Ok(match reset {
        // Both are fixed-width `YYYY-MM-DD HH:MM:SS` UTC, so the lexical
        // comparison is the chronological one.
        Some(r) if r > midnight => (r, true),
        _ => (midnight, false),
    })
}

/// The same boundary as [`WINDOW_START_SQL`], resolved, plus whether a manual
/// reset is what moved it.
///
/// The flag exists so the UI can stop calling the number "today". After a reset
/// the counter genuinely is not today's spend — it reads 0 on a day that has
/// already burned 14.6M — and a spend guard that mislabels its own scope is
/// worse than one that isn't there.
pub fn window_start(conn: &Connection) -> Result<(String, bool), AppError> {
    window_start_utc(conn)
}

/// Zero the daily counters by moving the window start to now.
///
/// Non-destructive on purpose: `task_runs` keeps every row and every token
/// count, so the history and the per-run figures are untouched — only the
/// question "how much since when" changes. Zeroing the columns instead would
/// make the runs themselves lie about what they cost.
///
/// **One sharp edge:** the window filters on `started_at`, so resetting while a
/// run is in flight drops that run's entire spend from the counter, including
/// the part it hasn't spent yet. That is the honest reading of "forgive what's
/// been spent" — but it means a reset issued mid-run undercounts the day by up
/// to one full run, which is currently ~15M.
///
/// Binary skew fails in the safe direction, as usual here: a leftover pre-v15
/// `cxtasks-runner` counts from midnight and has never heard of this key, so it
/// goes on refusing to start work the reset just re-authorised. "Doesn't run
/// until the new runner ships" is recoverable; the opposite would be a reset
/// that half-applied.
pub fn reset_budget_window(conn: &Connection) -> Result<(), AppError> {
    let now: String = conn.query_row("SELECT datetime('now')", [], |r| r.get(0))?;
    set(conn, KEY_BUDGET_RESET_AT, &now)
}

fn get_i64(conn: &Connection, key: &str, default: i64) -> i64 {
    get(conn, key)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(default)
}

/// The runner's operating limits, read fresh on every tick so a change in the
/// UI takes effect without restarting the LaunchAgent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetSettings {
    pub max_concurrent: i64,
    pub timeout_minutes: i64,
    pub daily_run_cap: i64,
    #[serde(default = "default_token_cap")]
    pub daily_token_cap: i64,
    /// Ceiling on ONE run, enforced live by killing the child. `0` disables it.
    #[serde(default = "default_run_token_cap")]
    pub run_token_cap: i64,
    #[serde(default = "default_max_turns")]
    pub max_turns: i64,
    /// Model for background runs. A task's own `model` overrides this.
    #[serde(default = "default_model")]
    pub model: String,
    /// Daily sub-cap on agent tripwire checks. `0` disables the sub-cap (the
    /// daily cap still applies). See [`KEY_TRIPWIRE_DAILY_TOKEN_CAP`].
    #[serde(default = "default_tripwire_token_cap")]
    pub tripwire_daily_token_cap: i64,
}

fn default_tripwire_token_cap() -> i64 {
    DEFAULT_TRIPWIRE_DAILY_TOKEN_CAP
}

fn default_model() -> String {
    DEFAULT_MODEL.to_string()
}

fn default_token_cap() -> i64 {
    DEFAULT_DAILY_TOKEN_CAP
}

fn default_run_token_cap() -> i64 {
    DEFAULT_RUN_TOKEN_CAP
}

fn default_max_turns() -> i64 {
    DEFAULT_MAX_TURNS
}

impl Default for BudgetSettings {
    fn default() -> Self {
        Self {
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            timeout_minutes: DEFAULT_TIMEOUT_MINUTES,
            daily_run_cap: DEFAULT_DAILY_RUN_CAP,
            daily_token_cap: DEFAULT_DAILY_TOKEN_CAP,
            run_token_cap: DEFAULT_RUN_TOKEN_CAP,
            max_turns: DEFAULT_MAX_TURNS,
            model: DEFAULT_MODEL.to_string(),
            tripwire_daily_token_cap: DEFAULT_TRIPWIRE_DAILY_TOKEN_CAP,
        }
    }
}

pub fn budget_settings(conn: &Connection) -> BudgetSettings {
    BudgetSettings {
        // Hard ceiling of 4 regardless of what's stored: the point of the guard
        // is that a fat-fingered settings value can't start a swarm.
        max_concurrent: get_i64(conn, KEY_MAX_CONCURRENT, DEFAULT_MAX_CONCURRENT).clamp(1, 4),
        timeout_minutes: get_i64(conn, KEY_TIMEOUT_MINUTES, DEFAULT_TIMEOUT_MINUTES)
            .clamp(MIN_TIMEOUT_MINUTES, MAX_TIMEOUT_MINUTES),
        daily_run_cap: get_i64(conn, KEY_DAILY_RUN_CAP, DEFAULT_DAILY_RUN_CAP).clamp(0, 200),
        daily_token_cap: get_i64(conn, KEY_DAILY_TOKEN_CAP, DEFAULT_DAILY_TOKEN_CAP)
            .clamp(0, 100_000_000),
        run_token_cap: get_i64(conn, KEY_RUN_TOKEN_CAP, DEFAULT_RUN_TOKEN_CAP)
            .clamp(MIN_RUN_TOKEN_CAP, MAX_RUN_TOKEN_CAP),
        max_turns: get_i64(conn, KEY_MAX_TURNS, DEFAULT_MAX_TURNS).clamp(1, 200),
        model: get(conn, KEY_MODEL)
            .ok()
            .flatten()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(default_model),
        tripwire_daily_token_cap: get_i64(
            conn,
            KEY_TRIPWIRE_DAILY_TOKEN_CAP,
            DEFAULT_TRIPWIRE_DAILY_TOKEN_CAP,
        )
        .clamp(0, 100_000_000),
    }
}

pub fn save_budget_settings(conn: &Connection, s: &BudgetSettings) -> Result<(), AppError> {
    set(conn, KEY_MAX_CONCURRENT, &s.max_concurrent.clamp(1, 4).to_string())?;
    set(
        conn,
        KEY_TIMEOUT_MINUTES,
        &s.timeout_minutes
            .clamp(MIN_TIMEOUT_MINUTES, MAX_TIMEOUT_MINUTES)
            .to_string(),
    )?;
    set(conn, KEY_DAILY_RUN_CAP, &s.daily_run_cap.clamp(0, 200).to_string())?;
    set(conn, KEY_MODEL, s.model.trim())?;
    set(
        conn,
        KEY_DAILY_TOKEN_CAP,
        &s.daily_token_cap.clamp(0, 100_000_000).to_string(),
    )?;
    set(
        conn,
        KEY_RUN_TOKEN_CAP,
        &s.run_token_cap.clamp(0, 100_000_000).to_string(),
    )?;
    set(conn, KEY_MAX_TURNS, &s.max_turns.clamp(1, 200).to_string())?;
    set(
        conn,
        KEY_TRIPWIRE_DAILY_TOKEN_CAP,
        &s.tripwire_daily_token_cap.clamp(0, 100_000_000).to_string(),
    )?;
    Ok(())
}

/// Everything the UI needs to show background consumption at a glance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetStatus {
    pub settings: BudgetSettings,
    pub running: i64,
    pub runs_today: i64,
    pub output_tokens_today: i64,
    /// Input + cache-read + cache-write + output. This is the number that
    /// actually tracks rate-limit consumption; `output_tokens_today` alone is
    /// off by ~99% because every run reloads the CLAUDE.md context.
    pub total_tokens_today: i64,
    /// The slice of `total_tokens_today` spent by agent tripwire checks. Also
    /// IN the total — this is a breakdown, not a second counter.
    #[serde(default)]
    pub tripwire_tokens_today: i64,
    /// True when the STOP_BACKGROUND file exists.
    pub paused: bool,
    /// Where the counters above start measuring from (UTC, `datetime()` form).
    pub window_started_at: String,
    /// True when a manual reset — not midnight — is what set that boundary. The
    /// UI must stop saying "today" when it is.
    pub window_reset: bool,
}

pub fn budget_status(conn: &Connection) -> Result<BudgetStatus, AppError> {
    let (window_started_at, window_reset) = window_start(conn)?;
    Ok(BudgetStatus {
        settings: budget_settings(conn),
        running: crate::db::runs::count_running_background(conn)?,
        runs_today: crate::db::runs::count_background_runs_today(conn)?,
        output_tokens_today: crate::db::runs::output_tokens_today(conn)?,
        total_tokens_today: crate::db::runs::total_tokens_today(conn)?,
        tripwire_tokens_today: crate::db::runs::tripwire_tokens_today(conn)?,
        paused: crate::paths::background_paused(),
        window_started_at,
        window_reset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;

    #[test]
    fn defaults_apply_when_unset() {
        let conn = test_conn();
        let s = budget_settings(&conn);
        assert_eq!(s.max_concurrent, 2);
        assert_eq!(s.timeout_minutes, 20);
        assert_eq!(s.daily_run_cap, 20);
        assert_eq!(
            s.model, DEFAULT_MODEL,
            "background runs must default to the cheaper model, not the session default"
        );
    }

    #[test]
    fn concurrency_is_hard_clamped_even_if_the_db_says_otherwise() {
        let conn = test_conn();
        set(&conn, KEY_MAX_CONCURRENT, "50").unwrap();
        assert_eq!(budget_settings(&conn).max_concurrent, 4);
        set(&conn, KEY_MAX_CONCURRENT, "0").unwrap();
        assert_eq!(budget_settings(&conn).max_concurrent, 1);
    }

    #[test]
    fn garbage_values_fall_back_to_defaults() {
        let conn = test_conn();
        set(&conn, KEY_TIMEOUT_MINUTES, "twenty").unwrap();
        assert_eq!(budget_settings(&conn).timeout_minutes, 20);
    }

    #[test]
    fn round_trip_settings() {
        let conn = test_conn();
        save_budget_settings(
            &conn,
            &BudgetSettings {
                max_concurrent: 3,
                timeout_minutes: 45,
                daily_run_cap: 5,
                daily_token_cap: 500_000,
                run_token_cap: 250_000,
                max_turns: 12,
                model: "claude-haiku-4-5-20251001".into(),
                tripwire_daily_token_cap: 1_000_000,
            },
        )
        .unwrap();
        let s = budget_settings(&conn);
        assert_eq!((s.max_concurrent, s.timeout_minutes, s.daily_run_cap), (3, 45, 5));
        assert_eq!(s.model, "claude-haiku-4-5-20251001");
        assert_eq!(s.daily_token_cap, 500_000);
        assert_eq!(s.run_token_cap, 250_000);
        assert_eq!(s.max_turns, 12);
        assert_eq!(s.tripwire_daily_token_cap, 1_000_000);
    }

    /// A settings blob from a pre-R2 client carries no tripwire cap; it must
    /// read as the default rather than as a refused save or a zero (= off).
    #[test]
    fn a_settings_blob_without_the_tripwire_cap_reads_as_the_default() {
        let s: BudgetSettings = serde_json::from_str(
            r#"{"max_concurrent":2,"timeout_minutes":20,"daily_run_cap":20}"#,
        )
        .unwrap();
        assert_eq!(s.tripwire_daily_token_cap, DEFAULT_TRIPWIRE_DAILY_TOKEN_CAP);
        assert_eq!(budget_settings(&test_conn()).tripwire_daily_token_cap, DEFAULT_TRIPWIRE_DAILY_TOKEN_CAP);
    }

    /// Never reuse is the invariant; gaps are fine. A number lost to a failed
    /// insert must simply never appear again — two tasks sharing "T42" would
    /// make every written-down reference ambiguous, unrepairably.
    #[test]
    fn claim_short_id_is_monotonic_and_gap_tolerant() {
        let conn = test_conn();
        // `.unwrap().unwrap()` throughout: with no lease there is no ceiling,
        // so every claim must succeed. A `None` here would mean the lease guard
        // had leaked into the standalone path.
        let first = claim_short_id(&conn).unwrap().unwrap();
        let claims: Vec<i64> = (0..5).map(|_| claim_short_id(&conn).unwrap().unwrap()).collect();
        assert_eq!(
            claims,
            (first + 1..=first + 5).collect::<Vec<_>>(),
            "consecutive claims must step by exactly one"
        );

        // Simulate a burned claim: the caller took a number and never used it.
        let burned = claim_short_id(&conn).unwrap().unwrap();
        let after = claim_short_id(&conn).unwrap().unwrap();
        assert_eq!(after, burned + 1, "a burned claim leaves a gap, not a reuse");

        // And the counter is durable — re-seeding must not rewind it.
        let stored: i64 = get(&conn, KEY_NEXT_SHORT_ID)
            .unwrap()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(stored, after + 1);
    }

    /// The ceiling stops the claim, atomically, and only when a lease exists.
    #[test]
    fn a_claim_stops_at_the_lease_ceiling() {
        let conn = test_conn();
        crate::sync::leases::adopt(&conn, crate::sync::leases::Lease { lo: 10, hi: 12 }).unwrap();

        assert_eq!(claim_short_id(&conn).unwrap(), Some(10));
        assert_eq!(claim_short_id(&conn).unwrap(), Some(11));
        assert_eq!(claim_short_id(&conn).unwrap(), Some(12));
        assert_eq!(claim_short_id(&conn).unwrap(), None, "13 is outside the lease");
        assert_eq!(claim_short_id(&conn).unwrap(), None, "and it stays refused");

        // The cursor did NOT advance past the ceiling — a later lease starting
        // at 13 must still be adoptable.
        let stored: i64 = get(&conn, KEY_NEXT_SHORT_ID).unwrap().unwrap().parse().unwrap();
        assert_eq!(stored, 13, "a refused claim must not burn the number");
    }

    /// A renewed lease resumes numbering where the ceiling stopped it.
    #[test]
    fn a_fresh_lease_resumes_numbering() {
        let conn = test_conn();
        crate::sync::leases::adopt(&conn, crate::sync::leases::Lease { lo: 10, hi: 10 }).unwrap();
        assert_eq!(claim_short_id(&conn).unwrap(), Some(10));
        assert_eq!(claim_short_id(&conn).unwrap(), None);

        crate::sync::leases::adopt(&conn, crate::sync::leases::Lease { lo: 40, hi: 49 }).unwrap();
        assert_eq!(claim_short_id(&conn).unwrap(), Some(40));
    }

    /// **The standalone path is unchanged.** No lease, no ceiling, forever.
    #[test]
    fn with_no_lease_a_claim_is_never_refused() {
        let conn = test_conn();
        for _ in 0..2_000 {
            assert!(claim_short_id(&conn).unwrap().is_some());
        }
    }

    /// The counters read the key through raw SQL, so the constant and the
    /// string in [`WINDOW_START_SQL`] are two independent spellings of one
    /// name. Renaming the constant alone would leave a reset that writes a key
    /// nothing reads — the button would appear to work and change nothing.
    #[test]
    fn the_window_expression_reads_the_key_the_reset_writes() {
        assert!(
            WINDOW_START_SQL.contains(KEY_BUDGET_RESET_AT),
            "WINDOW_START_SQL must name {KEY_BUDGET_RESET_AT}; got {WINDOW_START_SQL}"
        );
    }

    #[test]
    fn the_window_starts_at_local_midnight_until_something_resets_it() {
        let conn = test_conn();
        let (start, reset) = window_start(&conn).unwrap();
        assert!(!reset, "an untouched budget window is not a reset one");
        let midnight: String = conn
            .query_row("SELECT datetime(date('now','localtime'), 'utc')", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(start, midnight);

        reset_budget_window(&conn).unwrap();
        let (after, reset) = window_start(&conn).unwrap();
        assert!(reset, "a manual reset must be reported as one");
        assert!(
            after >= midnight,
            "the reset moves the window forward, never back: {after} < {midnight}"
        );
    }

    /// A stamp from a previous day must not shrink today's window — otherwise
    /// yesterday's reset would go on hiding this morning's runs, and the daily
    /// cap would never see a full day again.
    #[test]
    fn a_stale_reset_is_superseded_by_midnight_with_no_cleanup() {
        let conn = test_conn();
        set(&conn, KEY_BUDGET_RESET_AT, "2000-01-01 12:00:00").unwrap();
        let (start, reset) = window_start(&conn).unwrap();
        assert!(!reset, "a stamp older than midnight is not a live reset");
        assert!(start.starts_with("20"));
        assert_ne!(start, "2000-01-01 12:00:00");
    }

    #[test]
    fn schema_version_key_is_untouched_by_settings_writes() {
        let conn = test_conn();
        save_budget_settings(&conn, &BudgetSettings::default()).unwrap();
        // Compare against the constant, not a literal — hardcoding the number
        // here made this test fail on every single migration, which is noise
        // rather than signal. What it actually guards is that writing settings
        // doesn't clobber the version key.
        let v = get(&conn, "schema_version").unwrap();
        assert_eq!(
            v.as_deref(),
            Some(crate::db::schema::CURRENT_VERSION.to_string().as_str())
        );
    }
    // ── the budget window's timezone ───────────────────────────────────────

    /// **The multi-host property.** The window is decided by a value in the
    /// DATABASE, so two processes compute the same instant whatever their own
    /// environments say — which is the whole reason it stopped being
    /// `date('now','localtime')`.
    ///
    /// Discriminating because it asserts the two settings produce DIFFERENT
    /// answers as well as that each is right: a `window_start_utc` that ignored
    /// the key and returned process-local midnight would pass the first half
    /// and fail here.
    #[test]
    fn the_budget_window_is_decided_by_the_stored_zone_not_the_process() {
        use chrono::TimeZone;
        let conn = crate::db::schema::test_conn();

        let expected_for = |zone: &str| -> String {
            let tz: chrono_tz::Tz = zone.parse().unwrap();
            let day = chrono::Utc::now().with_timezone(&tz).date_naive();
            tz.from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
                .earliest()
                .unwrap()
                .with_timezone(&chrono::Utc)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        };

        set(&conn, KEY_BUDGET_TZ, "America/Chicago").unwrap();
        let chicago = window_start_utc(&conn).unwrap().0;
        assert_eq!(chicago, expected_for("America/Chicago"));

        set(&conn, KEY_BUDGET_TZ, "Asia/Tokyo").unwrap();
        let tokyo = window_start_utc(&conn).unwrap().0;
        assert_eq!(tokyo, expected_for("Asia/Tokyo"));

        assert_ne!(
            chicago, tokyo,
            "the stored zone is being ignored — every host would fall back to \
             its own TZ, which is the bug this key exists to close"
        );
    }

    /// An unset key must behave exactly as every existing database does today.
    /// A machine that has always been the only host must not change behaviour
    /// because a column was added for a machine it has never met.
    #[test]
    fn an_unset_zone_falls_back_to_this_process_rather_than_to_utc() {
        use chrono::TimeZone;
        let conn = crate::db::schema::test_conn();
        assert!(get(&conn, KEY_BUDGET_TZ).unwrap().is_none());

        let day = chrono::Utc::now().with_timezone(&chrono::Local).date_naive();
        let expected = chrono::Local
            .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .unwrap()
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        assert_eq!(window_start_utc(&conn).unwrap().0, expected);
    }

    /// Junk must not take the budget guard offline. A zone name from a newer
    /// build, or a typo, falls back rather than erroring — an unreadable
    /// setting that made every counter fail would disable the cap entirely,
    /// which is the wrong direction to be wrong in.
    #[test]
    fn an_unparseable_zone_falls_back_instead_of_failing_the_counters() {
        let conn = crate::db::schema::test_conn();
        set(&conn, KEY_BUDGET_TZ, "Mars/Olympus_Mons").unwrap();
        let (w, reset) = window_start_utc(&conn).unwrap();
        assert!(!reset);
        assert_eq!(w.len(), 19, "not a comparable YYYY-MM-DD HH:MM:SS stamp: {w}");
    }

    /// A manual reset still wins over midnight, whatever the zone.
    #[test]
    fn a_manual_reset_still_moves_the_window_past_midnight() {
        let conn = crate::db::schema::test_conn();
        set(&conn, KEY_BUDGET_TZ, "America/Chicago").unwrap();
        assert!(!window_start_utc(&conn).unwrap().1);
        reset_budget_window(&conn).unwrap();
        let (w, was_reset) = window_start_utc(&conn).unwrap();
        assert!(was_reset, "a reset must be reported, or the UI calls it 'today'");
        assert!(w > window_start_utc_midnight_only(&conn));
    }

    /// Midnight alone, for the assertion above — the reset must be strictly
    /// later or it is not doing anything.
    fn window_start_utc_midnight_only(conn: &Connection) -> String {
        let saved = get(conn, KEY_BUDGET_RESET_AT).unwrap();
        set(conn, KEY_BUDGET_RESET_AT, "").unwrap();
        let m = window_start_utc(conn).unwrap().0;
        if let Some(v) = saved {
            set(conn, KEY_BUDGET_RESET_AT, &v).unwrap();
        }
        m
    }
}
