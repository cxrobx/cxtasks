//! A task outcome is separate from the agent process exiting successfully.
//! Applies to ordinary work of any kind; reviews and tripwire verdicts have
//! their own contracts. The report is the agent's evidence for HUMAN review,
//! never an independent certification that a task is done.

use serde::Deserialize;

pub const MARKER: &str = "CXTASKS_RESULT:";
pub const READY: &str = "ready_for_review";
pub const NEEDS_ACTION: &str = "needs_action";
pub const UNVERIFIED: &str = "unverified";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    ReadyForReview,
    NeedsAction,
    NeedsVerification,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    status: Status,
    verified: Vec<String>,
    remaining: Vec<String>,
}

pub struct Assessment {
    pub status: &'static str,
    pub reason: Option<String>,
}

fn unverified(reason: &str) -> Assessment {
    Assessment {
        status: UNVERIFIED,
        reason: Some(reason.into()),
    }
}

/// Fail closed when the report is absent, malformed, or contradicts itself.
/// A ready report still only means the agent supplied evidence for review.
pub fn assess(text: &str) -> Assessment {
    let Some(line) = text.lines().rev().find(|line| !line.trim().is_empty()) else {
        return unverified("Verify the result: the run produced no completion report.");
    };
    let Some(json) = line.trim().strip_prefix(MARKER) else {
        return unverified(
            "Verify the result: the run did not report what was verified and what remains.",
        );
    };
    if json.len() > 32_000
        || text
            .lines()
            .filter(|line| line.trim().starts_with(MARKER))
            .count()
            != 1
    {
        return unverified("Verify the result: the completion report is invalid or ambiguous.");
    }
    let Ok(report) = serde_json::from_str::<Report>(json.trim()) else {
        return unverified("Verify the result: the completion report could not be read.");
    };
    if report.verified.len() > 20
        || report.remaining.len() > 20
        || report
            .verified
            .iter()
            .chain(&report.remaining)
            .any(|item| item.trim().is_empty() || item.chars().count() > 1000)
    {
        return unverified("Verify the result: the completion report contains invalid evidence or remaining steps.");
    }
    let remaining = report
        .remaining
        .iter()
        .map(|item| item.trim())
        .collect::<Vec<_>>()
        .join("; ");
    match report.status {
        // Remaining work outranks a contradictory claim of readiness.
        Status::ReadyForReview | Status::NeedsAction if !remaining.is_empty() => Assessment {
            status: NEEDS_ACTION,
            reason: Some(super::providers::bounded(&remaining, 1000)),
        },
        Status::ReadyForReview if !report.verified.is_empty() => Assessment {
            status: READY,
            reason: None,
        },
        Status::ReadyForReview => {
            unverified("Verify the requested outcome: the run supplied no evidence.")
        }
        Status::NeedsAction => unverified(
            "Identify the remaining work: the run reported incomplete work without a next step.",
        ),
        Status::NeedsVerification => Assessment {
            status: UNVERIFIED,
            reason: Some(if remaining.is_empty() {
                "Verify the requested outcome before accepting this work.".into()
            } else {
                super::providers::bounded(&remaining, 1000)
            }),
        },
    }
}

pub fn label(status: &str) -> &str {
    match status {
        READY => "Ready for review",
        NEEDS_ACTION => "Needs action",
        UNVERIFIED => "Needs verification",
        "succeeded" => "Run finished",
        other => other,
    }
}

/// Shared by both providers. Evidence depends on the TASK, never its tool tier.
pub const EXPECTATIONS: &str = "\nCOMPLETION CONTRACT:\n\
Read the task's notes and subtasks before deciding its scope; use get_task when available. \
Identify the requested deliverable and the evidence needed to check it. Keep that goal even \
when your permissions only allow investigation: a plan does not complete a task requesting action.\n\n\
Before ending, compare the result with the whole request and relevant subtasks. Use evidence \
appropriate to this task: sources and reasoning for research, coverage and constraints for a \
plan, saved artifacts for a document, observed behavior for a change, or confirmation for a \
requested action. Tests, installation, publishing, sending, and external actions are required \
only when the request calls for them; do not invent extra deliverables.\n\n\
Passing a build, mocked checks, or preparation alone does not verify a requested real-world \
outcome. Do all authorized work you can. If a required step needs unavailable tools, access, \
approval, or human interaction, preserve the result and identify that exact step and why it \
remains. Never claim an action happened without evidence. Do not bypass your permissions.\n";

pub const CONTRACT: &str = "\n\
End your FINAL MESSAGE with exactly one plain line, outside a code fence:\n\
CXTASKS_RESULT: {\"status\":\"ready_for_review\",\"verified\":[\"Specific evidence for the requested outcome\"],\"remaining\":[]}\n\
Use status ready_for_review only when all requested work is finished, evidence is present, \
and remaining is empty. This requests human review; it does not mark the task done. Use \
needs_action when required work remains, with concrete remaining steps. Use needs_verification \
when the result is prepared but a required check remains, listing that check in remaining. \
verified lists only what you actually checked (it may be empty for incomplete work). Each \
list may contain at most 20 short strings. The opening sentence must match this disposition: \
say what remains up front when the task is incomplete.\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exit_without_evidence_cannot_claim_task_success() {
        for text in [
            "",
            "Implemented everything. Tests pass.",
            "CXTASKS_RESULT: broken",
            "CXTASKS_RESULT: {\"status\":\"ready_for_review\",\"verified\":[],\"remaining\":[]}",
        ] {
            assert_eq!(assess(text).status, UNVERIFIED, "{text}");
        }
    }

    #[test]
    fn unfinished_delivery_outweighs_a_confident_opener_or_ready_claim() {
        let text = "Implemented the feature. All checks pass.\nCXTASKS_RESULT: {\"status\":\"ready_for_review\",\"verified\":[\"Build passed\"],\"remaining\":[\"Install the local app with approved access\",\"Check image paste in the running app\"]}";
        let result = assess(text);
        assert_eq!(result.status, NEEDS_ACTION);
        assert!(result.reason.unwrap().contains("Install the local app"));
    }

    #[test]
    fn research_plans_and_other_work_use_their_own_evidence() {
        for evidence in [
            "Compared the three supplied sources and cited the findings",
            "Checked the plan against the requested dates and budget",
            "Saved the requested report and reviewed its contents",
        ] {
            let text = format!(
                "Result ready.\n{MARKER} {}",
                serde_json::json!({"status":"ready_for_review","verified":[evidence],"remaining":[]})
            );
            assert_eq!(assess(&text).status, READY);
        }
    }

    #[test]
    fn required_checks_and_external_actions_remain_visible() {
        for (status, expected) in [
            ("needs_action", NEEDS_ACTION),
            ("needs_verification", UNVERIFIED),
        ] {
            let text = format!(
                "Prepared the result.\n{MARKER} {}",
                serde_json::json!({"status":status,"verified":[],"remaining":["Confirm access to the requested account"]})
            );
            let result = assess(&text);
            assert_eq!(result.status, expected);
            assert_eq!(
                result.reason.as_deref(),
                Some("Confirm access to the requested account")
            );
        }
    }

    #[test]
    fn ambiguous_or_malformed_reports_do_not_become_ready() {
        let ready = format!("{MARKER} {{\"status\":\"ready_for_review\",\"verified\":[\"Checked\"],\"remaining\":[]}}");
        for text in [format!("{ready}\n{ready}"), format!("{ready}\nMore work remains."),
            format!("{MARKER} {{\"status\":\"ready_for_review\",\"verified\":[\" \"],\"remaining\":[]}}"),
            format!("{MARKER} {{\"status\":\"needs_action\",\"verified\":[],\"remaining\":[]}}"),
            format!("{MARKER} {{\"status\":\"ready_for_review\",\"remaining\":[]}}"),
            format!("{MARKER} {{\"status\":\"unknown\",\"verified\":[\"Checked\"],\"remaining\":[]}}") ] {
            assert_eq!(assess(&text).status, UNVERIFIED, "{text}");
        }
    }
}
