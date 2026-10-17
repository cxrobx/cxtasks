//! What a background run is *for* — continue the work, or audit the last
//! handoff.
//!
//! Read this beside [`crate::executor`]. The two modules answer opposite
//! questions about the same problem, and keeping them in one file would blur
//! exactly the distinction that makes each one correct:
//!
//! * `executor` documents **why the enabled flag needed a SECOND column** —
//!   because a pre-v8 runner draining `bg_allowed = 1` would claim a Codex task
//!   and spawn Claude. Different binary, different argv, an OS sandbox
//!   described in a prompt where none existed. Unrecoverable.
//! * this module documents **why intent needs only ONE** — because a pre-v16
//!   runner draining a review task does something merely *disappointing*.
//!
//! # Why one column is safe here
//!
//! Traced through a leftover pre-v16 `cxtasks-runner` handed a task with
//! `bg_intent = 'review'`:
//!
//! | step | differs? |
//! |---|---|
//! | `db::tasks::list_queued_for_background` | no — nothing filters on intent |
//! | `runner::preflight` | no |
//! | `runner::ToolTier::for_task` | no |
//! | the checkpoint decision | no |
//! | the spawned argv | no |
//! | `prompt.txt` | **yes** — ~400 characters of English |
//!
//! So the failure is "you got work when you asked for a critique", and because
//! the old `finish_run` doesn't reset the intent either, the flag stays armed
//! and the review happens one cycle late — against a *fresher* handoff. That is
//! graceful degradation, which is why intent does not need `executor`'s
//! two-column encoding.
//!
//! # The condition that makes it safe, which a future change can break
//!
//! **This holds only while no Rust code derives a capability from
//! [`BackgroundIntent`].** Not the [`ToolTier`], not the repo gate, not the
//! checkpoint decision. A review *preselects* the read-only tier **in the UI
//! and nowhere else**: the frontend writes `bg_writes_allowed = false` as a real
//! patch on the arming gesture, so what reaches the database is an ordinary
//! tier the user can override.
//!
//! The next reader will see "review preselects read-only" and reach to harden
//! it in Rust — `if intent == Review { tier = ReadOnly }` looks like closing a
//! hole. It opens one: a pre-v16 runner would then compute a *different* tier
//! than a v16 runner for the same row, which is invariant 15's exact shape, and
//! this module's argument collapses. Pinned by
//! `db::schema::tests::a_legacy_runner_still_claims_a_review_task_and_would_run_it_as_work`,
//! which deliberately asserts the OPPOSITE of its `..._cannot_claim_a_codex_task`
//! twin — the two side by side are what make "one column here, two columns
//! there" legible rather than looking like an inconsistency to tidy up.
//!
//! # The third variant is MACHINE-ONLY
//!
//! `Check` (Release 2 of tripwires) is what an `agent:<condition>` tripwire
//! evaluation is recorded as on `task_runs.intent`. It is never a value of
//! `tasks.bg_intent`: the picker does not offer it, the MCP cannot set it, and
//! [`crate::db::tasks::update`] refuses a patch carrying it. A check is driven
//! by `tripwire_runtime` — the lease is the claim — and never by
//! `status = 'queued'`, so a pre-R2 runner can neither produce nor drain one,
//! and the one-column argument above still holds: the only thing an old binary
//! does with a `check` row is read it back as `Work` and print it as a run.
//!
//! **The condition above is deliberately NOT what bounds a check.** A check's
//! read-only tier, its tool grant and its token ceiling are all pinned by the
//! RUNNER'S OWN LANE (`runner::tripwire::agent_pass`), which knows it is
//! evaluating a tripwire because it was the thing that decided to. Nothing
//! reads `intent == Check` off a row to decide a capability, which is what
//! keeps this consistent with the rule that intent never decides one.
//!
//! [`ToolTier`]: crate::runner::ToolTier

use serde::{Deserialize, Serialize};

/// What the next background dispatch of a task is for.
///
/// Deliberately an enum rather than a free string, for the same reason as
/// [`BackgroundExecutor`]: it is written into the DB and it selects which
/// framing an unattended agent is handed, so "reveiw" must be rejected at the
/// boundary rather than becoming a run that silently does the wrong job.
///
/// [`BackgroundExecutor`]: crate::executor::BackgroundExecutor
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundIntent {
    /// Do the task. The default, and what every pre-v16 task and run is.
    #[default]
    Work,
    /// Audit the most recent handoff instead of continuing the work.
    Review,
    /// Evaluate an `agent:` tripwire — a bounded, read-only run whose final
    /// message is a VERDICT, not a handoff. Machine-only: see the module docs.
    Check,
    /// Say what a tripped tripwire's evidence MEANS — a tool-less run whose
    /// final message is an interpretation, not a handoff. Machine-only, for
    /// the same reasons as `Check`, and driven by `OnTrip::Interpret` rather
    /// than by `status = 'queued'`.
    Interpret,
}

impl BackgroundIntent {
    pub const ALL: [BackgroundIntent; 4] =
        [Self::Work, Self::Review, Self::Check, Self::Interpret];

    /// The intents a PERSON may arm on a task. `Check` and `Interpret` are
    /// absent on purpose — they are what the runner records, never what a
    /// caller chooses.
    pub const USER_SELECTABLE: [BackgroundIntent; 2] = [Self::Work, Self::Review];

    /// The value stored in `tasks.bg_intent` / `task_runs.intent`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Review => "review",
            Self::Check => "check",
            Self::Interpret => "interpret",
        }
    }

    /// Human label for a control or a badge.
    ///
    /// "Continue" rather than "Work" because the control is only ever shown on a
    /// task that has already been run — there is nothing to review otherwise —
    /// so the honest opposite of "audit the last handoff" is "carry on from it",
    /// not "do the task", which is what the user thinks they already asked for.
    pub fn label(self) -> &'static str {
        match self {
            Self::Work => "Continue",
            Self::Review => "Review",
            Self::Check => "Check",
            Self::Interpret => "Interpret",
        }
    }

    /// Strict parse, for anything a caller typed.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "work" => Ok(Self::Work),
            "review" => Ok(Self::Review),
            "check" => Ok(Self::Check),
            "interpret" => Ok(Self::Interpret),
            other => Err(format!(
                "unknown background intent '{other}' (expected one of {})",
                Self::ALL.iter().map(|i| i.as_str()).collect::<Vec<_>>().join(", ")
            )),
        }
    }

    /// Lenient parse, for a value already in the database.
    ///
    /// Falls back to `Work` on anything unrecognized rather than failing the
    /// read — one bad row must not make `list_tasks` error and render the whole
    /// board empty. Unlike [`BackgroundExecutor::from_db`], the fallback needs
    /// no accompanying safety argument: `Work` is what the row would have been
    /// before this column existed, and a task framed as work when it wanted a
    /// critique is the same degradation the module docs already accept.
    ///
    /// [`BackgroundExecutor::from_db`]: crate::executor::BackgroundExecutor::from_db
    pub fn from_db(raw: &str) -> Self {
        Self::parse(raw).unwrap_or_else(|_| {
            log::warn!("unknown bg_intent '{raw}' in the database; treating it as work");
            Self::Work
        })
    }

    pub fn is_review(self) -> bool {
        self == Self::Review
    }

    /// A tripwire evaluation. A verdict is not a handoff: `history_block` and
    /// `reviewable_run` skip these, and `after_run_patch` never sees one.
    pub fn is_check(self) -> bool {
        self == Self::Check
    }

    /// A run the RUNNER started off a tripwire rather than off the queue, and
    /// whose final message is therefore **not a handoff**.
    ///
    /// This, not [`Self::is_check`], is what `history_block` and
    /// `reviewable_run` filter on. A verdict replayed as "the handoff" would
    /// hand the next agent a one-line judgment in place of the work it was
    /// supposed to continue; an interpretation replayed the same way would
    /// hand it a reading of a shell script's output. Both already live on the
    /// task as comments under their own author, which is where they belong.
    ///
    /// It also names the lane the daily token SUB-CAP covers
    /// (`db::runs::tripwire_tokens_today`): a tripwire that spends is a
    /// tripwire that spends, whichever of the two it was.
    pub fn is_tripwire_run(self) -> bool {
        matches!(self, Self::Check | Self::Interpret)
    }
}

impl std::fmt::Display for BackgroundIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl rusqlite::ToSql for BackgroundIntent {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::from(self.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_db_representation() {
        for i in BackgroundIntent::ALL {
            assert_eq!(BackgroundIntent::from_db(i.as_str()), i);
            assert_eq!(BackgroundIntent::parse(i.as_str()).unwrap(), i);
        }
    }

    #[test]
    fn parse_is_strict_but_case_insensitive() {
        assert_eq!(BackgroundIntent::parse("REVIEW").unwrap(), BackgroundIntent::Review);
        assert_eq!(BackgroundIntent::parse(" review ").unwrap(), BackgroundIntent::Review);
        assert!(BackgroundIntent::parse("reveiw").is_err());
        assert!(BackgroundIntent::parse("").is_err());
    }

    #[test]
    fn an_unknown_stored_value_reads_as_work_rather_than_failing() {
        assert_eq!(BackgroundIntent::from_db("audit"), BackgroundIntent::Work);
    }

    /// `check` is on the run row and nowhere a person chooses from.
    #[test]
    fn check_is_readable_but_not_user_selectable() {
        assert_eq!(BackgroundIntent::from_db("check"), BackgroundIntent::Check);
        assert!(BackgroundIntent::Check.is_check());
        assert!(!BackgroundIntent::USER_SELECTABLE.contains(&BackgroundIntent::Check));
        assert!(BackgroundIntent::ALL.contains(&BackgroundIntent::Check));
    }

    /// The safe value and the default value must coincide — that is what makes
    /// `DEFAULT 'work'` a complete migration with nothing to backfill.
    #[test]
    fn the_default_is_work() {
        assert_eq!(BackgroundIntent::default(), BackgroundIntent::Work);
        assert_eq!(BackgroundIntent::default().as_str(), "work");
    }
}
