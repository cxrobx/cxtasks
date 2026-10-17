//! Bearer-token auth, and the scope split.
//!
//! # Why this is not "just a task list"
//!
//! `queue_background` starts an unattended agent with whatever tool grant the
//! task carries, in whatever repo it names. This endpoint is remote code
//! execution with a nice UI, and losing a phone must not hand somebody the
//! portfolio. Hence two tokens rather than one.

use axum::http::{header, Request, StatusCode};
use subtle::ConstantTimeEq;

/// What a caller is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Everything, including queueing agents. For the Mac.
    Full,
    /// Read and edit tasks, and queue agents. For the phone.
    Board,
}

impl Scope {
    /// Verbs a `board` token may not reach.
    ///
    /// # Why starting an agent is no longer one of them
    ///
    /// It was, and the reasoning stands on its own terms: a lost phone that can
    /// queue a write-tier run against a live repo is not recoverable the way an
    /// edited task is. Chris weighed that against the feature it blocks —
    /// filing a request from the phone and having it WORKED, not just captured
    /// — and chose the feature (2026-09-19), knowing the trade.
    ///
    /// What still holds the line, so this is narrower than it looks: a run's
    /// tool grant comes from the task's own tier and defaults to
    /// [`ToolTier::ReadOnly`][tier], the one tier that provably cannot mutate a
    /// repo; `intake` only arms what the caller explicitly armed; and the
    /// budget verbs below are still refused, so a phone cannot raise its own
    /// spend ceiling.
    ///
    /// Reverting this is one line. If it is reverted, revert
    /// `a_board_token_may_start_an_agent` with it — a test asserting the old
    /// behaviour is what makes the decision legible next time.
    ///
    /// [tier]: cxtasks_core::runner::ToolTier
    pub fn may_call(self, cmd: &str) -> bool {
        match self {
            Scope::Full => true,
            Scope::Board => !matches!(cmd, "save_budget_settings" | "reset_budget_window"),
        }
    }
}

/// Compare in constant time.
///
/// A byte-at-a-time `==` leaks the token's prefix through timing, and this one
/// is guessable at leisure over a tailnet.
fn same(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

pub fn scope_for<B>(req: &Request<B>, full: &str, board: Option<&str>) -> Option<Scope> {
    let raw = req.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    let presented = raw.strip_prefix("Bearer ")?.trim();
    if same(presented, full) {
        return Some(Scope::Full);
    }
    if let Some(board) = board {
        if same(presented, board) {
            return Some(Scope::Board);
        }
    }
    None
}

pub const UNAUTHORIZED: StatusCode = StatusCode::UNAUTHORIZED;

#[cfg(test)]
mod tests {
    use super::*;

    /// The board scope still refuses to spend money.
    ///
    /// A lost phone must not be able to raise its own ceiling. This is the half
    /// of the original split that survived; see `may_call` for why the other
    /// half was given up deliberately.
    #[test]
    fn a_board_token_cannot_spend_money() {
        for denied in ["save_budget_settings", "reset_budget_window"] {
            assert!(!Scope::Board.may_call(denied), "{denied} must be denied to board");
            assert!(Scope::Full.may_call(denied), "{denied} must be allowed to full");
        }
    }

    /// ...but it MAY start an agent, which is a deliberate reversal.
    ///
    /// Pinned so that reverting the decision has to come here and say so,
    /// rather than happening as a side effect of someone tightening `may_call`
    /// and finding nothing that objected.
    #[test]
    fn a_board_token_may_start_an_agent() {
        for allowed in [
            "intake", "intake_route", "intake_resolve", "intake_places", "queue_background",
            // Starting a project from the phone is the same decision as starting
            // an agent from it: both were weighed and allowed on purpose.
            "create_project", "add_project_repo",
        ] {
            assert!(Scope::Board.may_call(allowed), "{allowed} must work from the phone");
        }
    }

    /// ...but it is a task app, so it must still be able to do task things.
    ///
    /// A scope that denies too much is a scope nobody uses, and the failure
    /// mode is Chris putting the full token on his phone.
    #[test]
    fn a_board_token_can_still_run_the_board() {
        for allowed in [
            "list_tasks", "get_task", "create_task", "update_task", "complete_task",
            "trash_task", "add_comment", "set_task_tags", "view_counts", "project_board",
        ] {
            assert!(Scope::Board.may_call(allowed), "{allowed} must work from the phone");
        }
    }

    /// A wrong token is a wrong token, whatever its shape.
    #[test]
    fn only_an_exact_bearer_token_matches() {
        let full = "f".repeat(64);
        let board = "b".repeat(64);
        let req = |v: &str| {
            axum::http::Request::builder()
                .header(axum::http::header::AUTHORIZATION, v)
                .body(())
                .unwrap()
        };
        assert_eq!(scope_for(&req(&format!("Bearer {full}")), &full, Some(&board)), Some(Scope::Full));
        assert_eq!(scope_for(&req(&format!("Bearer {board}")), &full, Some(&board)), Some(Scope::Board));
        assert_eq!(scope_for(&req(&format!("Bearer {}", "f".repeat(63))), &full, Some(&board)), None);
        assert_eq!(scope_for(&req(&full), &full, Some(&board)), None, "the scheme is required");
        assert_eq!(scope_for(&req("Bearer "), &full, Some(&board)), None);
        // No board token configured: the board value must not become a skeleton key.
        assert_eq!(scope_for(&req(&format!("Bearer {board}")), &full, None), None);
    }

    /// The comparison is constant-time. Asserting the property is not possible
    /// here, so assert the thing that guarantees it: `same` is the only
    /// comparison, and it goes through `subtle`.
    #[test]
    fn the_comparison_rejects_a_prefix_match() {
        let full = "abcdef".repeat(8);
        assert!(same(&full, &full));
        assert!(!same(&full, &full[..full.len() - 1]));
        assert!(!same(&full[..4], &full));
    }
}
