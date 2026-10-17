//! The Codex provider — `codex exec`, non-interactive.
//!
//! # What was verified against the installed CLI (0.140.0), and what wasn't
//!
//! Getting this wrong is not a compile error, so the flags below were checked
//! against the real binary rather than against documentation:
//!
//! * **`--ask-for-approval` and `--sandbox` are GLOBAL flags, not `exec` flags.**
//!   `codex exec --help` does not list either. They have to appear *before* the
//!   `exec` subcommand; put them after and clap rejects the invocation.
//! * **`sandbox_workspace_write.network_access` is a real config field** (it is
//!   in the binary's `SandboxWorkspaceWrite` struct alongside `writable_roots`
//!   and `exclude_slash_tmp`), so the `-c` override below is not a typo that
//!   silently does nothing. But see [`NETWORK_OFF`] — the claim it supports is
//!   narrower than "the run has no network".
//! * **A stdio MCP can be supplied per run with `mcp_servers.<name>` config.**
//!   `command`, `args`, `env`, `required`, `enabled_tools` and
//!   `default_tools_approval_mode` were all checked through `codex mcp list`
//!   with the real CLI. That gives Codex the same reduced write-back surface as
//!   Claude without installing it in the user's global Codex configuration.
//! * **`--search` is off unless passed**, so web search is absent by omission.
//!
//! # Two traps that cost real time
//!
//! **`codex` on PATH is a `#!/usr/bin/env node` shim**, not a native binary —
//! the real executable is vendored inside the npm package. The runner is a
//! LaunchAgent, whose `PATH` is roughly `/usr/bin:/bin:/usr/sbin:/sbin`, and
//! Chris's `node` lives under nvm. So spawning the resolved path directly from
//! the runner fails with `env: node: No such file or directory` — a failure that
//! looks like "Codex is broken" and is really "the interpreter isn't on PATH".
//! [`spawn_path`] fixes it by prepending the shim's own directory (nvm puts
//! `node` and `codex` side by side) and, failing that, resolving the interpreter
//! through a login shell.
//!
//! **`codex` in Chris's interactive shell is a function**, not the binary — it
//! injects `--ask-for-approval on-request` (`~/.zshrc`). Exactly the same shape
//! as the `claude` wrapper in gotcha #7, and with exactly the same consequence:
//! a permission or flag test typed into a terminal proves nothing about what
//! this code does. Test `command codex`, or the path [`find_cli`] returns.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::db::tasks::Task;
use crate::runner::providers::{bounded, command_label, RunRequest, RunSummary, TraceEvent};
use crate::runner::ToolTier;

/// Where Codex is told to write its final message.
///
/// A file rather than "the last `agent_message` we saw on stdout", because the
/// file is what the CLI itself considers the answer: a run whose stream we
/// mis-parse still produces a correct result, and the stream stays a fallback.
pub const LAST_MESSAGE_FILE: &str = "codex-last-message.txt";

/// Turn off the workspace sandbox's network access.
///
/// **What this is worth, precisely.** The key is real and the sandbox does block
/// egress. What could NOT be demonstrated from outside a paid run is that this
/// override is the thing doing the blocking — `codex sandbox` (the free probe)
/// denies DNS with the value set to *either* `true` or `false`, so it is not an
/// oracle for what `codex exec` does. Treat this as defence in depth, not as a
/// seal, and do not write "Codex background runs cannot reach the network"
/// anywhere on the strength of it. That is the same mistake invariant 12
/// documents for `WebFetch`/`WebSearch`: denying the obvious route is worth
/// doing and is not the same as closing the network.
pub const NETWORK_OFF: &str = "sandbox_workspace_write.network_access=false";

/// Locate the `codex` CLI.
///
/// Login shell first for the same reason as [`crate::dispatch::find_claude_cli`]:
/// a GUI app on macOS doesn't inherit the shell's PATH, so `/usr/bin/which` runs
/// against a bare-bones PATH and misses every user install — which is where
/// Codex actually lives (nvm).
pub fn find_cli() -> Option<PathBuf> {
    if let Ok(output) = Command::new("/bin/zsh")
        .arg("-lc")
        // `command -v` deliberately, and it is load-bearing: `-lc` is
        // non-interactive so `.zshrc` is not sourced and the `codex` shell
        // function is not defined, but `command` also makes that explicit.
        .arg("command -v codex")
        .output()
    {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !s.is_empty() {
                let p = PathBuf::from(&s);
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }

    for candidate in &["/opt/homebrew/bin/codex", "/usr/local/bin/codex"] {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return Some(p);
        }
    }
    if let Some(home) = dirs::home_dir() {
        for rel in &[
            ".local/bin/codex",
            ".codex/bin/codex",
            ".bun/bin/codex",
            ".volta/bin/codex",
            ".asdf/shims/codex",
        ] {
            let p = home.join(rel);
            if p.exists() {
                return Some(p);
            }
        }

        // nvm deliberately does not put a stable shim in ~/.nvm/bin. Its
        // executables live under one directory per Node version, and a
        // LaunchAgent does not source nvm.sh, so neither the bare PATH nor the
        // login-shell probe above can see them. Prefer the newest installed
        // version that actually contains Codex; `spawn_path` then adds the
        // matching `node` beside it when the runner starts the npm shim.
        if let Some(p) = newest_nvm_cli(&home) {
            return Some(p);
        }

        // The macOS ChatGPT app ships the same native Codex CLI. This is a
        // useful final fallback for a machine that can use Codex in the
        // desktop app but has not installed the npm package globally.
        for p in [
            PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex"),
            home.join("Applications/ChatGPT.app/Contents/Resources/codex"),
        ] {
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// The newest nvm-managed Node version that has a Codex install.
///
/// This is filesystem discovery rather than `source nvm.sh`: the availability
/// probe runs while the launch console is being configured, and sourcing the
/// whole version manager on every probe is both slow and shell-config
/// dependent. Version directories use nvm's `vMAJOR.MINOR.PATCH` naming, so a
/// numeric key avoids the `v9` > `v24` mistake of lexical sorting.
fn newest_nvm_cli(home: &Path) -> Option<PathBuf> {
    let versions = home.join(".nvm/versions/node");
    let mut candidates: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(versions)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let cli = entry.path().join("bin/codex");
            if !cli.exists() {
                return None;
            }
            let name = entry.file_name();
            let version = name
                .to_string_lossy()
                .trim_start_matches('v')
                .split('.')
                .map(|part| {
                    part
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>()
                        .parse::<u64>()
                        .unwrap_or(0)
                })
                .collect();
            Some((version, cli))
        })
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().next().map(|(_, path)| path)
}

/// Is Codex installed and logged in?
///
/// Both halves are checked at **queue** time, because the two failures need
/// different answers from the user and neither is discoverable from a task
/// sitting at `queued`. Never inspects, copies, or writes credentials — the
/// answer is `codex login status`'s exit code, and re-authenticating is
/// something only Chris can do.
pub fn preflight() -> Result<(), String> {
    let Some(cli) = find_cli() else {
        return Err(
            "The Codex CLI (`codex`) wasn't found. Install it (e.g. \
             `npm install -g @openai/codex`) and try again."
                .into(),
        );
    };
    login_status(&cli)
}

/// `codex login status` — read-only, no tokens spent.
pub fn login_status(cli: &Path) -> Result<(), String> {
    let output = Command::new(cli)
        .arg("login")
        .arg("status")
        .env("PATH", spawn_path(cli))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run `codex login status`: {e}"))?;

    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr)
        .lines()
        .chain(String::from_utf8_lossy(&output.stdout).lines())
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no detail")
        .to_string();
    Err(format!(
        "Codex is not logged in ({detail}). Run `codex login` in a terminal, then queue \
         this task again."
    ))
}

/// A `PATH` for the child that can actually find Codex's interpreter.
///
/// See the module docs: the installed `codex` is a `#!/usr/bin/env node` script
/// and the runner's inherited PATH (launchd's) does not contain nvm's `node`.
/// The shim's own directory is tried first because that is where nvm puts both;
/// a login shell is the fallback for a layout where they're apart.
pub fn spawn_path(cli: &Path) -> String {
    let inherited = std::env::var("PATH").unwrap_or_default();
    let mut prefix: Vec<String> = Vec::new();

    if let Some(dir) = cli.parent() {
        prefix.push(dir.to_string_lossy().to_string());
    }

    if let Some(interp) = shebang_interpreter(cli) {
        let already = cli
            .parent()
            .map(|d| d.join(&interp).exists())
            .unwrap_or(false);
        if !already {
            if let Some(dir) = resolve_via_login_shell(&interp) {
                prefix.push(dir);
            }
        }
    }

    prefix.push(inherited);
    prefix.join(":")
}

/// The interpreter a `#!/usr/bin/env NAME` script needs, if it is one.
fn shebang_interpreter(cli: &Path) -> Option<String> {
    let bytes = std::fs::read(cli).ok()?;
    // Only the first line matters, and a real binary's first bytes are not `#!`.
    let first = bytes.split(|b| *b == b'\n').next()?;
    let line = std::str::from_utf8(first).ok()?.trim();
    let rest = line.strip_prefix("#!")?.trim();
    let mut parts = rest.split_whitespace();
    let head = parts.next()?;
    if head.ends_with("/env") {
        // `#!/usr/bin/env node` — the name we need is the next word.
        return parts.next().map(str::to_string);
    }
    // `#!/usr/bin/node` — already absolute, nothing to look up.
    None
}

fn resolve_via_login_shell(program: &str) -> Option<String> {
    let out = Command::new("/bin/zsh")
        .arg("-lc")
        .arg(format!("command -v {program}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    path.parent().map(|d| d.to_string_lossy().to_string())
}

/// The sandbox policy for a tier.
///
/// Codex has no read-plus-shell-but-no-writes mode, so `Shell` and `Write` both
/// map to `workspace-write`. That is not a downgrade of the guarantee, because
/// the guarantee was never the tool list: a shell can write either way — which
/// is exactly why [`ToolTier::needs_checkpoint`] already treats those two tiers
/// identically and snapshots the tree for both. What separates them here is the
/// prompt, and the honest framing of the middle tier is "commands and tests,
/// not deliberate edits" rather than "cannot write".
pub fn sandbox_for(tier: ToolTier) -> &'static str {
    match tier {
        ToolTier::ReadOnly => "read-only",
        ToolTier::Shell | ToolTier::Write => "workspace-write",
    }
}

/// One `-c` value describing the reduced CXTasks stdio MCP.
///
/// JSON string/array syntax is also valid TOML syntax, which lets serde do the
/// path escaping instead of maintaining a second, subtly different quoting
/// implementation here. The client allow-list is defence in depth beside the
/// server's reduced router: even a skewed binary cannot advertise another
/// CXTasks tool to this run. `required` closes the third state between
/// "present" and "absent" — if the file exists but cannot initialize, fail the
/// run instead of giving the agent a tail that promises tools it never got.
fn writeback_config_override(bin: &Path) -> String {
    let command = serde_json::to_string(&bin.to_string_lossy())
        .expect("serializing an MCP binary path cannot fail");
    let tools = serde_json::to_string(crate::mcp::server::BACKGROUND_TOOLS)
        .expect("serializing the static MCP tool list cannot fail");
    format!(
        "mcp_servers.cxtasks={{command={command},args=[],env={{{}=\"1\",{}=\"codex\"}},\
         required=true,enabled_tools={tools},default_tools_approval_mode=\"approve\"}}",
        crate::mcp::server::BACKGROUND_ENV,
        crate::mcp::server::AGENT_ENV
    )
}

/// One `codex exec` invocation.
pub fn command(cli: &Path, req: &RunRequest) -> Command {
    let writeback = super::writeback_binary();
    command_with_writeback(cli, req, writeback.as_deref())
}

fn command_with_writeback(cli: &Path, req: &RunRequest, writeback: Option<&Path>) -> Command {
    let mut cmd = Command::new(cli);

    // ── GLOBAL flags: these must precede `exec` (see the module docs) ────
    cmd.arg("--ask-for-approval")
        // Never, not `on-request`: nobody is watching, and an unattended run
        // that stops to ask is a run that holds a concurrency slot until the
        // wall-clock timeout kills it.
        .arg("never")
        .arg("--sandbox")
        .arg(sandbox_for(req.tier))
        .arg("-c")
        .arg(NETWORK_OFF);
    if let Some(bin) = writeback {
        cmd.arg("-c").arg(writeback_config_override(bin));
    }
    // `--dangerously-bypass-approvals-and-sandbox` is deliberately never passed.
    // It is the one flag that would make "unattended" mean "unsandboxed", and
    // there is no task worth reaching for it — pinned by a test below.

    cmd.arg("exec")
        // A misspelled config key would otherwise make the run appear healthy
        // while silently dropping either the sandbox guard or the MCP. The
        // installed CLI supports strict validation for `exec`.
        .arg("--strict-config")
        // Don't file CXTasks runs into Codex's own session history: this is our
        // queue's work, and it would bury the user's real `codex resume` list.
        .arg("--ephemeral")
        // The counterpart of Claude's `--strict-mcp-config`: don't inherit the
        // user's personal MCP servers or a permissive `config.toml` into an
        // unattended run. Auth still resolves from CODEX_HOME, so this costs
        // nothing but blast radius.
        .arg("--ignore-user-config")
        .arg("--json")
        .arg("--cd")
        .arg(&req.repo)
        .arg("--output-last-message")
        .arg(req.scratch.join(LAST_MESSAGE_FILE));

    // Only safe for the read-only tier, and only because the sandbox — not git —
    // is what stops mutation there. The other two tiers are refused outright
    // without a repo by `runner::preflight`, since their whole undo story is the
    // checkpoint.
    if req.tier == ToolTier::ReadOnly {
        cmd.arg("--skip-git-repo-check");
    }

    // Passed ONLY when the task names one. There is no `DEFAULT_CODEX_MODEL`
    // constant on purpose: pinning an OpenAI model name in this repo means it
    // goes stale on their release schedule, and a stale name is a run that dies
    // at spawn for a reason with nothing to do with the task.
    if let Some(model) = req.model.as_deref().filter(|m| !m.trim().is_empty()) {
        cmd.arg("--model").arg(model);
    }

    cmd.arg(&req.prompt)
        .current_dir(&req.repo)
        .env("PATH", spawn_path(cli))
        .env("CXTASKS_BACKGROUND_RUN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// The background-Codex tail of the seed prompt.
pub fn prompt_tail(task: &Task) -> String {
    prompt_tail_with_writeback(task, super::writeback_binary().is_some())
}

fn prompt_tail_with_writeback(task: &Task, has_tools: bool) -> String {
    let mut s = String::new();
    s.push_str(
        "This is an UNATTENDED CXTasks background run under Codex. Nobody is watching, so do \
         not ask questions — make the most reasonable call and say what you assumed. Work only \
         within the sandbox you were granted; if something is genuinely outside it, say so \
         rather than trying to work around it.\n\n",
    );
    if !task.bg_writes_allowed && !task.bg_shell_allowed {
        s.push_str(
            "You are in a READ-ONLY sandbox. You may read the repo and run read-only commands; \
             attempts to modify anything will be refused by the sandbox, so don't spend the run \
             on them. Investigate and report.\n\n",
        );
    } else if !task.bg_writes_allowed {
        s.push_str(
            "You may run commands — builds, tests, linters, git inspection. Do NOT deliberately \
             edit source files: this task was granted commands, not edits. Commands that create \
             caches, lockfiles or build artifacts are expected and fine; the working tree was \
             snapshotted before you started, so those are reviewable and undoable.\n\n",
        );
    } else {
        s.push_str(
            "You may edit files and run commands. The working tree was snapshotted before you \
             started, so your changes can be reviewed as a diff and rolled back. Implement the \
             work and verify it.\n\n",
        );
    }
    // The SAME handoff ask as Claude's, parameterized by the SAME sibling
    // binary check that controls `command`. A dev tree or app bundle without
    // the binary still degrades cleanly to a final-message-only run.
    s.push_str(&super::claude::handoff_tail(task, has_tools));
    s
}

/// Fold one line of `codex exec --json` (JSONL) into the summary.
///
/// Token buckets are accumulated **raw** here and normalized in [`finalize`] —
/// `cached_input_tokens` is a subset of `input_tokens`, so subtracting once at
/// the end is both simpler and correct across a multi-turn run, where doing it
/// per event would need the same clamp repeated on every turn.
pub fn absorb_line(summary: &mut RunSummary, line: &str) -> Vec<TraceEvent> {
    let mut events = Vec::new();
    let line = line.trim();
    if line.is_empty() {
        return events;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return events;
    };

    // `thread_id` is Codex's session handle. Accept `session_id` too: the field
    // has been spelled both ways across versions and neither is worth failing
    // a run over.
    for key in ["thread_id", "session_id"] {
        if let Some(id) = v.get(key).and_then(|s| s.as_str()).filter(|s| !s.is_empty()) {
            summary.session_id = Some(id.to_string());
        }
    }
    if let Some(m) = v.get("model").and_then(|m| m.as_str()) {
        summary.model = Some(m.to_string());
    }

    match v.get("type").and_then(|t| t.as_str()) {
        Some("turn.completed") => {
            // ONLY this event's `usage` is read. Codex also carries a running
            // `total_token_usage` elsewhere in its event vocabulary, and
            // absorbing both would double-count the entire run.
            if let Some(usage) = v.get("usage") {
                for (field, slot) in [
                    ("input_tokens", &mut summary.input_tokens),
                    ("cached_input_tokens", &mut summary.cache_read_tokens),
                    ("output_tokens", &mut summary.output_tokens),
                ] {
                    if let Some(n) = usage.get(field).and_then(|n| n.as_i64()) {
                        RunSummary::add(slot, n);
                    }
                }
            }
        }
        Some("item.started") | Some("item.completed") | Some("item.updated") => {
            if let Some(item) = v.get("item") {
                let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if item_type == "agent_message" {
                    if matches!(v.get("type").and_then(|t| t.as_str()), Some("item.completed")) {
                        if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                            summary.assistant_text.push_str(t);
                            summary.assistant_text.push('\n');
                        }
                    }
                } else if let Some((tool_name, event_summary)) = summarize_item(item_type, item) {
                    let call_id = item.get("id").and_then(|id| id.as_str()).map(str::to_string);
                    let failed = item.get("status").and_then(|s| s.as_str()) == Some("failed")
                        || item.get("error").is_some_and(|e| !e.is_null())
                        || item.get("exit_code").and_then(|n| n.as_i64()).is_some_and(|n| n != 0);
                    let completed = matches!(
                        v.get("type").and_then(|t| t.as_str()),
                        Some("item.completed")
                    ) || matches!(
                        item.get("status").and_then(|s| s.as_str()),
                        Some("completed") | Some("failed")
                    );
                    events.push(if completed {
                        let mut event = TraceEvent::tool_finished(call_id, failed);
                        // Codex completion events retain their safe item
                        // metadata, so they do not need the writer's start cache.
                        event.tool_name = Some(tool_name);
                        event.summary = event_summary;
                        event
                    } else {
                        TraceEvent::tool_started(call_id, tool_name, event_summary)
                    });
                }
            }
        }
        Some("turn.failed") | Some("error") => {
            let msg = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .or_else(|| v.get("message").and_then(|m| m.as_str()))
                .unwrap_or("Codex reported a failure with no message");
            summary.error_text = Some(msg.to_string());
            events.push(TraceEvent::notice(format!(
                "Run error · {}",
                bounded(msg, 160)
            )));
        }
        _ => {}
    }
    events
}

fn summarize_item(
    item_type: &str,
    item: &serde_json::Value,
) -> Option<(String, String)> {
    match item_type {
        "command_execution" => {
            let command = item
                .get("command")
                .and_then(|v| v.as_str())
                .map(command_label)
                .unwrap_or_else(|| "command".into());
            Some(("Command".into(), bounded(&format!("Command {command}"), 240)))
        }
        "file_change" => {
            let changes = item.get("changes").and_then(|v| v.as_array());
            let summary = changes
                .and_then(|rows| rows.first().map(|first| (rows, first)))
                .and_then(|(rows, first)| {
                    first.get("path").and_then(|v| v.as_str()).map(|path| {
                        if rows.len() == 1 {
                            format!("File change {path}")
                        } else {
                            format!("File changes · {} files", rows.len())
                        }
                    })
                })
                .unwrap_or_else(|| "File change".into());
            Some(("File change".into(), bounded(&summary, 240)))
        }
        "mcp_tool_call" => {
            let server = item.get("server").and_then(|v| v.as_str());
            let tool = item
                .get("tool")
                .or_else(|| item.get("name"))
                .and_then(|v| v.as_str());
            let label = match (server, tool) {
                (Some(server), Some(tool)) => format!("MCP {server}.{tool}"),
                (_, Some(tool)) => format!("MCP {tool}"),
                _ => "MCP tool".into(),
            };
            Some(("MCP".into(), bounded(&label, 240)))
        }
        "web_search" => Some(("Web search".into(), "Web search".into())),
        // In particular, never persist reasoning items.
        _ => None,
    }
}

/// Read the authoritative final message and normalize the token buckets.
///
/// **Must run on every exit path, including timeout.** Skipping it leaves
/// `input_tokens` still including `cached_input_tokens`, which double-counts the
/// cached prefix into the daily budget — and over-counting the guard that stops
/// runs is a quiet way to make the feature look broken.
pub fn finalize(summary: &mut RunSummary, req: &RunRequest) {
    if let Ok(text) = std::fs::read_to_string(req.scratch.join(LAST_MESSAGE_FILE)) {
        if !text.trim().is_empty() {
            summary.result_text = Some(text.trim_end().to_string());
        }
    }

    // Codex reports `input_tokens` INCLUDING the cached prefix, and
    // `cached_input_tokens` as that prefix. The rest of CXTasks sums
    // input + cache_read + cache_write + output, so leaving both as reported
    // counts the cache twice. Split them instead: the sum still equals Codex's
    // own `total_tokens`, and the buckets line up with Claude's meaning of the
    // same column names.
    if let (Some(input), Some(cached)) = (summary.input_tokens, summary.cache_read_tokens) {
        summary.input_tokens = Some((input - cached).max(0));
    }
    // Codex has no prompt-cache *write* concept to report, and a 0 here would
    // be a claim we can't support. None renders as absent.
    summary.cache_write_tokens = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tasks::{create, NewTask};
    use std::time::Duration;

    fn req(tier: ToolTier, model: Option<&str>) -> RunRequest {
        RunRequest {
            task_id: "t".into(),
            repo: PathBuf::from("/tmp"),
            tier,
            kind: crate::runner::RunKind::Work,
            prompt: "do the thing".into(),
            scratch: PathBuf::from("/tmp/cxtasks-codex-test-scratch"),
            model: model.map(str::to_string),
            max_turns: 150,
            timeout: Duration::from_secs(60),
            token_ceiling: 0,
            extra_mcp: Vec::new(),
        }
    }

    fn argv(cmd: &Command) -> Vec<String> {
        cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect()
    }

    /// `--ask-for-approval` and `--sandbox` are GLOBAL flags: clap rejects the
    /// invocation if they land after the subcommand. Checked by position, not
    /// by presence, because "the flag is in the argv" is exactly the assertion
    /// that would pass on a command the CLI refuses to parse.
    #[test]
    fn global_flags_precede_the_exec_subcommand() {
        let args = argv(&command(Path::new("/bin/codex"), &req(ToolTier::ReadOnly, None)));
        let exec = args.iter().position(|a| a == "exec").expect("no exec subcommand");
        for global in ["--ask-for-approval", "--sandbox", "-c"] {
            let at = args
                .iter()
                .position(|a| a == global)
                .unwrap_or_else(|| panic!("{global} missing from {args:?}"));
            assert!(at < exec, "{global} must come before `exec`, got {args:?}");
        }
        for exec_flag in [
            "--strict-config",
            "--ephemeral",
            "--ignore-user-config",
            "--json",
            "--cd",
        ] {
            let at = args
                .iter()
                .position(|a| a == exec_flag)
                .unwrap_or_else(|| panic!("{exec_flag} missing from {args:?}"));
            assert!(at > exec, "{exec_flag} must come after `exec`, got {args:?}");
        }
    }

    /// An unattended run must never stop to ask a human who isn't there — it
    /// would hold a concurrency slot until the timeout kills it.
    #[test]
    fn approval_is_always_never() {
        for tier in [ToolTier::ReadOnly, ToolTier::Shell, ToolTier::Write] {
            let args = argv(&command(Path::new("/bin/codex"), &req(tier, None)));
            let at = args.iter().position(|a| a == "--ask-for-approval").unwrap();
            assert_eq!(args[at + 1], "never", "{tier:?}");
        }
    }

    /// The one flag that would turn "unattended" into "unsandboxed".
    #[test]
    fn the_dangerous_bypass_flag_is_never_passed() {
        for tier in [ToolTier::ReadOnly, ToolTier::Shell, ToolTier::Write] {
            let args = argv(&command(Path::new("/bin/codex"), &req(tier, None)));
            for banned in [
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-hook-trust",
                // Web search is off by omission; passing it would opt an
                // unattended run into network egress on purpose.
                "--search",
            ] {
                assert!(!args.contains(&banned.to_string()), "{tier:?} passed {banned}");
            }
        }
    }

    #[test]
    fn network_access_is_disabled_at_every_tier() {
        for tier in [ToolTier::ReadOnly, ToolTier::Shell, ToolTier::Write] {
            let args = argv(&command(Path::new("/bin/codex"), &req(tier, None)));
            assert!(
                args.contains(&NETWORK_OFF.to_string()),
                "{tier:?} did not disable workspace network access"
            );
        }
    }

    /// Codex gets the same three-tool write-back surface as Claude, but through
    /// a per-run config override rather than global installation. Keep every
    /// half here: the environment reduces the server router, `enabled_tools`
    /// constrains the client, `approve` prevents an unattended permission
    /// prompt, and `required` prevents a half-started MCP from failing open.
    #[test]
    fn an_available_writeback_binary_is_configured_as_a_required_reduced_mcp() {
        let bin = Path::new(r#"/Applications/CX Tasks "dev"/cxtasks-mcp"#);
        let value = writeback_config_override(bin);
        assert!(
            value.contains(
                r#"command="/Applications/CX Tasks \"dev\"/cxtasks-mcp""#
            ),
            "the path must survive TOML quoting: {value}"
        );
        assert!(value.contains("CXTASKS_BACKGROUND_RUN=\"1\""), "{value}");
        // T238: without this, Codex's comments are signed "claude".
        assert!(value.contains("CXTASKS_AGENT=\"codex\""), "{value}");
        assert!(value.contains("required=true"), "{value}");
        assert!(value.contains("default_tools_approval_mode=\"approve\""), "{value}");
        for tool in crate::mcp::server::BACKGROUND_TOOLS {
            assert!(value.contains(&format!("\"{tool}\"")), "{tool} missing: {value}");
        }
        assert!(!value.contains("complete_task"), "the client allow-list widened: {value}");

        let args = argv(&command_with_writeback(
            Path::new("/bin/codex"),
            &req(ToolTier::ReadOnly, None),
            Some(bin),
        ));
        let exec = args.iter().position(|a| a == "exec").unwrap();
        let at = args.iter().position(|a| a == &value).expect("MCP override missing");
        assert_eq!(args[at - 1], "-c");
        assert!(at < exec, "config overrides are global flags: {args:?}");
    }

    #[test]
    fn a_missing_writeback_binary_leaves_only_the_network_config_override() {
        let args = argv(&command_with_writeback(
            Path::new("/bin/codex"),
            &req(ToolTier::ReadOnly, None),
            None,
        ));
        let values: Vec<&str> = args
            .windows(2)
            .filter(|pair| pair[0] == "-c")
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(values, vec![NETWORK_OFF]);
    }

    #[test]
    fn tiers_map_to_the_right_sandbox() {
        let sandbox_of = |tier| {
            let args = argv(&command(Path::new("/bin/codex"), &req(tier, None)));
            let at = args.iter().position(|a| a == "--sandbox").unwrap();
            args[at + 1].clone()
        };
        assert_eq!(sandbox_of(ToolTier::ReadOnly), "read-only");
        assert_eq!(sandbox_of(ToolTier::Shell), "workspace-write");
        assert_eq!(sandbox_of(ToolTier::Write), "workspace-write");
    }

    /// Read-only may run outside a git repo because the sandbox, not git, is
    /// what stops mutation there. The mutating tiers must not carry the flag —
    /// their undo story is the checkpoint, which needs a repo.
    #[test]
    fn only_read_only_skips_the_git_repo_check() {
        let skips = |tier| {
            argv(&command(Path::new("/bin/codex"), &req(tier, None)))
                .contains(&"--skip-git-repo-check".to_string())
        };
        assert!(skips(ToolTier::ReadOnly));
        assert!(!skips(ToolTier::Shell));
        assert!(!skips(ToolTier::Write));
    }

    #[test]
    fn the_model_flag_appears_only_when_the_task_names_one() {
        let without = argv(&command(Path::new("/bin/codex"), &req(ToolTier::ReadOnly, None)));
        assert!(
            !without.contains(&"--model".to_string()),
            "no model set must mean the CLI's own default, not a hard-coded name"
        );
        let with = argv(&command(
            Path::new("/bin/codex"),
            &req(ToolTier::ReadOnly, Some("gpt-5.2-codex")),
        ));
        let at = with.iter().position(|a| a == "--model").expect("--model missing");
        assert_eq!(with[at + 1], "gpt-5.2-codex");
        // Still after `exec`, where the flag is documented.
        assert!(at > with.iter().position(|a| a == "exec").unwrap());
    }

    #[test]
    fn the_prompt_is_the_final_argument() {
        let args = argv(&command(Path::new("/bin/codex"), &req(ToolTier::Write, Some("m"))));
        assert_eq!(args.last().map(String::as_str), Some("do the thing"));
    }

    // ── JSONL parsing ──────────────────────────────────────────────────

    #[test]
    fn jsonl_extracts_thread_id_message_and_usage() {
        let mut s = RunSummary::default();
        absorb_line(&mut s, r#"{"type":"thread.started","thread_id":"th_123"}"#);
        absorb_line(
            &mut s,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"looked at it"}}"#,
        );
        absorb_line(
            &mut s,
            r#"{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50}}"#,
        );
        assert_eq!(s.session_id.as_deref(), Some("th_123"));
        assert_eq!(s.best_text(), "looked at it");
        // Still RAW here — normalization is finalize's job.
        assert_eq!(s.input_tokens, Some(1000));
        assert_eq!(s.cache_read_tokens, Some(800));
    }

    #[test]
    fn jsonl_emits_command_lifecycle_without_command_output() {
        let mut s = RunSummary::default();
        let started = absorb_line(
            &mut s,
            r#"{"type":"item.started","item":{"id":"item-1","type":"command_execution","command":"/bin/zsh -lc 'cargo test'","status":"in_progress","aggregated_output":"a secret"}}"#,
        );
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].summary, "Command cargo test");
        assert_eq!(started[0].phase, "started");
        assert!(!format!("{started:?}").contains("a secret"));

        let completed = absorb_line(
            &mut s,
            r#"{"type":"item.completed","item":{"id":"item-1","type":"command_execution","command":"cargo test","status":"completed","exit_code":0}}"#,
        );
        assert_eq!(completed[0].phase, "succeeded");
    }

    #[test]
    fn jsonl_emits_file_changes_and_ignores_reasoning() {
        let mut s = RunSummary::default();
        let changed = absorb_line(
            &mut s,
            r#"{"type":"item.completed","item":{"id":"item-2","type":"file_change","changes":[{"path":"src/main.rs","kind":"update"}],"status":"completed"}}"#,
        );
        assert_eq!(changed[0].summary, "File change src/main.rs");
        assert!(absorb_line(
            &mut s,
            r#"{"type":"item.completed","item":{"id":"item-3","type":"reasoning","text":"private chain of thought"}}"#,
        )
        .is_empty());
    }

    /// The cached prefix is a SUBSET of `input_tokens`. Leaving both as
    /// reported counts it twice against the daily token cap, which is the one
    /// number the budget guard rests on.
    #[test]
    fn finalize_splits_cached_tokens_out_of_the_input_bucket() {
        let mut s = RunSummary::default();
        absorb_line(
            &mut s,
            r#"{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50}}"#,
        );
        finalize(&mut s, &req(ToolTier::ReadOnly, None));
        assert_eq!(s.input_tokens, Some(200));
        assert_eq!(s.cache_read_tokens, Some(800));
        assert_eq!(s.output_tokens, Some(50));
        assert_eq!(s.cache_write_tokens, None, "Codex reports no cache writes");
        // The total the budget meter sums is Codex's own total, exactly once.
        assert_eq!(200 + 800 + 50, 1050);
    }

    #[test]
    fn finalize_never_produces_a_negative_input_bucket() {
        let mut s = RunSummary::default();
        // Defensive: a version that already excludes the cache would underflow.
        absorb_line(
            &mut s,
            r#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":900,"output_tokens":5}}"#,
        );
        finalize(&mut s, &req(ToolTier::ReadOnly, None));
        assert_eq!(s.input_tokens, Some(0));
    }

    #[test]
    fn multi_turn_usage_accumulates() {
        let mut s = RunSummary::default();
        for _ in 0..3 {
            absorb_line(
                &mut s,
                r#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":10}}"#,
            );
        }
        finalize(&mut s, &req(ToolTier::ReadOnly, None));
        assert_eq!(s.input_tokens, Some(180));
        assert_eq!(s.cache_read_tokens, Some(120));
        assert_eq!(s.output_tokens, Some(30));
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        let mut s = RunSummary::default();
        absorb_line(&mut s, "Reading prompt from stdin...");
        absorb_line(&mut s, "");
        absorb_line(&mut s, "{\"type\": truncated");
        absorb_line(
            &mut s,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"fine"}}"#,
        );
        assert_eq!(s.best_text(), "fine");
    }

    #[test]
    fn a_reported_failure_is_captured_even_without_a_message() {
        let mut s = RunSummary::default();
        absorb_line(&mut s, r#"{"type":"turn.failed","error":{"message":"model overloaded"}}"#);
        assert_eq!(s.error_text.as_deref(), Some("model overloaded"));
        assert_eq!(s.best_text(), "model overloaded");
    }

    /// The file the CLI was told to write is what the CLI considers the answer.
    /// A stream we mis-parse must not be able to override it.
    #[test]
    fn the_last_message_file_outranks_the_streamed_text() {
        let scratch = std::env::temp_dir().join("cxtasks-codex-lastmsg");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join(LAST_MESSAGE_FILE), "the real answer\n").unwrap();

        let mut r = req(ToolTier::ReadOnly, None);
        r.scratch = scratch.clone();
        let mut s = RunSummary::default();
        absorb_line(
            &mut s,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"streamed guess"}}"#,
        );
        finalize(&mut s, &r);
        assert_eq!(s.best_text(), "the real answer");
        std::fs::remove_dir_all(&scratch).ok();
    }

    /// An empty or missing file must fall through to the stream rather than
    /// blanking a run that did produce output.
    #[test]
    fn an_empty_last_message_file_falls_back_to_the_stream() {
        let scratch = std::env::temp_dir().join("cxtasks-codex-emptymsg");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::write(scratch.join(LAST_MESSAGE_FILE), "   \n").unwrap();

        let mut r = req(ToolTier::ReadOnly, None);
        r.scratch = scratch.clone();
        let mut s = RunSummary::default();
        absorb_line(
            &mut s,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"streamed"}}"#,
        );
        finalize(&mut s, &r);
        assert_eq!(s.best_text(), "streamed");
        std::fs::remove_dir_all(&scratch).ok();
    }

    // ── PATH repair for the node shim ──────────────────────────────────

    #[test]
    fn a_env_shebang_names_its_interpreter() {
        let dir = std::env::temp_dir().join("cxtasks-shebang");
        std::fs::create_dir_all(&dir).unwrap();

        let shim = dir.join("shim.js");
        std::fs::write(&shim, "#!/usr/bin/env node\nconsole.log(1)\n").unwrap();
        assert_eq!(shebang_interpreter(&shim).as_deref(), Some("node"));

        let absolute = dir.join("absolute.sh");
        std::fs::write(&absolute, "#!/bin/sh\necho hi\n").unwrap();
        assert_eq!(
            shebang_interpreter(&absolute),
            None,
            "an absolute interpreter needs no PATH lookup"
        );

        let binary = dir.join("binary");
        std::fs::write(&binary, [0x7f, b'E', b'L', b'F', 0, 1, 2, 3]).unwrap();
        assert_eq!(shebang_interpreter(&binary), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The launchd failure this exists to prevent: the CLI's own directory has
    /// to lead the child's PATH, or `#!/usr/bin/env node` can't resolve `node`
    /// from a LaunchAgent's bare environment.
    #[test]
    fn spawn_path_leads_with_the_cli_directory() {
        let path = spawn_path(Path::new("/opt/nvm/versions/node/v24/bin/codex"));
        assert!(
            path.starts_with("/opt/nvm/versions/node/v24/bin:"),
            "cli dir must come first: {path}"
        );
        assert!(path.contains(&std::env::var("PATH").unwrap_or_default()));
    }

    #[test]
    fn nvm_discovery_uses_the_newest_version_that_contains_codex() {
        let home = std::env::temp_dir().join(format!(
            "cxtasks-nvm-discovery-{}",
            uuid::Uuid::new_v4()
        ));
        let old = home.join(".nvm/versions/node/v9.9.9/bin");
        let current = home.join(".nvm/versions/node/v24.11.1/bin");
        let newer_without_codex = home.join(".nvm/versions/node/v25.0.0/bin");
        for dir in [&old, &current, &newer_without_codex] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(old.join("codex"), "#!/bin/sh\n").unwrap();
        std::fs::write(current.join("codex"), "#!/bin/sh\n").unwrap();

        assert_eq!(newest_nvm_cli(&home), Some(current.join("codex")));
        std::fs::remove_dir_all(home).ok();
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
                bg_executor: Some(crate::executor::BackgroundExecutor::Codex),
                bg_shell_allowed: Some(shell),
                bg_writes_allowed: Some(writes),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn every_tail_matches_writeback_availability_and_forbids_self_completion() {
        for (shell, writes) in [(false, false), (true, false), (true, true)] {
            for has_tools in [true, false] {
                let p = prompt_tail_with_writeback(&task_with(shell, writes), has_tools);
                assert!(p.contains("do not ask questions"), "{shell}/{writes}: {p}");
                assert!(p.contains("FINAL MESSAGE is the result"), "{shell}/{writes}");
                assert!(!p.contains("complete_task"), "self-completion leaked: {p}");
                if has_tools {
                    for tool in crate::mcp::server::BACKGROUND_TOOLS {
                        assert!(
                            p.contains(&format!("mcp__cxtasks__{tool}")),
                            "{shell}/{writes}: {tool} missing: {p}"
                        );
                    }
                    assert!(p.contains("three CXTasks tools and no others"), "{p}");
                } else {
                    assert!(
                        !p.contains("mcp__cxtasks__"),
                        "{shell}/{writes}: missing MCP must name no tools: {p}"
                    );
                    assert!(
                        p.contains("no CXTasks tools in this run"),
                        "{shell}/{writes}: say the final message is the only channel"
                    );
                }
                // The shared half — Codex asks for the same handoff Claude does.
                assert!(p.contains("Write it as a HANDOFF"), "{shell}/{writes}");
            }
        }
    }

    /// Codex is the executor arming a review **preselects**, so this is the
    /// DEFAULT path for the feature, not an edge case. It catches the review
    /// branch landing in `claude::prompt_tail` — where Codex would never see it
    /// — rather than in the shared `handoff_tail` both providers read.
    #[test]
    fn codex_asks_for_the_same_verdict_claude_does() {
        let mut reviewing = task_with(false, false);
        reviewing.bg_intent = crate::intent::BackgroundIntent::Review;
        let p = prompt_tail_with_writeback(&reviewing, true);
        assert!(p.contains("The VERDICT is the deliverable"), "{p}");
        assert!(p.contains("giving your VERDICT"), "{p}");
        assert!(
            !p.contains("the plan IS the deliverable"),
            "a Codex reviewer must not be told to write the plan it is auditing: {p}"
        );
        // Byte-identical to Claude's, because what makes a good handoff has
        // nothing to do with which agent wrote it.
        assert!(
            p.ends_with(&crate::runner::providers::claude::handoff_tail(&reviewing, true)),
            "the handoff ask must stay SHARED, not forked per provider: {p}"
        );
    }

    /// The middle tier is described truthfully: commands CAN mutate caches and
    /// build artifacts, so promising "no writes" here would be false — and
    /// saying nothing invites the agent to reach for `sed -i`.
    #[test]
    fn the_commands_tier_is_described_as_intent_not_as_a_hard_boundary() {
        let p = prompt_tail(&task_with(true, false));
        assert!(p.contains("Do NOT deliberately edit source files"));
        assert!(p.contains("snapshotted before you started"));
        assert!(!p.contains("READ-ONLY sandbox"));
    }

    #[test]
    fn the_read_only_tail_says_the_sandbox_will_refuse_writes() {
        let p = prompt_tail(&task_with(false, false));
        assert!(p.contains("READ-ONLY sandbox"));
        assert!(p.contains("refused by the sandbox"));
    }
}
