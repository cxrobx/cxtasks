//! Which agent runs a task in the background.
//!
//! Foreground dispatch (`Open in Claude`, `Copy Seed Prompt`) is deliberately
//! **not** covered by this: it opens a Ghostty window the user watches, and
//! adding a second terminal agent there buys nothing the user couldn't get by
//! typing `codex` themselves. This type only ever decides what the unattended
//! runner spawns.
//!
//! # Why the enabled flag is stored per-executor
//!
//! The obvious schema — one `bg_allowed` boolean plus a `bg_executor` string —
//! is unsafe against the binary skew this DB always has. Three binaries (app,
//! `cxtasks-mcp`, `cxtasks-runner`) share one file and do not upgrade in
//! lockstep, so after a v8 app ships there is a window in which a leftover
//! **pre-v8 runner** is still draining the queue. That runner's drain query is
//! `... AND bg_allowed = 1`; it has never heard of `bg_executor`. A Codex task
//! stored as `bg_allowed = 1, bg_executor = 'codex'` would therefore be claimed
//! by it and executed with **Claude** — silently, with Claude's tool grants,
//! against a prompt written for Codex.
//!
//! So the enabled flag lives in a column the old runner cannot see:
//!
//! | executor | `bg_allowed` | `bg_codex_allowed` |
//! |---|---|---|
//! | `claude` | the flag     | 0 |
//! | `codex`  | 0            | the flag |
//!
//! `bg_allowed` keeps its exact legacy meaning — "this task may be run
//! unattended **by Claude**" — so every pre-v8 reader stays correct rather than
//! merely non-crashing, and a Codex task is invisible to it. [`Task`] then
//! exposes one *computed* `bg_allowed` to the rest of the app, so nothing
//! downstream has to know the encoding exists.
//!
//! [`Task`]: crate::db::tasks::Task

use serde::{Deserialize, Serialize};

/// The background agent a task is dispatched to.
///
/// Deliberately an enum rather than a free string: it is written into the DB,
/// accepted over MCP, and selects a process to spawn, so "codx" must be a
/// rejected value at every one of those boundaries rather than a task that
/// quietly never runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundExecutor {
    /// Claude Code (`claude -p`). The default, and what every pre-v8 task is.
    #[default]
    Claude,
    /// OpenAI Codex CLI (`codex exec`).
    Codex,
}

impl BackgroundExecutor {
    pub const ALL: [BackgroundExecutor; 2] = [Self::Claude, Self::Codex];

    /// The value stored in `tasks.bg_executor` / `task_runs.executor`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// Human label for a badge, a notification, or a prompt — the agent's full
    /// product name, which is what you want when the sentence is about the run
    /// ("Claude Code run succeeded") rather than about a button.
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    /// The name used inside an ACTION label — "Delegate to Claude".
    ///
    /// Shorter than [`label`] on purpose: the two dispatch actions sit next to
    /// each other in the same menu, and "Open in Claude" / "Delegate to Claude
    /// Code" would read as two different destinations when they are the same
    /// agent. What still has to survive is naming the agent at all — reading
    /// "Delegate to Codex" is how you catch the wrong executor while it's still
    /// free to notice.
    pub fn action_label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        }
    }

    /// The column holding "this task may run unattended under this executor".
    ///
    /// See the module docs — the split is a skew guard, not a normalization
    /// mistake.
    pub fn enabled_column(self) -> &'static str {
        match self {
            Self::Claude => "bg_allowed",
            Self::Codex => "bg_codex_allowed",
        }
    }

    /// Strict parse, for anything a caller typed: MCP parameters, IPC payloads.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            other => Err(format!(
                "unknown background executor '{other}' (expected one of {})",
                Self::ALL
                    .iter()
                    .map(|e| e.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// Lenient parse, for a value already in the database.
    ///
    /// Falls back to `Claude` on anything unrecognized instead of failing the
    /// read, because the alternative is one bad row making `list_tasks` return
    /// an error and the whole board render empty. That fallback is safe here
    /// **only** because the enabled flag is stored per-executor: a future
    /// `bg_executor = 'gemini'` row read by this binary comes back as Claude
    /// *with `bg_allowed = 0`*, so it is displayed slightly wrong and never
    /// dispatched — which is the right way round.
    pub fn from_db(raw: &str) -> Self {
        Self::parse(raw).unwrap_or_else(|_| {
            log::warn!("unknown bg_executor '{raw}' in the database; treating it as claude");
            Self::Claude
        })
    }

    /// SQL that is true for a task opted into unattended runs under whichever
    /// executor it names.
    ///
    /// Generated from [`Self::ALL`] rather than hand-written at each call site,
    /// so a third executor cannot be added and then forgotten in one of the two
    /// queries that decide what an unattended agent is handed
    /// (`list_queued_for_background` and `claim_for_background`) — a mismatch
    /// there is a task that is listed and then never claimable, i.e. a queue
    /// that spins forever with nothing on screen to explain it.
    pub fn enabled_sql() -> String {
        let clauses: Vec<String> = Self::ALL
            .iter()
            .map(|e| format!("(bg_executor = '{}' AND {} = 1)", e.as_str(), e.enabled_column()))
            .collect();
        format!("({})", clauses.join(" OR "))
    }
}

impl std::fmt::Display for BackgroundExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl rusqlite::ToSql for BackgroundExecutor {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::from(self.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_db_representation() {
        for e in BackgroundExecutor::ALL {
            assert_eq!(BackgroundExecutor::from_db(e.as_str()), e);
            assert_eq!(BackgroundExecutor::parse(e.as_str()).unwrap(), e);
        }
    }

    #[test]
    fn parse_is_strict_but_case_insensitive() {
        assert_eq!(BackgroundExecutor::parse("CODEX").unwrap(), BackgroundExecutor::Codex);
        assert_eq!(BackgroundExecutor::parse(" codex ").unwrap(), BackgroundExecutor::Codex);
        assert!(BackgroundExecutor::parse("codx").is_err());
        assert!(BackgroundExecutor::parse("").is_err());
    }

    /// A row from a newer binary must not break a read — but it must also never
    /// be dispatched, which is what the per-executor enabled column buys.
    #[test]
    fn an_unknown_stored_value_reads_as_claude_rather_than_failing() {
        assert_eq!(BackgroundExecutor::from_db("gemini"), BackgroundExecutor::Claude);
    }

    #[test]
    fn the_two_executors_use_different_enabled_columns() {
        assert_eq!(BackgroundExecutor::Claude.enabled_column(), "bg_allowed");
        assert_eq!(BackgroundExecutor::Codex.enabled_column(), "bg_codex_allowed");
        // The whole skew guard: the legacy column must belong to Claude alone.
        for e in BackgroundExecutor::ALL {
            if e != BackgroundExecutor::Claude {
                assert_ne!(
                    e.enabled_column(),
                    "bg_allowed",
                    "{e} must not reuse the legacy column a pre-v8 runner queries"
                );
            }
        }
    }

    #[test]
    fn enabled_sql_covers_every_executor() {
        let sql = BackgroundExecutor::enabled_sql();
        for e in BackgroundExecutor::ALL {
            assert!(sql.contains(e.as_str()), "{sql} is missing {e}");
            assert!(sql.contains(e.enabled_column()), "{sql} is missing {}", e.enabled_column());
        }
    }
}
