//! `cxtasks-runner` — drains queued background tasks.
//!
//! Normally installed as a LaunchAgent (`com.cxtasks.app.runner`) and left
//! alone. Six flags for operating it by hand:
//!   --once      run a single tick and exit (what the tests use)
//!   --status    print the gate decision and exit, changing nothing
//!   --health    one line, exit 0 or 1 — for a container healthcheck
//!   --dry-run   claim, materialize, build the argv, print the prompt, roll the
//!               claim back, spawn nothing. How a new host gets validated
//!               without spending a token.
//!   --sync-now  reconcile with the peer once and exit — the shell's way in.
//!   --prose-audit  score the last seven days of agent-written notes, comments
//!               and results with the MCP's render check; print the evidence
//!               and exit 0 when enough of them would lose text or read as a
//!               wall, 1 when not. That is the `script:` tripwire contract, so
//!               the weekly check is a two-line shim in the tripwires directory
//!               that execs this — the scoring lives in ONE place
//!               (`cxtasks_core::prose`), not in a script beside it.
//!   --tag-audit  the same contract for the tag vocabulary: near-duplicate
//!               tags, a new tag restating a project or field, or a burst of
//!               new one-off tags (`cxtasks_core::db::tags::audit`, the matcher
//!               the MCP's new-tag guard uses).
//!
//! `--sync-now` lives HERE rather than being a verb, because `sync_now` is in
//! `api::HOST_LOCAL` and is refused over `/rpc` on purpose: routed through a
//! server it would configure the server. That refusal is right and it left the
//! shell with no route at all — during the investigation that produced this
//! flag, triggering one reconcile took a throwaway `cargo run --example` that
//! opened `paths::db_path()` by hand. This binary is already installed beside
//! the app, already resolves the same database, and already runs unattended.
//!
//! The kill switch is a file, so stopping it never requires this binary:
//!   touch ~/Library/Application\ Support/com.cxtasks.app/STOP_BACKGROUND

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cxtasks_core::{db, paths, runner};

/// The weekly formatting audit, in the `script:` tripwire's exit-code
/// contract: 0 = tripped (stdout is the evidence), 1 = not yet, 2 = error.
/// A failure exits 2 explicitly: returning it through `main` would exit 1,
/// which the tripwire reads as "not yet" and the failure would go unseen.
fn prose_audit_once(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    const WINDOW_DAYS: i64 = 7;
    let audit = match cxtasks_core::prose::audit(conn, WINDOW_DAYS) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("prose audit failed: {e}");
            std::process::exit(2);
        }
    };
    println!("{}", audit.evidence());
    std::process::exit(if audit.tripped() { 0 } else { 1 });
}

/// The weekly tag-vocabulary audit, same exit-code contract as
/// [`prose_audit_once`].
fn tag_audit_once(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    const WINDOW_DAYS: i64 = 7;
    let audit = match db::tags::audit(conn, WINDOW_DAYS) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("tag audit failed: {e}");
            std::process::exit(2);
        }
    };
    println!("{}", audit.evidence());
    std::process::exit(if audit.tripped() { 0 } else { 1 });
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // `runner::finish_run` posts its "run finished" banner through the
    // platform-neutral hook now, so this binary has to say what platform it is.
    // Without this line the LaunchAgent would still drain runs correctly and
    // silently stop notifying — the kind of regression nobody files a bug about
    // because nothing appears to be broken.
    // A helper binary may NOT talk to UNUserNotificationCenter — gotcha #34
    // says the attempt terminates the process. `HelperNotifier` spawns
    // `cxtasks --notify` instead, which is `std::process::Command` and nothing
    // else, and is why this binary can live in the neutral crate at all.
    cxtasks_core::notify::set_notifier(Box::new(cxtasks_core::notify_helper::HelperNotifier));
    #[cfg(target_os = "macos")]
    cxtasks_core::platform::set_drain_probe(Box::new(cxtasks_core::launchd::is_installed));

    let args: Vec<String> = std::env::args().skip(1).collect();
    let once = args.iter().any(|a| a == "--once");
    let status_only = args.iter().any(|a| a == "--status");
    let health_only = args.iter().any(|a| a == "--health");
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let sync_now = args.iter().any(|a| a == "--sync-now");
    let prose_audit = args.iter().any(|a| a == "--prose-audit");
    let tag_audit = args.iter().any(|a| a == "--tag-audit");

    // **An unknown flag must not fall through to the drain loop.** Every mode
    // above is opt-in, so anything unrecognised — a typo, or a flag a NEWER
    // caller passed to an OLDER binary — used to mean "ignore it and start
    // draining unattended agents", forever, in the foreground. That is the
    // loudest possible wrong answer to a misspelling, and it is reachable by
    // exactly the skew this repo always has: three binaries, not upgraded in
    // lockstep.
    const FLAGS: &[&str] = &[
        "--once",
        "--status",
        "--health",
        "--dry-run",
        "--sync-now",
        "--prose-audit",
        "--tag-audit",
    ];
    if let Some(unknown) = args.iter().find(|a| !FLAGS.contains(&a.as_str())) {
        eprintln!("cxtasks-runner: unknown argument {unknown}");
        eprintln!("  known flags: {}", FLAGS.join(" "));
        eprintln!("  (no flags at all = run the drain loop)");
        std::process::exit(2);
    }

    // The runner may be the first process to touch the DB after an upgrade.
    let conn = runner::open_db()?;
    if let Err(e) = db::schema::initialize(&conn) {
        log::warn!("schema migration on runner startup failed: {e}");
    }

    // Before the sweep: an audit only reads, and a tripwire runs it from
    // inside a live runner's tick, so it must not reclaim anything on the way.
    if prose_audit {
        return prose_audit_once(&conn);
    }
    if tag_audit {
        return tag_audit_once(&conn);
    }

    // A run left `running` by a crash occupies a concurrency slot forever, so
    // this must happen before the gate is ever consulted. The cutoff is no
    // longer passed in — `sweep_abandoned` derives it from the configured
    // timeout itself, so there is one place the policy lives rather than one
    // per call site.
    //
    // `initialize` above already swept, and this is not redundant: it re-reads
    // after migrations have run, and `--status` below reports the gate, which
    // would otherwise print a slot count including rows that are about to be
    // reclaimed. The real ongoing recovery is in `runner::tick`, which sweeps
    // every pass — startup-only reaping cannot help a runner that stays up for
    // days and loses track of a run without dying.
    let settings = db::meta::budget_settings(&conn);
    match db::runs::sweep_abandoned(&conn) {
        Ok(0) => {}
        Ok(n) => log::info!("reclaimed {n} abandoned run(s) from a previous process"),
        Err(e) => log::warn!("could not reclaim abandoned runs: {e}"),
    }

    // Before `--status`, because a healthcheck must be the cheapest thing this
    // binary can do and must not depend on anything `--status` prints.
    if health_only {
        return health(&conn);
    }

    if dry_run {
        return dry_run_once(&conn);
    }

    if sync_now {
        return reconcile_once(conn);
    }

    if status_only {
        let running = db::runs::count_running_background(&conn)?;
        let today = db::runs::count_background_runs_today(&conn)?;
        let tokens = db::runs::total_tokens_today(&conn)?;
        let cap = db::hosts::local(
            &conn,
            &cxtasks_core::routing::HostCapability::declared_for_this_host(),
        )?;
        let gate = runner::gate(
            paths::background_paused(),
            &settings,
            &cap,
            running,
            today,
            tokens,
        );
        println!("host:          {} ({})", cap.label, cap.id);
        println!(
            "capability:    {} · {} · {}",
            cap.max_tier.as_str(),
            cap.workspaces
                .iter()
                .map(|w| w.as_str())
                .collect::<Vec<_>>()
                .join("+"),
            if cap.enabled { "enabled" } else { "DISABLED" }
        );
        println!("paused:        {}", paths::background_paused());
        println!("kill switch:   {}", paths::kill_switch_path().display());
        println!("settings:      {settings:?}");
        println!("running now:   {running}");
        println!("runs today:    {today}");
        println!("tokens today:  {tokens} / {} cap", settings.daily_token_cap);
        // The agent-check slice of that total, against its own sub-cap — the
        // number to read when the day's budget went somewhere and no task ran.
        match db::runs::tripwire_tokens_today(&conn) {
            Ok(n) => println!(
                "check tokens:  {n} / {} sub-cap (agent tripwires; included in the total above)",
                settings.tripwire_daily_token_cap
            ),
            Err(e) => println!("check tokens:  unreadable ({e})"),
        }
        println!("gate:          {gate:?}");
        // The free lane's own line: armed tripwires, how many the next tick
        // would evaluate, and how many are mid-attempt. Printed even when the
        // gate is closed, because a closed BUDGET gate does not stop scripts —
        // only the switches do.
        match db::tripwires::counts(&conn) {
            Ok(c) => println!(
                "tripwires:     {} armed · {} due now · {} leased{}",
                c.armed,
                c.due_now,
                c.leased,
                if gate.is_switched_off() { " (switched off — nothing will run)" } else { "" }
            ),
            Err(e) => println!("tripwires:     unreadable ({e})"),
        }

        // Which agents THIS binary can actually reach. Printed from the runner
        // rather than the app because that is the process whose environment
        // matters: the app inherits a login session's PATH and the LaunchAgent
        // does not, so "Codex works in the app" is not evidence that the runner
        // can spawn it (gotcha #19). This is the check to run after installing
        // and before enabling the first Codex task.
        println!("executors:");
        for executor in cxtasks_core::executor::BackgroundExecutor::ALL {
            let provider = runner::Provider::for_executor(executor);
            let cli = provider
                .find_cli()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "not found".into());
            let state = match provider.preflight() {
                Ok(()) => "ready".to_string(),
                Err(e) => format!("UNAVAILABLE — {e}"),
            };
            println!("  {:<8} {state}\n           cli: {cli}", executor.as_str());
        }
        return Ok(());
    }
    drop(conn);

    let in_flight = Arc::new(AtomicUsize::new(0));
    log::info!(
        "cxtasks-runner starting (tick {}s, kill switch {})",
        runner::TICK_SECONDS,
        paths::kill_switch_path().display()
    );

    // The control loop runs on its OWN connection and its own thread, because
    // its whole value is being prompt: a Stop pressed while a 20-second tick is
    // mid-spawn must not wait for that tick to finish. Sharing the drain loop's
    // connection would put it behind exactly the work it may need to interrupt.
    //
    // Not started for `--once`, which is a synchronous "do one batch" used by
    // tests and by hand — a background thread writing heartbeats would outlive
    // the assertion it was meant to be independent of.
    if !once {
        std::thread::spawn(|| {
            let conn = match runner::open_db() {
                Ok(c) => c,
                Err(e) => {
                    // Fatal to the control loop, not to the runner. Draining
                    // still works; what is lost is prompt cancellation and this
                    // host appearing alive, and saying so is better than a
                    // silent half-working runner.
                    log::error!("control loop could not open the database ({e}); \
                                 cancels and heartbeats are OFF for this process");
                    return;
                }
            };
            let mut since_beat = runner::HEARTBEAT_SECONDS;
            loop {
                let beat = since_beat >= runner::HEARTBEAT_SECONDS;
                match runner::control_pass(&conn, beat) {
                    Ok(true) => since_beat = 0,
                    Ok(false) => {}
                    Err(e) => log::warn!("control pass failed: {e}"),
                }
                if beat {
                    if let Err(e) = runner::mirror_pass(&conn) {
                        log::warn!("mirror pass failed: {e}");
                    }
                }
                std::thread::sleep(Duration::from_secs(runner::CONTROL_TICK_SECONDS));
                since_beat += runner::CONTROL_TICK_SECONDS;
            }
        });
    }

    loop {
        match runner::tick(&in_flight) {
            Ok(0) => {}
            Ok(n) => log::info!("started {n} background run(s)"),
            Err(e) => log::error!("tick failed: {e}"),
        }

        if once {
            // Let the spawned run threads finish before exiting, so --once is
            // actually usable as a synchronous "do one batch" command.
            while in_flight.load(Ordering::SeqCst) > 0 {
                std::thread::sleep(Duration::from_millis(500));
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(runner::TICK_SECONDS));
    }
}

/// Reconcile with the peer once, print what moved, and exit.
///
/// The same `sync::auto::now` the app's five-minute loop and its Settings
/// button call — so a reconcile run from a shell to debug the automatic one is
/// actually exercising the automatic one, rather than a second implementation
/// that might not share the bug.
fn reconcile_once(mut conn: rusqlite::Connection) -> anyhow::Result<()> {
    let report = cxtasks_core::sync::auto::now(&mut conn)?;
    println!(
        "swept {} · pushed {} · failed {} · pulled {} in, {} out · images {} up, {} down",
        report.swept,
        report.pushed,
        report.failed.len(),
        report.pulled.inserted_or_updated,
        report.pulled.deleted,
        report.blobs.uploaded,
        report.blobs.downloaded,
    );
    for failure in &report.failed {
        eprintln!("  refused #{}: {}", failure.local_seq, failure.err);
    }
    if report.held_back {
        // Not an error, and it must not read as one: staying behind is the
        // ordering rule working. Said out loud because the next line somebody
        // would otherwise write is "why did nothing come down".
        println!("  the pull was HELD BACK — something is still un-pushed, so nothing was pulled.");
    }
    if let Some(lease) = report.lease_renewed {
        println!("  lease renewed: T{}–T{}", lease.lo, lease.hi);
    }
    Ok(())
}

/// One line and an exit code, for a container healthcheck.
///
/// # What it asserts, and the one that is not obvious
///
/// Reaching the database and having a usable executor are the expected checks.
/// The third is the one worth having: **this process's `$HOME` must carry no
/// `permissions.allow` rules.**
///
/// Invariant 7 rests on `--disallowedTools` beating the settings file, but
/// `--allowedTools` is ADDITIVE over `~/.claude/settings.json`, and Chris's
/// holds ~70 rules including `Bash(bash:*)` and `Bash(git push:*)`. A container
/// is given a dedicated empty `$HOME` precisely so those rules are absent. That
/// is a property of a deployment, and deployments drift — somebody mounts the
/// real home to fix something and forgets. Asserting it here means the container
/// reports unhealthy instead of quietly running unattended agents with a shell.
///
/// **On the Mac the same rules are a NOTE, not a failure** (`home_rules_verdict`).
/// The Mac runner is Chris's own login with his real settings by design, and the
/// tiers hold there because `--disallowedTools` beats the allow list. A check
/// that fails on every Mac run is one you learn to ignore — including on the
/// host where it matters. The split is by OS, not by an env var, so a container
/// cannot drift out of the strict branch by losing a setting.
fn health(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let mut problems: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    let cap = match db::hosts::local(
        conn,
        &cxtasks_core::routing::HostCapability::declared_for_this_host(),
    ) {
        Ok(c) => Some(c),
        Err(e) => {
            problems.push(format!("registry unreadable: {e}"));
            None
        }
    };

    if let Some(cap) = &cap {
        let ready: Vec<_> = cap
            .executors
            .iter()
            .filter(|e| runner::Provider::for_executor(**e).preflight().is_ok())
            .collect();
        if ready.is_empty() {
            problems.push(format!(
                "no declared executor is usable ({})",
                cap.executors
                    .iter()
                    .map(|e| e.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    if let Some(rules) = inherited_permission_rules() {
        match home_rules_verdict(rules, cfg!(target_os = "macos")) {
            Err(problem) => problems.push(problem),
            Ok(note) => notes.push(note),
        }
    }

    if problems.is_empty() {
        println!(
            "ok · host {} · {}",
            cap.as_ref().map(|c| c.label.as_str()).unwrap_or("?"),
            cap.as_ref().map(|c| c.max_tier.as_str()).unwrap_or("?")
        );
        for n in &notes {
            println!("note: {n}");
        }
        Ok(())
    } else {
        for p in &problems {
            eprintln!("unhealthy: {p}");
        }
        std::process::exit(1);
    }
}

/// Inherited permission rules: a FAILURE on a host whose `$HOME` must be empty
/// (every non-Mac host — the container), a NOTE on the Mac, whose home is
/// Chris's own on purpose. See `health`.
fn home_rules_verdict(rules: usize, is_mac: bool) -> Result<String, String> {
    if is_mac {
        Ok(format!(
            "$HOME grants {rules} permission rule(s) — expected on the Mac, where \
             the tiers hold by --disallowedTools; only a container must be empty"
        ))
    } else {
        Err(format!(
            "this process's $HOME grants {rules} permission rule(s) to every agent it \
             spawns; a runner host must have an empty settings file (invariant 7)"
        ))
    }
}

/// How many `permissions.allow` / `permissions.deny` rules this process's
/// `$HOME` would hand a spawned agent. `None` when there are none.
///
/// Only meaningful on a host that is SUPPOSED to be empty — the Mac's own
/// settings file is full of rules by design, which is why this is checked by
/// `--health` (run in the container) and not by the runner at startup.
fn inherited_permission_rules() -> Option<usize> {
    if std::env::var_os("CXTASKS_ALLOW_INHERITED_PERMISSIONS").is_some() {
        return None;
    }
    let path = dirs::home_dir()?.join(".claude/settings.json");
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let count = ["allow", "deny"]
        .iter()
        .filter_map(|k| parsed.get("permissions")?.get(k)?.as_array())
        .map(|a| a.len())
        .sum::<usize>();
    (count > 0).then_some(count)
}

/// Claim one task, build everything a real run would, print it, and roll back.
///
/// The point is validating a host — its mounts, its mirror, its CLI, its
/// `$HOME`, the argv it would actually exec — **without spending a token**. It
/// is deliberately not a `--once` with a flag: a run that spawns and then is
/// told to stop has already cost money and already touched the workspace.
///
/// The claim is rolled back rather than left, so a dry run does not consume the
/// task it examined.
fn dry_run_once(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let cap = db::hosts::local(
        conn,
        &cxtasks_core::routing::HostCapability::declared_for_this_host(),
    )?;
    println!("host       {} ({})", cap.label, cap.id);
    println!("capability {} · {}", cap.max_tier.as_str(), cap.workspaces
        .iter().map(|w| w.as_str()).collect::<Vec<_>>().join("+"));
    println!("drain sql  {}", cxtasks_core::routing::queue_sql(&cap));

    let queued = db::tasks::list_queued_for_background(conn, &cap, 1)?;
    let Some(task) = queued.into_iter().next() else {
        println!("\nnothing queued that this host may claim — that IS the answer");
        return Ok(());
    };
    println!(
        "\ntask       {} {}",
        task.short_id.map(|n| format!("T{n}")).unwrap_or_default(),
        task.title
    );

    match runner::preflight(&task) {
        Ok(repo) => println!("workspace  {}", repo.display()),
        Err(e) => {
            println!("REFUSED    {e}");
            return Ok(());
        }
    }
    let provider = runner::Provider::for_task(&task);
    match provider.find_cli() {
        Some(cli) => println!("cli        {}", cli.display()),
        None => println!("cli        NOT FOUND — this host could not spawn {}", provider.executor()),
    }

    // The tool surface, which for a detached run is the ONLY bound there is —
    // and the thing most likely to be configured on one host and not another.
    // `preflight` above does not look at it; `ready` does, at execute time, and
    // by then nobody is watching. Printing it here is the point of a dry run.
    let granted = task.bg_mcp_servers.as_deref().unwrap_or_default();
    match cxtasks_core::mcp_registry::resolve_all(granted) {
        Ok((ready, missing)) => {
            if granted.trim().is_empty() {
                println!("mcp        write-back only (no servers granted)");
            } else {
                for r in &ready {
                    println!("mcp        {} -> {}", r.name, r.program);
                }
                for m in &missing {
                    println!("mcp        {m} NOT AVAILABLE on this host — the run would be refused");
                }
            }
        }
        Err(e) => println!("mcp        REFUSED  {e}"),
    }

    // The REAL prompt, through the same builder a real run uses. Reading it is
    // most of the value here: a mirror's context paragraph, the history replay
    // and the tool grant are all things you want to see before an unattended
    // agent acts on them.
    let repo = runner::preflight(&task).unwrap_or_default();
    let prompt = cxtasks_core::dispatch::build_prompt(
        &task,
        &repo,
        cxtasks_core::dispatch::PromptTarget::Background(provider.executor()),
    );
    println!("\n─── prompt ───\n{prompt}\n─── end ───");
    println!("\nnothing was claimed, nothing was spawned.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::home_rules_verdict;

    /// The container's check must stay a failure — it is the only thing that
    /// catches a real home mounted over the empty one.
    #[test]
    fn inherited_rules_fail_off_the_mac() {
        let e = home_rules_verdict(293, false).unwrap_err();
        assert!(e.contains("293") && e.contains("invariant 7"), "{e}");
    }

    /// On the Mac they are expected, so they must not make `--health` exit 1.
    #[test]
    fn inherited_rules_are_a_note_on_the_mac() {
        let n = home_rules_verdict(293, true).unwrap();
        assert!(n.contains("293") && n.contains("expected on the Mac"), "{n}");
    }
}
