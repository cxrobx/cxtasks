//! The Claude Code provider — `claude -p`, unchanged from the pre-Codex runner.
//!
//! Everything here was already load-bearing before there was a second provider;
//! the move into this module is a relocation, not a rewrite. The comments that
//! explain *why* each flag is present travel with the code, because that is the
//! only place they mean anything.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::db::tasks::Task;
use crate::runner::providers::{
    bounded, command_label, RunRequest, RunSummary, StopReason, TraceEvent,
};

/// Write the one-server `--mcp-config` into the run's scratch dir.
///
/// A file rather than inline JSON: the flag accepts both, but a path is what
/// shows up legibly in `ps` and in the run's own scratch dir when something has
/// gone wrong, and it sidesteps quoting a JSON blob through the argv.
///
/// `env` is set **explicitly** rather than relying on the child inheriting the
/// var this same `Command` sets. Inheritance is how it works today, but the
/// reduced tool surface is a safety property and it must not rest on how the
/// CLI happens to spawn its servers.
fn mcp_config_path(req: &RunRequest) -> Option<std::path::PathBuf> {
    // An interpretation gets NO server, which under `--strict-mcp-config` means
    // no MCP tools at all. It does not need one: the runner writes its final
    // message onto the task itself, exactly as it records a check's verdict, so
    // handing it `comment_task` would add a way to write without adding a way
    // to say anything new. Returning `None` here is also what makes
    // `denied_for_interpret`'s claim complete — otherwise "zero tools" would
    // mean "zero tools plus whatever the router serves".
    if req.kind == super::RunKind::Interpret {
        return None;
    }
    let bin = super::writeback_binary()?;
    let mut servers = serde_json::Map::new();
    // The name here is what `mcp__cxtasks__*` in the tool grant refers to —
    // see `mcp::server::BACKGROUND_TOOLS_CSV`. The env var picks the router:
    // a check gets `CHECK_ENV` and therefore ONE tool (`get_task`) — no way
    // to comment, no way to file — because a verdict is not a handoff and a
    // check must not be able to write anything but its final message.
    let mode_env = match req.kind {
        super::RunKind::Work => crate::mcp::server::BACKGROUND_ENV,
        super::RunKind::Check => crate::mcp::server::CHECK_ENV,
        // Unreachable — the early return above. Spelled out rather than
        // `_ =>` so adding a fourth kind is a compile error here, where
        // choosing a router is a safety decision.
        super::RunKind::Interpret => return None,
    };
    servers.insert(
        "cxtasks".into(),
        serde_json::json!({
            "command": bin.to_string_lossy(),
            "args": [],
            "env": { mode_env: "1" }
        }),
    );

    // Whatever this task was explicitly granted, and nothing else. Combined
    // with `--strict-mcp-config` below, this file is the COMPLETE set of MCP
    // servers the run can see — the user's own ~/.claude.json is ignored
    // entirely, which is what stops an unattended run discovering that cxmail
    // can send email. For a detached run it is also the whole answer to "what
    // can this reach", since there is no repo doing that job.
    for extra in &req.extra_mcp {
        servers.insert(
            extra.name.to_string(),
            serde_json::json!({ "command": extra.program, "args": extra.args }),
        );
    }

    let config = serde_json::json!({ "mcpServers": servers });
    let path = req.scratch.join("mcp.json");
    std::fs::write(&path, serde_json::to_string_pretty(&config).ok()?).ok()?;
    Some(path)
}

/// One `claude -p` invocation.
pub fn command(cli: &Path, req: &RunRequest) -> Command {
    let tier = req.tier;
    // A check's grant is pinned by its KIND, not by the task's tier: the tier
    // is what the task's work asked for, and a check evaluating a write-enabled
    // task must not inherit `Edit`. `allowed_for_check` names only the read
    // tools plus `get_task`; `denied_for_check` is the whole dangerous list.
    let (allowed, denied) = match req.kind {
        super::RunKind::Work => (tier.allowed_with_writeback(), tier.denied()),
        super::RunKind::Check => (
            crate::runner::ToolTier::allowed_for_check(),
            crate::runner::ToolTier::denied_for_check(),
        ),
        super::RunKind::Interpret => (
            crate::runner::ToolTier::allowed_for_interpret().to_string(),
            crate::runner::ToolTier::denied_for_interpret(),
        ),
    };
    let mut cmd = Command::new(cli);
    cmd.current_dir(&req.repo)
        .arg("-p")
        .arg(&req.prompt)
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose");
    // An EMPTY grant is passed by omitting the flag, not as `""`. The CLI reads
    // an empty string as a one-element list containing the empty name and
    // answers `matches no known tool` on stderr — the same silent-no-op shape
    // invariant 7 warns about for a typo'd deny rule, arriving from the other
    // side. Omission is safe here and only here: the deny list below is what
    // enforces, and for an interpretation it names the read tools too.
    if !allowed.is_empty() {
        cmd.arg("--allowedTools").arg(&allowed);
    }
    // The half that actually enforces. `--allowedTools` is additive over
    // the user's `~/.claude/settings.json`, so without this an unattended
    // "read-only" run inherits every `Bash(...)` rule Chris ever approved
    // in an interactive session — measured: it ran `echo` and wrote a file.
    // See `ToolTier`.
    cmd.arg("--disallowedTools").arg(denied);

    // Explicit model: unattended runs default to Sonnet 5 rather than
    // inheriting the session default (often Opus), which drains the
    // subscription limit far faster for the same mechanical work. Always
    // populated on this path — unlike Codex, where a missing model means "let
    // the CLI decide".
    if let Some(model) = req.model.as_deref().filter(|m| !m.trim().is_empty()) {
        cmd.arg("--model").arg(model);
    }

    // Don't load the user's ~20 global MCP servers. Three reasons, in
    // order of importance:
    //  1. Blast radius. `--allowedTools` blocks EXECUTION (verified: the
    //     agent answers "NONE" when asked for email/finance tools), but the
    //     definitions are still in context — the agent can see that cxmail
    //     can send email and that browser control exists. An unattended run
    //     should not be told those capabilities are nearby.
    //  2. CPU. meeting-copilot measured ~6.7s → ~0.66s per spawn from this
    //     flag alone, and concurrent spawns thrash without it.
    //  3. Tokens. Worth ~6k of cache-write per run; real but the smallest
    //     of the three.
    // Foreground dispatch deliberately does NOT use this: those sessions need
    // the full cxtasks MCP so they can call complete_task.
    //
    // **`--strict-mcp-config` means "only what --mcp-config names", not "no
    // MCP at all"** — which is what lets an unattended run keep every reason
    // above while still being able to write back. The config below names one
    // server, and that server hands out three tools (see
    // `mcp::server::BACKGROUND_TOOLS`); the ~5.3k of definitions the full
    // 15-tool surface would cost drops to ~550 tokens, and `complete_task` is
    // absent from the router rather than deny-listed, so the self-completion
    // race that was observed live cannot be re-entered by prompt.
    //
    // If the binary isn't beside us the flag stays on with no config, which is
    // exactly the old behaviour: no tools, run still works, handoff travels in
    // the final message. `prompt_tail` is told which case it's in so it never
    // instructs an agent to call a tool it doesn't have.
    if let Some(config) = mcp_config_path(req) {
        cmd.arg("--mcp-config").arg(config);
    }
    cmd.arg("--strict-mcp-config")
        // Bound runaway multi-turn burn. Each turn re-sends the reloaded
        // CLAUDE.md, so cost scales with turns: a measured single-turn call was
        // ~89k tokens while a few-turn tool-using run hit ~234k. Without a
        // ceiling one confused task can spend the whole daily budget.
        .arg("--max-turns")
        .arg(req.max_turns.to_string())
        // Belt-and-braces marker for a `PreToolUse` hook (see
        // `~/.claude/hooks/`), not the primary gate — the tool grant above
        // is. Hook scripts inherit this process's env, so a hook keyed on
        // this var is a no-op for the user's own interactive sessions.
        .env("CXTASKS_BACKGROUND_RUN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if req.kind == super::RunKind::Check {
        // The same belt-and-braces marker a hook can key on, for the narrower
        // kind. Set on the process too, not only in `mcp.json`, so a hook sees
        // it whether or not the CLI ever spawns the server.
        cmd.env(crate::mcp::server::CHECK_ENV, "1");
    }
    cmd
}

/// The background-Claude tail of the seed prompt.
///
/// Deliberately the OPPOSITE instruction from the foreground path. The runner
/// captures the final message from the stream and marks the task
/// `needs_review`; a human decides whether it's actually done. If the agent
/// called `complete_task` itself it would (a) race the runner's status write
/// and (b) skip that review entirely — observed live, a background task marked
/// itself `done` and left the run row dangling.
pub fn prompt_tail(task: &Task) -> String {
    let mut s = String::new();
    if !task.bg_writes_allowed && !task.bg_shell_allowed {
        s.push_str(
            "This is an UNATTENDED background run with writes disabled. Do not edit, \
             create or delete files, and do not run commands with side effects. \
             Investigate and report.\n\n",
        );
    } else if !task.bg_writes_allowed {
        s.push_str(
            "This is an UNATTENDED background run. Nobody is watching, so do not ask \
             questions — make the most reasonable call and say what you assumed. You have a \
             shell for inspection, tests and CLIs, but do NOT deliberately edit source files: \
             this task was granted commands, not edits.\n\n",
        );
    } else {
        s.push_str(
            "This is an UNATTENDED background run. Nobody is watching, so do not ask \
             questions — make the most reasonable call and say what you assumed.\n\n",
        );
    }
    s.push_str(&handoff_tail(task, super::writeback_binary().is_some()));
    s
}

/// What to leave behind — the half of the tail that isn't about permissions.
///
/// **The old version asked for "a short summary", and that was the bug.** A
/// background run ends with the task in `in_review` and its result filed on the
/// run row, and the next agent dispatched against that task gets the original
/// seed prompt back. So "short" optimised the one artifact that has to carry
/// everything the run learned, and a second delegate re-derived it all from
/// scratch. What is wanted is a handoff: enough that someone with none of this
/// context can act without repeating the investigation.
///
/// Split out and taking `has_tools` explicitly so it can be tested both ways
/// without a filesystem: a prompt that tells an agent to call
/// `mcp__cxtasks__file_followup` when the MCP binary isn't beside the runner is
/// worse than one that never mentions it — the agent burns turns on a tool that
/// isn't there and then reports failure for a reason unrelated to the task.
pub fn handoff_tail(task: &Task, has_tools: bool) -> String {
    let mut s = String::new();
    // Permissions constrain what can be done, not what the task requested.
    // A read-only run must distinguish completed research from unfinished action.
    let investigating = !task.bg_writes_allowed && !task.bg_shell_allowed;
    let reviewing = task.bg_intent.is_review();
    // The opening-line ask stays ONE paragraph and swaps its noun — outcome
    // becomes verdict. `runner::handoff_summary` lifts that line verbatim into
    // the comment log, so a second competing "START WITH…" bolted on below
    // would make every review's comment-log line a preamble.
    let opener = if reviewing {
        "START WITH ONE SENTENCE giving your VERDICT. That opening line is lifted verbatim \
         into the task's history and is the only thing shown before someone opens the full \
         text, so make it the judgement (\"Handoff is sound except step 3: \
         lib/webhook.ts:88 has no retry to remove\"), not a restatement of the title. If you \
         open with a markdown heading, put that sentence directly under it.\n\n"
    } else {
        "START WITH ONE SENTENCE saying what came of the run. That opening line is lifted \
         verbatim into the task's history and is the only thing shown before someone opens \
         the full text, so make it the outcome (\"Found the 429: a missing retry on \
         lib/webhook.ts:88\"), not a restatement of the title. If you open with a markdown \
         heading, put that sentence directly under it.\n\n"
    };
    s.push_str(
        "Your FINAL MESSAGE is the result. It is captured automatically and written onto \
         the task. Work runs report whether the result needs work, verification, or \
         human review; review runs leave their verdict — nothing else you say \
         survives this run.\n\n\
         Write it as a HANDOFF, not a summary. Assume the reader is a fresh agent or a \
         human with none of your context, working from the task alone.\n\n",
    );
    s.push_str(opener);
    s.push_str(
        "Then cover:\n\
         - What you did, and what you found — with the sources, artifacts, observations, \
         file paths or commands that matter, so nobody repeats the work.\n\
         - What you did NOT do, and why: out of scope, blocked, or you ran out of turns.\n\
         - The next concrete step, specific enough to act on.\n\
         - Every assumption you made, and anything you'd want to be wrong about.\n\
         Say plainly if you couldn't finish. An honest partial handoff is worth more \
         than a confident summary of an incomplete run.\n\n\
         It renders as markdown in the app, so after the opening line use short \
         sections, bullets or a table rather than one long paragraph, and put code, \
         paths and commands in backticks (outside them `Vec<String>` shows as \"Vec\").\n",
    );
    // **The deliverable is where this would have shipped broken.** `investigating`
    // is true for every DEFAULT review — read-only is what arming one preselects
    // — so without the review arm first, a reviewer is told "the plan IS the
    // deliverable", i.e. to redo the planning work it was dispatched to audit.
    // Matched on the PAIR so the review arm covers every tier: a write-capable
    // review still holds `Edit`, and being told to write a critique while
    // holding it pulls straight into fixing.
    match (reviewing, investigating) {
        (true, _) => s.push_str(
            "\nThe VERDICT is the deliverable — not a better plan, and not the work. Write it \
             as markdown: what the handoff got right, then each claim you found wrong or \
             unverifiable with the evidence that settles it (the file and line you opened, the \
             command you ran and what it printed), then what it missed. End with whether the \
             next step it proposes is the one to take. If it is sound, say so in a sentence and \
             stop — length is not the measure here.\n",
        ),
        (false, true) => s.push_str(
            "\nThis run has read-only access. Complete research, analysis, or planning when \
             that is what the task requests. If the task requests an action or a change, \
             investigate what you can and report that required action as remaining work; \
             do not replace the requested deliverable with a plan and claim it finished.\n",
        ),
        (false, false) => {}
    }
    if has_tools {
        s.push_str(
            "\nYou have three CXTasks tools and no others:\n\
             - `mcp__cxtasks__get_task` — re-read this task, its comments and its subtasks.\n\
             - `mcp__cxtasks__comment_task` — leave notes as you go. Use it for anything \
             worth keeping that isn't the final answer: a dead end you ruled out, a \
             decision and its reason. Comments survive; your reasoning does not. Keep each \
             one under 2,000 characters — the log is a chronology someone scans, and the \
             detail belongs in your final message, which is kept in full.\n\
             - `mcp__cxtasks__file_followup` — file real work you found that is out of \
             scope for THIS task. It becomes a separate task linked to this one, \
             untriaged for a human to review. Do not \
             use it for work you were asked to do; do it, or explain why you couldn't.\n\
             There is deliberately no way to mark this task done or change its status. A \
             human reviews the result — that is the whole point of an unattended run.\n",
        );
    } else {
        s.push_str(
            "\nYou have no CXTasks tools in this run, so the final message is the ONLY \
             channel. Put the follow-ups you'd have filed in it, as a short list.\n",
        );
    }
    if !reviewing {
        s.push_str(super::super::completion::EXPECTATIONS);
        s.push_str(super::super::completion::CONTRACT);
    }
    s
}

/// Every tool this runner knows about, for the salvage's `--disallowedTools`.
///
/// The salvage has exactly ONE turn. Any tool call consumes it and the salvage
/// then produces nothing at all, so "no tools" is **correctness** here rather
/// than caution — the same reasoning as `--max-turns 1` itself, one level down.
///
/// Derived from the union of the runner's dangerous list and the widest tier's
/// grant, so a new entry in either is denied here without a second edit. Naming
/// them is what enforces: omission is not denial (invariant 7), and the read
/// tools in particular are in nobody's deny list otherwise.
fn salvage_denied() -> String {
    let mut names: Vec<&str> = crate::runner::DANGEROUS_TOOLS.to_vec();
    for tool in crate::runner::ToolTier::Write.allowed().split(',') {
        if !names.contains(&tool) {
            names.push(tool);
        }
    }
    names.join(",")
}

/// One extra turn, resuming a cut-off session purely to get the handoff out.
///
/// **Why this is safe only after a turn cutoff.** `error_max_turns` means the
/// loop ended cleanly *between* turns, so the transcript is well-formed and
/// `--resume` picks it up. A wall-clock kill can leave a `tool_use` with no
/// matching `tool_result`, which makes resume 400 — not a thing to discover at
/// the end of a twenty-minute run. And a run stopped by the *token* ceiling
/// never salvages at all: spending another turn immediately after a spend guard
/// fires is incoherent.
///
/// No `--mcp-config` (so `--strict-mcp-config` leaves the run with no MCP at
/// all), because the write-back tools are tools like any other and there is one
/// turn to spend.
pub fn salvage_command(cli: &Path, req: &RunRequest, task: &Task, session_id: &str) -> Command {
    let mut cmd = Command::new(cli);
    cmd.current_dir(&req.repo)
        .arg("-p")
        .arg(salvage_prompt(task))
        .arg("--resume")
        .arg(session_id)
        .arg("--max-turns")
        .arg("1")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--disallowedTools")
        .arg(salvage_denied())
        .arg("--strict-mcp-config");
    // Same model as the run being rescued: switching mid-session would change
    // who is writing the handoff, and the cheap default is already in force.
    if let Some(model) = req.model.as_deref().filter(|m| !m.trim().is_empty()) {
        cmd.arg("--model").arg(model);
    }
    cmd.env("CXTASKS_BACKGROUND_RUN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// The salvage's prompt: the cutoff notice, then [`handoff_tail`] **verbatim**.
///
/// Reusing the whole tail rather than restating a shorter version of it is the
/// point — invariant 18 turns on there being exactly one definition of what a
/// handoff is, and a second, terser copy here would be the one that drifted.
/// `has_tools: false` is not a degradation either: it is literally true of this
/// invocation, and it is already the branch that says the final message is the
/// only channel.
pub fn salvage_prompt(task: &Task) -> String {
    format!(
        "Your run was stopped at its turn ceiling before you finished. This is ONE extra \
         turn and it exists only to rescue the handoff: you have no tools, so do not try \
         to read, run or change anything — write up what you already know from the work \
         you just did, and say plainly where it was cut off.\n\n{}",
        handoff_tail(task, false)
    )
}

/// Fold one line of `--output-format stream-json` into the summary.
///
/// The stream is one JSON object per line. Every field is optional because the
/// exact shape has changed across CLI versions — an unrecognized line is
/// skipped, never fatal.
pub fn absorb_line(summary: &mut RunSummary, line: &str) -> Vec<TraceEvent> {
    let mut events = Vec::new();
    let line = line.trim();
    if line.is_empty() {
        return events;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return events; // Not JSON (a stray log line) — skip it.
    };

    if let Some(id) = v.get("session_id").and_then(|s| s.as_str()) {
        summary.session_id = Some(id.to_string());
    }
    if let Some(m) = v
        .get("message")
        .and_then(|m| m.get("model"))
        .and_then(|m| m.as_str())
    {
        summary.model = Some(m.to_string());
    }

    // Usage can appear on the result event or nested under message.usage.
    for usage in [
        v.get("usage"),
        v.get("message").and_then(|m| m.get("usage")),
    ]
    .into_iter()
    .flatten()
    {
        // Accumulate every bucket: a multi-turn run reports usage per
        // message, and the cache buckets dominate the real cost.
        for (field, slot) in [
            ("output_tokens", &mut summary.output_tokens),
            ("input_tokens", &mut summary.input_tokens),
            ("cache_read_input_tokens", &mut summary.cache_read_tokens),
            (
                "cache_creation_input_tokens",
                &mut summary.cache_write_tokens,
            ),
        ] {
            if let Some(n) = usage.get(field).and_then(|n| n.as_i64()) {
                RunSummary::add(slot, n);
            }
        }
    }

    match v.get("type").and_then(|t| t.as_str()) {
        Some("result") => {
            if let Some(r) = v.get("result").and_then(|r| r.as_str()) {
                summary.result_text = Some(r.to_string());
            }
            // **The terminating event's `subtype` is where a stopped run says
            // why**, and reading only `result` threw that away. An
            // `error_max_turns` result carries NO `result` field at all — it
            // announces itself through `subtype`/`is_error` — and stderr on
            // that path is empty, so the run was recorded as a bare `failed`
            // with five lines of narration and nothing that named the ceiling.
            // Diagnosing one took a dig through
            // `~/.claude/projects/…/<session>.jsonl` for a `max_turns_reached`
            // line that never reaches this stream.
            //
            // Handled generally rather than for this one subtype: every other
            // `error_*` is invisible today for exactly the same reason, so an
            // unrecognized one at least names itself.
            match v.get("subtype").and_then(|s| s.as_str()) {
                Some("error_max_turns") => {
                    let n = |keys: [&str; 2]| {
                        keys.iter()
                            .find_map(|k| v.get(*k).and_then(|n| n.as_i64()))
                            .unwrap_or(0)
                    };
                    summary.stop_reason = Some(StopReason::MaxTurns {
                        // Ours anyway, so `resolved` fills it when absent.
                        limit: n(["max_turns", "maxTurns"]),
                        reached: n(["num_turns", "turnCount"]),
                    });
                }
                Some(other) if other.starts_with("error") => {
                    summary.error_text = Some(format!("The CLI ended the run with `{other}`."));
                }
                _ => {}
            }
        }
        Some("assistant") => {
            if let Some(content) = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for block in content {
                    match block.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                summary.assistant_text.push_str(t);
                                summary.assistant_text.push('\n');
                            }
                        }
                        Some("tool_use") => {
                            let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("Tool");
                            let input = block.get("input").unwrap_or(&serde_json::Value::Null);
                            events.push(TraceEvent::tool_started(
                                block
                                    .get("id")
                                    .and_then(|id| id.as_str())
                                    .map(str::to_string),
                                name.to_string(),
                                summarize_tool(name, input),
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("user") => {
            if let Some(content) = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        events.push(TraceEvent::tool_finished(
                            block
                                .get("tool_use_id")
                                .and_then(|id| id.as_str())
                                .map(str::to_string),
                            block
                                .get("is_error")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                        ));
                    }
                }
            }
        }
        _ => {}
    }
    events
}

fn input_str<'a>(input: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| input.get(*key).and_then(|v| v.as_str()))
}

/// Reduce known Claude tool inputs to a useful label without copying arbitrary
/// JSON into the trace. Unknown tools still show their name, which is more
/// honest than guessing which of their fields are safe or meaningful.
fn summarize_tool(name: &str, input: &serde_json::Value) -> String {
    let summary = match name {
        "Read" | "Write" | "Edit" => input_str(input, &["file_path", "path"])
            .map(|path| format!("{name} {path}"))
            .unwrap_or_else(|| name.to_string()),
        "Grep" => {
            let pattern = input_str(input, &["pattern"]).map(|p| bounded(p, 60));
            let path = input_str(input, &["path"]);
            match (pattern, path) {
                (Some(pattern), Some(path)) => format!("Grep “{pattern}” in {path}"),
                (Some(pattern), None) => format!("Grep “{pattern}”"),
                _ => "Grep".into(),
            }
        }
        "Glob" => input_str(input, &["pattern"])
            .map(|pattern| format!("Glob {}", bounded(pattern, 80)))
            .unwrap_or_else(|| "Glob".into()),
        "Bash" => input_str(input, &["command"])
            .map(|command| format!("Bash {}", command_label(command)))
            .unwrap_or_else(|| "Bash".into()),
        _ => name.to_string(),
    };
    bounded(&summary, 240)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tasks::{create, NewTask};
    use crate::runner::ToolTier;
    use std::path::PathBuf;
    use std::time::Duration;

    fn req(tier: ToolTier, model: Option<&str>) -> RunRequest {
        RunRequest {
            task_id: "t".into(),
            repo: PathBuf::from("/tmp"),
            tier,
            kind: crate::runner::RunKind::Work,
            prompt: "do the thing".into(),
            scratch: PathBuf::from("/tmp/scratch"),
            model: model.map(str::to_string),
            max_turns: 150,
            timeout: Duration::from_secs(60),
            token_ceiling: 0,
            extra_mcp: Vec::new(),
        }
    }

    fn argv(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect()
    }

    /// **What replaces the repo gate is the tool surface.**
    ///
    /// `--strict-mcp-config` means "only what `--mcp-config` names", so this
    /// file is the COMPLETE set of MCP servers a run can see — the user's own
    /// ~/.claude.json is ignored. A detached run has no repo doing the bounding,
    /// so this file IS the bound, and the default of nothing-but-write-back is
    /// the reason an ungranted detached run can read nothing at all.
    #[test]
    fn the_mcp_config_names_exactly_what_was_granted_and_nothing_else() {
        let scratch = std::env::temp_dir().join(format!("cxtasks-mcpcfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&scratch).unwrap();

        let mut r = req(ToolTier::ReadOnly, None);
        r.scratch = scratch.clone();

        // Ungranted: the write-back router, alone.
        if let Some(path) = mcp_config_path(&r) {
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let servers = v["mcpServers"].as_object().unwrap();
            assert_eq!(servers.len(), 1, "an ungranted run must see only cxtasks");
            assert!(servers.contains_key("cxtasks"));
        }

        // Granted one server: exactly that one is added.
        r.extra_mcp = vec![crate::mcp_registry::ResolvedServer {
            name: "vault",
            program: "/opt/vault-mcp".into(),
            args: vec!["--stdio".into()],
        }];
        if let Some(path) = mcp_config_path(&r) {
            let v: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let servers = v["mcpServers"].as_object().unwrap();
            assert_eq!(servers.len(), 2);
            assert_eq!(servers["vault"]["command"], "/opt/vault-mcp");
            assert_eq!(servers["vault"]["args"][0], "--stdio");
            assert!(
                servers.contains_key("cxtasks"),
                "the write-back router must survive a grant — a run that cannot report \
                 back is a run whose result is lost (invariant 18)"
            );
        }
        std::fs::remove_dir_all(scratch).ok();
    }

    /// Network egress stays denied at EVERY tier, and is unaffected by any of
    /// this. Not an oversight to fix later: what an unattended run may fetch
    /// and what it may send are their own question.
    #[test]
    fn web_tools_stay_denied_at_every_tier() {
        for tier in [ToolTier::ReadOnly, ToolTier::Shell, ToolTier::Write] {
            let denied = tier.denied();
            assert!(denied.contains("WebFetch"), "{tier:?}: {denied}");
            assert!(denied.contains("WebSearch"), "{tier:?}: {denied}");
        }
    }

    #[test]
    fn command_carries_both_halves_of_the_tool_grant() {
        let cmd = command(
            Path::new("/bin/claude"),
            &req(ToolTier::ReadOnly, Some("m")),
        );
        let args = argv(&cmd);
        assert!(args.contains(&"--allowedTools".to_string()));
        assert!(
            args.contains(&"--disallowedTools".to_string()),
            "omission is not denial — the deny list is the half that enforces"
        );
        assert!(args.contains(&"--strict-mcp-config".to_string()));
        assert!(args.contains(&"--max-turns".to_string()));

        // The write-back tools ride in `--allowedTools` at EVERY tier. An MCP
        // tool named in neither `--allowedTools` nor the user's settings is a
        // permission prompt, and an unattended run has nobody to answer it — so
        // the tool would simply never fire, silently. Leaving a handoff must
        // not depend on whether the run was allowed to edit files.
        let allowed = args
            .iter()
            .position(|a| a == "--allowedTools")
            .and_then(|i| args.get(i + 1))
            .expect("--allowedTools has a value");
        for tool in crate::mcp::server::BACKGROUND_TOOLS_CSV.split(',') {
            assert!(
                allowed.contains(tool),
                "{tool} missing from the grant: {allowed}"
            );
        }
        // ...and the tier's own grant is still intact beside them.
        assert!(allowed.contains("Read"), "{allowed}");
    }

    #[test]
    fn command_omits_the_model_flag_when_no_model_is_given() {
        let cmd = command(Path::new("/bin/claude"), &req(ToolTier::ReadOnly, None));
        assert!(!argv(&cmd).contains(&"--model".to_string()));
    }

    // ── stream-json parsing ────────────────────────────────────────────

    #[test]
    fn stream_extracts_result_session_and_tokens() {
        let mut s = RunSummary::default();
        absorb_line(
            &mut s,
            r#"{"type":"system","subtype":"init","session_id":"abc-1"}"#,
        );
        absorb_line(
            &mut s,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working"}],"usage":{"output_tokens":10}}}"#,
        );
        absorb_line(
            &mut s,
            r#"{"type":"result","result":"All green.","usage":{"output_tokens":5}}"#,
        );
        assert_eq!(s.session_id.as_deref(), Some("abc-1"));
        assert_eq!(s.output_tokens, Some(15), "usage should accumulate");
        assert_eq!(s.best_text(), "All green.");
    }

    /// **The defect this whole change starts from.** An `error_max_turns`
    /// result carries no `result` field — it announces itself through
    /// `subtype` — so reading only `result` recorded a run stopped by our own
    /// ceiling as a bare `failed` with nothing anywhere naming the ceiling.
    /// stderr was empty too, so the `--- stderr ---` branch never fired either.
    #[test]
    fn a_turn_cutoff_is_read_off_the_subtype_because_it_carries_no_result() {
        let mut s = RunSummary::default();
        absorb_line(
            &mut s,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"still working"}]}}"#,
        );
        absorb_line(
            &mut s,
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":31,"usage":{"output_tokens":5}}"#,
        );
        assert_eq!(
            s.stop_reason,
            Some(StopReason::MaxTurns {
                limit: 0,
                reached: 31
            }),
            "the limit is ours to fill in; the turn it died on is only here"
        );
        assert_eq!(
            s.error_text, None,
            "our ceiling stopping a healthy run is not the provider reporting a failure"
        );
        // And the narration survives, which is what makes a salvage worth doing.
        assert_eq!(s.best_text(), "still working");

        // The limit we passed fills the gap the event left, so the sentence the
        // task ends up carrying names both numbers.
        assert_eq!(
            s.stop_reason.unwrap().resolved(150).describe(),
            "hit its 150-turn ceiling at turn 31"
        );
    }

    /// The hole was general, not specific to one subtype: every `error_*` was
    /// invisible for the same reason. An unrecognized one must at least name
    /// itself rather than producing a run that failed for no stated cause.
    #[test]
    fn any_other_error_subtype_at_least_says_what_it_was() {
        let mut s = RunSummary::default();
        absorb_line(
            &mut s,
            r#"{"type":"result","subtype":"error_during_execution","is_error":true}"#,
        );
        assert_eq!(s.stop_reason, None, "only a turn cutoff is a stop reason");
        let err = s.error_text.expect("an error subtype must be surfaced");
        assert!(err.contains("error_during_execution"), "{err}");
    }

    /// A healthy run must be untouched by the subtype parsing above.
    #[test]
    fn a_successful_result_subtype_changes_nothing() {
        let mut s = RunSummary::default();
        absorb_line(
            &mut s,
            r#"{"type":"result","subtype":"success","is_error":false,"num_turns":4,"result":"All green."}"#,
        );
        assert_eq!(s.best_text(), "All green.");
        assert_eq!(s.stop_reason, None);
        assert_eq!(s.error_text, None);
    }

    /// The salvage's whole value is that it can only WRITE. One turn is
    /// available, so any tool call spends it and the salvage produces nothing —
    /// which means an un-denied tool doesn't degrade the feature, it silently
    /// removes it. `--disallowedTools` is the half that enforces (invariant 7),
    /// and the read tools are in nobody else's deny list.
    #[test]
    fn the_salvage_resumes_with_one_turn_and_no_tools_at_all() {
        let task = task_with(true, true);
        let cmd = salvage_command(
            Path::new("/bin/claude"),
            &req(ToolTier::Write, Some("claude-sonnet-5")),
            &task,
            "sess-abc",
        );
        let args = argv(&cmd);

        let at = args
            .iter()
            .position(|a| a == "--resume")
            .expect("no --resume");
        assert_eq!(args[at + 1], "sess-abc");
        let turns = args
            .iter()
            .position(|a| a == "--max-turns")
            .expect("no --max-turns");
        assert_eq!(args[turns + 1], "1");

        // No MCP config beside `--strict-mcp-config` means no MCP at all — the
        // write-back tools are tools like any other with one turn to spend.
        assert!(args.contains(&"--strict-mcp-config".to_string()));
        assert!(
            !args.contains(&"--mcp-config".to_string()),
            "the salvage must not load a server it has no turn to call: {args:?}"
        );

        let denied = args
            .iter()
            .position(|a| a == "--disallowedTools")
            .and_then(|i| args.get(i + 1))
            .expect("--disallowedTools has a value");
        for tool in crate::runner::DANGEROUS_TOOLS {
            assert!(denied.contains(tool), "{tool} not denied: {denied}");
        }
        for read_tool in ["Read", "Glob", "Grep"] {
            assert!(
                denied.contains(read_tool),
                "{read_tool} is in no other deny list, so leaving it out here is the \
                 silent failure: {denied}"
            );
        }

        // Same model as the run being rescued — switching mid-session would
        // change who writes the handoff.
        let model = args
            .iter()
            .position(|a| a == "--model")
            .expect("no --model");
        assert_eq!(args[model + 1], "claude-sonnet-5");
    }

    /// One definition of what a handoff is (invariant 18). A second, terser copy
    /// written for the salvage is the one that would drift.
    #[test]
    fn the_salvage_prompt_reuses_the_handoff_ask_verbatim() {
        let task = task_with(false, false);
        let prompt = salvage_prompt(&task);
        assert!(prompt.contains("stopped at its turn ceiling"), "{prompt}");
        assert!(
            prompt.ends_with(&handoff_tail(&task, false)),
            "the ask must be the shared one, not a paraphrase: {prompt}"
        );
        assert!(
            !prompt.contains("mcp__cxtasks__"),
            "the salvage has no MCP, so it must name no tools: {prompt}"
        );
    }

    #[test]
    fn stream_falls_back_to_assistant_text_without_a_result_event() {
        let mut s = RunSummary::default();
        absorb_line(
            &mut s,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"partial answer"}]}}"#,
        );
        assert_eq!(s.best_text(), "partial answer");
    }

    #[test]
    fn stream_skips_garbage_lines_instead_of_dying() {
        let mut s = RunSummary::default();
        absorb_line(&mut s, "not json at all");
        absorb_line(&mut s, "");
        absorb_line(&mut s, "{ truncated");
        absorb_line(&mut s, r#"{"type":"result","result":"ok"}"#);
        assert_eq!(s.best_text(), "ok");
    }

    #[test]
    fn stream_reports_empty_runs_honestly() {
        let s = RunSummary::default();
        assert_eq!(s.best_text(), "(the run produced no output)");
    }

    #[test]
    fn stream_ignores_tool_use_blocks_in_assistant_text() {
        let mut s = RunSummary::default();
        let events = absorb_line(
            &mut s,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-1","name":"Read","input":{"file_path":"/repo/src/main.rs"}},{"type":"text","text":"read the file"}]}}"#,
        );
        assert_eq!(s.best_text(), "read the file");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].call_id.as_deref(), Some("tool-1"));
        assert_eq!(events[0].summary, "Read /repo/src/main.rs");
        assert_eq!(events[0].phase, "started");
    }

    #[test]
    fn stream_matches_tool_results_without_persisting_their_output() {
        let mut s = RunSummary::default();
        let events = absorb_line(
            &mut s,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","is_error":true,"content":"secret output"}]}}"#,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].call_id.as_deref(), Some("tool-1"));
        assert_eq!(events[0].phase, "failed");
        assert!(events[0].summary.is_empty());
        assert!(!format!("{events:?}").contains("secret output"));
    }

    // ── prompt tail ────────────────────────────────────────────────────

    fn task_with(shell: bool, writes: bool) -> Task {
        let conn = crate::db::schema::test_conn();
        create(
            &conn,
            &NewTask {
                title: "x".into(),
                repo_path: Some("/tmp".into()),
                bg_allowed: Some(true),
                bg_shell_allowed: Some(shell),
                bg_writes_allowed: Some(writes),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn read_only_tail_forbids_edits_and_self_completion() {
        let p = prompt_tail(&task_with(false, false));
        assert!(p.contains("Do not edit, create or delete files"));
        // Self-completion is structural now: `complete_task` is not in the
        // router a background run sees, so the tail must not name it at all.
        assert!(!p.contains("complete_task"), "{p}");
        assert!(p.contains("FINAL MESSAGE is the result"));
    }

    /// The tail has to match what the run can actually reach. A prompt that
    /// names `file_followup` in a tree where the MCP binary was never built
    /// (or never copied into the bundle — gotcha #5) burns turns on a tool
    /// that isn't there, then reports a failure unrelated to the task.
    #[test]
    fn the_tail_only_names_the_write_back_tools_when_they_exist() {
        let t = task_with(false, false);

        let with_tools = handoff_tail(&t, true);
        assert!(with_tools.contains("mcp__cxtasks__file_followup"));
        assert!(with_tools.contains("mcp__cxtasks__comment_task"));
        assert!(with_tools.contains("three CXTasks tools and no others"));

        let without = handoff_tail(&t, false);
        assert!(
            !without.contains("mcp__cxtasks__"),
            "no MCP means no tool names: {without}"
        );
        assert!(
            without.contains("final message is the ONLY channel"),
            "and it has to say where the follow-ups go instead: {without}"
        );
    }

    /// **The ship-blocking one.** `investigating` is `!writes && !shell`, which
    /// is true for every DEFAULT review — read-only is exactly what arming one
    /// preselects. So without the review branch the reviewer is handed "the plan
    /// IS the deliverable", i.e. told to redo the planning work it was
    /// dispatched to audit: the failure this whole feature exists to prevent,
    /// one level down.
    ///
    /// Paired with the work-tier assertion so the "fix" cannot be deleting the
    /// read-only branch instead of branching it.
    #[test]
    fn a_review_is_asked_for_a_verdict_rather_than_a_plan() {
        let mut reviewing = task_with(false, false);
        reviewing.bg_intent = crate::intent::BackgroundIntent::Review;
        let tail = handoff_tail(&reviewing, true);
        assert!(tail.contains("The VERDICT is the deliverable"), "{tail}");
        assert!(
            !tail.contains("the plan IS the deliverable"),
            "a reviewer must not be told to write the plan it is auditing: {tail}"
        );

        // Every tier, not just read-only: a write-capable review still holds
        // `Edit`, and a critique brief written while holding it pulls into
        // fixing.
        let mut writing = task_with(true, true);
        writing.bg_intent = crate::intent::BackgroundIntent::Review;
        let write_tail = handoff_tail(&writing, true);
        assert!(
            write_tail.contains("The VERDICT is the deliverable"),
            "{write_tail}"
        );

        // Read-only limits the available actions, not the requested deliverable.
        let working = task_with(false, false);
        let work_tail = handoff_tail(&working, true);
        assert!(
            work_tail.contains("do not replace the requested deliverable with a plan"),
            "{work_tail}"
        );
        assert!(
            !work_tail.contains("The VERDICT is the deliverable"),
            "{work_tail}"
        );
        assert!(work_tail.contains(super::super::super::completion::MARKER));
        assert!(!write_tail.contains(super::super::super::completion::MARKER));
    }

    /// `runner::handoff_summary` lifts the opening line verbatim into the
    /// comment log. Bolting on a SECOND "START WITH ONE SENTENCE" instead of
    /// parameterising the one that exists makes every review's comment-log line
    /// a preamble about how to write, rather than the verdict.
    #[test]
    fn a_review_opens_with_the_verdict_because_that_is_what_the_comment_log_shows() {
        let mut reviewing = task_with(false, false);
        reviewing.bg_intent = crate::intent::BackgroundIntent::Review;
        let tail = handoff_tail(&reviewing, true);
        assert_eq!(
            tail.matches("START WITH ONE SENTENCE").count(),
            1,
            "exactly one opening-line ask, or the summary becomes a preamble: {tail}"
        );
        assert!(tail.contains("giving your VERDICT"), "{tail}");
        assert!(
            !tail.contains("saying what came of the run"),
            "the work phrasing must be replaced, not accompanied: {tail}"
        );
    }

    /// The shell tier is not read-only and not write — saying "do not edit
    /// files" to a run that can `>` a file is a lie, and saying nothing invites
    /// it to reach for `sed -i`. It gets its own wording.
    #[test]
    fn shell_tail_permits_commands_but_asks_for_no_deliberate_edits() {
        let p = prompt_tail(&task_with(true, false));
        assert!(p.contains("do not ask"));
        assert!(p.contains("do NOT deliberately edit source files"));
        assert!(!p.contains("Do not edit, create or delete files"));
    }

    #[test]
    fn write_tail_says_dont_ask_questions() {
        let p = prompt_tail(&task_with(true, true));
        assert!(p.contains("do not ask"));
        assert!(!p.contains("Do not edit, create or delete files"));
        assert!(!p.contains("do NOT deliberately edit source files"));
    }
}
