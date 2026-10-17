//! Runner tests.
//!
//! **No test here may reach a real agent service or spend a single token.**
//! The end-to-end coverage runs against [`fake_codex`], a shell script that
//! speaks the same JSONL protocol as `codex exec` and can be told to fail in
//! each of the ways the real thing does. That is what lets the spawn → drain →
//! parse → record path be exercised for real — process, pipes, timeout, exit
//! code — instead of only the pure pieces around it.

use super::*;
use crate::db::tasks::{create, NewTask};
use crate::executor::BackgroundExecutor;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn settings(max: i64, cap: i64) -> BudgetSettings {
    BudgetSettings {
        max_concurrent: max,
        timeout_minutes: 20,
        daily_run_cap: cap,
        daily_token_cap: 0, // most gate tests aren't about tokens
        run_token_cap: 0,
        max_turns: crate::db::meta::DEFAULT_MAX_TURNS,
        model: crate::db::meta::DEFAULT_MODEL.to_string(),
        tripwire_daily_token_cap: crate::db::meta::DEFAULT_TRIPWIRE_DAILY_TOKEN_CAP,
    }
}

/// Limits for an end-to-end fake-CLI run: no ceilings unless the test is about
/// one, and a short wall clock so a hang fails the test in seconds.
fn limits(seconds: u64) -> RunLimits {
    RunLimits {
        timeout: Duration::from_secs(seconds),
        model: None,
        max_turns: crate::db::meta::DEFAULT_MAX_TURNS,
        token_ceiling: 0,
        mirror: None,
        extra_mcp: Vec::new(),
    }
}

/// A host that constrains nothing, so these tests keep asserting about the
/// GLOBAL budget rather than accidentally about a host ceiling. The per-host
/// ceilings have their own tests below; mixing the two here would mean a
/// change to the local default silently rewrote what every one of these means.
fn cap() -> crate::routing::HostCapability {
    crate::routing::HostCapability::local_default("test-host".into(), "test".into())
}

#[test]
fn token_cap_stops_runs_before_the_run_count_does() {
    let mut s = settings(2, 20);
    s.daily_token_cap = 2_000_000;
    // Only 9 runs in, but they were expensive ones.
    assert_eq!(
        gate(false, &s, &cap(), 0, 9, 2_100_000),
        Gate::TokenCapReached { today: 2_100_000, cap: 2_000_000 }
    );
    // Same run count, cheap runs — still open.
    assert_eq!(gate(false, &s, &cap(), 0, 9, 500_000), Gate::Open { slots: 2 });
}

#[test]
fn a_zero_token_cap_means_unlimited() {
    let mut s = settings(2, 0);
    s.daily_token_cap = 0;
    assert_eq!(gate(false, &s, &cap(), 0, 0, 99_000_000), Gate::Open { slots: 2 });
}

#[test]
fn kill_switch_beats_everything() {
    assert_eq!(gate(true, &settings(2, 20), &cap(), 0, 0, 0), Gate::Paused);
}

#[test]
fn concurrency_limit_is_respected() {
    assert_eq!(gate(false, &settings(2, 20), &cap(), 0, 0, 0), Gate::Open { slots: 2 });
    assert_eq!(gate(false, &settings(2, 20), &cap(), 1, 0, 0), Gate::Open { slots: 1 });
    assert_eq!(
        gate(false, &settings(2, 20), &cap(), 2, 0, 0),
        Gate::AtConcurrencyLimit { running: 2, max: 2 }
    );
    // Never negative, even if somehow over the limit.
    assert_eq!(
        gate(false, &settings(2, 20), &cap(), 5, 0, 0),
        Gate::AtConcurrencyLimit { running: 5, max: 2 }
    );
}

#[test]
fn daily_cap_stops_work_even_with_free_concurrency() {
    // Nothing running, both concurrency slots free — the cap still wins.
    assert_eq!(
        gate(false, &settings(2, 5), &cap(), 0, 5, 0),
        Gate::DailyCapReached { today: 5, cap: 5 }
    );
    // Already over the cap (settings lowered mid-day) must also stop.
    assert_eq!(
        gate(false, &settings(2, 5), &cap(), 0, 9, 0),
        Gate::DailyCapReached { today: 9, cap: 5 }
    );
}

#[test]
fn daily_cap_clamps_the_slot_count_near_the_limit() {
    // 2 concurrent slots free, but only 1 run left in today's budget.
    assert_eq!(gate(false, &settings(2, 5), &cap(), 0, 4, 0), Gate::Open { slots: 1 });
}

#[test]
fn a_zero_daily_cap_means_unlimited_not_blocked() {
    assert_eq!(gate(false, &settings(2, 0), &cap(), 0, 999, 0), Gate::Open { slots: 2 });
}

/// The concurrency counters are executor-blind, so a mixed queue cannot exceed
/// what a single-agent queue could. A per-executor budget would let "2 max"
/// quietly become four processes.
#[test]
fn one_concurrency_budget_is_shared_across_both_executors() {
    let s = settings(2, 20);
    // Two Claude runs in flight leaves no room for a Codex one.
    assert_eq!(
        gate(false, &s, &cap(), 2, 0, 0),
        Gate::AtConcurrencyLimit { running: 2, max: 2 },
        "a Codex run must not get its own slot on top of Claude's"
    );
    // `count_running_background` is likewise not filtered by executor — one
    // running run of either kind consumes one of the two slots.
    assert_eq!(gate(false, &s, &cap(), 1, 0, 0), Gate::Open { slots: 1 });
}

/// A host ceiling NARROWS, never widens. `max_concurrent` on a host is the
/// smaller of the two, so configuring a host cannot buy slots the global
/// setting never granted.
#[test]
fn a_host_ceiling_can_only_lower_the_concurrency_limit() {
    let s = settings(4, 20);
    let mut c = cap();
    c.max_concurrent = 1;
    assert_eq!(gate(false, &s, &c, 0, 0, 0), Gate::Open { slots: 1 });
    assert_eq!(
        gate(false, &s, &c, 1, 0, 0),
        Gate::AtConcurrencyLimit { running: 1, max: 1 }
    );

    // ...and a host asking for MORE than the global setting gets the global one.
    c.max_concurrent = 99;
    assert_eq!(gate(false, &s, &c, 0, 0, 0), Gate::Open { slots: 4 });
}

/// 0 means "no host opinion". A host that has never been configured must
/// behave exactly as it did before the column existed.
#[test]
fn a_host_with_no_concurrency_opinion_uses_the_global_setting() {
    let s = settings(3, 20);
    let mut c = cap();
    c.max_concurrent = 0;
    assert_eq!(gate(false, &s, &c, 0, 0, 0), Gate::Open { slots: 3 });
}

/// Headroom RESERVES budget, it never creates any. A host at 50% stops at half
/// the pool; the pool itself is unchanged, so two hosts cannot together spend
/// more than one could.
#[test]
fn headroom_reserves_a_share_of_one_pool_rather_than_creating_a_second() {
    let mut s = settings(2, 0);
    s.daily_token_cap = 10_000_000;
    let mut nas = cap();
    nas.headroom_pct = 50;

    // The Mac, with no headroom, may spend up to the whole pool.
    assert_eq!(gate(false, &s, &cap(), 0, 0, 6_000_000), Gate::Open { slots: 2 });
    // The NAS, at the same moment, is already past its share.
    assert_eq!(
        gate(false, &s, &nas, 0, 0, 6_000_000),
        Gate::HostHeadroomReached { today: 6_000_000, ceiling: 5_000_000 }
    );
    // Below its share it runs normally.
    assert_eq!(gate(false, &s, &nas, 0, 0, 4_000_000), Gate::Open { slots: 2 });
}

/// Global guards are reported BEFORE host ones. A host that says "I am at my
/// share of a cap that is already gone" sends the reader to fix the wrong
/// number.
#[test]
fn the_global_cap_is_reported_before_a_host_ceiling() {
    let mut s = settings(2, 0);
    s.daily_token_cap = 1_000_000;
    let mut nas = cap();
    nas.headroom_pct = 50;
    assert_eq!(
        gate(false, &s, &nas, 0, 0, 2_000_000),
        Gate::TokenCapReached { today: 2_000_000, cap: 1_000_000 },
        "the whole pool is spent; saying 'my share is spent' names the wrong cap"
    );
}

/// Gotcha #31, applied to hosts: a runner that declines must say WHICH guard
/// fired. Three different reasons a host might not run, three distinct answers.
#[test]
fn a_host_that_declines_says_which_of_the_three_reasons_it_was() {
    let s = settings(2, 20);

    let mut off = cap();
    off.enabled = false;
    assert_eq!(gate(false, &s, &off, 0, 0, 0), Gate::HostDisabled);

    let mut paused = cap();
    paused.paused = true;
    assert_eq!(gate(false, &s, &paused, 0, 0, 0), Gate::HostPaused);

    let mut no_agent = cap();
    no_agent.executors_ready.clear();
    // ...and it has to have LOOKED. An empty ready-list with no heartbeat means
    // nobody checked yet, which is taken at its word rather than as a refusal.
    no_agent.last_seen_at = Some(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string());
    assert_eq!(gate(false, &s, &no_agent, 0, 0, 0), Gate::HostDisabled);

    // And the GLOBAL switch still beats all of them, so "everything is off"
    // reads as one decision rather than as whichever host answered first.
    assert_eq!(gate(true, &s, &off, 0, 0, 0), Gate::Paused);
}

const TIERS: [ToolTier; 3] = [ToolTier::ReadOnly, ToolTier::Shell, ToolTier::Write];

fn split(s: &str) -> Vec<&str> {
    s.split(',').filter(|t| !t.is_empty()).collect()
}

/// The property the whole gate rests on: for every tier, each dangerous
/// tool is in exactly one of `allowed()`/`denied()` — never in both, and
/// never in neither.
///
/// Neither is the failure mode that shipped: a tool merely left out of
/// `allowed()` is NOT denied, because `--allowedTools` is additive over the
/// user's `~/.claude/settings.json`. So a new entry in `DANGEROUS_TOOLS`
/// that nobody remembers to deny would be silently permitted.
#[test]
fn every_dangerous_tool_is_explicitly_allowed_or_explicitly_denied() {
    for tier in TIERS {
        let allowed = split(tier.allowed());
        let raw = tier.denied();
        let denied = split(&raw);
        for tool in DANGEROUS_TOOLS {
            let a = allowed.contains(tool);
            let d = denied.contains(tool);
            assert!(
                a ^ d,
                "{tier:?}: {tool} is {} — it must be in exactly one list",
                if a { "in BOTH lists" } else { "in NEITHER list" }
            );
        }
    }
}

#[test]
fn read_only_denies_execution_and_writing() {
    let raw = ToolTier::ReadOnly.denied();
    let denied = split(&raw);
    for banned in ["Bash", "BashOutput", "KillShell", "Edit", "Write", "NotebookEdit"] {
        assert!(
            denied.contains(&banned),
            "read-only must explicitly DENY {banned} — omission is not denial: {denied:?}"
        );
    }
    assert!(ToolTier::ReadOnly.allowed().contains("Read"));
    assert!(ToolTier::ReadOnly.allowed().contains("Grep"));
}

/// The tier that exists so a repo sweep can run `git` without also being
/// handed `Edit`/`Write`.
#[test]
fn shell_gets_a_shell_but_not_the_editing_tools() {
    let allowed = split(ToolTier::Shell.allowed());
    let raw = ToolTier::Shell.denied();
    let denied = split(&raw);
    assert!(allowed.contains(&"Bash"));
    for banned in ["Edit", "Write", "NotebookEdit"] {
        assert!(denied.contains(&banned), "Shell must deny {banned}");
    }
}

/// An unattended run has no legitimate need for network egress, and is
/// explicitly told nobody is watching — denied at EVERY tier, not just
/// read-only. (Not a complete egress guard above ReadOnly, which still has
/// `curl` via Bash — see `ToolTier::denied`.)
#[test]
fn no_tier_allows_the_network_tools() {
    for tier in TIERS {
        let raw = tier.denied();
        for banned in ["WebFetch", "WebSearch"] {
            assert!(split(&raw).contains(&banned), "{tier:?} must deny {banned}");
        }
    }
}

/// A subagent's tool grants are not this tier's grants, so `Task` is denied
/// everywhere — otherwise a read-only run spawns a helper that isn't.
#[test]
fn no_tier_can_spawn_a_subagent() {
    for tier in TIERS {
        let raw = tier.denied();
        assert!(split(&raw).contains(&"Task"), "{tier:?} must deny Task");
    }
}

/// A shell can write, so `Shell` is checkpointed exactly like `Write`.
/// Gating the snapshot on "has Edit/Write" is the bug `needs_checkpoint`
/// replaces. Provider-independent: Codex's `workspace-write` sandbox is just
/// as mutation-capable as Claude's `Bash`.
#[test]
fn every_mutation_capable_tier_is_checkpointed() {
    assert!(!ToolTier::ReadOnly.needs_checkpoint());
    assert!(ToolTier::Shell.needs_checkpoint());
    assert!(ToolTier::Write.needs_checkpoint());
}

#[test]
fn preflight_refuses_a_task_without_opt_in() {
    let conn = crate::db::schema::test_conn();
    let t = db::tasks::create(
        &conn,
        &db::tasks::NewTask {
            title: "x".into(),
            repo_path: Some("/tmp".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(preflight(&t).unwrap_err().contains("not opted into"));
}

fn write_task(conn: &Connection, dir: &std::path::Path) -> Task {
    db::tasks::create(
        conn,
        &db::tasks::NewTask {
            title: "x".into(),
            repo_path: Some(dir.to_string_lossy().to_string()),
            bg_allowed: Some(true),
            bg_writes_allowed: Some(true),
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn preflight_refuses_writes_outside_a_git_repo() {
    let conn = crate::db::schema::test_conn();
    let dir = std::env::temp_dir().join("cxtasks-preflight-nogit");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let err = preflight(&write_task(&conn, &dir)).unwrap_err();
    assert!(err.contains("not a git repository"), "{err}");
    std::fs::remove_dir_all(&dir).ok();
}

fn init_git_repo(dir: &std::path::Path) {
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(&args)
            .output()
            .unwrap();
    }
}

/// The behaviour change that checkpointing bought: a write-enabled run no
/// longer requires a clean tree. v1 refused here, which made the feature
/// unusable in any repo with work in progress.
#[test]
fn preflight_allows_writes_on_a_dirty_git_repo() {
    let conn = crate::db::schema::test_conn();
    let dir = std::env::temp_dir().join("cxtasks-preflight-dirty");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    init_git_repo(&dir);
    std::fs::write(dir.join("wip.txt"), "uncommitted work\n").unwrap();

    // Claude's preflight also asks whether the CLI is installed; on a machine
    // without it that is a legitimate refusal, so assert on the repo half only.
    match preflight(&write_task(&conn, &dir)) {
        Ok(_) => {}
        Err(e) => assert!(
            !e.contains("git repository"),
            "a dirty tree must be allowed — the checkpoint captures it: {e}"
        ),
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// A shell can write, so it is repo-gated exactly like `Write` — and
/// **refused** rather than quietly downgraded to read-only. A silent
/// downgrade still "succeeds": the task comes back `needs_review` with an
/// answer the agent assembled from whatever it could reach instead.
#[test]
fn preflight_refuses_a_shell_run_outside_a_git_repo() {
    let conn = crate::db::schema::test_conn();
    let dir = std::env::temp_dir().join("cxtasks-preflight-shell-nogit");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let t = db::tasks::create(
        &conn,
        &db::tasks::NewTask {
            title: "x".into(),
            repo_path: Some(dir.to_string_lossy().to_string()),
            bg_allowed: Some(true),
            bg_shell_allowed: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    let err = preflight(&t).unwrap_err();
    assert!(err.contains("not a git repository"), "{err}");
    std::fs::remove_dir_all(&dir).ok();
}

/// Codex is not exempt from the checkpoint rule. Its `workspace-write` sandbox
/// is mutation-capable, so a Codex task at the shell or write tier needs the
/// same git repo for the same reason.
#[test]
fn preflight_refuses_a_codex_write_run_outside_a_git_repo() {
    let conn = crate::db::schema::test_conn();
    let dir = std::env::temp_dir().join("cxtasks-preflight-codex-nogit");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let t = db::tasks::create(
        &conn,
        &db::tasks::NewTask {
            title: "x".into(),
            repo_path: Some(dir.to_string_lossy().to_string()),
            bg_allowed: Some(true),
            bg_executor: Some(BackgroundExecutor::Codex),
            bg_writes_allowed: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    let err = preflight(&t).unwrap_err();
    assert!(err.contains("not a git repository"), "{err}");
    std::fs::remove_dir_all(&dir).ok();
}

/// `bg_writes_allowed` wins over `bg_shell_allowed` — writes already imply
/// a shell, so the pair is a precedence, not a combination. Pinned because
/// the tier is what picks the tool grant, and reading it the other way
/// around would hand a write-enabled task the shell-only list.
#[test]
fn writes_outrank_shell_when_both_are_set() {
    let conn = crate::db::schema::test_conn();
    let t = db::tasks::create(
        &conn,
        &db::tasks::NewTask {
            title: "x".into(),
            bg_allowed: Some(true),
            bg_shell_allowed: Some(true),
            bg_writes_allowed: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(ToolTier::for_task(&t), ToolTier::Write);
}

// ── model resolution ──────────────────────────────────────────────────

#[test]
fn claude_always_gets_an_explicit_model_and_codex_does_not() {
    let conn = crate::db::schema::test_conn();
    let s = settings(2, 20);

    let claude = create(
        &conn,
        &NewTask {
            title: "c".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        model_for(&claude, &s).as_deref(),
        Some(crate::db::meta::DEFAULT_MODEL),
        "an unattended Claude run must be pinned to the cheap default, not the session's"
    );

    let codex = create(
        &conn,
        &NewTask {
            title: "x".into(),
            bg_executor: Some(BackgroundExecutor::Codex),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        model_for(&codex, &s),
        None,
        "Codex with no override must fall through to the CLI's own default, not a \
         hard-coded OpenAI model name"
    );
}

#[test]
fn a_task_level_model_overrides_both_defaults() {
    let conn = crate::db::schema::test_conn();
    let s = settings(2, 20);
    for executor in BackgroundExecutor::ALL {
        let t = create(
            &conn,
            &NewTask {
                title: format!("{executor}"),
                bg_executor: Some(executor),
                model: Some("pinned-model".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(model_for(&t, &s).as_deref(), Some("pinned-model"));
    }
}

// ── per-task run budget (v18) ─────────────────────────────────────────

/// The override wins; absent falls back to the global. Same shape as
/// `model_for`, and the resolved number is what gets recorded on the run row.
#[test]
fn a_task_level_budget_overrides_the_global_and_absence_falls_back() {
    let conn = crate::db::schema::test_conn();
    let s = settings(2, 20);

    let plain = create(
        &conn,
        &NewTask {
            title: "plain".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(timeout_for(&plain, &s), s.timeout_minutes);
    assert_eq!(token_cap_for(&plain, &s), s.run_token_cap);

    let big = create(
        &conn,
        &NewTask {
            title: "big".into(),
            bg_timeout_minutes: Some(60),
            bg_run_token_cap: Some(60_000_000),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(timeout_for(&big, &s), 60);
    assert_eq!(token_cap_for(&big, &s), 60_000_000);
}

/// `0` disables the token ceiling and must NOT be read as "unset" — the
/// difference between "this run has no ceiling" and "this run gets the default
/// 25M" is the whole point of storing it.
#[test]
fn a_zero_token_cap_means_no_ceiling_not_the_default() {
    let conn = crate::db::schema::test_conn();
    let s = BudgetSettings {
        run_token_cap: 25_000_000,
        ..settings(2, 20)
    };
    let t = create(
        &conn,
        &NewTask {
            title: "uncapped".into(),
            bg_run_token_cap: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(token_cap_for(&t, &s), 0, "0 is a value, not an absence");
}

/// The per-task clamp must match the global one. A task allowed to store 500
/// minutes while `budget_settings` clamps to 120 would let the run outlive the
/// clock `sweep_abandoned` judges it by — invariant 20's failure with extra
/// steps. Enforced at the DB boundary as a refusal, so it can't be stored at
/// all rather than being silently clamped somewhere later.
#[test]
fn an_out_of_range_budget_is_refused_rather_than_clamped() {
    let conn = crate::db::schema::test_conn();
    for (timeout, cap) in [(Some(500), None), (Some(0), None), (None, Some(-5))] {
        let err = create(
            &conn,
            &NewTask {
                title: "over".into(),
                bg_timeout_minutes: timeout,
                bg_run_token_cap: cap,
                ..Default::default()
            },
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("must be between"),
            "the refusal has to name the legal range: {msg}"
        );
    }
    // And the bound itself is the global one, not a second opinion.
    assert_eq!(crate::db::meta::MAX_TIMEOUT_MINUTES, 120);
}

// ── the fake Codex CLI ────────────────────────────────────────────────

/// How the fake should behave. Each maps to a failure the real CLI has.
#[derive(Clone, Copy)]
enum FakeMode {
    /// Well-formed JSONL, a final-message file, a clean exit.
    Success,
    NeedsAction,
    /// Valid JSON lines that carry none of the fields we look for.
    Malformed,
    /// `codex login status`-style refusal: nonzero exit, message on stderr.
    AuthFailure,
    /// Never exits. Exercises the wall-clock kill.
    Hang,
    /// Reports a turn's worth of usage, then never exits. Exercises the
    /// **token** kill, which has to fire while the wall clock still has room.
    SpendThenHang,
    /// Emits `turn.failed` and exits nonzero.
    Error,
}

/// Write a stand-in for `codex` that speaks the same protocol.
///
/// A script rather than a mock object on purpose: the parts most likely to
/// break — pipe draining, the timeout kill, exit-code handling, reading the
/// `--output-last-message` file back — only exist when there is a real child
/// process, and a mock would assert that they were skipped correctly.
fn fake_codex(name: &str, mode: FakeMode) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cxtasks-fake-codex-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("codex");

    // Records the argv so a test can assert on what the runner actually passed.
    // Separated by RS (0x1e), NOT newlines: the prompt is multi-line, so a
    // newline-delimited record would split it across "arguments" and every
    // assertion about the prompt would silently be testing its last line.
    let preamble = r#"#!/bin/sh
printf '%s\036' "$@" > "$(dirname "$0")/argv.txt"
# Mimic the real CLI: the value of --output-last-message is the file to write.
out=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--output-last-message" ]; then out="$a"; fi
  prev="$a"
done
"#;

    let body = match mode {
        FakeMode::Success => {
            r#"
echo '{"type":"thread.started","thread_id":"th_fake"}'
echo '{"type":"item.completed","item":{"type":"agent_message","text":"streamed text"}}'
echo '{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50}}'
[ -n "$out" ] && printf '%s\n' 'the final answer' 'CXTASKS_RESULT: {"status":"ready_for_review","verified":["Compared the supplied sources and cited the conclusion"],"remaining":[]}' > "$out"
exit 0
"#
        }
        FakeMode::NeedsAction => r#"
echo '{"type":"thread.started","thread_id":"th_partial"}'
[ -n "$out" ] && printf '%s\n' 'Prepared the requested report.' 'CXTASKS_RESULT: {"status":"needs_action","verified":["Reviewed the saved report"],"remaining":["Send the report using the connected account; this run lacks account access"]}' > "$out"
exit 0
"#,
        FakeMode::Malformed => {
            r#"
echo 'not json at all'
echo '{"unexpected":"shape"}'
echo '{"type":"something.else"}'
exit 0
"#
        }
        FakeMode::AuthFailure => {
            r#"
echo 'Not logged in. Run `codex login`.' >&2
exit 1
"#
        }
        FakeMode::Hang => {
            r#"
echo '{"type":"thread.started","thread_id":"th_hang"}'
while true; do sleep 1; done
"#
        }
        FakeMode::SpendThenHang => {
            // Normalized this is 200 + 800 + 50 = 1050; the naive four-bucket
            // sum would read 1850. The test's ceiling sits between them.
            r#"
echo '{"type":"thread.started","thread_id":"th_spend"}'
echo '{"type":"item.completed","item":{"type":"agent_message","text":"got some way in"}}'
echo '{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50}}'
while true; do sleep 1; done
"#
        }
        FakeMode::Error => {
            r#"
echo '{"type":"thread.started","thread_id":"th_err"}'
echo '{"type":"turn.failed","error":{"message":"model overloaded"}}'
echo 'codex: giving up' >&2
exit 1
"#
        }
    };

    std::fs::write(&path, format!("{preamble}{body}")).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

fn recorded_argv(cli: &std::path::Path) -> Vec<String> {
    let raw = std::fs::read_to_string(cli.parent().unwrap().join("argv.txt")).unwrap_or_default();
    raw.split('\u{1e}')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// A queued Codex task in a real (throwaway) git repo.
fn codex_task(conn: &Connection, name: &str, writes: bool) -> (Task, PathBuf) {
    let dir = std::env::temp_dir().join(format!("cxtasks-codex-repo-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    init_git_repo(&dir);
    let t = create(
        conn,
        &NewTask {
            title: format!("codex {name}"),
            prompt: Some("investigate the thing".into()),
            repo_path: Some(dir.to_string_lossy().to_string()),
            bg_allowed: Some(true),
            bg_executor: Some(BackgroundExecutor::Codex),
            bg_writes_allowed: Some(writes),
            ..Default::default()
        },
    )
    .unwrap();
    (t, dir)
}

#[test]
fn a_successful_codex_run_records_result_session_and_normalized_tokens() {
    let conn = crate::db::schema::test_conn();
    let (task, repo) = codex_task(&conn, "ok", true);
    let cli = fake_codex("ok", FakeMode::Success);

    let outcome = run_one(&task, &repo, Provider::Codex, &cli, &limits(30));

    assert_eq!(outcome.status, "ready_for_review");
    assert_eq!(outcome.executor, BackgroundExecutor::Codex);
    assert_eq!(outcome.session_id.as_deref(), Some("th_fake"));
    assert_eq!(
        outcome.result_text.as_deref(),
        Some("the final answer\nCXTASKS_RESULT: {\"status\":\"ready_for_review\",\"verified\":[\"Compared the supplied sources and cited the conclusion\"],\"remaining\":[]}"),
        "the --output-last-message file outranks the streamed text"
    );
    // Normalized: cached split out of input, no cache-write claim.
    assert_eq!(outcome.input_tokens, Some(200));
    assert_eq!(outcome.cache_read_tokens, Some(800));
    assert_eq!(outcome.output_tokens, Some(50));
    assert_eq!(outcome.cache_write_tokens, None);

    // …and the argv the runner really passed, end to end rather than from the
    // pure builder: it is the layer between them that would drop a flag.
    let args = recorded_argv(&cli);
    assert!(args.contains(&"never".to_string()));
    assert!(args.contains(&"workspace-write".to_string()));
    assert!(args.contains(&"--ephemeral".to_string()));
    assert!(args.contains(&"--ignore-user-config".to_string()));
    assert!(!args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));

    std::fs::remove_dir_all(&repo).ok();
}

/// A run whose output we can't read is a run that produced nothing — it must
/// say so rather than inventing a summary or crashing the runner.
#[test]
fn malformed_codex_output_is_an_honest_empty_result_not_a_panic() {
    let conn = crate::db::schema::test_conn();
    let (task, repo) = codex_task(&conn, "malformed", false);
    let cli = fake_codex("malformed", FakeMode::Malformed);

    let outcome = run_one(&task, &repo, Provider::Codex, &cli, &limits(30));
    assert_eq!(outcome.status, "unverified", "exit 0 with junk output cannot establish task completion");
    assert!(outcome.result_text.as_deref().unwrap().contains("no output"));
    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn a_zero_exit_with_undelivered_work_is_saved_and_held_for_continuation() {
    let conn = crate::db::schema::test_conn();
    let (task, repo) = codex_task(&conn, "unfinished-delivery", false);
    let cli = fake_codex("unfinished-delivery", FakeMode::NeedsAction);
    let run = db::runs::start(
        &conn, &task.id, "background", None, BackgroundExecutor::Codex,
        crate::intent::BackgroundIntent::Work, Some(20),
    ).unwrap();
    let outcome = run_one(&task, &repo, Provider::Codex, &cli, &limits(30));

    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(outcome.status, "needs_action");
    let reason = outcome.stop_detail.as_deref().unwrap();
    assert!(reason.contains("Send the report using the connected account"));
    assert!(outcome.result_text.as_deref().unwrap().starts_with("Needs action: Send the report"));
    db::tasks::update(&conn, &task.id, &db::tasks::TaskPatch {
        status: Some("in_progress".into()), ..Default::default()
    }).unwrap();
    super::finish_run_with_conn(&conn, &task, &run.id, &outcome).unwrap();
    let held = db::tasks::get(&conn, &task.id).unwrap().unwrap();
    assert_eq!(held.status, "todo");
    assert!(held.blocked_reason.as_deref().unwrap().contains("this run lacks account access"));
    let waiting: bool = conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM tasks WHERE id = ?1 AND {})", db::tasks::waiting_sql()),
        [&task.id], |row| row.get(0),
    ).unwrap();
    assert!(waiting, "undelivered work belongs in Waiting rather than review");
    let saved = db::runs::get(&conn, &run.id).unwrap().unwrap();
    assert_eq!(saved.status, "needs_action");
    assert!(saved.finished_at.is_some());
    assert_eq!(saved.result_text, outcome.result_text);
    let comments = db::comments::for_task(&conn, &task.id).unwrap();
    assert!(comments.iter().any(|comment| comment.body.contains("Needs action") && comment.body.contains("Send the report")));
    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn a_claude_result_with_an_unchecked_outcome_needs_verification() {
    let conn = crate::db::schema::test_conn();
    let (task, repo) = codex_task(&conn, "unchecked-outcome", false);
    let cli = fake_claude_check("unchecked-outcome", "Prepared the itinerary.\nCXTASKS_RESULT: {\"status\":\"needs_verification\",\"verified\":[\"Compared the supplied dates and budget\"],\"remaining\":[\"Confirm opening hours with the venue; this run has no network access\"]}");
    let outcome = run_one(&task, &repo, Provider::Claude, &cli, &limits(30));
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(outcome.status, "unverified");
    assert!(outcome.result_text.as_deref().unwrap().starts_with("Needs verification: Confirm opening hours"));
    let patch = super::after_run_patch(Some("in_progress"), &outcome.status, outcome.stop_detail.as_deref()).unwrap();
    assert_eq!(patch.status.as_deref(), Some("todo"));
    assert!(patch.blocked_reason.as_ref().unwrap().as_deref().unwrap().contains("Confirm opening hours"));
    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn a_review_keeps_its_verdict_contract_instead_of_becoming_unverified_work() {
    let conn = crate::db::schema::test_conn();
    let (mut task, repo) = codex_task(&conn, "review-verdict", false);
    task.bg_intent = crate::intent::BackgroundIntent::Review;
    let cli = fake_claude_check("review-verdict", "The report cites the provided sources and addresses the requested comparison.");
    let outcome = run_one(&task, &repo, Provider::Claude, &cli, &limits(30));
    assert_eq!(outcome.status, "succeeded");
    assert!(outcome.stop_detail.is_none());
    let prompt = recorded_argv(&cli).last().unwrap().clone();
    assert!(!prompt.contains("CXTASKS_RESULT:"));
    std::fs::remove_dir_all(&repo).ok();
}

/// The CLI exiting nonzero with a message on stderr — what a logged-out Codex
/// looks like if it slips past preflight.
#[test]
fn a_nonzero_exit_fails_the_run_and_keeps_stderr() {
    let conn = crate::db::schema::test_conn();
    let (task, repo) = codex_task(&conn, "auth", false);
    let cli = fake_codex("auth", FakeMode::AuthFailure);

    let outcome = run_one(&task, &repo, Provider::Codex, &cli, &limits(30));
    assert_eq!(outcome.status, "failed");
    assert_eq!(outcome.exit_code, Some(1));
    let text = outcome.result_text.unwrap();
    assert!(text.contains("Not logged in"), "stderr must survive: {text}");
    std::fs::remove_dir_all(&repo).ok();
}

/// `turn.failed` on a stream is a failure even though the process is still
/// running normally at that point — the run must not be recorded as succeeded
/// just because the CLI managed to exit tidily.
#[test]
fn an_in_band_failure_event_fails_the_run() {
    let conn = crate::db::schema::test_conn();
    let (task, repo) = codex_task(&conn, "err", false);
    let cli = fake_codex("err", FakeMode::Error);

    let outcome = run_one(&task, &repo, Provider::Codex, &cli, &limits(30));
    assert_eq!(outcome.status, "failed");
    assert!(outcome.result_text.unwrap().contains("model overloaded"));
    std::fs::remove_dir_all(&repo).ok();
}

/// The timeout has to KILL the child, not just stop waiting for it. A run that
/// is merely abandoned holds its concurrency slot until the process dies on its
/// own — which, for a hung agent, is never.
#[test]
fn the_timeout_kills_a_hung_child() {
    let conn = crate::db::schema::test_conn();
    let (task, repo) = codex_task(&conn, "hang", false);
    let cli = fake_codex("hang", FakeMode::Hang);

    let started = std::time::Instant::now();
    let outcome = run_one(&task, &repo, Provider::Codex, &cli, &limits(2));
    let elapsed = started.elapsed();

    assert_eq!(outcome.status, "timeout");
    assert!(
        elapsed < Duration::from_secs(20),
        "run_one returned after {elapsed:?} — the child was not killed"
    );
    // The partial stream still counts: a session id we captured before the kill
    // is how the run stays traceable.
    assert_eq!(outcome.session_id.as_deref(), Some("th_hang"));
    std::fs::remove_dir_all(&repo).ok();
}

/// The per-run token ceiling has to KILL, for the same reason the wall clock
/// does — and it has to fire while the wall clock still has plenty of room, or
/// it is just a slower timeout.
///
/// **This necessarily exercises the Codex parse path** (there is no
/// `fake_claude`), which is the one that needs `live_total`'s normalization: the
/// stream reports `input_tokens` including its cached prefix. So the ceiling is
/// set at 1500 — above the honest 1050 this run costs, below the 1850 a naive
/// four-bucket sum would read. A double-counting implementation kills here and
/// fails this test; the correct one runs on and is killed by the wall clock.
/// The second half then pins that the recorded spend matches what the daily
/// counter will charge, which is the number the guard would otherwise misreport.
#[test]
fn the_token_ceiling_kills_a_run_and_records_what_the_budget_will_charge() {
    let conn = crate::db::schema::test_conn();

    let (task, repo) = codex_task(&conn, "overspend-naive", false);
    let cli = fake_codex("overspend-naive", FakeMode::SpendThenHang);
    let mut over_the_naive_sum = limits(4);
    over_the_naive_sum.token_ceiling = 1500;
    let outcome = run_one(&task, &repo, Provider::Codex, &cli, &over_the_naive_sum);
    assert_eq!(
        outcome.status, "timeout",
        "1050 normalized tokens must not trip a 1500 ceiling — a sum that reads \
         1850 here is counting the cached prefix twice"
    );
    std::fs::remove_dir_all(&repo).ok();

    let (task, repo) = codex_task(&conn, "overspend", false);
    let cli = fake_codex("overspend", FakeMode::SpendThenHang);
    let mut over = limits(60);
    over.token_ceiling = 1000;

    let started = std::time::Instant::now();
    let outcome = run_one(&task, &repo, Provider::Codex, &cli, &over);
    let elapsed = started.elapsed();

    assert_eq!(outcome.status, "budget_stopped");
    assert!(
        elapsed < Duration::from_secs(30),
        "returned after {elapsed:?} — the child was not killed, only abandoned"
    );

    // The recorded buckets are the normalized ones, so `total_tokens` on the
    // row equals the `spent` the guard reported. A guard that misreports its
    // own scope is worse than no guard (gotcha #30).
    let charged = outcome.input_tokens.unwrap_or(0)
        + outcome.cache_read_tokens.unwrap_or(0)
        + outcome.cache_write_tokens.unwrap_or(0)
        + outcome.output_tokens.unwrap_or(0);
    assert_eq!(charged, 1050);
    let text = outcome.result_text.clone().unwrap();
    assert!(text.contains("1050 of 1000 tokens"), "{text}");
    assert!(
        text.contains("got some way in"),
        "the partial narration is the only thing a stopped run leaves: {text}"
    );
    // And the task-facing sentence names the guard rather than saying "failed".
    let detail = outcome.stop_detail.clone().unwrap();
    assert!(detail.contains("per-run token ceiling"), "{detail}");

    std::fs::remove_dir_all(&repo).ok();
}

/// **A spend guard must not immediately spend more.** The absence of a second
/// spawn is not something a reader would notice was deliberate, so the decision
/// is a named function with this assertion on it rather than an `if` buried in
/// `run_one_inner`.
///
/// The timeout case is the other half: after a wall-clock kill the transcript
/// can hold a `tool_use` with no matching `tool_result`, which makes `--resume`
/// a 400 — not a thing to discover at the end of a twenty-minute run.
#[test]
fn only_a_turn_cutoff_salvages_a_handoff() {
    let cutoff = Some(providers::StopReason::MaxTurns {
        limit: 150,
        reached: 151,
    });

    assert!(super::should_salvage(None, cutoff));

    assert!(
        !super::should_salvage(
            Some(process::KillReason::TokenCeiling {
                spent: 26_000_000,
                cap: 25_000_000
            }),
            cutoff
        ),
        "spending another turn straight after a spend guard fires is incoherent"
    );
    assert!(
        !super::should_salvage(Some(process::KillReason::Timeout), cutoff),
        "a killed run's transcript may not be resumable at all"
    );
    assert!(
        !super::should_salvage(None, None),
        "a run that ended on its own already said what it had to say"
    );
}

/// The scratch dir is per **task**, not per run, so yesterday's final message
/// is sitting there when today's run starts. Reporting it as this run's answer
/// is the worst kind of wrong: confident, plausible, and stale.
#[test]
fn a_stale_last_message_file_is_not_reported_as_this_runs_answer() {
    let conn = crate::db::schema::test_conn();
    let (task, repo) = codex_task(&conn, "stale", false);
    let scratch = crate::dispatch::scratch_dir_for(&task);
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(
        scratch.join(providers::codex::LAST_MESSAGE_FILE),
        "an answer from a previous run",
    )
    .unwrap();

    let cli = fake_codex("stale", FakeMode::Malformed); // writes no file
    let outcome = run_one(&task, &repo, Provider::Codex, &cli, &limits(30));
    let text = outcome.result_text.unwrap();
    assert!(
        !text.contains("previous run"),
        "a stale result file leaked into a new run: {text}"
    );
    std::fs::remove_dir_all(&repo).ok();
    std::fs::remove_dir_all(&scratch).ok();
}

/// The read-only tier must reach the process as `--sandbox read-only`, checked
/// through the real spawn rather than only through the command builder.
#[test]
fn a_read_only_codex_task_spawns_a_read_only_sandbox() {
    let conn = crate::db::schema::test_conn();
    let (task, repo) = codex_task(&conn, "ro", false);
    let cli = fake_codex("ro", FakeMode::Success);

    run_one(&task, &repo, Provider::Codex, &cli, &limits(30));

    let args = recorded_argv(&cli);
    let at = args.iter().position(|a| a == "--sandbox").expect("no --sandbox");
    assert_eq!(args[at + 1], "read-only");
    assert!(args.contains(&"--skip-git-repo-check".to_string()));
    std::fs::remove_dir_all(&repo).ok();
}

/// The prompt the process receives must be the CODEX prompt, and its write-back
/// claim must match the same sibling-binary check used by the command builder.
#[test]
fn a_codex_run_receives_the_codex_prompt() {
    let conn = crate::db::schema::test_conn();
    let (task, repo) = codex_task(&conn, "prompt", false);
    let cli = fake_codex("prompt", FakeMode::Success);

    run_one(&task, &repo, Provider::Codex, &cli, &limits(30));

    let args = recorded_argv(&cli);
    let prompt = args.last().expect("no prompt argument");
    assert!(prompt.contains("READ-ONLY sandbox"), "{prompt}");
    assert!(prompt.contains("investigate the thing"), "the task's own seed prompt is missing");
    assert!(!prompt.contains("complete_task"), "{prompt}");
    if providers::writeback_binary().is_some() {
        assert!(prompt.contains("mcp__cxtasks__file_followup"), "{prompt}");
        assert!(
            args.iter().any(|a| a.starts_with("mcp_servers.cxtasks=")),
            "the prompt promises tools but argv has no MCP config: {args:?}"
        );
    } else {
        assert!(prompt.contains("no CXTasks tools in this run"), "{prompt}");
        assert!(!prompt.contains("mcp__cxtasks__"), "{prompt}");
        assert!(
            !args.iter().any(|a| a.starts_with("mcp_servers.cxtasks=")),
            "argv configured an MCP that the prompt says is absent: {args:?}"
        );
    }
    std::fs::remove_dir_all(&repo).ok();
}

// ── what a finished run does to its task ───────────────────────────────

/// **A one-shot arming, not a sticky preference.** Without the reset, every
/// subsequent dispatch of the task is a review — so the second one audits the
/// first one's verdict, forever, and the user has no way to get back to the
/// work except noticing and un-ticking a control they thought was spent.
#[test]
fn a_finished_run_disarms_the_review_so_the_next_dispatch_is_work_again() {
    let patch = super::after_run_patch(Some("in_progress"), "succeeded", None).expect("advances");
    assert_eq!(patch.status.as_deref(), Some("in_review"));
    assert_eq!(patch.bg_intent, Some(crate::intent::BackgroundIntent::Work));

    // NOT on the failure branch: a review that died at spawn never happened, so
    // consuming the arming would send the user back to re-queue a review they
    // never got. The task parks under a hold (`todo` + `blocked_reason`), so a
    // still-armed review cannot surprise anyone in the meantime.
    for failed in ["failed", "timeout", "cancelled"] {
        let patch = super::after_run_patch(Some("in_progress"), failed, None).expect("advances");
        assert_eq!(patch.status.as_deref(), Some("todo"), "{failed}");
        assert!(
            patch.blocked_reason.as_ref().is_some_and(|r| r.is_some()),
            "{failed}: the park is the HOLD, and a hold with no reason is invisible"
        );
        assert_eq!(
            patch.bg_intent, None,
            "{failed}: a run that never produced a verdict must not consume the arming"
        );
    }
}

#[test]
fn a_ready_work_report_requests_review_and_never_marks_the_task_done() {
    let patch = super::after_run_patch(Some("in_progress"), "ready_for_review", None).unwrap();
    assert_eq!(patch.status.as_deref(), Some("in_review"));
    assert_eq!(patch.bg_intent, Some(crate::intent::BackgroundIntent::Work));
    assert!(patch.blocked_reason.is_none());
    for status in ["ready_for_review", "needs_action", "unverified"] {
        assert!(super::after_run_patch(Some("done"), status, Some("Remaining work")).is_none());
    }
}

/// **The whole point of the change, at the surface a human actually reads.**
/// The task used to land on "Background run failed — see the latest run for
/// details", which is what sent Chris digging through
/// `~/.claude/projects/…/<session>.jsonl` to find a `max_turns_reached` line.
/// It now names the guard, the number, and whether anything was rescued.
///
/// The task parks under a HOLD (v27: `todo` + `blocked_reason`, never a
/// `blocked` status): there IS no completed work to review, and the hold is
/// exactly "waiting on a human to look at that run".
#[test]
fn a_run_stopped_by_one_of_our_guards_says_which_one_on_the_task() {
    let patch = super::after_run_patch(
        Some("in_progress"),
        "turns_exhausted",
        Some("hit its 150-turn ceiling at turn 151 — a handoff was salvaged"),
    )
    .expect("advances");
    assert_eq!(patch.status.as_deref(), Some("todo"));
    assert_eq!(
        patch.blocked_reason.as_ref().and_then(|r| r.as_deref()),
        Some(
            "Background run hit its 150-turn ceiling at turn 151 — a handoff was \
             salvaged; see the latest run"
        )
    );

    // Without a detail it falls back to today's generic string, which at least
    // names the shape of the failure.
    let generic = super::after_run_patch(Some("in_progress"), "failed", None).expect("advances");
    assert_eq!(
        generic.blocked_reason.as_ref().and_then(|r| r.as_deref()),
        Some("Background run failed — see the latest run for details")
    );

    // A stop detail is still not a reason to overrule a human who moved the
    // task mid-run — the guard above it wins, as it does for everything else.
    assert!(
        super::after_run_patch(Some("done"), "turns_exhausted", Some("hit its ceiling")).is_none()
    );
}

/// The reset lives INSIDE the `should_advance` guard. That guard exists so a
/// human's mid-run decision wins over the runner's — and `bg_intent` is the
/// field a human is most likely to have just changed, so a reset that fired
/// regardless would override exactly what the guard protects.
#[test]
fn a_task_a_human_moved_mid_run_keeps_its_intent_as_well_as_its_status() {
    for moved in [Some("done"), Some("todo"), Some("queued"), Some("in_review"), None] {
        assert!(
            super::after_run_patch(moved, "succeeded", None).is_none(),
            "{moved:?}: neither the status nor the intent may be rewritten"
        );
    }
}

// ── the handoff summary that goes in the comment log ───────────────────

/// The comment carries a POINTER, and a pointer that says nothing is worse
/// than none — it occupies the one line a human reads while scanning a task.
#[test]
fn the_summary_reaches_past_a_title_to_the_sentence_that_says_what_happened() {
    // The shape agents actually produce: a heading naming the task back at
    // you, then the real answer underneath.
    let s = handoff_summary(
        "## Handoff: Legal risk review for scaling toward 100 users\n\n\
         **Scope of this run:** read-only investigation. No files were changed.\n\n\
         ### Direct answer\n- Telemetry is low-risk.",
    );
    assert!(s.starts_with("Handoff: Legal risk review"), "{s}");
    assert!(
        s.contains("read-only investigation"),
        "a heading alone restates the title and says nothing: {s}"
    );
    assert!(!s.contains('#') && !s.contains("**"), "markers must go: {s}");
}

#[test]
fn a_summary_that_opens_with_prose_stops_at_the_first_sentence() {
    let s = handoff_summary(
        "Found the 429: a missing retry on lib/webhook.ts:88.\n\n\
         The rest of this is the detail nobody needs in the comment log.",
    );
    assert_eq!(s, "Found the 429: a missing retry on lib/webhook.ts:88.");
}

#[test]
fn summary_skips_fences_rules_and_blank_lines_rather_than_quoting_them() {
    let s = handoff_summary("\n\n---\n```\n```\n\n- **Done:** the retry now backs off.\n");
    assert_eq!(s, "Done: the retry now backs off.");
}

#[test]
fn summary_is_bounded_and_says_it_was_cut() {
    let s = handoff_summary(&"word ".repeat(200));
    assert!(s.chars().count() <= SUMMARY_CHARS + 1, "{}", s.len());
    assert!(s.ends_with('…'), "silent truncation reads as the whole line: {s}");
}

/// An empty or whitespace-only handoff must not produce a comment claiming
/// there is a full handoff to go and read.
#[test]
fn an_empty_handoff_summarizes_to_nothing() {
    assert_eq!(handoff_summary("   \n\n  "), "");
}


// ── agent tripwire checks (Release 2) ─────────────────────────────────

/// A stand-in for `claude` that answers a tripwire check. Records its argv
/// like `fake_codex`, then speaks stream-json: one assistant turn with usage,
/// one `result` carrying `final_message`.
fn fake_claude_check(name: &str, final_message: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cxtasks-fake-claude-check-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("claude");
    // JSON-encode the message so newlines and quotes survive the round trip —
    // and emit it with `printf '%s'`, NOT `echo`: macOS `/bin/sh` is bash in
    // POSIX mode with `xpg_echo`, so `echo` expands the `\n` inside the JSON
    // string into a real newline and the result event arrives as two invalid
    // lines. Found by the verdict silently reading as "no VERDICT line".
    let encoded = serde_json::to_string(final_message).unwrap();
    let body = format!(
        r#"#!/bin/sh
printf '%s\036' "$@" > "$(dirname "$0")/argv.txt"
printf '%s\n' '{{"type":"system","subtype":"init","session_id":"chk-1"}}'
printf '%s\n' '{{"type":"assistant","message":{{"model":"claude-haiku-4-5-20251001","content":[{{"type":"text","text":"looking"}}],"usage":{{"input_tokens":100,"cache_read_input_tokens":900,"output_tokens":10}}}}}}'
printf '%s\n' '{{"type":"result","result":{encoded},"usage":{{"output_tokens":5}}}}'
exit 0
"#
    );
    std::fs::write(&path, body).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

/// A task with an `agent:` tripwire and a plain (non-git) repo directory —
/// a check needs no checkpoint, so it needs no git.
fn agent_task(conn: &Connection, name: &str, condition: &str) -> (Task, PathBuf) {
    let dir = std::env::temp_dir().join(format!("cxtasks-check-repo-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let t = create(
        conn,
        &NewTask {
            title: format!("wait for {name}"),
            body: Some("Outside users means not Chris and not admins.".into()),
            prompt: Some("DO THE WORK: build the whole feature".into()),
            repo_path: Some(dir.to_string_lossy().to_string()),
            tripwire: Some(format!("agent:{condition}")),
            tripwire_every_minutes: Some(60),
            // Deliberately a write-enabled task with no per-run ceiling: the
            // check must inherit neither.
            bg_writes_allowed: Some(true),
            bg_run_token_cap: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    (t, dir)
}

/// The check grant is the read tools plus ONE MCP tool, and every dangerous
/// tool is named in its deny list — the same property the tiers hold.
#[test]
fn the_check_grant_is_read_only_plus_get_task_and_denies_everything_dangerous() {
    let raw = ToolTier::allowed_for_check();
    let allowed = split(&raw);
    for want in ["Read", "Glob", "Grep", "mcp__cxtasks__get_task"] {
        assert!(allowed.contains(&want), "{allowed:?}");
    }
    assert_eq!(allowed.len(), 4, "exactly the served tool, nothing else: {allowed:?}");
    for tool in crate::mcp::server::BACKGROUND_TOOLS_CSV.split(',') {
        if tool != "mcp__cxtasks__get_task" {
            assert!(!allowed.contains(&tool), "a check must not be granted {tool}");
        }
    }
    let raw = ToolTier::denied_for_check();
    let denied = split(&raw);
    for tool in DANGEROUS_TOOLS {
        assert!(denied.contains(tool), "a check must deny {tool}");
    }
}

/// End to end against the fake CLI, with zero real tokens: the check prompt
/// is the CONDITION rather than the seed prompt, the argv carries the pinned
/// grant and bounds, and the final message becomes a trip.
#[test]
fn an_agent_tripwire_is_evaluated_by_a_fake_claude_and_trips() {
    let conn = crate::db::schema::test_conn();
    let (task, repo) = agent_task(&conn, "trip", "PocketBuddy has 50 outside users");
    let cli = fake_claude_check(
        "trip",
        "I read users.csv.\n\nVERDICT: TRIPPED\nEVIDENCE: users.csv lists 51 outside users.",
    );
    let settings = BudgetSettings::default();
    let limits = tripwire::check_limits(&task, &settings);
    assert_eq!(limits.token_ceiling, crate::tripwire::CHECK_TOKEN_CEILING, "bg_run_token_cap = 0 must not disable the check ceiling");
    assert_eq!(limits.max_turns, crate::tripwire::CHECK_MAX_TURNS);

    let outcome = run_one_check(&task, &repo, &cli, &limits);
    assert_eq!(outcome.status, "succeeded", "{outcome:?}");
    assert_eq!(outcome.executor, BackgroundExecutor::Claude);
    assert_eq!(outcome.input_tokens, Some(100));
    assert_eq!(outcome.cache_read_tokens, Some(900));
    assert_eq!(outcome.output_tokens, Some(15));
    assert_eq!(
        tripwire::verdict_outcome(&outcome),
        crate::tripwire::Outcome::Tripped("users.csv lists 51 outside users.".into())
    );

    // The argv the runner really passed.
    let args = recorded_argv(&cli);
    let after = |flag: &str| -> String {
        let i = args.iter().position(|a| a == flag).unwrap_or_else(|| panic!("{flag} missing: {args:?}"));
        args[i + 1].clone()
    };
    assert_eq!(after("--allowedTools"), ToolTier::allowed_for_check());
    assert_eq!(after("--disallowedTools"), ToolTier::denied_for_check());
    assert_eq!(after("--max-turns"), crate::tripwire::CHECK_MAX_TURNS.to_string());
    assert_eq!(after("--model"), crate::db::meta::DEFAULT_MODEL);
    assert!(args.contains(&"--strict-mcp-config".to_string()));
    let prompt = after("-p");
    assert!(prompt.contains("TRIPWIRE CHECK"), "{prompt}");
    assert!(prompt.contains("CONDITION: PocketBuddy has 50 outside users"), "{prompt}");
    assert!(prompt.contains("Outside users means not Chris"), "the notes travel: {prompt}");
    assert!(!prompt.contains("DO THE WORK"), "the seed prompt must NOT: {prompt}");
    assert!(prompt.contains("VERDICT: TRIPPED | NOT_YET | UNSURE"), "{prompt}");
    // The scratch dir holds the check prompt beside, not over, the work's.
    let scratch = crate::dispatch::scratch_dir_for(&task);
    assert!(scratch.join("check-prompt.txt").is_file());

    std::fs::remove_dir_all(&repo).ok();
}

/// The recorded path: a run row with `intent = check` that is the lease,
/// tokens visible in BOTH counters, admission refusing a second check, and
/// the verdict landing on the tripwire — while the task's history stays
/// blind to it.
#[test]
fn a_recorded_check_is_the_lease_counts_in_both_budgets_and_leaves_no_history() {
    let mut conn = crate::db::schema::test_conn();
    let (task, repo) = agent_task(&conn, "recorded", "the moon is full");
    let settings = BudgetSettings::default();
    let host_max = settings.max_concurrent;

    let run = db::runs::start_check(
        &mut conn,
        &task.id,
        task.tripwire.as_deref().unwrap(),
        host_max,
        BackgroundExecutor::Claude,
        tripwire::check_timeout_minutes(&task, &settings),
    )
    .unwrap()
    .expect("the first check is admitted");
    assert!(run.intent.is_check());
    assert_eq!(run.mode, "background");
    assert_eq!(run.timeout_minutes, Some(crate::tripwire::CHECK_TIMEOUT_MINUTES));
    let rt = db::tripwires::for_task(&conn, &task.id).unwrap().unwrap();
    assert_eq!(rt.lease_id.as_deref(), Some(run.id.as_str()), "the run id IS the lease");
    // No task status change: the task is still armed and hidden.
    let t = db::tasks::get(&conn, &task.id).unwrap().unwrap();
    assert_eq!(t.status, "inbox");
    assert!(t.tripwire_armed());

    // One in flight per host — and it holds a concurrency slot.
    assert_eq!(db::runs::count_running_checks(&conn).unwrap(), 1);
    assert_eq!(db::runs::count_running_background(&conn).unwrap(), 1);
    let (other, other_repo) = agent_task(&conn, "second", "something else");
    assert!(
        db::runs::start_check(&mut conn, &other.id, other.tripwire.as_deref().unwrap(), host_max, BackgroundExecutor::Claude, 10)
            .unwrap()
            .is_none(),
        "a second check must wait for the first"
    );
    assert!(db::tripwires::for_task(&conn, &other.id).unwrap().unwrap().lease_id.is_none(), "and takes no lease");

    // The check comes back NOT_YET.
    let cli = fake_claude_check("recorded", "VERDICT: NOT_YET\nEVIDENCE: 4 signups, none linked.");
    let limits = tripwire::check_limits(&task, &settings);
    let outcome = run_one_check(&task, &repo, &cli, &limits);
    tripwire::finish_check(&conn, &task, &run.id, &outcome);

    let run = db::runs::get(&conn, &run.id).unwrap().unwrap();
    assert_eq!(run.status, "succeeded");
    assert!(run.finished_at.is_some());
    assert_eq!(run.total_tokens(), 1015);
    assert_eq!(db::runs::tripwire_tokens_today(&conn).unwrap(), 1015);
    assert_eq!(db::runs::total_tokens_today(&conn).unwrap(), 1015, "a check is in the day's total too");
    assert_eq!(db::runs::count_running_checks(&conn).unwrap(), 0);

    let rt = db::tripwires::for_task(&conn, &task.id).unwrap().unwrap();
    assert!(rt.lease_id.is_none());
    assert_eq!(rt.last_outcome.as_deref(), Some("not_yet"));
    assert_eq!(rt.last_note.as_deref(), Some("4 signups, none linked."));
    assert!(db::tasks::get(&conn, &task.id).unwrap().unwrap().tripwire_armed());
    // Not-yet is silent on the task: no comment, no status change.
    assert!(db::comments::for_task(&conn, &task.id).unwrap().is_empty());

    // A verdict is not a handoff: nothing replays it, nothing reviews it.
    assert_eq!(crate::dispatch::history_block(&conn, &task), "");
    assert!(crate::dispatch::reviewable_run(&conn, &task.id).is_none());

    // And the second one is admitted now.
    assert!(
        db::runs::start_check(&mut conn, &other.id, other.tripwire.as_deref().unwrap(), host_max, BackgroundExecutor::Claude, 10)
            .unwrap()
            .is_some()
    );

    std::fs::remove_dir_all(&repo).ok();
    std::fs::remove_dir_all(&other_repo).ok();
}

/// A TRIPPED verdict moves the task the way a script's exit 0 does — same
/// `record`, same transaction, same comment under the tripwire author.
#[test]
fn a_tripped_verdict_surfaces_the_task_with_the_evidence() {
    let mut conn = crate::db::schema::test_conn();
    let (task, repo) = agent_task(&conn, "surface", "the release is out");
    let settings = BudgetSettings::default();
    let run = db::runs::start_check(&mut conn, &task.id, task.tripwire.as_deref().unwrap(), 2, BackgroundExecutor::Claude, 10)
        .unwrap()
        .unwrap();
    let cli = fake_claude_check("surface", "## Check\nVERDICT: TRIPPED\nEVIDENCE: CHANGELOG.md line 3 says 2.0.0 shipped.");
    let outcome = run_one_check(&task, &repo, &cli, &tripwire::check_limits(&task, &settings));
    tripwire::finish_check(&conn, &task, &run.id, &outcome);

    let t = db::tasks::get(&conn, &task.id).unwrap().unwrap();
    assert_eq!(t.tripwire_state.as_deref(), Some("tripped"));
    assert!(!t.tripwire_armed(), "surfaced");
    let log = db::comments::for_task(&conn, &task.id).unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].author, db::comments::AUTHOR_TRIPWIRE);
    assert!(log[0].body.contains("CHANGELOG.md line 3"), "{}", log[0].body);
    std::fs::remove_dir_all(&repo).ok();
}

/// `check` is a run-row value only: a task cannot be armed as one.
#[test]
fn a_task_cannot_be_armed_as_a_check() {
    let conn = crate::db::schema::test_conn();
    let t = create(&conn, &NewTask { title: "x".into(), ..Default::default() }).unwrap();
    let err = db::tasks::update(
        &conn,
        &t.id,
        &db::tasks::TaskPatch {
            bg_intent: Some(crate::intent::BackgroundIntent::Check),
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("machine-only"), "{err}");
    assert_eq!(db::tasks::get(&conn, &t.id).unwrap().unwrap().bg_intent, crate::intent::BackgroundIntent::Work);
}

/// A check's preflight skips the work's opt-in and never needs git; its
/// `ready` refuses a write-capable MCP grant.
#[test]
fn a_check_needs_no_opt_in_and_no_git_but_refuses_a_writable_server() {
    let conn = crate::db::schema::test_conn();
    let (task, repo) = agent_task(&conn, "ready", "anything");
    assert!(!task.bg_allowed, "fixture: the task is NOT opted into background runs");
    assert!(!crate::checkpoint::is_git_repo(&repo), "fixture: not a git repo");
    // Work would be refused on both counts; a check is not.
    assert!(preflight(&task).is_err());
    let cli_present = Provider::Claude.find_cli().is_some();
    match preflight_for(&task, RunKind::Check) {
        Ok(p) => assert_eq!(p, repo),
        Err(e) => assert!(!cli_present && e.contains("claude"), "{e}"),
    }
    let ws = ready_for(&conn, &cap(), &task, "run-x", RunKind::Check);
    match ws {
        Ok(w) => assert_eq!(w.kind, crate::routing::WorkspaceKind::Live),
        Err(e) => assert!(!cli_present && e.contains("claude"), "{e}"),
    }
    std::fs::remove_dir_all(&repo).ok();
}

// ── interpretation runs: the repo-less, tool-less lane ───────────────────

/// A repo-LESS task with a script tripwire whose trip asks to be interpreted —
/// the T226 shape. Not a git repo, no repo at all.
fn interpret_task(conn: &Connection, name: &str) -> Task {
    create(
        conn,
        &NewTask {
            title: format!("read the {name} report"),
            body: Some("Baseline was 147 TB written. Under ~100 GB/day is fine.".into()),
            prompt: Some("DO THE WORK: build the whole feature".into()),
            tripwire: Some(format!("script:{name}-report")),
            tripwire_on_trip: Some("interpret".into()),
            tripwire_every_minutes: Some(60),
            // Deliberately write-enabled with no per-run ceiling: an
            // interpretation must inherit neither.
            bg_writes_allowed: Some(true),
            bg_run_token_cap: Some(0),
            ..Default::default()
        },
    )
    .unwrap()
}

/// **The grant that makes a repo-less run on the Mac safe.** Zero allowed
/// tools, and the deny list names the READ tools as well as every dangerous
/// one — because `--allowedTools` is additive over the user's own
/// `permissions.allow` (invariant 7), so leaving `Read` out of the grant
/// accomplishes exactly nothing.
#[test]
fn an_interpretation_is_granted_nothing_and_is_denied_the_read_tools_by_name() {
    assert_eq!(ToolTier::allowed_for_interpret(), "", "an interpretation has no tools");

    let raw = ToolTier::denied_for_interpret();
    let denied = split(&raw);
    for tool in DANGEROUS_TOOLS {
        assert!(denied.contains(tool), "an interpretation must deny {tool}");
    }
    for tool in ["Read", "Glob", "Grep"] {
        assert!(
            denied.contains(&tool),
            "{tool} is the whole point: a scratch cwd is no boundary for a tool that takes an \
             absolute path, so it has to be NAMED: {denied:?}"
        );
    }
    // And it is strictly more restrictive than the tier below which nothing
    // exists — a check still reads, an interpretation does not.
    let raw_check = ToolTier::denied_for_check();
    let check = split(&raw_check);
    for tool in &check {
        assert!(denied.contains(tool), "an interpretation must deny everything a check does");
    }
    assert!(denied.len() > check.len(), "...and more: {denied:?} vs {check:?}");
}

/// End to end against the fake CLI, zero real tokens: a task with NO repo,
/// the evidence in the prompt in full, no `--allowedTools` flag at all, no
/// `--mcp-config`, and the final message read back as the interpretation.
#[test]
fn an_interpretation_runs_with_no_repo_no_tools_and_reads_the_whole_evidence() {
    let conn = crate::db::schema::test_conn();
    let task = interpret_task(&conn, "io-write");
    assert!(task.repo_path.is_none(), "the point of the fixture is that there is no repo");

    // Longer than `NOTE_CHARS`, which is exactly the case this feature exists
    // for: the comment on the task can only hold the first 500 characters.
    let evidence = format!(
        "SSD write report\n\nPercentage Used: 10%\nData Units Written: 288,202,399 [147 TB]\n\n\
         PER DAY\n{}\n  2026-09-30  host  426.39 GB   top: cloud-drive-daemon 46.8",
        (0..40)
            .map(|i| format!("  2026-09-{:02}  host    4.96 GB   top: cxmail 1.7", i % 28 + 1))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(evidence.chars().count() > crate::tripwire::NOTE_CHARS * 2);

    let cli = fake_claude_check(
        "interpret",
        "Nothing needs fixing yet, but one day is 85x the others.\n\n`426.39 GB` on 2026-09-30.",
    );
    let settings = BudgetSettings::default();
    let limits = tripwire::interpret_limits(&task, &settings);
    assert_eq!(
        limits.token_ceiling,
        crate::tripwire::INTERPRET_TOKEN_CEILING,
        "bg_run_token_cap = 0 must not disable the interpretation ceiling"
    );
    assert_eq!(limits.max_turns, crate::tripwire::INTERPRET_MAX_TURNS);

    // The cwd is app-owned scratch, NEVER $HOME and never a repo.
    let cwd = crate::paths::run_workspace("interp-test-run");
    std::fs::create_dir_all(&cwd).unwrap();
    assert_ne!(cwd, dirs::home_dir().unwrap(), "an interpretation must never run at $HOME");

    crate::dispatch::write_interpret_evidence(&task, &evidence).unwrap();
    let outcome = run_one_interpret(&task, &cwd, &cli, &limits);
    assert_eq!(outcome.status, "succeeded", "{outcome:?}");
    assert!(
        outcome.result_text.as_deref().unwrap().contains("85x the others"),
        "{outcome:?}"
    );

    let args = recorded_argv(&cli);
    assert!(
        !args.contains(&"--allowedTools".to_string()),
        "an empty grant is the ABSENT flag, not `\"\"` — the CLI answers an empty name with \
         `matches no known tool` and carries on: {args:?}"
    );
    assert!(
        !args.contains(&"--mcp-config".to_string()),
        "no server, so `--strict-mcp-config` means no MCP tools at all: {args:?}"
    );
    assert!(args.contains(&"--strict-mcp-config".to_string()), "{args:?}");
    let after = |flag: &str| -> String {
        let i = args.iter().position(|a| a == flag).unwrap_or_else(|| panic!("{flag} missing: {args:?}"));
        args[i + 1].clone()
    };
    assert_eq!(after("--disallowedTools"), ToolTier::denied_for_interpret());
    assert_eq!(after("--max-turns"), crate::tripwire::INTERPRET_MAX_TURNS.to_string());

    let prompt = after("-p");
    assert!(prompt.contains("426.39 GB"), "the WHOLE evidence travels: {prompt}");
    assert!(prompt.contains("Percentage Used: 10%"), "{prompt}");
    assert!(prompt.contains("Under ~100 GB/day is fine"), "the notes travel: {prompt}");
    assert!(prompt.contains("NO tools"), "it must be told it cannot look: {prompt}");
    assert!(!prompt.contains("DO THE WORK"), "the seed prompt must NOT: {prompt}");
    assert!(!prompt.contains("VERDICT:"), "an interpretation is not a verdict: {prompt}");

    // The scratch dir holds all three beside each other, never over.
    let scratch = crate::dispatch::scratch_dir_for(&task);
    assert!(scratch.join(crate::dispatch::INTERPRET_PROMPT_FILE).is_file());
    assert!(scratch.join(crate::dispatch::INTERPRET_EVIDENCE_FILE).is_file());

    std::fs::remove_dir_all(&cwd).ok();
}

/// With nothing staged there is nothing to read, and that is an error rather
/// than a prompt asking a model to interpret the empty string.
#[test]
fn an_interpretation_with_no_staged_evidence_is_refused() {
    let conn = crate::db::schema::test_conn();
    let task = interpret_task(&conn, "unstaged");
    let scratch = crate::dispatch::scratch_dir_for(&task);
    let _ = std::fs::remove_file(scratch.join(crate::dispatch::INTERPRET_EVIDENCE_FILE));
    let err = crate::dispatch::interpret_prompt(&task, std::path::Path::new("/tmp"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("no staged evidence"), "{err}");

    crate::dispatch::write_interpret_evidence(&task, "   \n ").unwrap();
    let err = crate::dispatch::interpret_prompt(&task, std::path::Path::new("/tmp"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("nothing to interpret"), "{err}");
}

/// The daily cap is asked FIRST and the sub-cap second (`runner::gate`'s
/// ordering: a lane should not name its own share of a budget that is already
/// gone), and each reserves the whole ceiling rather than an estimate.
#[test]
fn the_interpretation_admission_asks_the_global_cap_before_its_own() {
    use crate::runner::tripwire::{admit_interpret, InterpretAdmission};
    // Room everywhere.
    assert_eq!(
        admit_interpret(0, 50_000_000, 0, 5_000_000, 1_000_000),
        InterpretAdmission::Admitted
    );
    // The sub-cap alone is spent.
    assert!(matches!(
        admit_interpret(0, 50_000_000, 4_500_000, 5_000_000, 1_000_000),
        InterpretAdmission::SubCapReached { .. }
    ));
    // Both are spent: the GLOBAL one is reported.
    assert!(matches!(
        admit_interpret(49_999_000, 50_000_000, 4_999_000, 5_000_000, 1_000_000),
        InterpretAdmission::DailyCapReached { .. }
    ));
    // A zero cap is off, on either axis — the other still holds.
    assert_eq!(
        admit_interpret(99_000_000, 0, 0, 0, 1_000_000),
        InterpretAdmission::Admitted
    );
    // The reservation is the whole ceiling, not the spend so far: exactly
    // enough room is admitted and one token less is not.
    assert_eq!(
        admit_interpret(49_000_000, 50_000_000, 0, 0, 1_000_000),
        InterpretAdmission::Admitted
    );
    assert!(matches!(
        admit_interpret(49_000_001, 50_000_000, 0, 0, 1_000_000),
        InterpretAdmission::DailyCapReached { .. }
    ));
}
