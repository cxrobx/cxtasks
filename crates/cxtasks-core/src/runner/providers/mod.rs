//! The two background agents, behind one shape.
//!
//! Everything that is *about running unattended agents at all* — claiming work,
//! budgets, checkpoints, timeouts, recording the run, moving the task to
//! `needs_review` — lives in [`super`] and is written once. Everything that is
//! *about a particular CLI* — where its binary is, how its flags spell "don't
//! ask me anything", what its event stream looks like, how it reports tokens —
//! lives in a provider module and is written twice, because the two CLIs
//! genuinely disagree about all of it.
//!
//! The seam is deliberately narrow: a provider builds a [`Command`], absorbs
//! stdout lines into a [`RunSummary`], and gets one chance to finalize. It does
//! not decide whether a run may start, and it cannot skip the checkpoint.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::db::tasks::Task;
use crate::executor::BackgroundExecutor;
use crate::runner::ToolTier;

pub mod claude;
pub mod codex;

/// The reduced write-back MCP binary, when this install can actually launch it.
///
/// Both providers derive their command line and their prompt tail from this
/// one check. A dev tree that has not built `cxtasks-mcp`, or an app bundle
/// missing the manual copy, must degrade to a final-message-only run instead
/// of telling an agent to call a tool that cannot exist.
pub(crate) fn writeback_binary() -> Option<PathBuf> {
    let bin = crate::paths::sibling_binary("cxtasks-mcp");
    bin.is_file().then_some(bin)
}

/// What a run is FOR at the PROCESS level — the thing that decides which
/// tool grant and which prompt file a provider builds from.
///
/// Distinct from [`crate::intent::BackgroundIntent`] on purpose. Intent is a
/// column a person can arm and a pre-R2 binary can read back as `work`;
/// invariant 19 says nothing may derive a capability from it. `RunKind` is
/// never stored: it is decided by the runner LANE that started the run —
/// `tick` starts `Work`, `tripwire::agent_pass` starts `Check` — so the grant
/// a check gets is pinned by the code that knows it is a check, not read off
/// a row that might have been written by anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunKind {
    /// A task's work (or its review): the tier is the task's, the prompt is
    /// `prompt.txt`, the write-back router serves three tools.
    #[default]
    Work,
    /// An `agent:` tripwire evaluation: read-only pinned, `check-prompt.txt`,
    /// one MCP tool (`get_task`), no salvage, no checkpoint.
    Check,
    /// Saying what a tripped tripwire's evidence MEANS:
    /// `interpret-prompt.txt`, **zero tools** — no shell, no `Read`, no MCP
    /// server at all — no salvage, no checkpoint, no repo.
    ///
    /// The absence of tools is the safety argument, not an economy. A
    /// repo-less run on the Mac cannot be bounded by its workspace (`cwd` is
    /// no boundary for `Bash`, and `Read` takes absolute paths), so this one
    /// is bounded by having nothing to reach with: its whole input is the
    /// evidence in its prompt and its whole output is its final message.
    Interpret,
}

/// Everything a provider needs to build one invocation.
#[derive(Debug, Clone)]
pub struct RunRequest {
    pub task_id: String,
    pub repo: PathBuf,
    pub tier: ToolTier,
    /// Work or a tripwire check. See [`RunKind`].
    pub kind: RunKind,
    /// The seed prompt, already assembled by [`crate::dispatch`] — identical in
    /// structure to the one a foreground session gets, with a provider-specific
    /// tail.
    pub prompt: String,
    /// Per-task scratch dir. A provider may write into it (Codex puts its
    /// final message there); nothing else may.
    pub scratch: PathBuf,
    /// An explicit model, or `None` to let the CLI pick its own default.
    ///
    /// `None` is a real choice, not a missing value: hard-coding an OpenAI
    /// model name here would go stale the moment Codex ships a new default, and
    /// a wrong model name is a run that dies at spawn for a reason unrelated to
    /// the task. Claude's path always fills this in, because its default is a
    /// deliberate cost decision (Sonnet, not the session's Opus).
    pub model: Option<String>,
    /// Claude's per-run turn ceiling. Codex has no equivalent — it is bounded
    /// by the wall-clock timeout, the per-run token ceiling below, and the
    /// shared daily token budget.
    ///
    /// Since the ceiling exists this is a **loop detector**, not the cost
    /// guard it used to be: turns correlate with spend only loosely (context
    /// grows every turn, so late turns cost several times what early ones do),
    /// and set low enough to bound cost it fires on healthy runs instead.
    pub max_turns: i64,
    pub timeout: Duration,
    /// Per-run token ceiling, enforced live by [`crate::runner::process`].
    /// `0` disables it.
    ///
    /// Provider-neutral by construction — see the note on
    /// [`Provider::live_total`], which is the only per-provider part.
    pub token_ceiling: i64,
    /// Extra MCP servers this run may reach, already resolved against
    /// [`crate::mcp_registry`] on THIS host.
    ///
    /// Empty by DEFAULT for every run, detached or not — and grantable for
    /// either. A repo-bound task can be told to search the notes vault as well
    /// as its own tree; a detached one has nothing else, which is why the
    /// default matters more there.
    ///
    /// A resolved list rather than a name list because by the time a command is
    /// being built, "can this host provide it" is already settled —
    /// [`crate::runner::ready`] refused the run otherwise.
    pub extra_mcp: Vec<crate::mcp_registry::ResolvedServer>,
}

/// Why a run ended early **at the provider's own report**, as opposed to being
/// killed by us (that is [`crate::runner::process::KillReason`]).
///
/// Deliberately not folded into [`RunSummary::error_text`], which means "the
/// provider reported a failure in-band". A turn cutoff is not that: the agent
/// was working correctly and our own ceiling stopped it. It also could not be
/// carried there even if the claim were close enough — [`RunSummary::best_text`]
/// reaches `error_text` only when both `result_text` and `assistant_text` are
/// empty, and a cut-off run always has narration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Claude's `error_max_turns`: our `--max-turns` was reached.
    MaxTurns { limit: i64, reached: i64 },
}

impl StopReason {
    /// Fill in a limit the terminating event didn't state.
    ///
    /// The event is the only place `reached` can come from, but the limit is
    /// something *we* passed — so an event that omits it is still fully
    /// explainable, and reporting `0-turn ceiling` would not be.
    pub fn resolved(self, configured_limit: i64) -> Self {
        match self {
            Self::MaxTurns { limit, reached } => Self::MaxTurns {
                limit: if limit > 0 { limit } else { configured_limit },
                reached,
            },
        }
    }

    /// The clause that names the guard, for both the run's own text and the
    /// task's `blocked_reason`. Written to slot after "Run" / "Background run".
    ///
    /// Degrades rather than inventing: an event without a turn count says
    /// which ceiling fired and stops there.
    pub fn describe(self) -> String {
        match self {
            Self::MaxTurns { limit, reached } if reached > 0 => {
                format!("hit its {limit}-turn ceiling at turn {reached}")
            }
            Self::MaxTurns { limit, .. } => format!("hit its {limit}-turn ceiling"),
        }
    }
}

/// What we managed to extract from a run, whatever produced it.
///
/// Every field is optional because both CLIs have changed their event shapes
/// across versions and an unrecognized line must be skipped, never fatal: a run
/// that emits garbage is a failed run, not a crashed runner.
#[derive(Debug, Default, Clone)]
pub struct RunSummary {
    pub session_id: Option<String>,
    pub output_tokens: Option<i64>,
    pub input_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub model: Option<String>,
    /// The authoritative final answer, when the provider gives us one.
    pub result_text: Option<String>,
    /// Fallback: concatenated assistant text, if no final-result event arrives.
    pub assistant_text: String,
    /// A failure the provider reported in-band. Surfaced even on a zero exit,
    /// because "the CLI exited 0 having told us it failed" is a real shape.
    pub error_text: Option<String>,
    /// One of **our own** guards stopped a healthy run — see [`StopReason`].
    pub stop_reason: Option<StopReason>,
}

/// One safe, provider-neutral execution-trace event.
///
/// Provider adapters are the only code allowed to look at raw tool inputs.
/// They reduce them immediately to this bounded display shape; raw JSON,
/// reasoning and tool output never cross into the persistence layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEvent {
    pub kind: String,
    pub call_id: Option<String>,
    pub tool_name: Option<String>,
    pub phase: String,
    pub summary: String,
}

impl TraceEvent {
    pub fn tool_started(call_id: Option<String>, tool_name: String, summary: String) -> Self {
        Self {
            kind: "tool".into(),
            call_id,
            tool_name: Some(tool_name),
            phase: "started".into(),
            summary,
        }
    }

    pub fn tool_finished(call_id: Option<String>, failed: bool) -> Self {
        Self {
            kind: "tool".into(),
            call_id,
            tool_name: None,
            phase: if failed { "failed" } else { "succeeded" }.into(),
            // The progress writer replaces these from the matching start. The
            // result event intentionally carries no tool output of its own.
            summary: String::new(),
        }
    }

    pub fn notice(summary: String) -> Self {
        Self {
            kind: "notice".into(),
            call_id: None,
            tool_name: None,
            phase: "failed".into(),
            summary,
        }
    }
}

/// One line, whitespace-normalized and Unicode-safe, capped for DB/UI use.
pub(crate) fn bounded(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let mut out = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

/// A deliberately conservative command label. Full shell strings can contain
/// inline credentials or source text, so the trace keeps only the executable
/// plus a tiny allow-list of familiar developer subcommands.
/// **Two kinds of noise, and they must not be treated alike** — conflating them
/// broke `zsh -lc 'cargo test'`, which is the case the wrapper list exists for.
///
/// A WRAPPER is followed by the real command IN THE SAME SEGMENT, so its token
/// is skipped and the scan continues: `/bin/zsh -lc 'cargo test'` is still
/// `cargo test`.
const SHELL_WRAPPERS: &[&str] = &[
    "sh", "bash", "zsh", "env", "time", "nohup", "exec", "command", "sudo",
];

/// SETUP is followed by its own arguments, not by a command, so the whole
/// segment is discarded: in `cd /repo && cargo test` the answer is `cargo
/// test`, and merely skipping `cd` would take `/repo` as the program.
///
/// This is why the activity log read "Bash export" ninety times for a run doing
/// real work — it was naming the thing that got out of the way.
const SHELL_SETUP: &[&str] = &["cd", "export", "set", "unset", "source", ".", "then", "do"];

/// The first token of `segment` that looks like the program being run, plus the
/// argument after it if there is one.
fn segment_parts(segment: &str) -> Option<(String, Option<String>)> {
    let mut executable: Option<String> = None;
    let mut candidate_arg: Option<String> = None;
    for token in segment.split_whitespace() {
        if token.contains('=') || token.starts_with('-') {
            continue;
        }
        let clean = token.trim_matches(|c: char| c == '\'' || c == '"');
        let basename = clean.rsplit('/').next().unwrap_or(clean);
        if executable.is_none() && SHELL_SETUP.contains(&basename) {
            // Discard the SEGMENT, not just the word — see `SHELL_SETUP`.
            return None;
        }
        if executable.is_none() && SHELL_WRAPPERS.contains(&basename) {
            continue;
        }
        if executable.is_none() {
            executable = Some(basename.to_string());
        } else {
            candidate_arg = Some(clean.to_string());
            break;
        }
    }
    executable.map(|e| (e, candidate_arg))
}

/// The fallback for a command that is nothing but shell noise (`export FOO=1`
/// on its own). Same scan, minus the noise rejection, so the label is still the
/// word the user typed rather than "command".
fn first_token_parts(segment: &str) -> Option<(String, Option<String>)> {
    segment
        .split_whitespace()
        .find(|t| !t.contains('=') && !t.starts_with('-'))
        .map(|t| {
            let clean = t.trim_matches(|c: char| c == '\'' || c == '"');
            (
                clean.rsplit('/').next().unwrap_or(clean).to_string(),
                None,
            )
        })
}

pub(crate) fn command_label(command: &str) -> String {
    // A real command is usually several joined by `&&`, `;` or a pipe, and the
    // interesting one is rarely the first — `export SP=…; cd $SP && python3
    // build.py` is three segments of which only the last does anything. Take
    // the first segment that is not pure shell noise, and fall back to the
    // first segment overall so a command that IS only setup still names itself.
    let segments: Vec<&str> = command
        .split(|c| c == ';' || c == '|' || c == '&')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    let (executable, candidate_arg) = match segments
        .iter()
        .find_map(|segment| segment_parts(segment))
        .or_else(|| segments.first().and_then(|s| first_token_parts(s)))
    {
        Some(parts) => parts,
        None => return "command".into(),
    };
    // A positional argument may itself be a password, token, URL or source
    // snippet. Preserve only a small allow-list of familiar, non-sensitive
    // developer subcommands; every other command still identifies its binary.
    let safe_arg = candidate_arg.filter(|arg| {
        matches!(
            (executable.as_str(), arg.as_str()),
            (
                "cargo",
                "test" | "check" | "build" | "fmt" | "clippy" | "run"
            ) | ("npm" | "pnpm" | "yarn", "test" | "run" | "build" | "install")
                | ("git", "status" | "diff" | "log" | "show" | "rev-parse")
        )
    });
    match safe_arg {
        Some(arg) => bounded(&format!("{executable} {arg}"), 80),
        None => bounded(&executable, 80),
    }
}

#[cfg(test)]
mod trace_tests {
    use super::*;

    #[test]
    /// The activity log read "Bash export" ninety-odd times for a run that was
    /// doing real work. `export`/`cd` are setup, and labelling a command after
    /// the thing that got out of the way is worse than labelling it "Bash":
    /// it looks like information.
    #[test]
    fn a_label_names_the_command_that_ran_not_the_setup() {
        assert_eq!(
            command_label("export SP=/tmp/x; cd $SP && python3 build.py"),
            "python3"
        );
        assert_eq!(command_label("cd /repo && cargo test"), "cargo test");
        assert_eq!(command_label("set -e; npm run build"), "npm run");
        // A pipe is a segment boundary too — the first real program wins.
        assert_eq!(command_label("cat foo.txt | head -20"), "cat");
    }

    /// A command that is ONLY setup still has to say something. Falling through
    /// to the generic "command" here would lose the one word there was.
    #[test]
    fn a_pure_setup_command_still_names_itself() {
        assert_eq!(command_label("export FOO=1"), "export");
        assert_eq!(command_label("cd /somewhere"), "cd");
    }

    #[test]
    fn command_labels_skip_shell_wrappers_and_inline_assignments() {
        assert_eq!(command_label("/bin/zsh -lc 'cargo test'"), "cargo test");
        assert_eq!(
            command_label("API_TOKEN=do-not-store curl -H 'Authorization: secret' /health"),
            "curl"
        );
    }

    /// **The test that stops the ceiling and the daily counter disagreeing
    /// about what a token is.** The same buckets mean different things to the
    /// two CLIs, so a live sum has to ask the provider — and the answer has to
    /// be what `finalize` will settle on, not merely close to it.
    #[test]
    fn the_live_total_reads_each_providers_buckets_in_its_own_units() {
        let mut s = RunSummary {
            input_tokens: Some(1000),
            cache_read_tokens: Some(800),
            output_tokens: Some(50),
            ..Default::default()
        };

        // Claude's four buckets are independent: exactly `TaskRun::total_tokens`.
        assert_eq!(Provider::Claude.live_total(&s), 1850);
        // Codex reports `input_tokens` INCLUDING the cached prefix, so the naive
        // sum counts 800 of them twice.
        assert_eq!(
            Provider::Codex.live_total(&s),
            1050,
            "a live Codex sum must not double-count the cached prefix"
        );

        // And it agrees with what the run will actually be charged: run the real
        // normalization and total the row the way the daily counter does.
        let mut settled = s.clone();
        let req = RunRequest {
            task_id: "t".into(),
            repo: PathBuf::from("/tmp"),
            tier: crate::runner::ToolTier::ReadOnly,
            prompt: String::new(),
            scratch: std::env::temp_dir().join("cxtasks-live-total-test"),
            kind: RunKind::Work,
            model: None,
            max_turns: 150,
            timeout: Duration::from_secs(60),
            token_ceiling: 0,
            extra_mcp: Vec::new(),
        };
        codex::finalize(&mut settled, &req);
        let charged = settled.input_tokens.unwrap_or(0)
            + settled.cache_read_tokens.unwrap_or(0)
            + settled.cache_write_tokens.unwrap_or(0)
            + settled.output_tokens.unwrap_or(0);
        assert_eq!(Provider::Codex.live_total(&s), charged);

        // The clamp too, so the two stay equal on the shape that would underflow
        // rather than only on the normal one.
        s.input_tokens = Some(100);
        s.cache_read_tokens = Some(900);
        let mut settled = s.clone();
        codex::finalize(&mut settled, &req);
        assert_eq!(
            Provider::Codex.live_total(&s),
            settled.input_tokens.unwrap_or(0)
                + settled.cache_read_tokens.unwrap_or(0)
                + settled.output_tokens.unwrap_or(0)
        );
    }

    /// A limit the terminating event didn't state is still ours, so the sentence
    /// a user reads names a real ceiling rather than `0-turn`.
    #[test]
    fn a_stop_reason_without_a_stated_limit_borrows_the_one_we_passed() {
        let stated = StopReason::MaxTurns {
            limit: 30,
            reached: 31,
        };
        assert_eq!(stated.resolved(150), stated, "a stated limit is not ours to rewrite");
        assert_eq!(
            StopReason::MaxTurns {
                limit: 0,
                reached: 31
            }
            .resolved(150)
            .describe(),
            "hit its 150-turn ceiling at turn 31"
        );
        // No turn count is a degradation, not an invitation to invent one.
        assert_eq!(
            StopReason::MaxTurns {
                limit: 0,
                reached: 0
            }
            .resolved(150)
            .describe(),
            "hit its 150-turn ceiling"
        );
    }
}

impl RunSummary {
    /// The final answer if we got one, else whatever the agent said along the
    /// way, else an honest admission that there was nothing.
    pub fn best_text(&self) -> String {
        if let Some(r) = self.result_text.as_deref().filter(|r| !r.trim().is_empty()) {
            return r.to_string();
        }
        let fallback = self.assistant_text.trim();
        if !fallback.is_empty() {
            return fallback.to_string();
        }
        if let Some(e) = self.error_text.as_deref().filter(|e| !e.trim().is_empty()) {
            return e.trim().to_string();
        }
        "(the run produced no output)".to_string()
    }

    /// Add `n` to a bucket, treating `None` as zero. Usage arrives per message
    /// or per turn and has to accumulate.
    pub(crate) fn add(slot: &mut Option<i64>, n: i64) {
        *slot = Some(slot.unwrap_or(0) + n);
    }
}

/// Which CLI a run goes to. A plain enum rather than a boxed trait: there are
/// exactly two, dispatch is a `match`, and the concrete type stays visible in
/// every test assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub fn for_executor(executor: BackgroundExecutor) -> Self {
        match executor {
            BackgroundExecutor::Claude => Self::Claude,
            BackgroundExecutor::Codex => Self::Codex,
        }
    }

    pub fn executor(self) -> BackgroundExecutor {
        match self {
            Self::Claude => BackgroundExecutor::Claude,
            Self::Codex => BackgroundExecutor::Codex,
        }
    }

    pub fn for_task(task: &Task) -> Self {
        Self::for_executor(task.bg_executor)
    }

    /// Locate the CLI, or `None` if it isn't installed.
    pub fn find_cli(self) -> Option<PathBuf> {
        match self {
            Self::Claude => crate::dispatch::find_claude_cli(),
            Self::Codex => codex::find_cli(),
        }
    }

    /// Read-only "could a run start right now" check, run at **queue** time so
    /// the user hears about a missing install while they're still looking at
    /// the task — rather than twenty seconds later as a `blocked` status.
    ///
    /// Cheap by construction: a `command -v` and, for Codex, `codex login
    /// status`. It never touches credentials itself — the answer is an exit
    /// code, and re-authenticating is the user's job.
    pub fn preflight(self) -> Result<(), String> {
        match self {
            Self::Claude => match self.find_cli() {
                Some(_) => Ok(()),
                None => Err("The Claude Code CLI (`claude`) wasn't found on PATH. Install it \
                             (e.g. `npm install -g @anthropic-ai/claude-code`) and try again."
                    .into()),
            },
            Self::Codex => codex::preflight(),
        }
    }

    /// The instructions appended to the shared task context. See
    /// [`crate::dispatch::build_prompt`] for the part both share.
    pub fn prompt_tail(self, task: &Task) -> String {
        match self {
            Self::Claude => claude::prompt_tail(task),
            Self::Codex => codex::prompt_tail(task),
        }
    }

    /// Build the invocation. Pure — spawning is [`crate::runner::process`]'s
    /// job — so a test can assert on the exact argv without running anything.
    pub fn command(self, cli: &Path, req: &RunRequest) -> Command {
        match self {
            Self::Claude => claude::command(cli, req),
            Self::Codex => codex::command(cli, req),
        }
    }

    /// One extra turn that asks a cut-off run for its handoff, or `None` when
    /// this provider has no safe way to resume.
    ///
    /// Codex returns `None`: it has no `--max-turns`, so it never produces the
    /// [`StopReason`] that triggers a salvage in the first place, and a
    /// speculative `codex resume` would be a second paid run started on a
    /// guess.
    pub fn salvage_command(
        self,
        cli: &Path,
        req: &RunRequest,
        task: &Task,
        session_id: &str,
    ) -> Option<Command> {
        match self {
            Self::Claude => Some(claude::salvage_command(cli, req, task, session_id)),
            Self::Codex => None,
        }
    }

    /// Everything this run has touched **so far**, in the units the daily
    /// counter will eventually charge it.
    ///
    /// The per-run ceiling is enforced in the shared poll loop (invariant 7's
    /// neighbourhood: a per-provider copy is how one of them ends up
    /// unbounded), so this is the one piece of it that has to know which CLI
    /// produced the numbers — and it exists because a naive sum of the four
    /// buckets is **wrong for Codex**.
    ///
    /// **"So far" is the load-bearing phrase, and it is only true of Claude.**
    /// Measured against Codex CLI 0.140.0: `codex exec --json` emits exactly
    /// ONE `turn.completed` — the only event carrying usage — at the very end
    /// of the run, even for a multi-step one (verified with a three-tool-call
    /// probe, and again by a real 170-second background run that reported
    /// 1.22M tokens in a single event and was never stopped by a 300k
    /// ceiling). So for Codex this returns 0 for the run's entire life and the
    /// ceiling **cannot fire**; Codex stays bounded by the wall-clock timeout
    /// and the daily cap alone. Claude reports per message and is genuinely
    /// bounded (verified live: killed at 327,875 against a 300,000 ceiling).
    ///
    /// This is not a reason to delete the Codex arm. The normalization is what
    /// the ceiling *would* read the moment that CLI reports incrementally, and
    /// getting it wrong then would be silent. It is a reason not to write
    /// "background runs are bounded at 25M" without saying which agent — the
    /// same discipline [`codex::NETWORK_OFF`] already documents for the
    /// network claim.
    ///
    /// Codex reports `input_tokens` *including* `cached_input_tokens`, and
    /// [`codex::absorb_line`] stores both as reported; the subtraction that
    /// makes those buckets mean what Claude's mean happens in
    /// [`codex::finalize`], at the end of the run. So a live sum would
    /// over-count a Codex run by its whole cache-read volume — nearly 2× on a
    /// cache-heavy run. The ceiling would fire early (the safe direction) but
    /// the number it *recorded* would be one the daily counter later disagreed
    /// with, which is the worse failure: a guard that misreports its own scope.
    ///
    /// Deliberately the same arithmetic `finalize` will settle on, clamp
    /// included, rather than a shortcut that happens to agree in the normal
    /// case. Do **not** move the normalization into `absorb_line` instead:
    /// `finalize`'s doc explains it must run on every exit path, and
    /// per-event subtraction diverges from once-at-the-end the moment an event
    /// carries one field without the other.
    pub fn live_total(self, s: &RunSummary) -> i64 {
        let (input, cached, output, written) = (
            s.input_tokens.unwrap_or(0),
            s.cache_read_tokens.unwrap_or(0),
            s.output_tokens.unwrap_or(0),
            s.cache_write_tokens.unwrap_or(0),
        );
        match self {
            // Four independent buckets, exactly as `TaskRun::total_tokens` sums
            // them.
            Self::Claude => input + cached + output + written,
            Self::Codex => (input - cached).max(0) + cached + output,
        }
    }

    /// Fold one line of stdout into the summary. Must never panic.
    pub fn absorb(self, summary: &mut RunSummary, line: &str) -> Vec<TraceEvent> {
        match self {
            Self::Claude => claude::absorb_line(summary, line),
            Self::Codex => codex::absorb_line(summary, line),
        }
    }

    /// Last word after the process exits — where Codex reads the file it was
    /// told to write its final message into.
    pub fn finalize(self, summary: &mut RunSummary, req: &RunRequest) {
        match self {
            Self::Claude => {}
            Self::Codex => codex::finalize(summary, req),
        }
    }
}
