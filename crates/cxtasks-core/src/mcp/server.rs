//! CXTasks MCP server — the capture and query surface for Claude sessions.
//!
//! The task surface — `file_task`, `list_tasks`, `get_task`, `update_task`,
//! `complete_task`, `archive_task`/`unarchive_task`, `next_task`, `search_tasks` — plus the relations
//! (`block_task`, `tag_task`, `comment_task`, `list_tags`), the projects layer
//! (`create_project`, `update_project`, `project_for_repo`) and the GSD layer
//! (`project_board`, `post_project_update`, `upsert_section`). `file_followup`
//! is background-only and absent from an interactive session.
//!
//! No count here on purpose: it has been wrong twice (said fourteen, said
//! sixteen, was eighteen). The tools are `#[tool]`-annotated below; grep them.
//!
//! **Every task-id parameter accepts either form of reference**: the UUID, or
//! the speakable T-number (`T42`). See `Self::resolve_task_id`.
//!
//! **The cwd problem.** An MCP server is a separate process; it cannot see the
//! calling session's working directory, and `std::env::current_dir()` here
//! returns wherever the *client* was launched from — not the repo the user is
//! sitting in. So `repo_path` is an explicit parameter on `file_task`, and
//! `claude-plugin/commands/task.md` instructs Claude to pass its own cwd. When
//! it's omitted the task is still filed as a plain todo with no scoped
//! project context — foreground dispatch (`Open in Claude` / `Copy Seed
//! Prompt`) falls back to the user's home directory rather than refusing to
//! dispatch (`dispatch::resolve_repo_or_home`); unattended background runs
//! still require an explicit `repo_path` (`dispatch::resolve_repo`, strict).

use crate::db;
use crate::mcp::bridge::{notify, Envelope};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, InitializeRequestParams, InitializeResult, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::ErrorData as McpError;
use rmcp::{schemars, tool, tool_handler, tool_router, RoleServer, ServerHandler};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// ─── How prose is written ─────────────────────────────────────────────────

/// How to write text that renders in the app — carried by the notes fields'
/// OWN descriptions, not the server instructions.
///
/// **Claude Code shows a model only the first 2,048 characters of a server's
/// instructions** (gotcha #48). The formatting rules first lived there, after
/// character 2,519, and no agent ever saw them; the tests passed because they
/// checked what the server SENT. Parameter descriptions arrive whole — the
/// ~1,500-character `tripwire` one is proof — so the rules ride on the fields
/// they govern, and `every_prose_field_carries_the_formatting_rules` pins it.
///
/// A macro rather than a `const` because it must be a LITERAL: `concat!`
/// splices it into the two field descriptions, and `concat!` takes no consts.
macro_rules! notes_format {
    () => {
        "Renders as markdown in the app, so write it to be scanned. Open with one line saying what this is or where it stands. Then structure: `##` headings, bullets, a table for comparisons, `- [ ]` steps (they render as checkboxes), `<details><summary>…</summary>` to fold long material. Paragraphs of two or three sentences, never one long block. Code, paths, commands and anything in angle brackets go in backticks; outside them `Vec<String>` renders as \"Vec\". Draw a diagram only when a picture carries the mechanism: inline SVG as in an Onyx artifact (`<svg viewBox>` with rect/path/line/text and a `<marker>` arrow), coloured with var(--clay), var(--slate), var(--oat), var(--white) and var(--gray-300) so it follows the light and dark themes. Scripts, style blocks, iframes and on* handlers are stripped. The reply ends with a RENDER CHECK line if anything won't show or reads as a wall."
    };
}

const FILE_TASK_BODY_DOC: &str = concat!(
    "Optional longer notes: context, links, acceptance criteria. ",
    notes_format!()
);

const UPDATE_TASK_BODY_DOC: &str = concat!(
    "Replace the notes body. This is the field `get_task` shows under the 'Notes' heading — there is no `notes` parameter, and passing one is refused rather than ignored. ",
    notes_format!()
);

// ─── Parameter structs ────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileTaskParams {
    #[schemars(description = "Short imperative title, e.g. 'fix the intake webhook retry'")]
    pub title: String,
    #[schemars(
        required,
        extend("enum" = ["chris-asked", "wont-fit", "decision", "other-repo"]),
        description = "WHY this is a task rather than work you do now. REQUIRED. `chris-asked` — Chris told you to file it in this session, by words or by a `/cxtasks:*` command he typed (the common case; nothing else to weigh). Otherwise you decided on your own, and the value is your answer to \"why not now?\": `wont-fit` (won't finish in this session's remaining context), `decision` (a decision or gate the code can't encode), `other-repo` (belongs to another repo or session). If your honest answer is \"it's small\" or \"so it isn't lost\", do NOT file — do it now. A self-initiated filing is tagged `agent-filed` and asks Chris before the row exists."
    )]
    pub why: Option<String>,
    #[schemars(description = FILE_TASK_BODY_DOC)]
    pub body: Option<String>,
    #[schemars(
        description = "The project this work belongs to, as the user calls it (e.g. 'pocketbuddy'). Prefer OMITTING it: when repo_path is given, the project is looked up from the directory's registered owner, which is always right. Do NOT derive it from the folder name — a repo's directory is frequently not its project's name (~/Projects/finance-app IS pocketbuddy), and inventing 'finance-app' creates a second, empty project the real work isn't in. Pass it only when the user named a project, or the repo isn't registered to one. A name that doesn't exist yet is created as an AREA (an ongoing bucket that never nags about a finish line) — if what you are filing is a finite piece of work with an end, use create_project with kind='project' and a parent first, so it can be closed later instead of quietly becoming a sixth permanent area."
    )]
    pub project: Option<String>,
    #[schemars(description = "Priority: 0 = P0 highest/drop-everything, 1 = P1, 2 = P2, 3 = P3 lowest. Defaults to 3. NOTE this is Meta-style — P0 is the MOST urgent, not the least.")]
    pub priority: Option<i64>,
    #[schemars(
        description = "DEADLINE — when the work must be DONE BY, ISO-8601 UTC with trailing Z (e.g. '2026-08-01T17:00:00.000Z'). A future due date does NOT hide the task: it stays visible and actionable now (in Open until the day, on Today once due). 'By Friday' / 'due EOD' is due_at; 'start Friday' is defer_until — a different field, and both can be set. Omit for no deadline."
    )]
    pub due_at: Option<String>,
    #[schemars(
        description = "START date — the task moves to Waiting and is hidden from the actionable views (Open, Inbox, Today, next_task and the background queue) until this ISO-8601 UTC instant, then reappears automatically. This is how CXTasks says 'planned / snoozed / start later': a DATE, not a status — the task keeps todo/inbox while it waits, and there is no 'planned' status to look for. Independent of due_at ('starts Monday, due Friday' = defer_until Monday + due_at Friday). Omit unless the user asked to defer — a deadline alone is due_at, and a deadline alone hides nothing."
    )]
    pub defer_until: Option<String>,
    #[schemars(
        description = "Recurrence rule: 'daily', 'weekly:MO,TH' (two-letter days), or 'monthly:15' (day of month). Omit for a one-off task."
    )]
    pub recurrence: Option<String>,
    #[schemars(
        description = "ABSOLUTE path to the repo this task should be worked in — pass YOUR CURRENT WORKING DIRECTORY when the task is about the code you are looking at. This is what lets CXTasks hand the task back to a Claude session later with the right context. Omit for a task that isn't about a codebase."
    )]
    pub repo_path: Option<String>,
    #[schemars(
        description = "SEED PROMPT — write this whenever repo_path is set. It is what a fresh agent reads when the task is picked up, and that agent has NONE of this conversation's context, so a bare title is not actionable. Include: the concrete files/symbols involved, the observable failure mode (exact error text if known), WHAT YOU ALREADY RULED OUT (highest-value part — it stops the next agent rediscovering it), what 'done' looks like as a checkable condition, and any constraints. Imperative, addressed to a competent stranger; never write 'as discussed above'. If the task is genuinely unscoped, say so and state that investigation is step one — an honestly vague prompt beats a falsely specific one."
    )]
    pub prompt: Option<String>,
    #[schemars(
        description = "Allow this task to be run unattended by the background runner. Defaults to false — only set true when the user explicitly asked for background execution."
    )]
    pub bg_allowed: Option<bool>,
    #[schemars(
        description = "This task has no repository and runs in a scratch directory on a host that declares a detached workspace. Defaults to false, and must NOT be set just because you don't know the repo — file it with repo_path instead. Only for work that genuinely has no codebase, e.g. research over the notes vault."
    )]
    #[serde(default)]
    pub detached: bool,
    #[schemars(
        description = "Comma-separated MCP server names a DETACHED run may reach, e.g. 'vault'. Empty (the default) grants nothing beyond task write-back, which makes a detached run able to read nothing at all. An unknown name is refused rather than ignored."
    )]
    pub bg_mcp_servers: Option<String>,
    #[schemars(
        description = "Which agent runs this task in the background: 'claude' (default) or 'codex'. Only set 'codex' when the user explicitly asked for Codex — otherwise leave it unset. Codex must be installed and logged in (`codex login`); the task is refused at queue time if it isn't."
    )]
    pub bg_executor: Option<String>,
    #[schemars(
        description = "Allow a background run to run shell commands WITHOUT granting file edits. Set this for investigation that still needs a command — anything using git, a test runner, or a CLI. Defaults to false. Requires bg_allowed and a git repo (the working tree is snapshotted first so the run stays undoable); the tree does NOT need to be clean. NOTE this is a narrower INTENT, not a hard boundary: commands can still create caches, lockfiles and build artifacts, which is why it is checkpointed like a write run."
    )]
    pub bg_shell_allowed: Option<bool>,
    #[schemars(
        description = "Allow a background run to make file edits. Defaults to false (read-only tools). Implies shell access. Requires bg_allowed and a git repo — the tree does NOT need to be clean, since the run is checkpointed and can be rolled back."
    )]
    pub bg_writes_allowed: Option<bool>,
    #[schemars(
        description = "Initial status: inbox (default), todo, or in_progress. There is no 'blocked' status — a task waiting on something outside CXTasks is a normal todo with blocked_reason set (a HOLD, like defer_until), which keeps it out of Open/Today/next_task and in Waiting until the reason is cleared. 'queued' is refused here: file the task first, then queue it with update_task(status='queued'), which runs the background-run guards. There is no 'planned'/'someday' status — a task that starts later is a normal todo with defer_until set."
    )]
    pub status: Option<String>,
    #[schemars(
        description = "Model for a background run of this task. For Claude: 'claude-sonnet-5', 'claude-opus-5', 'claude-haiku-5-5' — omit to use the app's background default (Sonnet 5). For Codex: omit to use the Codex CLI's own default, which is almost always right. Only set this if the user asked for a specific model, and make sure it belongs to the executor you chose."
    )]
    pub model: Option<String>,
    #[schemars(
        description = "Tags to attach, e.g. ['billable','quick-win']. Shared across tasks, case-insensitive, reused from the existing set (list_tags) — a new one needs `new_tags`. Use these for cross-cutting labels; use `project` for where the work lives, never a tag. A PERSON is a tag spelled `@name` (`@sam`, `@dana`): put one on any task that is a follow-up with, a favour from, or a wait on someone, so `search_tasks(people: [...])` and the Waiting view can answer 'what is open with Sam'."
    )]
    pub tags: Option<Vec<String>>,
    #[schemars(
        description = "Set true to CREATE a tag that does not exist yet. Without it, a new tag is refused and the refusal names the closest existing tags — reuse one of those when it fits. Only pass this for a label that will apply to OTHER tasks too; a word that describes one task belongs in its title or notes. Never overrides a tag that names a project, repo or field."
    )]
    pub new_tags: Option<bool>,
    #[schemars(
        description = "This is a DECISION — a question only Chris can answer, not work an agent can do ('pause or continue the sequence?', 'which entity signs?'). Tags it `decision`: it appears on Today's Decide strip, next_task never hands it to an agent, and the background queue refuses it. Write the body as the question, the options, and what each unblocks — Chris reads it cold. Different from `why: decision`, which says why YOU filed rather than acted."
    )]
    pub decision: Option<bool>,
    #[schemars(
        description = "Tasks this new task is BLOCKED BY — it can't start until they're done. Each entry is a UUID or a T-number like 'T42'. Different from a subtask: this is ordering between peers, possibly across projects."
    )]
    pub blocked_by: Option<Vec<String>>,
    #[schemars(
        description = "Section (phase) within the project to file this task under, e.g. 'phase 1 — schema'. Created on first use. REQUIRES project. A section that carries a target date is a milestone — set that with upsert_section."
    )]
    pub section: Option<String>,
    #[schemars(
        description = "Wall clock for a BACKGROUND run of this task, in minutes (1-120). Omit to use the global default (20). Raise it only for genuinely large work — a refactor across several files, or anything that ends with a build. Raising this ALONE is usually pointless: a Claude run typically hits the token ceiling first (a measured run touched 24.6M of the 25M default in 14 minutes), so raise bg_run_token_cap in the same call. Codex is the exception — its usage is only reported at the end, so the wall clock is the only thing bounding it."
    )]
    pub bg_timeout_minutes: Option<i64>,
    #[schemars(
        description = "Token ceiling for ONE background run of this task (0-100000000). Omit to use the global default (25000000); 0 disables the ceiling. This is TOTAL tokens — cache reads dominate and a single honest run can touch tens of millions. Bounds one run only: the daily cap still applies and is not overridable, so this cannot be used to spend more than the day allows."
    )]
    pub bg_run_token_cap: Option<i64>,
    #[schemars(
        description = "TRIPWIRE — a CONDITION the task waits for, where defer_until is a date. Two kinds. 'script:<name> [arg]' names an executable already in the app's tripwires directory on this Mac (~/Library/Application Support/com.cxtasks.app/tripwires/<name>, must be executable — the call is refused, naming the path, if it isn't there). It is a NAME, never a shell command; the one optional argument is passed as $1 and the script must treat it as untrusted data. Contract: exit 0 = the condition holds and stdout is the evidence; exit 1 = not yet; anything else = error. 'agent:<condition>' is a question in prose ('the README documents the --sync-now flag') answered by a bounded, read-only Claude run in the task's repo (Read/Glob/Grep + get_task, ~100k tokens a check, 12 turns, 10 min, 3M tokens max, Claude only) that ends VERDICT: TRIPPED | NOT_YET | UNSURE — UNSURE counts as a failed check. PREFER script: whenever a command can answer; an agent check costs tokens every time it runs, so set model: claude-haiku-5-5 on the task and leave tripwire_on_trip at surface so a wrong verdict is read before it closes anything. An agent tripwire needs a repo_path (the check runs in it). Either kind: while armed the task is hidden exactly like a deferred one (Open, Inbox, Today, next_task, the background queue) and sits in Waiting; when it trips the task surfaces with the evidence in its comments and a banner is posted. With defer_until also set the check does not run until that date passes. Use this instead of a gate task somebody has to remember to close by hand. Omit for an ordinary task."
    )]
    pub tripwire: Option<String>,
    #[schemars(
        description = "How often the tripwire is re-checked: 'manual' (default — only when someone asks via update_task check_tripwire), '5m', '15m', '1h', '6h', 'daily', '7d', or a number of minutes. Floor 1 minute for script:, 60 minutes for agent: — every agent check is a ~100k-token background run, so 5-minute checks would be 288 runs ≈ 28M tokens a day for ONE task, and the call is refused with that arithmetic. Daily is the sane cadence for an agent check. A cadenced tripwire needs the background runner to be on; the reply says if it isn't."
    )]
    pub tripwire_check_every: Option<String>,
    #[schemars(
        description = "What a trip does: 'surface' (default) puts the task back on the board with the evidence for a human to read; 'complete' ALSO marks it done at once so anything blocked_by it is released — which means nobody reads the evidence first. Use 'complete' only for a pure gate task whose whole job is to wait for the condition. 'monitor' makes it a STANDING check that fires every time something breaks (a weekly audit, a budget watch): it surfaces like 'surface', but completing the task means 'handled' — the tripwire re-arms and is checked again on the runner's next tick, it keeps being checked while the task is done, and a trip reopens it. Archive the task (or clear the tripwire) to retire a monitor. A monitor can't also have a recurrence."
    )]
    pub tripwire_on_trip: Option<String>,
}

/// Make a project exist, and record which directory it lives in.
///
/// The second half is the one that was unreachable. A project *record* could
/// already be conjured from outside the app — `tasks::create` calls
/// `projects::ensure_exists`, so any `file_task` carrying an explicit `project`
/// makes one — which is precisely invariant 17's phantom-project hazard. What
/// no MCP tool could touch was `project_repos`, so an agent that asked
/// `project_for_repo` and was told "not registered" had nowhere to go but the
/// app's right-click menu, and every task filed from that directory stayed
/// unfiled in the meantime.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateProjectParams {
    #[schemars(
        description = "The project name AS THE USER SAYS IT — 'pocketbuddy', 'Artist Advisory'. NEVER the repo's folder name: ~/Projects/finance-app IS pocketbuddy and ~/Projects/artisttracker IS Artist Advisory. Matched case-insensitively against the existing projects, and an existing one is reused rather than duplicated."
    )]
    pub name: String,
    #[schemars(
        description = "Absolute directory to register as this project's repo — normally your current working directory. THIS IS THE HALF THAT MATTERS: it is what makes file_task and project_for_repo resolve this directory (and everything under it) to this project from now on, instead of leaving work filed here unfiled. One project may own several directories — the web and iOS halves of one product — and the first one registered becomes the primary, which is what prefills a task's workspace. A directory another project already claims is refused, naming the owner."
    )]
    pub repo_path: Option<String>,
    #[schemars(
        description = "'area' (default) or 'project'. An AREA is a standing responsibility with no finish line — CXVentures, Artist Advisory, a product you maintain. A PROJECT is finite: a certification wave, a launch, a migration. It can carry a target date, it gets closed when it's done, and a quiet month in one is worth flagging where in an area it is just a lull. Pick 'project' only when you can say what finishing looks like; when unsure, leave it — an unclassified node should not start demanding a completion date."
    )]
    pub kind: Option<String>,
    #[schemars(
        description = "The AREA this node belongs inside, by name, e.g. 'CXVentures'. Areas nest to any depth (Products → Artist Advisory → a project); nothing nests inside a project, which holds sections. The area's counts, its board row and the list you get by clicking it all roll up everything nested under it, so filing a finite wave of work here keeps it out of the area's day-to-day signal without hiding it from the area. Omit for a standalone project with no home."
    )]
    pub parent: Option<String>,
}

/// Change a project's SHAPE — the half of PARA `create_project` can't reach.
///
/// Without this, an agent could file a finite wave of work into its own project
/// and then had no way to say the wave was over: closing, re-homing, re-typing
/// and dating all lived exclusively in the app's right-click menu. A lifecycle
/// you can enter from a session and only leave from the GUI is a lifecycle that
/// doesn't get used — the same dead end `create_project` closed for
/// `project_repos`, one level up.
///
/// Every rule (the depth cap, "an area has no finish line", "move the children
/// out first") is enforced in `db::projects` and only *reported* here, so this
/// tool cannot drift out of agreement with the menu that shares them.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateProjectParams {
    #[schemars(
        description = "The area or project to change, by name (case-insensitive)."
    )]
    pub project: String,
    #[schemars(
        description = "Switch between 'area' and 'project'. Promoting a project to an area CLEARS its target date and closed stamp — claims only a finite thing can make — and keeps its parent. Demoting an area that holds anything is refused; move it out first."
    )]
    pub kind: Option<String>,
    #[schemars(
        description = "The area to nest this project inside, by name. Pass an EMPTY STRING to lift it back out to the top level — an omitted key leaves the parent alone, and an empty name is impossible otherwise, so it is the only unambiguous way to say 'no area'. A project or an area can be nested, only inside an AREA, to any depth — never inside a project (which holds sections), and never inside itself or anything beneath it."
    )]
    pub parent: Option<String>,
    #[schemars(
        description = "The project's finish line, ISO-8601 UTC with a trailing Z. Pass an EMPTY STRING to clear it. Refused on an area, which has no finish line. This is the whole-project date; a phase within it is a section with a target date (upsert_section)."
    )]
    pub target_date: Option<String>,
    #[schemars(
        description = "true = this project is FINISHED (it is closed and archived in one step, leaving the board); false = reopen it. Refused on an area — areas don't end, so close the projects inside them instead. Closing does NOT touch the tasks: any still open are reported back, because closing around unfinished work should be a visible choice rather than a silent one."
    )]
    pub closed: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectBoardParams {
    #[schemars(
        description = "Restrict to one area or project and show its update log, milestones and — for an area — the projects nested inside it, in full. Omit for the whole board."
    )]
    pub project: Option<String>,
    #[schemars(description = "Max update-log entries when `project` is given (default 5, max 50)")]
    pub limit: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostProjectUpdateParams {
    #[schemars(description = "Project name. Created if it doesn't exist yet.")]
    pub project: String,
    #[schemars(
        description = "What actually happened, in prose: what moved, what's stuck, what you need. This is the record someone reads in three weeks asking 'why did this stall' — a bare 'progressing well' is worse than nothing."
    )]
    pub body: String,
    #[schemars(
        description = "New health: on_track | at_risk | off_track | paused. Omit to file a note WITHOUT changing the status. This is the only way health can change anywhere in CXTasks — there is deliberately no bare setter, so a status always has an explanation attached."
    )]
    pub health: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpsertSectionParams {
    #[schemars(description = "Project the section belongs to")]
    pub project: String,
    #[schemars(description = "Section (phase) name, e.g. 'phase 2 — the UI'")]
    pub name: String,
    #[schemars(
        description = "Target date, ISO-8601 UTC with trailing Z (e.g. '2026-09-01T00:00:00.000Z'). A section WITH a date is a milestone and appears on the GSD board; without one it's a plain grouping. Only applied when the section has no date yet — an existing date is never overwritten, so re-running a plan can't stomp a date that was moved by hand. Omit unless the user actually gave a timeline."
    )]
    pub target_date: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListTasksParams {
    #[schemars(
        description = "Which view: 'inbox' (what is left to file: open, no project, no section, no due date, not held — giving a task a project, a section or a due date takes it out), 'today' (due or overdue, plus open decisions — held tasks hidden until released), 'waiting' (the one 'not yet' list: held back by a start date, an armed tripwire, an open blocker or a written blocked reason; oldest first), 'open' (actionable RIGHT NOW: open and not held — the app's main list, and a future DUE date does not hide a task from it), 'all' (default: every open task, held ones included), 'in_review', or 'done'. A task 'missing' from today/open is HELD — it's in 'waiting' and 'all'. 'upcoming' is accepted as a legacy spelling of 'waiting'; that view was retired."
    )]
    pub view: Option<String>,
    #[schemars(
        description = "Restrict to one area or project. Naming an AREA also returns the tasks in the projects nested inside it — the area's counts on project_board roll up the same way, so the two always agree. Use the nested project's own name to see just that slice."
    )]
    pub project: Option<String>,
    #[schemars(
        description = "Restrict to one status: inbox|todo|blocked|queued|in_progress|in_review|done. 'queued' is set by the background runner, not by hand."
    )]
    pub status: Option<String>,
    #[schemars(
        description = "Only tasks carrying ALL of these tags (AND, not OR). Tags are shared across tasks and case-insensitive."
    )]
    pub tags: Option<Vec<String>>,
    #[schemars(
        description = "Hide tasks that have an unfinished blocker. Default false — blocked work still belongs on the board."
    )]
    pub hide_blocked: Option<bool>,
    #[schemars(description = "Max rows (default 30, max 200)")]
    pub limit: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetTaskParams {
    #[schemars(description = "Task reference: either the UUID or the short T-number as printed by list_tasks / file_task (e.g. 'T42' — case-insensitive).")]
    pub task_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateTaskParams {
    #[schemars(description = "Task reference: either the UUID or the short T-number as printed by list_tasks / file_task (e.g. 'T42' — case-insensitive).")]
    pub task_id: String,
    pub title: Option<String>,
    #[schemars(
        description = UPDATE_TASK_BODY_DOC
    )]
    pub body: Option<String>,
    #[schemars(
        description = "New status: inbox|todo|in_progress|in_review|done — or 'queued' to hand the task to the background runner. 'queued' is not a plain field write: it runs the same guards as the app's queue action (repo path required, an armed review needs a finished run to read, the agent's CLI must be installed and logged in), sets bg_allowed in the same write, and reports whether the runner is actually on. Prefer complete_task for finishing work — it also handles recurrence. There is no 'blocked' status: to mark a task waiting on something, set blocked_reason (a hold) and leave the status alone. To drop a task, call archive_task — there is no 'cancelled'."
    )]
    pub status: Option<String>,
    #[schemars(
        description = "The HOLD: what the task is waiting on, outside CXTasks (a client's sign-off, an API key). Independent of status — an in_progress task can be held. While set, the task leaves Open/Today/next_task and the background drain and sits in Waiting with the reason on its row. Pass an empty string to release it. Released automatically on 'done' and on 'queued' (re-queueing is the un-park gesture for a runner-written 'Background run refused' hold)."
    )]
    pub blocked_reason: Option<String>,
    #[schemars(description = "Priority: 0 = P0 highest, 3 = P3 lowest (Meta-style)")]
    pub priority: Option<i64>,
    pub project: Option<String>,
    #[schemars(
        description = "Deadline (done BY), ISO-8601 UTC with trailing Z. Not a start date — a future due_at keeps the task visible and actionable; use defer_until to hide it until it can start. The parameter is `due_at`; there is no `due`."
    )]
    pub due_at: Option<String>,
    #[schemars(
        description = "Start date: the task moves to Waiting and is hidden from the actionable views (Open, Inbox, Today, next_task and the background queue) until this ISO-8601 UTC instant, then it reappears automatically. 'Planned/snoozed' is this date, not a status — `status: \"deferred\"` does not exist, however a task's printout reads. Independent of due_at — both can be set, and due_at alone hides nothing."
    )]
    pub defer_until: Option<String>,
    #[schemars(description = "Recurrence: daily | weekly:MO,TH | monthly:15")]
    pub recurrence: Option<String>,
    #[schemars(description = "Absolute repo path for execution context")]
    pub repo_path: Option<String>,
    #[schemars(description = "Seed prompt for a dispatched session")]
    pub prompt: Option<String>,
    #[schemars(description = "Allow unattended background execution")]
    pub bg_allowed: Option<bool>,
    #[schemars(
        description = "Allow this task to run on a host that is not the machine holding its repo — e.g. so it can run while the laptop is shut. Only meaningful for read-only work: a remote host works from a mirror, which is a throwaway clone with no tree to write back to, so anything above read-only is refused at queue time."
    )]
    pub bg_remote_allowed: Option<bool>,
    #[schemars(
        description = "Comma-separated MCP server names a DETACHED run may reach, e.g. 'vault'. Pass an empty string to clear. Unknown names are refused rather than ignored — a task that asks for the vault and silently runs without it reports confidently on nothing."
    )]
    pub bg_mcp_servers: Option<String>,
    #[schemars(
        description = "Switch the background agent: 'claude' or 'codex'. The task stays enabled or disabled as it was. Any existing model override is CLEARED unless you pass a new `model` in the same call, since a model name only means something to one vendor."
    )]
    pub bg_executor: Option<String>,
    #[schemars(description = "Allow a background run to write files (implies shell access)")]
    pub bg_writes_allowed: Option<bool>,
    #[schemars(
        description = "Allow a background run to run shell commands without granting deliberate file edits (commands may still create caches and build artifacts; the run is checkpointed)"
    )]
    pub bg_shell_allowed: Option<bool>,
    #[schemars(
        description = "Model for background runs of this task. Must belong to the task's executor (e.g. claude-sonnet-5 for Claude); omit for the executor's default."
    )]
    pub model: Option<String>,
    #[schemars(
        description = "Wall clock for a BACKGROUND run of this task, in minutes (1-120). Pass -1 to CLEAR the override and go back to the global default (20). Raise it for large work — a multi-file refactor, anything ending in a build — and raise bg_run_token_cap in the same call, or the run will usually hit the token ceiling long before the new clock matters."
    )]
    pub bg_timeout_minutes: Option<i64>,
    #[schemars(
        description = "Token ceiling for ONE background run of this task (0-100000000, where 0 disables the ceiling entirely). Pass -1 to CLEAR the override and go back to the global default (25000000). This is TOTAL tokens, dominated by cache reads. Bounds one run only — the daily cap is separate and not overridable."
    )]
    pub bg_run_token_cap: Option<i64>,
    #[schemars(
        description = "Set or replace the tripwire: 'script:<name> [arg]' or 'agent:<condition>' — see file_task for both contracts (a script is a name in the tripwires directory, never a command, and $1 is untrusted data to it; an agent check is a bounded read-only Claude run in the task's repo that answers VERDICT: TRIPPED | NOT_YET | UNSURE — prefer script: when a command can answer, set model: claude-haiku-5-5, keep tripwire_on_trip at surface). Pass an empty string to REMOVE it. A new or changed spec re-arms the task, hiding it again until the check trips."
    )]
    pub tripwire: Option<String>,
    #[schemars(
        description = "Re-check cadence: 'manual' | '5m' | '1h' | '6h' | 'daily' | '7d' | <minutes>. Requires a tripwire. Floor 1 minute for script:, 60 minutes for agent: (each agent check is a ~100k-token run; 5-minute checks ≈ 288 runs ≈ 28M tokens a day for one task, and the floor refuses with that arithmetic — daily is the sane cadence)."
    )]
    pub tripwire_check_every: Option<String>,
    #[schemars(
        description = "'surface' (default), 'complete' — a trip also completes the task and releases what it blocks, unread — or 'monitor': a standing check. Completing a monitor means 'handled': it re-arms and is re-checked on the runner's next tick, it keeps being checked while done, and a trip reopens it. Archive it to retire it."
    )]
    pub tripwire_on_trip: Option<String>,
    #[schemars(
        description = "Ask the runner to evaluate the tripwire on its next tick (~20 s when it is on), whatever the cadence — the only way a 'manual' tripwire is ever checked. Refused while the task is deferred, because the date gate comes first. The RESULT lands on the task (state, comment) later, not in this reply; get_task shows it."
    )]
    pub check_tripwire: Option<bool>,
    #[schemars(
        description = "Put a tripped or errored tripwire back to 'armed', hiding the task again until the next trip. Fix the script first if it errored — an armed check that keeps failing surfaces again after five tries."
    )]
    pub rearm_tripwire: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompleteTaskParams {
    #[schemars(description = "Task reference: either the UUID or the short T-number as printed by list_tasks / file_task (e.g. 'T42' — case-insensitive).")]
    pub task_id: String,
    #[schemars(
        description = "What was actually done, written as a HANDOFF for someone with none of your context: what you did and found (name the files and symbols), what you did NOT do and why, and the next concrete step. Length is fine — the full text is kept on a run row and replayed to whoever picks this task up next; a one-line summary is derived from your OPENING LINE for the task's log, so make that first sentence the outcome and don't write the summary yourself. Renders as markdown: after that line use short sections, bullets or a table, never one long paragraph; code, paths and anything in angle brackets go in backticks."
    )]
    pub result: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectForRepoParams {
    #[schemars(
        description = "An absolute directory path — normally your current working directory. A subdirectory of a registered repo resolves to the same project."
    )]
    pub repo_path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NextTaskParams {
    #[schemars(
        description = "Restrict to one area or project. Naming an AREA also returns the tasks in the projects nested inside it — the area's counts on project_board roll up the same way, so the two always agree. So asking an area for what to do next can hand back work that lives in a project inside it."
    )]
    pub project: Option<String>,
}

/// Text **and/or** facets — the same surface the app's filter bar drives.
///
/// Facets live here rather than on a tool of their own for invariant 18's
/// reason one level up: `list_tasks` and `search_tasks` already both return
/// task lists, and a third would make the choice between them a coin flip an
/// agent sometimes loses. This tool's scope (trashed out, archived in, no view)
/// is already exactly what `db::search::filtered` wants, so the facets belong
/// on it and nowhere else.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchTasksParams {
    #[schemars(
        description = "Free-text query over title, notes and project. Optional — filters alone are a valid query."
    )]
    pub query: Option<String>,
    #[schemars(
        description = "Match ANY of these areas or projects. Naming an AREA also returns the tasks in the projects nested inside it — the area's counts on project_board roll up the same way, so the two always agree."
    )]
    pub projects: Option<Vec<String>>,
    #[schemars(
        description = "Match ANY of these statuses: inbox, todo, blocked, queued, in_progress, in_review, done"
    )]
    pub statuses: Option<Vec<String>>,
    #[schemars(description = "Match ANY of these priorities. 0 is HIGHEST, 3 is lowest.")]
    pub priorities: Option<Vec<i64>>,
    #[schemars(description = "Match tasks carrying ALL of these tags (narrowing, not ANY)")]
    pub tags: Option<Vec<String>>,
    #[schemars(description = "Due strictly BEFORE this local day, YYYY-MM-DD (the day itself is excluded)")]
    pub due_before: Option<String>,
    #[schemars(description = "Due strictly AFTER this local day, YYYY-MM-DD (the day itself is excluded)")]
    pub due_after: Option<String>,
    #[schemars(description = "Created strictly BEFORE this local day, YYYY-MM-DD")]
    pub created_before: Option<String>,
    #[schemars(description = "Created strictly AFTER this local day, YYYY-MM-DD")]
    pub created_after: Option<String>,
    #[schemars(
        description = "true = only tasks WAITING on something (a start date, an armed tripwire, an open blocker, or a `blocked` status); false = only tasks that are not. The app's Waiting view is this facet."
    )]
    pub waiting: Option<bool>,
    #[schemars(
        description = "Tasks involving ANY of these people. A person is a tag spelled `@name` (`@sam`, `@dana`); pass names with or without the `@`. `[\"*\"]` means any task with a person on it at all — the way to count relationship work against code work (or read the retrospective's mix)."
    )]
    pub people: Option<Vec<String>>,
    #[schemars(
        description = "Include finished tasks. Default false — closed work is                        left out unless you ask for it, or name `done` in statuses."
    )]
    pub include_done: Option<bool>,
    #[schemars(description = "Max results (default 20, max 200)")]
    pub limit: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetrospectiveParams {
    #[schemars(
        description = "Area or project to review. Naming an area includes its nested projects. Omit to review across projects."
    )]
    pub project: Option<String>,
    #[schemars(description = "Inclusive start date, YYYY-MM-DD, based on when work completed or was superseded")]
    pub from: Option<String>,
    #[schemars(description = "Inclusive end date, YYYY-MM-DD, based on when work completed or was superseded")]
    pub to: Option<String>,
    #[schemars(description = "Maximum closed/superseded tasks to scan (default 30, max 100)")]
    pub limit: Option<u32>,
}

impl SearchTasksParams {
    /// The facet half, as the DB's spec. `query` is folded in by the caller so
    /// the T-number shortcut can inspect it first.
    fn spec(&self) -> db::search::FilterSpec {
        db::search::FilterSpec {
            query: None,
            projects: self.projects.clone(),
            statuses: self.statuses.clone(),
            priorities: self.priorities.clone(),
            tags: self.tags.clone(),
            due_before: self.due_before.clone(),
            due_after: self.due_after.clone(),
            created_before: self.created_before.clone(),
            created_after: self.created_after.clone(),
            // The tool keeps its BOOL — an agent asking "what is waiting" wants
            // the whole predicate, and the four kinds are an app-side control.
            // `*` is that predicate by name, so both surfaces still route
            // through `db::tasks::waiting_sql` (invariant 29).
            waiting: match self.waiting {
                Some(true) => Some(vec!["*".to_string()]),
                _ => None,
            },
            exclude_waiting: match self.waiting {
                Some(false) => Some(vec!["*".to_string()]),
                _ => None,
            },
            people: self.people.clone(),
            // Same default the apps get. An agent asking "what is open in Acme"
            // should not have to filter the year's finished work out of the
            // answer, and one that genuinely wants it can say so — or name
            // `done` in `statuses`, which wins either way.
            exclude_closed: Some(!self.include_done.unwrap_or(false)),
            limit: Some(self.limit.unwrap_or(20).min(200)),
            // No view, no node, no sort, no negatives: `search_tasks` is the
            // search scope, and the negative facets are an app-side control
            // (invariant 21 pins the tool count).
            ..Default::default()
        }
    }

    fn has_facets(&self) -> bool {
        fn some<T>(v: &Option<Vec<T>>) -> bool {
            v.as_ref().is_some_and(|x| !x.is_empty())
        }
        some(&self.projects)
            || some(&self.statuses)
            || some(&self.priorities)
            || some(&self.tags)
            || some(&self.people)
            || self.waiting.is_some()
            || self.due_before.is_some()
            || self.due_after.is_some()
            || self.created_before.is_some()
            || self.created_after.is_some()
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlockTaskParams {
    #[schemars(description = "The task that is blocked (can't proceed). UUID or T-number like 'T42'.")]
    pub task_id: String,
    #[schemars(description = "The task it is waiting on. UUID or T-number like 'T42'.")]
    pub blocked_by_id: String,
    #[schemars(description = "Set true to REMOVE the dependency instead of adding it")]
    pub remove: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelateTaskParams {
    #[schemars(
        description = "The newer/replacement task for supersedes, or either task for relates_to. UUID or T-number."
    )]
    pub task_id: String,
    #[schemars(
        description = "The older task being superseded, or the other task for relates_to. UUID or T-number."
    )]
    pub other_task_id: String,
    #[schemars(description = "supersedes or relates_to (relates-to is also accepted)")]
    pub relation: String,
    #[schemars(description = "Set true to remove the relation instead of adding it")]
    pub remove: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TagTaskParams {
    #[schemars(description = "Task reference: either the UUID or the short T-number as printed by list_tasks / file_task (e.g. 'T42' — case-insensitive).")]
    pub task_id: String,
    #[schemars(description = "Tag names to attach or remove (case-insensitive). Reuse existing tags — list_tags shows them; a new one needs new_tags")]
    pub tags: Vec<String>,
    #[schemars(description = "Set true to REMOVE these tags from the task instead of adding")]
    pub remove: Option<bool>,
    #[schemars(
        description = "Set true to CREATE a tag that does not exist yet. Without it, a new tag is refused and the refusal names the closest existing tags — reuse one of those when it fits. Only pass this for a label that will apply to OTHER tasks too; a word that describes one task belongs in its title or notes. Never overrides a tag that names a project, repo or field."
    )]
    pub new_tags: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentTaskParams {
    #[schemars(description = "Task reference: either the UUID or the short T-number as printed by list_tasks / file_task (e.g. 'T42' — case-insensitive).")]
    pub task_id: String,
    #[schemars(description = "The comment text. Capped at 2,000 characters — see `handoff` for anything longer. Renders as markdown: short paragraphs or bullets, never one long block; code, paths and anything in angle brackets go in backticks.")]
    pub body: String,
    #[schemars(
        description = "Set true when `body` is a full HANDOFF rather than a note — you are leaving the task mid-way and want the next session to pick up where you stopped. The full text is stored on a run row and replayed verbatim into the next dispatch (no length cap), while the log gets a one-line summary derived from your opening line. Foreground sessions only: in an unattended run the handoff is captured from your FINAL MESSAGE and this is refused."
    )]
    pub handoff: Option<bool>,
}

/// Deliberately much smaller than [`FileTaskParams`].
///
/// Every field that tool asks a human-driven session to decide — project,
/// repo, executor, tool grants, due date, tags, dependencies — is either
/// inherited from the parent task or is not a background run's call to make.
/// Narrowing it is most of why this is a separate tool: the description is
/// ~600 chars against `file_task`'s ~6,300, which is a third of the whole
/// server's context cost.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileFollowupParams {
    #[schemars(description = "Reference of the task you are working on — the UUID or T-number from your prompt. The follow-up is a separate untriaged task linked to it.")]
    pub task_id: String,
    #[schemars(description = "What needs doing, as an imperative one-liner. 'Add a retry to the intake webhook on 429', not 'webhook issue'.")]
    pub title: String,
    #[schemars(description = "Why this is needed and what you already know: the files, the failure, what you ruled out. Written for an agent that will have none of your context. Renders as markdown: short paragraphs or bullets, with code and paths in backticks.")]
    pub body: Option<String>,
    #[schemars(description = "0=highest .. 3=lowest. Defaults to 3 — you are proposing work, not scheduling it.")]
    pub priority: Option<i64>,
}

// ─── Server ───────────────────────────────────────────────────────────────

/// [`CxTasksMcp::author`]'s decision, pure so it can be tested without a
/// process environment. An unparseable declaration is ignored rather than
/// trusted: it falls through to the client name.
fn author_for(declared: Option<&str>, client_name: Option<&str>) -> &'static str {
    use crate::executor::BackgroundExecutor;
    if let Some(e) = declared.and_then(|d| BackgroundExecutor::parse(d).ok()) {
        return e.as_str();
    }
    match client_name {
        Some(n) if n.to_ascii_lowercase().contains("codex") => BackgroundExecutor::Codex.as_str(),
        _ => BackgroundExecutor::Claude.as_str(),
    }
}

/// The env var that puts this server in **background mode**.
///
/// Set by the runner on the agent process AND written explicitly into the
/// `--mcp-config` it generates, so the reduced surface never depends on an
/// assumption about how the CLI passes its environment to a child.
pub const BACKGROUND_ENV: &str = "CXTASKS_BACKGROUND_RUN";

/// The only tools an unattended run gets.
///
/// **Chosen so that the failure this replaces becomes impossible rather than
/// forbidden.** A background run used to be told "do NOT call `complete_task`
/// or any other CXTasks tool" and given no MCP server at all, which closed the
/// hole by removing the whole channel — so the one genuinely useful thing an
/// unattended agent can do, leave a handoff the next one can pick up, had to
/// travel through its final message or not at all.
///
/// The tools NOT here are absent from the router (see [`CxTasksMcp::new`]),
/// not deny-listed. That matters twice over: a removed tool costs no tokens and
/// suggests no capability, and `complete_task` cannot be called by an agent
/// that was never told it exists — where a deny rule is only as good as the CLI
/// recognising the name (invariant 7's "matches no known tool" trap).
pub const BACKGROUND_TOOLS: &[&str] = &["comment_task", "file_followup", "get_task"];

/// The same list as `--allowedTools` names them: `mcp__<server>__<tool>`.
///
/// The server name is fixed by the `--mcp-config` the runner writes, so this
/// can be a constant rather than something threaded through the spawn.
pub const BACKGROUND_TOOLS_CSV: &str =
    "mcp__cxtasks__comment_task,mcp__cxtasks__file_followup,mcp__cxtasks__get_task";

/// The env var naming the AGENT this server serves — `claude` or `codex`, as
/// [`crate::executor::BackgroundExecutor::parse`] reads it.
///
/// The runner sets it in the MCP config it writes for a Codex run, so the
/// comments that run writes are signed as Codex. Absent, the author falls back
/// to the client's own `initialize` name (see [`CxTasksMcp::author`]).
pub const AGENT_ENV: &str = "CXTASKS_AGENT";

/// The env var that puts this server in **check mode** — an `agent:` tripwire
/// evaluation. Set by the runner's agent lane the way [`BACKGROUND_ENV`] is set
/// by its drain.
pub const CHECK_ENV: &str = "CXTASKS_CHECK_RUN";

/// The only tool a tripwire CHECK gets: re-read the task it is checking.
///
/// Narrower than [`BACKGROUND_TOOLS`] by two tools, and each absence is a
/// decision. No `comment_task`: a check's whole output is one VERDICT line in
/// its final message, and a check that could leave notes would leave a trail
/// of "not yet" on the task every hour. No `file_followup`: a check is asked a
/// yes/no question and must not turn it into work for somebody.
pub const CHECK_TOOLS: &[&str] = &["get_task"];

/// [`CHECK_TOOLS`] as `--allowedTools` names them.
pub const CHECK_TOOLS_CSV: &str = "mcp__cxtasks__get_task";

/// Which surface this server serves, decided once at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpMode {
    /// A person's Claude session: everything but `file_followup`.
    Interactive,
    /// An unattended work run: [`BACKGROUND_TOOLS`].
    Background,
    /// An `agent:` tripwire check: [`CHECK_TOOLS`].
    Check,
}

impl McpMode {
    /// Read the mode off the environment. Check wins over Background when
    /// both are set — the runner sets both on a check process, the narrower
    /// one being the one that describes it.
    pub fn from_env() -> Self {
        if std::env::var(CHECK_ENV).is_ok() {
            Self::Check
        } else if std::env::var(BACKGROUND_ENV).is_ok() {
            Self::Background
        } else {
            Self::Interactive
        }
    }

    /// Is this a reduced, unattended surface of either kind?
    pub fn is_unattended(self) -> bool {
        !matches!(self, Self::Interactive)
    }

    /// Does this mode KEEP the named tool?
    fn keeps(self, name: &str) -> bool {
        match self {
            Self::Interactive => !INTERACTIVE_ONLY_EXCLUSIONS.contains(&name),
            Self::Background => BACKGROUND_TOOLS.contains(&name),
            Self::Check => CHECK_TOOLS.contains(&name),
        }
    }
}

/// `file_followup` is background-only and hidden everywhere else.
///
/// An interactive session has `file_task`, which is richer in every way. Two
/// tools that both file a task would be a coin flip for the model, and the one
/// with the narrower contract would win some of the time for no reason.
const INTERACTIVE_ONLY_EXCLUSIONS: &[&str] = &["file_followup"];

/// What the client is told this server is FOR, served at `initialize`.
///
/// **The point of this text is the split between the two ways a task gets
/// filed, because only one of them has a bar.** The overwhelmingly common
/// path is Chris saying "file that" — a meeting action item, a bug for later,
/// an errand with no code anywhere near it. That path must not be gated: a
/// model that pauses to weigh whether an explicit request "deserves" a task
/// is broken, and the failure is invisible (the board just stays emptier than
/// it should).
///
/// The bar exists only for the case where the model decides on its own, which
/// is the one that produces noise. It is written as a test with both sides
/// stated rather than as discouragement, for the same reason: over-suppression
/// fails silently and would surface weeks later as work nobody captured.
///
/// This answers WHETHER to file. `file_task`'s own description answers HOW,
/// and the two deliberately do not restate each other.
///
/// **The bar now has a mechanical half, because the written one did not
/// hold.** On 2026-08-25 a cxventures session filed T147 and T148 — the second
/// a ~30-minute reorder it had full context for — instead of doing the work,
/// prompted by a pasted third-party review saying "worth a small task so they
/// don't vanish"; the rows say "Filed so it isn't lost." This text was in the
/// system prompt the whole time. When Chris asked "why not now?" the agent
/// produced the right answer at once, so the judgment was available and
/// nothing forced it at the moment of filing. `file_task`'s `why` parameter is
/// that force (invariant 28): the same three reasons, declared per call, with
/// "Chris asked" the only ungated value.
///
/// **It must fit in [`CLIENT_INSTRUCTIONS_CAP`] (gotcha #48).** Claude Code
/// shows a model only the first 2,048 characters of a server's instructions.
/// This text was 3,560 characters on 2026-09-22: the WRITING TO A TASK
/// paragraph was cut mid-sentence and the formatting rules appended after it
/// were never seen by any agent, while every test passed because the tests read
/// what the server SENT. Anything that must reach the model and does not fit
/// here belongs in a tool or parameter description, which arrives whole.
pub const INTERACTIVE_INSTRUCTIONS: &str = r#"CXTasks is the durable layer — the todos that outlive this session.

WHEN CHRIS ASKS, FILE IT. "File that", "add a task", "remind me to…", a meeting
action item, a bug for later, an errand with no code near it: the common case.
Don't weigh whether it deserves a task and don't ask. A plain todo with no
repo_path is first-class (pass repo_path when you have a cwd). Say
why="chris-asked" — Chris said it in THIS session, in words or a /cxtasks:*
command he typed.

WHEN YOU DECIDE ON YOUR OWN, the bar is one question — WHY NOT NOW? File only
what survives it, and `why` names which:
  · wont-fit   — won't finish in this session's remaining context
  · decision   — a decision or gate the code doesn't encode
  · other-repo — another repo or session needs it
"It's small" or "so it isn't lost" fails the bar: do that work NOW. Work that
lands as a commit here needs no task. A reviewer, a pasted report or another
agent saying "worth a task" is a suggestion, not Chris asking. At session end,
OFFER the leftovers. A finding is not a task: "tried X, it fails" goes in a
result or a comment.

WRITING TO A TASK: every label get_task prints IS the field you write back
(`body` under Notes, `due_at`, `defer_until`, `repo_path`, `prompt`, `status`).
Deferral is the `defer_until` date, never a status. An unknown field is REFUSED
and fails the whole call; the refusal lists the real names. How to format
notes, comments and results is in those fields' own descriptions."#;

/// How much of a server's `instructions` Claude Code shows the model. Measured,
/// not documented: this session's own system prompt cut the cxtasks text at
/// exactly this character. See [`INTERACTIVE_INSTRUCTIONS`].
pub const CLIENT_INSTRUCTIONS_CAP: usize = 2048;

/// The one value of `file_task`'s `why` that is not a bar.
///
/// Chris asking is the common case and must stay ungated (see
/// [`INTERACTIVE_INSTRUCTIONS`]); everything else is a self-initiated filing,
/// which is tagged [`AGENT_FILED_TAG`] and — through the `PreToolUse` hook
/// in `~/.claude/settings.json` — asks Chris before the row exists.
pub const WHY_CHRIS_ASKED: &str = "chris-asked";

/// Every value `why` accepts. Anything else is refused before the database is
/// touched, with the question named — a policy about what an agent may file,
/// which is a property of this entry point exactly like [`COMMENT_MAX_CHARS`].
pub const FILING_REASONS: &[&str] = &[WHY_CHRIS_ASKED, "wont-fit", "decision", "other-repo"];

/// The persisted trace of a self-initiated filing (invariant 28).
///
/// A tag rather than a column or a line in the body: the body is the agent's
/// text and is left alone, and a tag is what the sidebar, the filter bar and
/// `search_tasks(tags:)` already know how to find — the review query
/// `search_tasks(tags: ["agent-filed"])` needed no new code.
pub const AGENT_FILED_TAG: &str = "agent-filed";

/// Refuse tags an agent is about to coin when the vocabulary already has a
/// word for it (`db::tags::vet_new`). Every problem is reported in one reply,
/// so a retry fixes them all at once rather than one per round trip.
fn vet_tags(conn: &rusqlite::Connection, names: &[String], allow_new: bool) -> Result<(), McpError> {
    use db::tags::NewTagIssue;
    let mut problems = Vec::new();
    let mut unknown = false;
    for name in names {
        let issue = db::tags::vet_new(conn, name)
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
        let Some(issue) = issue else { continue };
        if allow_new && issue.overridable() {
            continue;
        }
        problems.push(match issue {
            NewTagIssue::ShadowsProject { tag, project } => format!(
                "#{tag} names the project/repo '{project}' — set `project` instead; a tag restating it can't be filtered any better and drifts when the task moves"
            ),
            NewTagIssue::IsAField { tag } => format!(
                "#{tag} restates a status, priority or Waiting facet the task already has — set that field instead"
            ),
            NewTagIssue::NearDuplicate { tag, existing } => format!(
                "#{tag} is new, and close to existing {} — use that",
                existing.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" / ")
            ),
            NewTagIssue::Unknown { tag } => {
                unknown = true;
                format!("#{tag} doesn't exist yet")
            }
        });
    }
    if problems.is_empty() {
        return Ok(());
    }
    let mut msg = format!("Tags refused, nothing was written:\n- {}", problems.join("\n- "));
    if unknown {
        let common = db::tags::list_with_counts(conn)
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.open_count > 0 && !db::tags::is_person(&t.name))
            .take(12)
            .map(|t| format!("#{}", t.name))
            .collect::<Vec<_>>()
            .join(" ");
        msg.push_str(&format!(
            "\nReuse an existing tag if one fits (most used: {common}). If this label will apply to other tasks too, retry with new_tags: true; if it only describes this task, drop it."
        ));
    }
    Err(McpError::invalid_params(msg, None))
}

/// The longest comment this server will write.
///
/// **The comment log is the one channel here that was unbounded on the write
/// side, and it was rotting.** Measured on the real DB: 61 agent comments
/// totalling 118k chars against 1 human comment of 40, with typical agent
/// entries running 2–4k. `dispatch::history_block` truncates every comment it
/// replays at 1,200 chars, so those entries reached the next agent as six
/// mid-sentence fragments — the bound held and the content did not.
///
/// 2,000 is deliberately *above* that replay bound rather than equal to it: a
/// chunky, legitimately-long note should be admitted whole even though a
/// prompt will clip it, while the 2–4k dumps that motivated this are refused.
/// The refusal names the channel those belong in instead — `handoff: true`, or
/// `complete_task`'s `result` — so it is a redirection, not a wall.
///
/// **Enforced at this handler only.** `db::comments::add` stays uncapped
/// because the app's own composer and `runner::finish_run` write through it,
/// and neither is the thing that needed bounding: one is a human typing, the
/// other already writes a derived ~200-char summary. A length policy in the
/// data layer would be a policy about *storage*; this is a policy about what an
/// agent is allowed to file, which is a property of the entry point.
pub const COMMENT_MAX_CHARS: usize = 2_000;

/// How many comments `get_task` renders, newest-first-then-oldest-first within
/// the window.
///
/// `get_task` is one of [`BACKGROUND_TOOLS`], so this output lands in unattended
/// runs too, and it was the ONLY unbounded region of it — runs are already
/// `take(5)` at 400 chars each. T32 alone returned 33k characters of comments.
const DETAIL_COMMENTS: usize = 10;

/// Per-comment character bound in `get_task`, deliberately **tied** to
/// [`COMMENT_MAX_CHARS`]: a comment this server was willing to accept must
/// render whole when it is read back. Set it lower and the tool truncates
/// entries it just told the caller were fine.
const DETAIL_COMMENT_CHARS: usize = COMMENT_MAX_CHARS;

#[derive(Clone)]
pub struct CxTasksMcp {
    tool_router: ToolRouter<Self>,
    db_path: String,
    /// Which surface this is.
    ///
    /// Kept on the struct rather than dropped after routing, because one rule
    /// cannot be expressed by removing a route: `comment_task` exists in both
    /// the interactive and background modes and its `handoff` parameter must
    /// be refused in exactly one of them (a router removes tools, never
    /// parameters). See the handler.
    mode: McpMode,
    /// Set once a real MCP client completes `initialize`. The orphan watchdog
    /// in `bin/mcp.rs` reaps processes that are never claimed.
    claimed: Arc<AtomicBool>,
    /// The `clientInfo.name` the MCP client gave at `initialize` — how a
    /// Codex session that was not started by the runner still signs as Codex.
    client_name: Arc<std::sync::Mutex<Option<String>>>,
}

impl CxTasksMcp {
    /// Who a comment, result or project update written through this server is
    /// signed as: the agent actually calling it, never a blanket "claude".
    ///
    /// [`AGENT_ENV`] wins — the runner states it outright for its own runs.
    /// Otherwise the client's `initialize` name decides (Codex identifies as
    /// `codex-mcp-client`), and anything unrecognised is Claude, the
    /// foreground agent.
    fn author(&self) -> &'static str {
        let declared = std::env::var(AGENT_ENV).ok();
        let client = self.client_name.lock().ok().and_then(|g| g.clone());
        author_for(declared.as_deref(), client.as_deref())
    }

    /// The raw open, split from [`Self::open_db`] so the queue path can hand
    /// connections to `dispatch::queue_for_background`, which speaks
    /// `AppError` (a `rusqlite::Error` converts via `#[from]`).
    fn open_db_raw(&self) -> Result<rusqlite::Connection, rusqlite::Error> {
        let conn = rusqlite::Connection::open(&self.db_path)?;
        // Per-connection in SQLite; the ON DELETE CASCADE on subtasks and runs
        // does not fire without it.
        if let Err(e) = conn.execute_batch("PRAGMA foreign_keys = ON;") {
            log::warn!("MCP open_db: PRAGMA foreign_keys = ON failed: {e}");
        }
        // Another process may hold a write lock (the app, the runner).
        if let Err(e) = conn.busy_timeout(std::time::Duration::from_secs(5)) {
            log::warn!("MCP open_db: busy_timeout failed: {e}");
        }
        Ok(conn)
    }

    fn open_db(&self) -> Result<rusqlite::Connection, McpError> {
        self.open_db_raw()
            .map_err(|e| McpError::internal_error(format!("DB open failed: {e}"), None))
    }

    pub fn claimed_flag(&self) -> Arc<AtomicBool> {
        self.claimed.clone()
    }

    /// Turn whichever form of reference the caller used into the UUID the db
    /// layer wants: a T-number (`T42`, `t42`) or the UUID itself.
    ///
    /// Applied at **every** id parameter on every tool. The point of a speakable
    /// id is that it works wherever an id works — a T-number that resolves in
    /// `get_task` but not in `block_task` is worse than none, because you only
    /// find the gap at the moment you're relying on it.
    ///
    /// Deliberately resolves trashed and archived tasks too (`resolve_ref` is
    /// unfiltered): "T42" written into a commit message last month has to keep
    /// meaning something.
    fn resolve_task_id(&self, raw: &str) -> Result<String, McpError> {
        let conn = self.open_db()?;
        db::tasks::resolve_ref(&conn, raw)
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?
            .map(|t| t.id)
            .ok_or_else(|| McpError::invalid_params(format!("task '{raw}' not found"), None))
    }

    async fn set_archived(&self, raw: &str, archive: bool) -> Result<CallToolResult, McpError> {
        let task_id = self.resolve_task_id(raw)?;
        let (verb, verb_past) =
            if archive { ("archive_task", "Archived") } else { ("unarchive_task", "Unarchived") };
        let task = {
            let mut conn = self.open_db()?;
            crate::api::dispatch_synced(&mut conn, verb, &serde_json::json!({ "id": task_id }))
                .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
            db::tasks::get(&conn, &task_id)
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?
                .ok_or_else(|| McpError::invalid_params(format!("task '{raw}' not found"), None))?
        };
        notify(&Envelope::new("task-updated", &task.id).with_tool(verb).with_summary(&task.title))
            .await;
        Self::text(format!("{verb_past}: {}\n  id: {}", task.title, task.id))
    }

    fn text(body: String) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![Content::text(body)]))
    }
}

/// Map an MCP integer override onto `TaskPatch`'s two-level option.
///
/// `None` (key omitted, or JSON `null`) → leave the stored value alone.
/// `Some(-1)` → clear the override, restoring the global default.
/// `Some(n)`  → set it; the range is checked in `db::tasks::update`.
///
/// The sentinel exists because a plain `Option<i64>` collapses "absent" and
/// "null" into the same `None`, so there is no other way for a caller to say
/// "put this back on the default" — and without it, an override could be set
/// from a session and only ever removed from the app.
fn clear_or_set(v: Option<i64>) -> Option<Option<i64>> {
    match v {
        None => None,
        Some(n) if n < 0 => Some(None),
        Some(n) => Some(Some(n)),
    }
}

/// Reject a repo_path that isn't a usable absolute directory rather than
/// storing a value that will fail at dispatch time, hours later.
fn validate_repo_path(raw: &str) -> Result<String, McpError> {
    let trimmed = raw.trim();
    let expanded = if let Some(rest) = trimmed.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| Path::new(trimmed).to_path_buf())
    } else {
        Path::new(trimmed).to_path_buf()
    };
    if !expanded.is_absolute() {
        return Err(McpError::invalid_params(
            format!("repo_path must be absolute, got '{trimmed}'. Pass your current working directory."),
            None,
        ));
    }
    if !expanded.is_dir() {
        return Err(McpError::invalid_params(
            format!("repo_path '{}' is not an existing directory", expanded.display()),
            None,
        ));
    }
    Ok(expanded.to_string_lossy().to_string())
}

fn parse_view(raw: Option<&str>) -> Result<db::tasks::View, McpError> {
    use db::tasks::View;
    Ok(match raw.map(str::trim).unwrap_or("all").to_ascii_lowercase().as_str() {
        "inbox" => View::Inbox,
        "today" => View::Today,
        // `upcoming` is the retired view's spelling, kept for the same reason
        // the serde alias is: an old plugin copy or an un-updated client must
        // land on the list that inherited its rows, not on an error.
        "waiting" | "upcoming" => View::Waiting,
        // The sidebar's actionable view. "anytime" is Things 3's word for the
        // same list — accepted so an agent reasoning in that vocabulary lands
        // on the right view instead of an error.
        "open" | "actionable" | "anytime" => View::Open,
        "all" | "" => View::All,
        // `needs_review` kept as an alias: it is the pre-v12 name and it is
        // written down in `claude-plugin/commands/tasks.md`, so a session running
        // an older copy of the plugin would otherwise start erroring.
        "in_review" | "needs_review" | "review" => View::InReview,
        "done" | "completed" => View::Done,
        other => {
            return Err(McpError::invalid_params(
                format!("unknown view '{other}' (expected inbox|today|waiting|open|all|in_review|done)"),
                None,
            ))
        }
    })
}

/// P0 is highest (Meta convention), P3 lowest.
fn priority_label(p: i64) -> &'static str {
    match p {
        0 => "P0 (highest)",
        1 => "P1",
        2 => "P2",
        _ => "P3 (lowest)",
    }
}

/// The speakable reference for a task: `T42`. Empty when the row predates v6
/// and hasn't been healed yet — see `db::tasks::backfill_short_ids`.
fn short_ref(t: &db::tasks::Task) -> Option<String> {
    t.short_id.map(|n| format!("T{n}"))
}

/// Say back what was actually searched for.
///
/// The result line has to name every filter it applied, because an empty
/// result is otherwise ambiguous in the one way that matters: "no task is due
/// before Aug 3" and "you typed a project name that doesn't exist" produce the
/// identical `No tasks matching 'x'.` An agent reading the first as the second
/// re-queries forever; reading the second as the first reports "nothing found"
/// about a filter that never had a chance to match.
fn describe_search(query: &str, p: &SearchTasksParams) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !query.is_empty() {
        parts.push(format!("'{query}'"));
    }
    let mut list = |label: &str, values: Option<&Vec<String>>| {
        if let Some(v) = values.filter(|v| !v.is_empty()) {
            parts.push(format!("{label} {}", v.join("/")));
        }
    };
    list("project", p.projects.as_ref());
    list("status", p.statuses.as_ref());
    list("tag", p.tags.as_ref());
    if let Some(v) = p.priorities.as_ref().filter(|v| !v.is_empty()) {
        let joined = v.iter().map(|n| format!("P{n}")).collect::<Vec<_>>().join("/");
        parts.push(format!("priority {joined}"));
    }
    for (label, value) in [
        ("due before", &p.due_before),
        ("due after", &p.due_after),
        ("created before", &p.created_before),
        ("created after", &p.created_after),
    ] {
        if let Some(d) = value.as_deref().filter(|d| !d.trim().is_empty()) {
            parts.push(format!("{label} {d}"));
        }
    }
    if parts.is_empty() {
        "everything".to_string()
    } else {
        parts.join(" + ")
    }
}

/// One line per task for list output.
///
/// Leads with the T-number so the reference is the first thing readable, and
/// still prints the UUID on the continuation line — the two audiences are
/// different. The T-number is for a human to say out loud or paste into a commit
/// message; the UUID is what a follow-up tool call has always used, and a test
/// pins that it stays.
fn format_line(t: &db::tasks::Task) -> String {
    let reference = short_ref(t).map(|r| format!("[{r}] ")).unwrap_or_default();
    let mut bits = vec![format!("[{}]", t.status)];
    // Only surface a priority that's above the default floor.
    if t.priority < db::tasks::PRIORITY_LOWEST {
        bits.push(format!("P{}", t.priority));
    }
    if let Some(p) = t.project.as_deref() {
        bits.push(format!("#{p}"));
    }
    if let Some(d) = t.due_at.as_deref() {
        bits.push(format!("due {d}"));
    }
    // A STILL-PENDING defer is called out per row: without it a deferred task
    // in an `all` listing is indistinguishable from an actionable one, and an
    // agent reads it as workable now. A defer that has already passed is
    // spent — printing it would label actionable work as waiting.
    if let Some(d) = t.defer_until.as_deref() {
        if crate::recurrence::parse_iso(d).is_some_and(|dt| dt > chrono::Utc::now()) {
            bits.push(format!("deferred until {d}"));
        }
    }
    if t.is_dispatchable() {
        bits.push("⎇ has repo".to_string());
    }
    format!("- {reference}{} — {}\n  id: {}", t.title, bits.join(" · "), t.id)
}

fn edge_ref(edge: &db::relations::RelationEdge) -> String {
    edge.short_id
        .map(|n| format!("T{n}"))
        .unwrap_or_else(|| edge.task_id.clone())
}

/// Search must distinguish replaced work from completed work. The task status
/// cannot say that — a superseded task may deliberately remain open so nobody
/// claims it finished — so the graph fact is rendered beside the ordinary row.
fn format_search_line(
    conn: &rusqlite::Connection,
    task: &db::tasks::Task,
) -> Result<String, crate::error::AppError> {
    let replacements = db::relations::superseded_by(conn, &task.id)?;
    if replacements.is_empty() {
        return Ok(format_line(task));
    }
    Ok(format!(
        "{}\n  superseded by: {}",
        format_line(task),
        replacements
            .iter()
            .map(|e| format!("{} ({})", edge_ref(e), e.title))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn format_retrospective(r: &db::retrospective::Retrospective) -> String {
    let cite = |e: &db::retrospective::RetrospectiveEvidence| {
        format!(
            "- [{} · {} · {}] {} — {}",
            e.task_ref.as_deref().unwrap_or(&e.task_id),
            e.closure,
            e.source,
            e.title,
            e.text
        )
    };
    let mut out = format!(
        "Retrospective: {} closed/superseded task(s), {} stored run/comment item(s) scanned.\n",
        r.tasks_scanned, r.source_items_scanned
    );
    out.push_str("\nWHAT WE LEARNED\n");
    if r.learned.is_empty() {
        out.push_str("- No stored run result or comment supplied learning evidence in this scope.\n");
    } else {
        out.push_str(&r.learned.iter().map(cite).collect::<Vec<_>>().join("\n"));
        out.push('\n');
    }
    out.push_str("\nWHAT KEEPS RECURRING\n");
    if r.recurring.is_empty() {
        out.push_str("- No term appeared across more than one task in this scope.\n");
    } else {
        for theme in &r.recurring {
            out.push_str(&format!(
                "- {} — {} task(s): {}\n",
                theme.term,
                theme.task_count,
                theme.task_refs.join(", ")
            ));
        }
    }
    out.push_str("\nTHE MIX (the October check — people on the closed work, or nobody)\n");
    out.push_str(&format!(
        "- {} closed task(s) had a person on them, {} had nobody. Nearly every task here carries a repo, so \"nobody\" reads as code.\n",
        r.mix.with_people, r.mix.without_people
    ));
    if !r.mix.people.is_empty() {
        out.push_str(&format!(
            "- With: {}\n",
            r.mix
                .people
                .iter()
                .map(|p| format!("{} ({})", p.name, p.task_count))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push_str("\nALREADY RULED OUT\n");
    if r.ruled_out.is_empty() {
        out.push_str("- No explicit ruled-out/dead-end evidence was found in stored history.\n");
    } else {
        out.push_str(&r.ruled_out.iter().map(cite).collect::<Vec<_>>().join("\n"));
        out.push('\n');
    }
    out
}

/// Turn an optional `bg_executor` parameter into the enum, strictly.
///
/// An unknown value is a **rejected call**, not a silent fallback to Claude.
/// The fallback looks kinder and is worse: `bg_executor: "codx"` would file a
/// task that reads as a normal Claude task, run under the wrong agent, and give
/// the caller no signal at all that its request was discarded.
fn parse_executor(raw: Option<&str>) -> Result<Option<crate::executor::BackgroundExecutor>, McpError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => crate::executor::BackgroundExecutor::parse(s)
            .map(Some)
            .map_err(|e| McpError::invalid_params(e, None)),
    }
}

/// Full detail for get_task / file_task confirmations.
/// Parse a tripwire spec exactly as the DB layer will, PLUS the one check only
/// this host can make: a `script:` must resolve to an executable HERE. The DB
/// layer cannot ask that — a row synced from the phone may name a script only
/// the Mac has — but an agent filing from this Mac gets told now, with the
/// path, rather than discovering an `error` state a tick later.
fn validate_tripwire_spec(raw: &str) -> Result<String, McpError> {
    let parsed = crate::tripwire::Tripwire::parse(raw)
        .map_err(|e| McpError::invalid_params(e, None))?;
    if let Some(name) = parsed.script_name() {
        crate::tripwire::script_path(name).map_err(|e| McpError::invalid_params(e, None))?;
    }
    Ok(parsed.to_string())
}

fn parse_cadence(raw: Option<&str>) -> Result<Option<Option<i64>>, McpError> {
    match raw {
        Some(r) => crate::tripwire::parse_every(r)
            .map(Some)
            .map_err(|e| McpError::invalid_params(e, None)),
        None => Ok(None),
    }
}

/// Gotcha #18's lesson applied to tripwires: a cadenced check with nothing
/// draining it reads exactly like one that is about to run.
fn runner_warning_for(t: &db::tasks::Task) -> Option<String> {
    if t.tripwire.is_some()
        && t.tripwire_every_minutes.is_some()
        && !crate::platform::drain_is_installed()
    {
        Some(
            "\nNOTE: this tripwire is cadenced but the background runner is NOT installed, so \
             nothing will evaluate it on a schedule. Turn the runner on in the app, or check \
             by hand with: /Applications/CXTasks.app/Contents/MacOS/cxtasks-runner --once\n"
                .to_string(),
        )
    } else {
        None
    }
}

/// The `tripwire:` block of a task's detail, joined from the runtime row.
fn format_tripwire(
    t: &db::tasks::Task,
    runtime: Option<&db::tripwires::Runtime>,
) -> Option<String> {
    let spec = t.tripwire.as_deref()?;
    let state = t
        .tripwire_state
        .as_deref()
        .map(crate::tripwire::TripwireState::from_db)
        .unwrap_or(crate::tripwire::TripwireState::Armed);
    let monitor = t.is_monitor();
    let state_label = match (state, monitor) {
        (crate::tripwire::TripwireState::Armed, false) => "armed (hidden until it trips)",
        (crate::tripwire::TripwireState::Armed, true) => "watching (a trip reopens the task)",
        (crate::tripwire::TripwireState::Tripped, _) => "TRIPPED",
        (crate::tripwire::TripwireState::Error, false) => "ERROR (surfaced; fix the check and re-arm)",
        (crate::tripwire::TripwireState::Error, true) => "ERROR (surfaced; fix the check, then complete the task to re-arm it)",
    };
    let mut s = format!(
        "  tripwire:   {spec} · every {} · {state_label}\n",
        crate::tripwire::format_every(t.tripwire_every_minutes)
    );
    match runtime.and_then(|r| r.checked_at.as_deref().map(|c| (c, r))) {
        Some((checked, r)) => {
            let outcome = r.last_outcome.as_deref().unwrap_or("?");
            let note = r
                .last_note
                .as_deref()
                .filter(|n| !n.trim().is_empty())
                .map(|n| format!(" — {n}"))
                .unwrap_or_default();
            s.push_str(&format!("              last check: {checked} UTC → {outcome}{note}\n"));
            if r.failures > 0 {
                s.push_str(&format!("              failures:   {} in a row\n", r.failures));
            }
        }
        None => s.push_str("              last check: never\n"),
    }
    if runtime.is_some_and(|r| r.requested_at.is_some()) {
        s.push_str("              check requested — waiting for the runner's next tick\n");
    }
    if let Some(at) = t.tripwire_tripped_at.as_deref() {
        s.push_str(&format!("              tripped at: {at} UTC\n"));
    }
    if crate::tripwire::OnTrip::from_db(t.tripwire_on_trip.as_deref())
        == crate::tripwire::OnTrip::Complete
    {
        s.push_str("              on trip:    completes the task (releases anything blocked by it)\n");
    }
    if monitor {
        s.push_str(
            "              monitor:    standing check — complete_task = handled (re-arms, re-checks \
             on the next tick); archive to retire it\n",
        );
    }
    Some(s)
}

fn format_detail(
    t: &db::tasks::Task,
    runs: &[db::runs::TaskRun],
    children: &[db::tasks::Task],
    tripwire: Option<&db::tripwires::Runtime>,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("{}\n", t.title));
    if let Some(r) = short_ref(t) {
        s.push_str(&format!("  ref:        {r}\n"));
    }
    s.push_str(&format!("  id:         {}\n", t.id));
    s.push_str(&format!("  status:     {}\n", t.status));
    s.push_str(&format!("  priority:   {}\n", priority_label(t.priority)));
    if let Some(p) = t.project.as_deref() {
        s.push_str(&format!("  project:    {p}\n"));
    }
    // These two print the WRITABLE field name, not a prettier label, and that
    // is the whole point of them. This output is read by agents (it is the MCP
    // surface, never the app), and an agent that has just read a task writes
    // back using the words the task showed it. Printing `due:` / `deferred:`
    // for fields you set as `due_at` / `defer_until` taught every reader the
    // wrong name at exactly the moment it was about to need the right one —
    // and `deferred:` landing in the same aligned column as `status:` also
    // made "deferred" read as a status value, which it has never been. Cost a
    // real silent no-op write on T93, 2026-09-19. Keep the names exact; if a
    // label here ever stops matching its `UpdateTaskParams` field, that is a
    // bug, not a cosmetic choice.
    if let Some(d) = t.due_at.as_deref() {
        s.push_str(&format!("  due_at:     {d}\n"));
    }
    if let Some(d) = t.defer_until.as_deref() {
        s.push_str(&format!("  defer_until: {d}\n"));
    }
    if let Some(r) = t.recurrence.as_deref() {
        s.push_str(&format!("  recurrence: {r}\n"));
    }
    if let Some(block) = format_tripwire(t, tripwire) {
        s.push_str(&block);
    }
    if let Some(r) = t.repo_path.as_deref() {
        s.push_str(&format!("  repo_path:  {r}\n"));
    }
    if t.bg_allowed {
        s.push_str(&format!(
            "  background: {} ({}{})\n",
            t.bg_executor.label(),
            // "shell, no writes" was never true — a shell can write, and the
            // checkpoint exists precisely because it can. Saying so here keeps
            // the MCP's description of the grant honest and identical to the
            // app's.
            match crate::runner::ToolTier::for_task(t) {
                crate::runner::ToolTier::Write => "edit workspace",
                crate::runner::ToolTier::Shell => "commands and tests",
                crate::runner::ToolTier::ReadOnly => "investigation only",
            },
            t.model
                .as_deref()
                .map(|m| format!(", model {m}"))
                .unwrap_or_else(|| match t.bg_executor {
                    crate::executor::BackgroundExecutor::Codex => ", Codex default model".into(),
                    crate::executor::BackgroundExecutor::Claude => String::new(),
                })
        ));
        // `get_task` IS a background tool, so a run that re-reads its own task
        // should be able to see it was dispatched as an audit rather than as
        // the work. Read-only: intent is deliberately NOT settable over MCP.
        if t.bg_intent.is_review() {
            s.push_str("  next run:   review\n");
        }
        // Only when overridden. Printing the effective value on every task
        // would make the global default look like a per-task decision, and the
        // whole point of the line is that this task is not on the default.
        if t.bg_timeout_minutes.is_some() || t.bg_run_token_cap.is_some() {
            let mut parts: Vec<String> = Vec::new();
            if let Some(m) = t.bg_timeout_minutes {
                parts.push(format!("{m} min"));
            }
            if let Some(c) = t.bg_run_token_cap {
                parts.push(match c {
                    0 => "no token ceiling".to_string(),
                    n => format!("{}M tokens", n / 1_000_000),
                });
            }
            s.push_str(&format!("  budget:     {} (overridden)\n", parts.join(" · ")));
        }
    }
    s.push_str(&format!("  created:    {}\n", t.created_at));
    if let Some(c) = t.completed_at.as_deref() {
        s.push_str(&format!("  completed:  {c}\n"));
    }
    // Same rule as due_at/defer_until above: name the field you write it back
    // with. "Notes" is the friendlier word and is what the app calls it, but
    // nothing human reads this string, and an agent that sees "Notes:" reaches
    // for `notes` — which update_task does not have.
    if let Some(body) = t.body.as_deref().filter(|b| !b.trim().is_empty()) {
        s.push_str(&format!("\nNotes (write as `body`):\n{body}\n"));
    }
    if let Some(p) = t.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
        s.push_str(&format!("\nSeed prompt (write as `prompt`):\n{p}\n"));
    }
    if !children.is_empty() {
        s.push_str("\nSubtasks:\n");
        for c in children {
            s.push_str(&format!("  - [{}] {} (id: {})\n", c.status, c.title, c.id));
        }
    }
    if !runs.is_empty() {
        s.push_str("\nRuns:\n");
        for r in runs.iter().take(5) {
            s.push_str(&format!(
                "  - {}{} {} started {}{}\n",
                r.mode,
                if r.intent.is_review() { " (review)" } else { "" },
                r.status,
                r.started_at,
                r.finished_at
                    .as_deref()
                    .map(|f| format!(", finished {f}"))
                    .unwrap_or_default()
            ));
            if let Some(text) = r.result_text.as_deref().filter(|t| !t.trim().is_empty()) {
                let preview: String = text.chars().take(400).collect();
                s.push_str(&format!("    result: {preview}\n"));
            }
        }
    }
    s
}

// ─── GSD board rendering ──────────────────────────────────────────────────

/// How long ago a bookkeeping timestamp was, compactly: `3h`, `2d`, `5w`.
///
/// Bookkeeping timestamps are space-separated UTC with no `Z`, so they are
/// parsed by SQLite rather than by Rust date maths — `julianday()` handles both
/// formats and sidesteps the "never compare these byte-wise" rule entirely.
fn age_label(conn: &rusqlite::Connection, created_at: &str) -> String {
    let days: f64 = conn
        .query_row(
            "SELECT julianday('now') - julianday(?1)",
            rusqlite::params![created_at],
            |r| r.get(0),
        )
        .unwrap_or(0.0_f64)
        .max(0.0);
    let hours = days * 24.0;
    if hours < 1.0 {
        "just now".to_string()
    } else if hours < 24.0 {
        format!("{}h ago", hours.floor() as i64)
    } else if days < 7.0 {
        format!("{}d ago", days.floor() as i64)
    } else {
        format!("{}w ago", (days / 7.0).floor() as i64)
    }
}

/// Health as a word, with `never reported` for NULL — deliberately not rendered
/// as anything that reads like "fine".
fn health_label(health: Option<&str>) -> &str {
    health.unwrap_or("never reported")
}

fn truncate(s: &str, max: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    format!("{}…", one_line.chars().take(max).collect::<String>())
}

fn format_board_line(conn: &rusqlite::Connection, row: &db::projects::BoardRow) -> String {
    let mut bits = vec![format!(
        "{} actionable, {} planned, {} done this wk",
        row.actionable_count, row.planned_count, row.done_this_week
    )];
    // Ahead of everything else: a finished run nobody has looked at is the one
    // thing on this board the reader does not already know.
    if row.in_review_count > 0 {
        bits.insert(0, format!("{} WAITING ON YOU", row.in_review_count));
    }
    if row.blocked_count > 0 {
        bits.push(format!("{} blocked", row.blocked_count));
    }
    if let Some(m) = &row.next_milestone {
        bits.push(format!(
            "next: {}{} ({}/{} open)",
            m.name,
            m.target_date
                .as_deref()
                .map(|d| format!(" due {d}"))
                .unwrap_or_default(),
            m.open_count,
            m.total_count
        ));
    }
    // A written update is garnish now, not the headline: it is reported when it
    // exists and its absence is silent. The old line shouted NO UPDATE EVER
    // POSTED, which described a reporting habit rather than the project, and
    // read as an accusation on a board where nothing was actually wrong.
    if let Some(u) = &row.last_update {
        bits.push(format!(
            "last update {}: {}",
            age_label(conn, &u.created_at),
            truncate(&u.body, 80)
        ));
    }
    if let Some(title) = row.next_task_title.as_deref() {
        let reference = row
            .next_task_short_id
            .map(|n| format!("[T{n}] "))
            .unwrap_or_default();
        bits.push(format!("do next: {reference}{title}"));
    }
    format!(
        "{} — {} · {}",
        activity_label(row),
        row.project,
        bits.join(" · ")
    )
}

/// The leading marker on a board line: what the CLOCK says, not what anyone
/// claimed. `health` still prints when it has been set, because a deliberate
/// "at_risk" outranks "something moved yesterday" — but it is never invented,
/// so a project nobody reports on is described rather than judged.
///
/// **The same [`db::projects::ActivityState`] gets different WORDS on an area
/// than on a project**, because it is not the same fact. A finite project that
/// hasn't moved in a month is stalled; an area that hasn't is just quiet, and
/// there is nothing to fix. One shared vocabulary would have to pick one of
/// those readings and be wrong about the other half of the board — which is the
/// dilution v19 exists to end, arriving in the label instead of the counts.
fn activity_label(row: &db::projects::BoardRow) -> String {
    use db::projects::ActivityState::*;
    let days = |word: &str| match row.idle_days {
        Some(d) => format!("{word} {d}d"),
        None => word.to_string(),
    };
    let state = match (row.activity, row.kind.is_area()) {
        (Active, _) => "active".to_string(),
        (Empty, _) => "empty".to_string(),
        // An area: quiet either way, and quiet is not a verdict.
        (Idle | Dormant, true) => days("quiet"),
        (Idle, false) => days("idle"),
        (Dormant, false) => days("stalled"),
    };
    match row.health.as_deref() {
        Some(h) => format!("[{state} · {h}]"),
        None => format!("[{state}]"),
    }
}

/// The board, areas first with their nested projects indented underneath.
///
/// Flat output would undo the split: the whole point of filing a certification
/// wave into its own project is that the reader can see it is *inside*
/// CXVentures without it being *mixed into* CXVentures, and a flat list says
/// neither. Standalone projects — finite work belonging to no area — follow at
/// the end under their own heading, so an unhomed project is visible rather than
/// silently sorted in among the areas.
fn format_board(conn: &rusqlite::Connection, rows: &[db::projects::BoardRow]) -> String {
    let mut out: Vec<String> = Vec::new();
    // Areas nest to any depth, so this is a tree walk from the top-level areas,
    // one `└` step per level. `seen` stops a hand-edited parent cycle from
    // recursing forever, and means a node is printed exactly once.
    fn walk(
        conn: &rusqlite::Connection,
        rows: &[db::projects::BoardRow],
        node: &db::projects::BoardRow,
        depth: usize,
        seen: &mut Vec<String>,
        out: &mut Vec<String>,
    ) {
        if seen.iter().any(|s| s.eq_ignore_ascii_case(&node.project)) {
            return;
        }
        seen.push(node.project.clone());
        let line = format_board_line(conn, node);
        out.push(if depth == 0 { line } else { format!("{}└ {line}", "    ".repeat(depth)) });
        for child in rows
            .iter()
            .filter(|c| c.parent.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(&node.project)))
        {
            walk(conn, rows, child, depth + 1, seen, out);
        }
    }
    let mut seen: Vec<String> = Vec::new();
    for row in rows.iter().filter(|r| r.kind.is_area() && r.parent.is_none()) {
        walk(conn, rows, row, 0, &mut seen, &mut out);
    }
    let orphans: Vec<&db::projects::BoardRow> = rows
        .iter()
        .filter(|r| !r.kind.is_area() && r.parent.is_none())
        .collect();
    if !orphans.is_empty() {
        out.push("
Projects with no area:".to_string());
        for row in orphans {
            out.push(format_board_line(conn, row));
        }
    }
    out.join("\n")
}

fn format_project_detail(
    row: &db::projects::BoardRow,
    updates: &[db::project_updates::ProjectUpdate],
    sections: &[db::projects::Section],
    children: &[&db::projects::BoardRow],
) -> String {
    let mut s = format!("{}\n", row.project);
    s.push_str(&format!(
        "  kind:     {}{}\n",
        row.kind.as_str(),
        match (row.parent.as_deref(), row.closed_at.as_deref()) {
            (Some(p), Some(c)) => format!(" in '{p}' · CLOSED {c}"),
            (Some(p), None) => format!(" in '{p}'"),
            (None, Some(c)) => format!(" · CLOSED {c}"),
            (None, None) => String::new(),
        }
    ));
    // `target_date:`, not `target:` — same rule as `format_detail`'s labels:
    // this output is read by agents, and the label is the name they will write
    // back with. `update_project` takes `target_date`, so `target:` sent a
    // reader to a field that does not exist. Missed when `format_detail` was
    // swept, and found by an adversarial review rather than by the test, which
    // only ever looked at `format_detail`.
    if let Some(d) = row.target_date.as_deref() {
        s.push_str(&format!("  target_date: {d}\n"));
    }
    s.push_str(&format!("  health:   {}\n", health_label(row.health.as_deref())));
    s.push_str(&format!(
        "  tasks:    {} open, {} blocked, {} done in the last 7 days\n",
        row.open_count, row.blocked_count, row.done_this_week
    ));
    if let Some(title) = row.next_task_title.as_deref() {
        let reference = row
            .next_task_short_id
            .map(|n| format!("[T{n}] "))
            .unwrap_or_default();
        s.push_str(&format!("  do next:  {reference}{title}\n"));
    }

    // An area's counts already include these, so listing them is what makes the
    // number legible: "33 open" over a bucket is noise until you can see which
    // of it is a finite wave with its own finish line.
    if !children.is_empty() {
        s.push_str("\nProjects inside it:\n");
        for c in children {
            s.push_str(&format!(
                "  - {} — {} open{}{}\n",
                c.project,
                c.open_count,
                c.target_date
                    .as_deref()
                    .map(|d| format!(", target {d}"))
                    .unwrap_or_default(),
                if c.closed_at.is_some() { " ✓ closed" } else { "" }
            ));
        }
    }

    let milestones: Vec<&db::projects::Section> =
        sections.iter().filter(|s| s.target_date.is_some()).collect();
    if milestones.is_empty() {
        s.push_str("\nMilestones: none (no section carries a target date)\n");
    } else {
        s.push_str("\nMilestones:\n");
        for m in milestones {
            s.push_str(&format!(
                "  - {} — due {}{}\n",
                m.name,
                m.target_date.as_deref().unwrap_or("?"),
                if m.completed_at.is_some() { "  ✓ met" } else { "" }
            ));
        }
    }

    if updates.is_empty() {
        s.push_str(
            "\nUpdates: none. Nothing has ever been written down about this project, \
             which is why its health reads as never reported.\n",
        );
    } else {
        s.push_str("\nUpdates (newest first):\n");
        for u in updates {
            s.push_str(&format!(
                "  [{}] {}{}: {}\n",
                u.created_at,
                u.author,
                u.health.as_deref().map(|h| format!(" → {h}")).unwrap_or_default(),
                u.body
            ));
        }
    }
    s
}

#[tool_router]
impl CxTasksMcp {
    pub fn new(db_path: String) -> Self {
        Self::with_mode(db_path, McpMode::from_env())
    }

    /// Build the server with the tool surface its caller is entitled to.
    ///
    /// Background mode keeps only [`BACKGROUND_TOOLS`] and **removes** the rest
    /// from the router, rather than leaving them present and refusing calls.
    /// The difference is the whole point: a removed tool is not in
    /// `tools/list`, so it costs nothing in context and — more importantly —
    /// never tells an unattended agent that a capability is nearby. That is the
    /// same reasoning `--strict-mcp-config` already applies to the user's other
    /// MCP servers (an unattended run should not learn that cxmail can send
    /// email), applied one level down to this server's own tools.
    ///
    /// The split is taken once here rather than checked per call, so there is
    /// exactly one place that decides, and `list_tools` and `call_tool` cannot
    /// disagree about what exists.
    pub fn with_mode(db_path: String, mode: McpMode) -> Self {
        let mut tool_router = Self::tool_router();
        let drop: Vec<String> = tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .filter(|name| !mode.keeps(name))
            .collect();
        for name in &drop {
            tool_router.remove_route(name);
        }
        if mode.is_unattended() {
            tracing::info!(
                "cxtasks-mcp: {mode:?} mode — {} tool(s) available, {} removed",
                tool_router.list_all().len(),
                drop.len()
            );
        }
        Self {
            tool_router,
            db_path,
            mode,
            claimed: Arc::new(AtomicBool::new(false)),
            client_name: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    #[tool(
        name = "file_task",
        description = "File a todo into CXTasks so it survives this session. Two params carry almost all the value and only YOU can supply them: `repo_path` (your current working directory — the MCP server cannot see it) and `prompt` (a seed prompt for the agent that picks this up later, which will have none of this conversation's context). Write a real seed prompt whenever repo_path is set; without it the next session starts over instead of starting work. Everything else is optional, and a task with no repo_path is a perfectly good plain todo."
    )]
    async fn file_task(
        &self,
        Parameters(params): Parameters<FileTaskParams>,
    ) -> Result<CallToolResult, McpError> {
        // WHY NOT NOW? — the one gate on a self-initiated filing (invariant 28).
        //
        // Checked before anything touches the database, and checked HERE rather
        // than by making `why` a plain `String`: a missing required field would
        // then surface as rmcp's generic "missing field `why`", which names no
        // values and asks no question. An empty string passes schema validation
        // and is treated as missing for the same reason.
        let why = match params.why.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(w) if FILING_REASONS.contains(&w) => w.to_string(),
            Some(other) => {
                return Err(McpError::invalid_params(
                    format!(
                        "why=\"{other}\" is not one of chris-asked, wont-fit, decision, \
                         other-repo. Chris's question for a self-initiated filing is \
                         \"why not now?\" — if the answer is \"it's small\" or \"so it \
                         isn't lost\", do it now instead of filing."
                    ),
                    None,
                ));
            }
            None => {
                return Err(McpError::invalid_params(
                    "file_task needs `why`: chris-asked (Chris told you to file it in this \
                     session), or for a self-initiated filing your answer to \"why not \
                     now?\" — wont-fit, decision or other-repo. If the answer is \"it's \
                     small\" or \"so it isn't lost\", do it now instead of filing."
                        .to_string(),
                    None,
                ));
            }
        };
        let self_initiated = why != WHY_CHRIS_ASKED;

        let repo_path = match params.repo_path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => Some(validate_repo_path(raw)?),
            None => None,
        };

        // Guard the combination that would otherwise sit in the queue forever.
        //
        // A DETACHED task is the deliberate exception, and it is an explicit
        // branch rather than a relaxed check: invariant 12 still refuses a
        // background run outside a declared workspace, and a container mount
        // set is one where `$HOME` is not. So no repo is legal only when
        // somebody has said `detached` out loud — never as a consequence of
        // leaving `repo_path` off, which is what filing from the wrong
        // directory looks like.
        if params.bg_allowed.unwrap_or(false) && repo_path.is_none() && !params.detached {
            return Err(McpError::invalid_params(
                "bg_allowed requires repo_path — a background run needs a working directory. \
                 (A repo-less task is possible, but it must say `detached: true` and can only \
                 run on a host that declares a detached workspace.)"
                    .to_string(),
                None,
            ));
        }
        if params.detached && repo_path.is_some() {
            return Err(McpError::invalid_params(
                "a detached task must not carry a repo_path — it would run against an empty \
                 scratch directory and report that the code does not exist"
                    .to_string(),
                None,
            ));
        }
        // `queued` is the runner's claim key, not a filing state — entering it
        // here would skip every check `queue_background` runs first (invariant
        // 16). Refuse with the remedy named, the `cancelled` precedent: the
        // two-step keeps queueing an explicit second decision, and routes it
        // through the shared guard path (`dispatch::queue_for_background`).
        if params.status.as_deref().and_then(db::tasks::normalize_status) == Some("queued") {
            return Err(McpError::invalid_params(
                "'queued' can't be set at filing time — file the task, then queue it with \
                 update_task(status: 'queued'), which runs the same checks as the app's \
                 queue action."
                    .to_string(),
                None,
            ));
        }
        // Strict: a typo'd executor must be a rejected call, not a task that is
        // filed looking fine and then never picked up by anything.
        let bg_executor = parse_executor(params.bg_executor.as_deref())?;
        // A tripwire is validated HERE for existence on this host, and only
        // here — see `validate_tripwire_spec`.
        let tripwire = match params.tripwire.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => Some(validate_tripwire_spec(raw)?),
            None => None,
        };
        let tripwire_every = parse_cadence(params.tripwire_check_every.as_deref())?.flatten();

        // Resolve the project from the DIRECTORY, not from its name. The caller
        // knows a cwd; only the database knows that `~/Projects/finance-app` is
        // pocketbuddy. Guessing from the basename — which is what the parameter
        // docs used to ask for — manufactures a project per repo whose folder
        // doesn't match its product name, permanently split from the real one.
        //
        // An explicit `project` still wins: a task can legitimately be filed
        // against a repo while belonging to another project. `owner` is kept so
        // the reply can SAY that happened rather than silently picking a side.
        let given = params.project.as_deref().map(str::trim).filter(|s| !s.is_empty());
        // Whether the CALLER named a workspace, captured before `repo_path` moves
        // into `NewTask` — `create` fills a blank one from the project's primary
        // repo, and the reply has to be able to tell the two apart.
        let repo_was_given = repo_path.is_some();
        let owner = match repo_path.as_deref() {
            Some(path) => {
                let conn = self.open_db()?;
                db::projects::for_repo_path(&conn, path)
                    .map_err(|e| McpError::internal_error(format!("{e}"), None))?
            }
            None => None,
        };
        let project = given.map(str::to_string).or_else(|| owner.clone());

        let new = db::tasks::NewTask {
            // Server-side creation: let the DB mint the id.
            id: None,
            title: params.title,
            body: params.body,
            status: params.status,
            priority: params.priority,
            project,
            due_at: params.due_at,
            defer_until: params.defer_until,
            recurrence: params.recurrence,
            parent_id: None,
            repo_path,
            prompt: params.prompt,
            bg_allowed: params.bg_allowed,
            bg_executor,
            bg_writes_allowed: params.bg_writes_allowed,
            bg_shell_allowed: params.bg_shell_allowed,
            // Remote eligibility is never inferred from a filing. It is a
            // routing decision, made deliberately — `update_task` sets it.
            bg_remote_allowed: None,
            bg_mcp_servers: params.bg_mcp_servers.clone(),
            model: params.model.clone(),
            section_id: None,
            // Not a `file_task` parameter: filing something as already-blocked
            // is a state you reach by editing, not by capturing.
            blocked_reason: None,
            // Out-of-range values are refused by `db::tasks::create` with the
            // legal range named, rather than clamped here — an agent that
            // asked for 60000 minutes and got 120 would never find out.
            bg_timeout_minutes: params.bg_timeout_minutes,
            bg_run_token_cap: params.bg_run_token_cap,
            tripwire,
            tripwire_every_minutes: tripwire_every,
            tripwire_on_trip: params.tripwire_on_trip.clone(),
        };

        if let Some(rule) = new.recurrence.as_deref() {
            crate::recurrence::Recurrence::parse(rule).map_err(|e| {
                McpError::invalid_params(format!("invalid recurrence '{rule}': {e}"), None)
            })?;
        }

        let task = {
            let conn = self.open_db()?;
            // BEFORE the row exists: a refused tag must not leave an untagged
            // task behind for the agent's retry to duplicate.
            if let Some(tags) = params.tags.as_ref() {
                vet_tags(&conn, tags, params.new_tags.unwrap_or(false))?;
            }
            db::tasks::create(&conn, &new)
                .map_err(|e| McpError::invalid_params(format!("{e}"), None))?
        };

        {
            let conn = self.open_db()?;
            if let Some(tags) = params.tags.as_ref() {
                for name in tags {
                    db::tags::attach(&conn, &task.id, name)
                        .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
                }
            }
            if params.decision.unwrap_or(false) {
                db::tags::attach(&conn, &task.id, db::tasks::DECISION_TAG)
                    .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
            }
            if self_initiated {
                // The persisted trace of the reason (invariant 28). `attach` is
                // an upsert plus INSERT OR IGNORE, so a caller who also passed
                // the tag by hand is harmless; `tags`/`task_tags` are synced
                // tables, so the mark travels with the row.
                db::tags::attach(&conn, &task.id, AGENT_FILED_TAG)
                    .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
            }
            if let Some(blockers) = params.blocked_by.as_ref() {
                for b in blockers {
                    // T-numbers accepted here too, which is what makes
                    // `/breakdown` workable: it files tasks in topological order
                    // and names each new task's blockers by the T-numbers the
                    // previous `file_task` calls just printed.
                    let blocker_id = self.resolve_task_id(b)?;
                    db::deps::add(&conn, &task.id, &blocker_id)
                        .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
                }
            }
            // Two steps rather than passing a `section_id` into the INSERT: the
            // caller names a section by NAME, and resolving that name to an id
            // means creating it when it doesn't exist. `NewTask.section_id`
            // stays None and this moves the task afterwards.
            if let Some(section) = params
                .section
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let Some(project) = task.project.as_deref() else {
                    return Err(McpError::invalid_params(
                        "`section` requires `project` — a section is a subdivision of a \
                         project, so there's nothing to file it under"
                            .to_string(),
                        None,
                    ));
                };
                let s = db::projects::upsert_section(&conn, Some(project), section, None)
                    .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
                db::projects::move_task_to_section(&conn, &task.id, Some(&s.id))
                    .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
            }
        }

        notify(
            &Envelope::new("task-created", &task.id)
                .with_tool("file_task")
                .with_summary(&task.title),
        )
        .await;

        let tripwire_rt = if task.tripwire.is_some() {
            let conn = self.open_db()?;
            db::tripwires::for_task(&conn, &task.id).unwrap_or_default()
        } else {
            None
        };
        let mut out = format!(
            "Filed task.\n\n{}",
            format_detail(&task, &[], &[], tripwire_rt.as_ref())
        );
        if self_initiated {
            // `format_detail` renders no tags, so without this line the reason
            // is invisible in the very transcript that filed it.
            out.push_str(&format!(
                "\nSelf-filed (why: {why}) — tagged {AGENT_FILED_TAG}; review with \
                 search_tasks(tags: [\"{AGENT_FILED_TAG}\"]).\n"
            ));
        }
        if task.tripwire_armed() {
            out.push_str(
                "\nArmed: the task is hidden from Open, Today and next_task until the tripwire \
                 trips; it sits in Waiting meanwhile.\n",
            );
        }
        if let Some(w) = runner_warning_for(&task) {
            out.push_str(&w);
        }
        match (given, owner.as_deref()) {
            // Filed against a repo that belongs to a different project. Not an
            // error — cross-project work is real — but silence here is how a
            // task ends up somewhere nobody looks.
            (Some(g), Some(o)) if !g.eq_ignore_ascii_case(o) => out.push_str(&format!(
                "\nNote: filed under '{g}', but that working directory is registered to \
                 '{o}'. Kept '{g}' because you asked for it — if that was a guess from the \
                 folder name, '{o}' is the project this repo actually belongs to.\n"
            )),
            // Resolved silently and correctly — say so, so the caller learns the
            // directory did the work and stops guessing next time.
            (None, Some(o)) => out.push_str(&format!(
                "\nFiled into '{o}', resolved from the working directory.\n"
            )),
            _ => {}
        }
        // The mapping resolving the OTHER way. Said out loud for the same reason
        // as the note above: a workspace that appeared without the caller asking
        // is a fact worth one line, not a surprise to discover at dispatch.
        if !repo_was_given {
            if let Some(inherited) = task.repo_path.as_deref() {
                out.push_str(&format!(
                    "\nWorkspace set to '{inherited}', inherited from the project's registered \
                     repo. Pass repo_path if the work is somewhere else.\n"
                ));
            }
        }
        // A dispatchable task with no seed prompt is the common miss: it looks
        // complete but the agent that picks it up gets only the title.
        if task.repo_path.is_some()
            && task.prompt.as_deref().map(str::trim).unwrap_or("").is_empty()
        {
            out.push_str(
                "\nNote: no seed prompt was stored, so a future session will get only the \
                 title and notes. If you know the files, the failure mode, or what you already \
                 ruled out, call update_task with a `prompt` — that context is the difference \
                 between the next agent working and re-investigating.\n",
            );
        }
        if task.repo_path.is_none() {
            out.push_str(
                "\nNote: no repo_path was stored. Foreground dispatch (Open in Claude / Copy \
                 Seed Prompt) still works — it falls back to the user's home directory with no \
                 project scope — but this task can NOT be run unattended in the background \
                 (bg_allowed requires an explicit repo_path, unless the task is detached). \
                 Pass repo_path (your cwd) for project-scoped dispatch or background \
                 eligibility.\n",
            );
        }
        // Last, so it is the line an agent reads right before moving on: what
        // the pane will drop from the notes it just wrote (`crate::prose`).
        if let Some(body) = task.body.as_deref() {
            out.push_str(&crate::prose::render_note(body, crate::prose::Channel::Notes));
        }
        Self::text(out)
    }

    #[tool(
        name = "list_tasks",
        description = "List tasks from a CXTasks view: inbox, today (due or overdue, plus any open `decision` for Chris; held tasks stay hidden until released), waiting (the one 'not yet' list — held back by a start date, an armed tripwire, an open blocker or a written blocked reason; oldest first), open (actionable right now — everything not held, future deadlines included), all (every open task, held ones included), in_review, or done. Optionally filter by project or status."
    )]
    async fn list_tasks(
        &self,
        Parameters(params): Parameters<ListTasksParams>,
    ) -> Result<CallToolResult, McpError> {
        let view = parse_view(params.view.as_deref())?;
        let conn = self.open_db()?;
        let tasks = db::tasks::list(
            &conn,
            &db::tasks::ListFilters {
                view: Some(view),
                project: params.project.clone(),
                status: params.status.clone(),
                tags: params.tags.clone(),
                hide_blocked: params.hide_blocked,
                include_subtasks: None,
                limit: Some(params.limit.unwrap_or(30).min(200)),
            },
        )
        .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;

        if tasks.is_empty() {
            return Self::text("No tasks match.".to_string());
        }
        let body = tasks.iter().map(format_line).collect::<Vec<_>>().join("\n");
        Self::text(format!("{} task(s):\n{body}", tasks.len()))
    }

    #[tool(
        name = "get_task",
        description = "Read one task in full: notes, seed prompt, execution context, subtasks and run history."
    )]
    async fn get_task(
        &self,
        Parameters(params): Parameters<GetTaskParams>,
    ) -> Result<CallToolResult, McpError> {
        let task_id = self.resolve_task_id(&params.task_id)?;
        let conn = self.open_db()?;
        let task = db::tasks::get(&conn, &task_id)
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?
            .ok_or_else(|| {
                McpError::invalid_params(format!("task {} not found", params.task_id), None)
            })?;
        let runs = db::runs::list_for_task(&conn, &task.id).unwrap_or_default();
        let children = db::tasks::list_children(&conn, &task.id).unwrap_or_default();
        let tripwire_rt = if task.tripwire.is_some() {
            db::tripwires::for_task(&conn, &task.id).unwrap_or_default()
        } else {
            None
        };
        let mut out = format_detail(&task, &runs, &children, tripwire_rt.as_ref());

        let tags = db::tags::for_task(&conn, &task.id).unwrap_or_default();
        if !tags.is_empty() {
            out.push_str(&format!(
                "\nTags: {}\n",
                tags.iter().map(|t| format!("#{}", t.name)).collect::<Vec<_>>().join(" ")
            ));
        }
        let blockers = db::deps::blocked_by(&conn, &task.id).unwrap_or_default();
        if !blockers.is_empty() {
            out.push_str("\nBlocked by:\n");
            for b in &blockers {
                out.push_str(&format!(
                    "  - [{}] {} (id: {}){}\n",
                    b.status,
                    b.title,
                    b.task_id,
                    if b.is_open { "  ← still open" } else { "" }
                ));
            }
        }
        let blocks = db::deps::blocking(&conn, &task.id).unwrap_or_default();
        if !blocks.is_empty() {
            out.push_str("\nBlocking:\n");
            for b in &blocks {
                out.push_str(&format!("  - [{}] {} (id: {})\n", b.status, b.title, b.task_id));
            }
        }
        let supersedes = db::relations::supersedes(&conn, &task.id).unwrap_or_default();
        if !supersedes.is_empty() {
            out.push_str("\nSupersedes:\n");
            for edge in &supersedes {
                out.push_str(&format!(
                    "  - [{}] {} (ref: {})\n",
                    edge.status,
                    edge.title,
                    edge_ref(edge)
                ));
            }
        }
        let superseded_by = db::relations::superseded_by(&conn, &task.id).unwrap_or_default();
        if !superseded_by.is_empty() {
            out.push_str("\nSuperseded by:\n");
            for edge in &superseded_by {
                out.push_str(&format!(
                    "  - [{}] {} (ref: {})  ← replacement; this task is historical\n",
                    edge.status,
                    edge.title,
                    edge_ref(edge)
                ));
            }
        }
        let related = db::relations::related(&conn, &task.id).unwrap_or_default();
        if !related.is_empty() {
            out.push_str("\nRelated:\n");
            for edge in &related {
                out.push_str(&format!(
                    "  - [{}] {} (ref: {})\n",
                    edge.status,
                    edge.title,
                    edge_ref(edge)
                ));
            }
        }
        // **Bounded, and it says what it dropped.** This was the one unbounded
        // region of the tool's output — the run list is already `take(5)` at 400
        // chars apiece — and `get_task` is one of `BACKGROUND_TOOLS`, so an
        // unattended run pulls it too. One real task returned 33k characters of
        // comments here.
        //
        // Newest `DETAIL_COMMENTS`, rendered oldest-first WITHIN that window
        // (`dispatch::history_block`'s shape), so the block still reads
        // chronologically while keeping the entries that matter. The omission
        // line goes FIRST: at the bottom it reads as a footnote to the last
        // comment rather than as a statement about the whole list.
        let comments = db::comments::for_task(&conn, &task.id).unwrap_or_default();
        if !comments.is_empty() {
            out.push_str("\nComments:\n");
            let skip = comments.len().saturating_sub(DETAIL_COMMENTS);
            if skip > 0 {
                out.push_str(&format!(
                    "  ({skip} older comment(s) omitted — the full log is on the task in the app.)\n"
                ));
            }
            for c in comments.iter().skip(skip) {
                let body = if c.body.chars().count() > DETAIL_COMMENT_CHARS {
                    let head: String = c.body.chars().take(DETAIL_COMMENT_CHARS).collect();
                    format!("{head}\n    […truncated — the comment continues in the app…]")
                } else {
                    c.body.clone()
                };
                out.push_str(&format!("  [{}] {}: {}\n", c.created_at, c.author, body));
            }
        }
        Self::text(out)
    }

    #[tool(
        name = "update_task",
        description = "Change fields on an existing task. Only the fields you pass are modified. Use complete_task rather than status='done' when finishing work — it also materializes the next instance of a recurring task."
    )]
    async fn update_task(
        &self,
        Parameters(params): Parameters<UpdateTaskParams>,
    ) -> Result<CallToolResult, McpError> {
        let repo_path = match params.repo_path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => Some(Some(validate_repo_path(raw)?)),
            None => None,
        };
        if let Some(rule) = params.recurrence.as_deref().filter(|r| !r.trim().is_empty()) {
            crate::recurrence::Recurrence::parse(rule).map_err(|e| {
                McpError::invalid_params(format!("invalid recurrence '{rule}': {e}"), None)
            })?;
        }

        // `status: "queued"` is the queue ACTION, not a field write. The DB
        // layer accepts every real status (`normalize_status`), and
        // `USER_STATUSES` is enforced only by the app's dropdown — so a raw
        // pass-through here was a third route into the drain queue that ran
        // none of `queue_background`'s checks (invariant 16; gotcha #18's
        // every-entry-point lesson). It routes through
        // `dispatch::queue_for_background` instead, the same guard path as the
        // app's queue button. Detected via `normalize_status` so "QUEUED" and
        // " queued " don't slip past as raw text.
        let queue_requested =
            params.status.as_deref().and_then(db::tasks::normalize_status) == Some("queued");

        // An empty string CLEARS the tripwire — the one field on this tool
        // with a clear gesture, because "take the condition off" is a real
        // edit and `-1` would be nonsense for a string.
        let tripwire = match params.tripwire.as_deref().map(str::trim) {
            Some("") => Some(None),
            Some(raw) => Some(Some(validate_tripwire_spec(raw)?)),
            None => None,
        };
        let tripwire_every = parse_cadence(params.tripwire_check_every.as_deref())?;

        // Computed before the patch takes the body. Only a body this call
        // WROTE is checked — a status flip must not re-lecture an agent about
        // notes somebody else wrote.
        let render = params
            .body
            .as_deref()
            .map(|b| crate::prose::render_note(b, crate::prose::Channel::Notes))
            .unwrap_or_default();
        let patch = db::tasks::TaskPatch {
            title: params.title,
            body: params.body.map(Some),
            status: if queue_requested { None } else { params.status },
            priority: params.priority,
            project: params.project.map(Some),
            due_at: params.due_at.map(Some),
            defer_until: params.defer_until.map(Some),
            recurrence: params.recurrence.map(Some),
            sort_order: None,
            repo_path,
            prompt: params.prompt.map(Some),
            bg_allowed: params.bg_allowed,
            bg_executor: parse_executor(params.bg_executor.as_deref())?,
            bg_writes_allowed: params.bg_writes_allowed,
            bg_shell_allowed: params.bg_shell_allowed,
            bg_remote_allowed: params.bg_remote_allowed,
            bg_mcp_servers: params.bg_mcp_servers.clone(),
            model: params.model.map(Some),
            section_id: None,
            blocked_reason: params.blocked_reason.map(Some),
            // Deliberately NOT a parameter on `update_task`, and not on
            // `file_task` either. Nothing is reviewable at filing time, so the
            // only value an agent could set is one `queue_background` refuses —
            // and a background run able to arm a review on its own task closes
            // exactly the loop invariant 18 exists to prevent. `update_task`
            // isn't in `BACKGROUND_TOOLS` today, so keeping the parameter off
            // leaves that two changes away rather than one. Every parameter
            // description is also paid on every interactive session, forever.
            bg_intent: None,
            // `-1` is the CLEAR sentinel — the one thing a plain `Option<i64>`
            // cannot otherwise express, since JSON `null` and an omitted key
            // both deserialize to `None` ("leave alone"). The same sentinel for
            // both fields on purpose: `0` could not serve, because it is a
            // legal value for `bg_run_token_cap` (it disables the ceiling), and
            // two different clear values in one tool is a trap.
            bg_timeout_minutes: clear_or_set(params.bg_timeout_minutes),
            bg_run_token_cap: clear_or_set(params.bg_run_token_cap),
            tripwire,
            tripwire_every_minutes: tripwire_every,
            tripwire_on_trip: params.tripwire_on_trip.map(Some),
            // The only state a caller may write — `db::tasks::update` refuses
            // anything else, and refuses this from `armed`.
            tripwire_state: if params.rearm_tripwire == Some(true) {
                Some(crate::tripwire::TripwireState::Armed.as_str().to_string())
            } else {
                None
            },
            tripwire_check_requested: params.check_tripwire.filter(|v| *v),
        };

        let task_id = self.resolve_task_id(&params.task_id)?;

        if queue_requested {
            // Ordinary edits in the same call are applied FIRST, so a
            // `repo_path` passed alongside the queue request is what the
            // guards see. Skipped when the call carries nothing else — an
            // empty patch would still bump `updated_at` for no reason.
            let has_field_edits = patch.title.is_some()
                || patch.body.is_some()
                || patch.priority.is_some()
                || patch.project.is_some()
                || patch.due_at.is_some()
                || patch.defer_until.is_some()
                || patch.recurrence.is_some()
                || patch.repo_path.is_some()
                || patch.prompt.is_some()
                || patch.bg_allowed.is_some()
                || patch.bg_executor.is_some()
                || patch.bg_writes_allowed.is_some()
                || patch.bg_shell_allowed.is_some()
                || patch.model.is_some()
                || patch.blocked_reason.is_some()
                || patch.tripwire.is_some()
                || patch.tripwire_every_minutes.is_some()
                || patch.tripwire_on_trip.is_some()
                || patch.tripwire_state.is_some()
                || patch.tripwire_check_requested.is_some();
            if has_field_edits {
                let conn = self.open_db()?;
                db::tasks::update(&conn, &task_id, &patch)
                    .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
            }

            let outcome = crate::dispatch::queue_for_background(
                |f| {
                    let conn = self.open_db_raw()?;
                    f(&conn)
                },
                &task_id,
            )
            .map_err(|e| {
                // The refusal must say what DID happen: with edits in the
                // same call the task has already moved, and "nothing changed"
                // would be the wrong thing for the caller to believe.
                let prefix = if has_field_edits {
                    "The field edits were applied, but the task was NOT queued"
                } else {
                    "Not queued"
                };
                McpError::invalid_params(format!("{prefix}: {e}"), None)
            })?;

            notify(&Envelope::new("task-updated", &outcome.task.id).with_tool("update_task"))
                .await;

            let mut out = format!(
                "Queued for {}.\n\n{}",
                outcome.task.bg_executor.label(),
                format_detail(&outcome.task, &[], &[], None)
            );
            // Gotcha #18: "Queued" with nothing draining the queue looks
            // identical to "Queued, running shortly" — say which it is.
            if outcome.paused {
                out.push_str(
                    "\nNOTE: background runs are PAUSED (the STOP_BACKGROUND kill switch is \
                     set). The task will sit in the queue until it's cleared.\n",
                );
            } else if !outcome.runner_installed {
                out.push_str(
                    "\nNOTE: the background runner is NOT installed, so nothing is draining \
                     the queue. Enable it in the app, or drain once with: \
                     /Applications/CXTasks.app/Contents/MacOS/cxtasks-runner --once\n",
                );
            }
            out.push_str(&render);
            return Self::text(out);
        }

        let (task, tripwire_rt) = {
            let conn = self.open_db()?;
            let task = db::tasks::update(&conn, &task_id, &patch)
                .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
            let rt = if task.tripwire.is_some() {
                db::tripwires::for_task(&conn, &task.id).unwrap_or_default()
            } else {
                None
            };
            (task, rt)
        };

        notify(&Envelope::new("task-updated", &task.id).with_tool("update_task")).await;
        let mut out = format!(
            "Updated.\n\n{}",
            format_detail(&task, &[], &[], tripwire_rt.as_ref())
        );
        if patch.tripwire_check_requested == Some(true) {
            out.push_str(
                "\nCheck requested. The runner evaluates it on its next tick; the outcome lands \
                 on the task (get_task shows it), not here.\n",
            );
        }
        if patch.tripwire_state.is_some() {
            out.push_str(
                "\nRe-armed: hidden from Open, Today and next_task again until the next trip.\n",
            );
        }
        if let Some(w) = runner_warning_for(&task) {
            out.push_str(&w);
        }
        out.push_str(&render);
        Self::text(out)
    }

    #[tool(
        name = "complete_task",
        description = "Mark a task done. If it recurs, the next instance is created automatically. Pass `result` to record what was actually done — a CXTasks-dispatched session should always call this when it finishes the work."
    )]
    async fn complete_task(
        &self,
        Parameters(params): Parameters<CompleteTaskParams>,
    ) -> Result<CallToolResult, McpError> {
        let task_id = self.resolve_task_id(&params.task_id)?;
        let (task, next) = {
            let conn = self.open_db()?;
            crate::recurrence::complete_with_recurrence_as(
                &conn,
                &task_id,
                params.result.as_deref(),
                self.author(),
            )
            .map_err(|e| McpError::invalid_params(format!("{e}"), None))?
        };

        notify(
            &Envelope::new("task-completed", &task.id)
                .with_tool("complete_task")
                .with_summary(&task.title),
        )
        .await;

        let mut out = format!("Completed: {}\n  id: {}", task.title, task.id);
        if let Some(next) = next {
            notify(&Envelope::new("task-created", &next.id).with_tool("complete_task")).await;
            out.push_str(&format!(
                "\n\nRecurring — next instance created:\n  id:  {}\n  due: {}",
                next.id,
                next.due_at.as_deref().unwrap_or("(none)")
            ));
        }
        if task.is_monitor() {
            out.push_str(
                "\n\nMonitor — handled, not retired: the tripwire is re-armed and the runner checks \
                 it again on its next tick. A trip reopens this task. Archive it to stop the check.",
            );
        }
        if let Some(result) = params.result.as_deref() {
            out.push_str(&crate::prose::render_note(result, crate::prose::Channel::Result));
        }
        Self::text(out)
    }

    /// Both go through `api::dispatch_synced` — the verb the app and the
    /// server already use — so archiving from a session is the same write (and
    /// the same sync record) as archiving from the menu. T269: before these
    /// existed, `update_task` told agents to archive and nothing could.
    #[tool(
        name = "archive_task",
        description = "Drop a task: archive it, and its subtasks with it. It leaves every list and board and is never handed out as work, but it is not deleted — search_tasks and get_task still find it, and unarchive_task brings it back. This is how to cancel or retire a task (there is no 'cancelled' status); use complete_task instead when the work was actually done."
    )]
    async fn archive_task(
        &self,
        Parameters(params): Parameters<GetTaskParams>,
    ) -> Result<CallToolResult, McpError> {
        self.set_archived(&params.task_id, true).await
    }

    #[tool(
        name = "unarchive_task",
        description = "Undo archive_task: the task returns to its lists with its status unchanged, along with the subtasks archived in the same step."
    )]
    async fn unarchive_task(
        &self,
        Parameters(params): Parameters<GetTaskParams>,
    ) -> Result<CallToolResult, McpError> {
        self.set_archived(&params.task_id, false).await
    }

    #[tool(
        name = "project_for_repo",
        description = "Which CXTasks project owns a directory? Pass a working directory (usually your cwd) and get the project name back, or a statement that it isn't registered. USE THIS INSTEAD OF GUESSING FROM THE FOLDER NAME — a repo's directory frequently isn't its project's name (~/Projects/finance-app is 'pocketbuddy', ~/Projects/artisttracker is 'Artist Advisory'), and filing under the folder name creates a second empty project beside the real one. file_task already does this lookup itself, so you only need this when you must know the project BEFORE filing — naming a section, or posting a project update. Matches subdirectories too."
    )]
    async fn project_for_repo(
        &self,
        Parameters(params): Parameters<ProjectForRepoParams>,
    ) -> Result<CallToolResult, McpError> {
        let conn = self.open_db()?;
        let owner = db::projects::for_repo_path(&conn, &params.repo_path)
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
        Self::text(match owner {
            Some(project) => format!("{project}\n\n('{}' belongs to it)", params.repo_path.trim()),
            None => format!(
                "No project is registered for '{}'.\n\nDo NOT fall back to the directory \
                 name — that is what creates a duplicate project. Ask which project this is, \
                 or file with repo_path and no project (it lands unfiled, which is honest and \
                 fixable). Once you know the name, `create_project` registers the directory \
                 to it — that call reuses an existing project rather than duplicating one, so \
                 mapping this repo onto a project already on the board is the same call. The \
                 app can do it too: right-click the project in the sidebar → Repositories…",
                params.repo_path.trim()
            ),
        })
    }

    #[tool(
        name = "create_project",
        description = "Create a CXTasks project and register the directory it lives in. Safe to call repeatedly — an existing project of that name is REUSED, never duplicated, and the result says which happened. This is the fix when project_for_repo reports a directory isn't registered: afterwards, tasks filed from there resolve to this project on their own. TWO RULES. (1) Never invent the name from the folder — ~/Projects/finance-app is 'pocketbuddy'. Check project_board first and reuse a name that already fits; if you genuinely don't know what the user calls this project, ASK. Filing a task unfiled is visible and fixable; a duplicate project silently splits the work across two boards and is not. (2) A directory belongs to at most one project — one already claimed is refused, naming its owner, and nothing is created. A project may own several directories (the web and iOS halves of one product); the first registered becomes the primary, which is what prefills a task's workspace. (3) Decide `kind`: an AREA is a standing responsibility that never finishes, a PROJECT is finite work that ends and gets closed. Default is area. A finite wave of work inside an ongoing area — a certification push inside CXVentures, a launch inside a product — belongs as kind='project' with `parent` set to that area, which keeps it out of the area's day-to-day signal while the area's counts still include it."
    )]
    async fn create_project(
        &self,
        Parameters(params): Parameters<CreateProjectParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = db::projects::normalize(&params.name);
        if name.is_empty() {
            return Err(McpError::invalid_params(
                "project name must not be empty".to_string(),
                None,
            ));
        }
        // Validated (absolute, exists) exactly as `file_task` does it, then
        // normalized the way the DB stores it so the comparisons below are
        // against the same spelling `for_repo_path` matches on.
        let repo = params
            .repo_path
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(validate_repo_path)
            .transpose()?
            .map(|p| db::projects::normalize_repo_path(&p));

        let (project, existed, mapping) = {
            let conn = self.open_db()?;
            let internal = |e: crate::error::AppError| McpError::internal_error(format!("{e}"), None);

            // **The ownership check runs BEFORE the project is created**, or a
            // refused mapping leaves a brand-new empty project behind — the
            // exact duplicate this tool's description warns against,
            // manufactured by the guard meant to prevent it.
            //
            // It also has to be `for_repo_path`, not `add_repo`'s own check.
            // That one keys on the UNIQUE index, so it catches only an EXACT
            // path collision — a *subdirectory* of another project's repo
            // sails past it and becomes a competing row that then wins every
            // lookup beneath itself, because resolution is longest-prefix.
            let claimed = match repo.as_deref() {
                Some(path) => db::projects::for_repo_path(&conn, path).map_err(internal)?,
                None => None,
            };
            if let Some(owner) = claimed.as_deref() {
                if !owner.eq_ignore_ascii_case(&name) {
                    return Err(McpError::invalid_params(
                        format!(
                            "'{}' already resolves to the project '{owner}'. A directory can \
                             belong to only one project — that is what lets a task filed there \
                             find its project from the directory alone. Nothing was created. \
                             File this work against '{owner}', or if the mapping is genuinely \
                             wrong, remove it there first (in the app: right-click '{owner}' in \
                             the sidebar → Repositories…).",
                            repo.as_deref().unwrap_or_default()
                        ),
                        None,
                    ));
                }
            }

            let existing = db::projects::get_by_name(&conn, &name).map_err(internal)?;
            let existed = existing.is_some();
            let project = match existing {
                Some(p) => p,
                None => {
                    let kind = match params.kind.as_deref().map(str::trim).filter(|k| !k.is_empty())
                    {
                        Some(raw) => db::projects::ProjectKind::parse(raw)
                            .map_err(|e| McpError::invalid_params(format!("{e}"), None))?,
                        None => db::projects::ProjectKind::default(),
                    };
                    db::projects::create(&conn, &name, kind, params.parent.as_deref())
                        .map_err(|e| McpError::invalid_params(format!("{e}"), None))?
                }
            };

            // `claimed` being Some here means the path already resolves to this
            // same project — either as its own row or through a parent repo.
            // Adding it again would at best duplicate a row and at worst pin
            // every lookup to a subdirectory, so the work is already done.
            let mapping = match (repo.as_deref(), claimed.is_some()) {
                (Some(path), false) => Some(
                    db::projects::add_repo(&conn, &project.name, path, false)
                        .map_err(|e| McpError::invalid_params(format!("{e}"), None))?,
                ),
                _ => None,
            };
            (project, existed, mapping)
        };

        notify(
            &Envelope::new("project-updated", "")
                .with_tool("create_project")
                .with_summary(&project.name),
        )
        .await;

        let mut out = format!(
            "Project '{}' {}.\n",
            project.name,
            if existed {
                "already existed — reused, not duplicated"
            } else {
                "created"
            }
        );
        match (mapping, repo.as_deref()) {
            (Some(r), _) => {
                out.push_str(&format!(
                    "  Registered {}{}.\n  Tasks filed from that directory or anything under it \
                     now resolve to '{}' — file_task and project_for_repo find it with no \
                     `project` argument.\n",
                    r.repo_path,
                    if r.is_primary { " as its primary repo" } else { "" },
                    project.name
                ));
            }
            (None, Some(path)) => {
                out.push_str(&format!(
                    "  {path} already resolved to this project; nothing to register.\n"
                ));
            }
            (None, None) => {
                out.push_str(
                    "  No repo registered, so nothing filed from a directory will resolve to \
                     it. Call again with repo_path to fix that.\n",
                );
            }
        }
        Self::text(out)
    }

    #[tool(
        name = "update_project",
        description = "Change an area or project's SHAPE: close a finished project, move it to a different area, retype it, or set its target date. This is how a finite piece of work ENDS — create_project can only start one. Every change reports what it did, and closing a project with tasks still open says so rather than refusing: closing around unfinished work is a choice worth seeing, not an error. Does not rename (rename lives in the app, since it MERGES onto an existing name) and never touches tasks."
    )]
    async fn update_project(
        &self,
        Parameters(params): Parameters<UpdateProjectParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.project.trim().to_string();
        if name.is_empty() {
            return Err(McpError::invalid_params(
                "project must not be empty".to_string(),
                None,
            ));
        }
        // Every arm below is a named refusal from `db::projects` on failure, so
        // the rules are reported, never re-implemented here.
        let bad = |e: crate::error::AppError| McpError::invalid_params(format!("{e}"), None);
        let internal = |e: crate::error::AppError| McpError::internal_error(format!("{e}"), None);

        let (before, after, still_open) = {
            let conn = self.open_db()?;
            let Some(before) = db::projects::get_by_name(&conn, &name).map_err(internal)? else {
                return Err(McpError::invalid_params(
                    format!(
                        "No area or project called '{name}'. project_board lists them;                          create_project makes one."
                    ),
                    None,
                ));
            };

            // Kind FIRST, deliberately. Promoting to an area clears the parent,
            // target date and closure, so running it after those in the same
            // call would silently discard what the caller just asked for —
            // whereas this order lets one call demote a node and date it.
            if let Some(raw) = params.kind.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
                let kind = db::projects::ProjectKind::parse(raw).map_err(bad)?;
                db::projects::set_kind(&conn, &name, kind).map_err(bad)?;
            }
            // An empty string is the clear sentinel, for `clear_or_set`'s reason
            // one type over: JSON `null` and an omitted key both deserialize to
            // `None`, so without it "no area" would be unsayable from a session.
            // Empty is safe as the sentinel because `projects::normalize` refuses
            // an empty name, so it can never collide with a real one.
            if let Some(raw) = params.parent.as_deref().map(str::trim) {
                let parent = if raw.is_empty() { None } else { Some(raw) };
                db::projects::set_parent(&conn, &name, parent).map_err(bad)?;
            }
            if let Some(raw) = params.target_date.as_deref().map(str::trim) {
                let date = if raw.is_empty() { None } else { Some(raw) };
                db::projects::set_target_date(&conn, &name, date).map_err(bad)?;
            }
            if let Some(closed) = params.closed {
                db::projects::set_closed(&conn, &name, closed).map_err(bad)?;
            }

            let after = db::projects::get_by_name(&conn, &name)
                .map_err(internal)?
                .ok_or_else(|| internal(crate::error::AppError::NotFound(name.clone())))?;
            // Counted through `expand_scope`, so closing an AREA-shaped mistake
            // reports the same number its badge does.
            let still_open = after.open_count;
            (before, after, still_open)
        };

        notify(
            &Envelope::new("project-updated", "")
                .with_tool("update_project")
                .with_summary(&after.name),
        )
        .await;

        let mut lines: Vec<String> = Vec::new();
        if before.kind != after.kind {
            lines.push(format!(
                "now a {} (was a {})",
                after.kind.as_str(),
                before.kind.as_str()
            ));
        }
        if before.parent != after.parent {
            lines.push(match after.parent.as_deref() {
                Some(p) => format!("moved into '{p}'"),
                None => "lifted out to the top level — it belongs to no area".to_string(),
            });
        }
        if before.target_date != after.target_date {
            lines.push(match after.target_date.as_deref() {
                Some(d) => format!("target date {d}"),
                None => "target date cleared".to_string(),
            });
        }
        if before.closed_at.is_some() != after.closed_at.is_some() {
            lines.push(match after.closed_at.as_deref() {
                Some(d) => format!("CLOSED {d} — archived, and off the board"),
                None => "reopened, and back on the board".to_string(),
            });
        }

        if lines.is_empty() {
            return Self::text(format!(
                "'{}' is unchanged — nothing you passed differed from what was stored.",
                after.name
            ));
        }
        let mut out = format!("{}:
", after.name);
        for line in &lines {
            out.push_str(&format!("  - {line}
"));
        }
        // Reported, never refused. An agent finishing the last task of a wave
        // should be able to close it; an agent closing one with six tasks left
        // should be told, in the same breath, what it just walked away from.
        if after.closed_at.is_some() && still_open > 0 {
            out.push_str(&format!(
                "
  {still_open} task(s) are still open in it. They are untouched and still                  findable — closing a project files the WORK away, it does not finish it.                  Reopen with closed:false, or move them somewhere still live.
"
            ));
        }
        Self::text(out)
    }

    #[tool(
        name = "next_task",
        description = "The single most pressing open task: overdue first, then priority, then nearest due date. Deferred, closed, superseded and `decision`-tagged tasks are skipped — a decision is Chris's to make, and the reply lists any that are waiting on him. Use this to answer 'what should I work on'."
    )]
    async fn next_task(
        &self,
        Parameters(params): Parameters<NextTaskParams>,
    ) -> Result<CallToolResult, McpError> {
        let conn = self.open_db()?;
        let task = db::tasks::next_task(&conn, params.project.as_deref())
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
        let decisions = db::tasks::open_decisions(&conn)
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
        let mut out = match task {
            None => "Nothing open. Inbox zero.".to_string(),
            Some(t) => {
                let children = db::tasks::list_children(&conn, &t.id).unwrap_or_default();
                format_detail(&t, &[], &children, None)
            }
        };
        // Never the answer — not agent work — but a session asking "what
        // next" should know what only Chris can move, and say so to him.
        if !decisions.is_empty() {
            out.push_str(&format!(
                "\n\n{} decision(s) waiting on Chris — not agent work; surface them, don't take them:\n{}",
                decisions.len(),
                decisions.iter().map(format_line).collect::<Vec<_>>().join("\n")
            ));
        }
        Self::text(out)
    }

    #[tool(
        name = "search_tasks",
        description = "Find tasks by free text and/or filters: projects, statuses, priorities, tags, people (`@name` tags), waiting (held back by a date/tripwire/blocker), due/created date bounds. Text and filters combine; filters alone work with no query. Different filters AND together, values within one OR (tags narrow instead). Searches every live task regardless of view, completed and archived included, trashed excluded."
    )]
    async fn search_tasks(
        &self,
        Parameters(params): Parameters<SearchTasksParams>,
    ) -> Result<CallToolResult, McpError> {
        let query = params.query.as_deref().map(str::trim).unwrap_or("");
        let faceted = params.has_facets();
        if query.is_empty() && !faceted {
            return Self::text("No results. (empty query)".to_string());
        }
        let conn = self.open_db()?;

        // A bare T-number is a LOOKUP, not a search. `short_id` is deliberately
        // outside the FTS index (indexing it would mean churning FTS rows on
        // every backfill), so a full-text search for "T42" finds nothing — which
        // reads as "that task doesn't exist" rather than "search doesn't do
        // that". Resolving directly also reaches trashed and archived tasks,
        // marked as such, because looking up a reference someone wrote down is
        // the case where you most need to be told the task was deleted rather
        // than told nothing.
        //
        // Skipped when filters are present: "T42 that is also done" is asking
        // to *test* a specific task against a predicate, and answering a lookup
        // with silence when it fails the test is the exact reading this branch
        // exists to prevent. With filters, the filter path runs and says what
        // matched.
        if !faceted && db::tasks::parse_short_ref(query).is_some() {
            let found = db::tasks::resolve_ref(&conn, query)
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
            return match found {
                None => Self::text(format!("No task {query}.")),
                Some(t) => {
                    let state = match (t.deleted_at.is_some(), t.archived_at.is_some()) {
                        (true, _) => " [trashed]",
                        (false, true) => " [archived]",
                        _ => "",
                    };
                    let line = format_search_line(&conn, &t)
                        .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
                    Self::text(format!("{line}{state}"))
                }
            };
        }

        // Text alone stays on the plain search path, byte-identical to what it
        // has always returned — including its bm25 relevance order, which the
        // filter path deliberately replaces with board order.
        let hits = if faceted {
            let mut spec = params.spec();
            spec.query = (!query.is_empty()).then(|| query.to_string());
            db::search::filtered(&conn, &spec)
        } else {
            db::search::search(&conn, query, params.limit.unwrap_or(20))
        }
        .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;

        let label = describe_search(query, &params);
        if hits.is_empty() {
            return Self::text(format!("No tasks matching {label}."));
        }
        let body = hits
            .iter()
            .map(|t| format_search_line(&conn, t))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?
            .join("\n");
        Self::text(format!("{} match(es) for {label}:\n{body}", hits.len()))
    }

    #[tool(
        name = "retrospective",
        description = "Review actual closed work for an area/project and/or inclusive date range. Returns cited evidence from task_runs.result_text and task_comments.body under four headings: what we learned, what recurs across tasks, the mix (how much of the closed work had a person on it vs. nobody — the October check), and what was already ruled out. Superseded work is labelled separately from completed work. Read-only."
    )]
    async fn retrospective(
        &self,
        Parameters(params): Parameters<RetrospectiveParams>,
    ) -> Result<CallToolResult, McpError> {
        let conn = self.open_db()?;
        let report = db::retrospective::read(
            &conn,
            &db::retrospective::RetrospectiveSpec {
                project: params.project,
                from: params.from,
                to: params.to,
                limit: params.limit,
            },
        )
        .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
        Self::text(format_retrospective(&report))
    }

    #[tool(
        name = "block_task",
        description = "Record that one task is BLOCKED BY another — it can't start until the other is done. Many-to-many and works across projects, unlike a subtask (which is containment within one parent). Blocked tasks are skipped by next_task and by the background runner. Circular dependencies are rejected. Pass remove=true to delete the link."
    )]
    async fn block_task(
        &self,
        Parameters(params): Parameters<BlockTaskParams>,
    ) -> Result<CallToolResult, McpError> {
        // BOTH ends resolve — a dependency is the one place two references are
        // named in one call, and accepting a T-number for only one of them is
        // exactly the inconsistency that makes the feature untrustworthy.
        let task_id = self.resolve_task_id(&params.task_id)?;
        let blocked_by_id = self.resolve_task_id(&params.blocked_by_id)?;
        let conn = self.open_db()?;
        if params.remove.unwrap_or(false) {
            db::deps::remove(&conn, &task_id, &blocked_by_id)
                .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
            notify(&Envelope::new("task-updated", &task_id).with_tool("block_task")).await;
            return Self::text("Dependency removed.".to_string());
        }
        db::deps::add(&conn, &task_id, &blocked_by_id)
            .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
        notify(&Envelope::new("task-updated", &task_id).with_tool("block_task")).await;

        let blockers = db::deps::blocked_by(&conn, &task_id).unwrap_or_default();
        let open = blockers.iter().filter(|b| b.is_open).count();
        Self::text(format!(
            "Blocked. This task now waits on {} task(s), {open} still open:\n{}",
            blockers.len(),
            blockers
                .iter()
                .map(|b| format!("  - [{}] {}", b.status, b.title))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }

    #[tool(
        name = "relate_task",
        description = "Preserve why a plan changed. relation='supersedes' means task_id is the NEW replacement and other_task_id is the OLDER task; the older task is no longer returned by next_task/background execution and both directions render in the app/MCP. It is refused when the older task has no run or comment—delete pure plan churn instead. relation='relates_to' is a symmetric context link. Pass remove=true to remove the edge."
    )]
    async fn relate_task(
        &self,
        Parameters(params): Parameters<RelateTaskParams>,
    ) -> Result<CallToolResult, McpError> {
        let task_id = self.resolve_task_id(&params.task_id)?;
        let other_task_id = self.resolve_task_id(&params.other_task_id)?;
        let kind = db::relations::RelationKind::parse(&params.relation)
            .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
        let conn = self.open_db()?;
        if params.remove.unwrap_or(false) {
            db::relations::remove(&conn, &task_id, &other_task_id, kind)
                .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
            notify(&Envelope::new("task-updated", &task_id).with_tool("relate_task")).await;
            notify(&Envelope::new("task-updated", &other_task_id).with_tool("relate_task")).await;
            return Self::text(format!("{} relation removed.", kind.as_str()));
        }
        db::relations::add(&conn, &task_id, &other_task_id, kind)
            .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
        notify(&Envelope::new("task-updated", &task_id).with_tool("relate_task")).await;
        notify(&Envelope::new("task-updated", &other_task_id).with_tool("relate_task")).await;
        let task = db::tasks::get(&conn, &task_id)
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?
            .ok_or_else(|| McpError::internal_error("task vanished", None))?;
        let other = db::tasks::get(&conn, &other_task_id)
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?
            .ok_or_else(|| McpError::internal_error("other task vanished", None))?;
        let explanation = match kind {
            db::relations::RelationKind::Supersedes => format!(
                "{} ({}) now supersedes {} ({}). The older task remains retrievable as \
                 history but will not be handed out as work.",
                short_ref(&task).unwrap_or(task.id),
                task.title,
                short_ref(&other).unwrap_or(other.id),
                other.title
            ),
            db::relations::RelationKind::RelatesTo => format!(
                "{} ({}) is now related to {} ({}).",
                short_ref(&task).unwrap_or(task.id),
                task.title,
                short_ref(&other).unwrap_or(other.id),
                other.title
            ),
        };
        Self::text(explanation)
    }

    #[tool(
        name = "tag_task",
        description = "Add or remove tags on a task. Tags are shared across tasks and case-insensitive — use them for cross-cutting labels (billable, quick-win), and REUSE existing ones: a tag that doesn't exist yet is refused unless `new_tags` is true, and the refusal names the closest existing tags. Two tags carry behaviour: `@name` marks a PERSON the task involves (`@sam`) and feeds search_tasks(people) and the Waiting view; `decision` marks a question only Chris can answer — it goes on Today's Decide strip and is refused by next_task and the background queue. Use `project` for where the work lives instead."
    )]
    async fn tag_task(
        &self,
        Parameters(params): Parameters<TagTaskParams>,
    ) -> Result<CallToolResult, McpError> {
        let task_id = self.resolve_task_id(&params.task_id)?;
        let conn = self.open_db()?;
        let removing = params.remove.unwrap_or(false);
        if !removing {
            vet_tags(&conn, &params.tags, params.new_tags.unwrap_or(false))?;
        }
        for name in &params.tags {
            if removing {
                db::tags::detach(&conn, &task_id, name)
                    .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
            } else {
                db::tags::attach(&conn, &task_id, name)
                    .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
            }
        }
        notify(&Envelope::new("task-updated", &task_id).with_tool("tag_task")).await;
        let now = db::tags::for_task(&conn, &task_id).unwrap_or_default();
        Self::text(format!(
            "Tags now: {}",
            if now.is_empty() {
                "(none)".to_string()
            } else {
                now.iter().map(|t| format!("#{}", t.name)).collect::<Vec<_>>().join(" ")
            }
        ))
    }

    #[tool(
        name = "comment_task",
        description = "Append a comment to a task's log — what happened, decisions made, findings, a dead end you ruled out. Signed as the agent calling it ('claude' or 'codex') so it's distinguishable from Chris's own notes. Prefer this over rewriting the task body: the body is the definition, comments are the history. Keep entries under 2,000 characters; the log is a scannable chronology, and a longer entry is truncated when it is replayed into the next session's prompt. If what you have is a full handoff rather than a note, pass `handoff: true` and it is stored in full on a run row instead."
    )]
    async fn comment_task(
        &self,
        Parameters(params): Parameters<CommentTaskParams>,
    ) -> Result<CallToolResult, McpError> {
        let task_id = self.resolve_task_id(&params.task_id)?;
        let body = params.body.trim();
        let handoff = params.handoff.unwrap_or(false);
        let len = body.chars().count();

        // **A background run cannot reroute its handoff through here**, and the
        // reason is ordering rather than tidiness. Its handoff is captured from
        // the CLI's final message and written by `runner::finish_run` when the
        // run ends; a note filed mid-run would land on a row with a LATER
        // `started_at` than the background run's own, so `history_block` — which
        // replays `finished.first()` — would hand the next agent this note and
        // call it the handoff, permanently. Gated here rather than by the
        // router because a router removes tools, not parameters.
        if handoff && self.mode.is_unattended() {
            return Err(McpError::invalid_params(
                "`handoff: true` is for foreground sessions only, and nothing was written. \
                 Your handoff is captured automatically from this run's FINAL MESSAGE — it is \
                 stored in full on the run and replayed to whoever picks this task up next, so \
                 put the detail there. Use this tool without `handoff` for notes as you go, \
                 under 2,000 characters."
                    .to_string(),
                None,
            ));
        }

        // The cap applies to notes, never to a handoff — `handoff: true` IS the
        // escape hatch the refusal below points at, and capping both would make
        // the advice circular.
        if !handoff && len > COMMENT_MAX_CHARS {
            let remedy = if self.mode.is_unattended() {
                "Your HANDOFF belongs in this run's FINAL MESSAGE, which is captured \
                 automatically, kept in full on the run, and replayed to whoever picks this \
                 task up next. Put the detail there and keep this note to what is worth \
                 scanning later."
            } else {
                "If this is a handoff — you are stopping and someone else continues — call this \
                 tool again with `handoff: true`, which keeps the full text on a run row and \
                 summarizes it here. If the work is finished, pass it as `complete_task`'s \
                 `result` instead, which does the same. Otherwise trim it to the part worth \
                 keeping."
            };
            return Err(McpError::invalid_params(
                format!(
                    "That comment is {len} characters; the log caps them at \
                     {COMMENT_MAX_CHARS}. Nothing was written. The comment log is a scannable \
                     chronology, and an entry this long is truncated mid-sentence when it is \
                     replayed into the next session's prompt. {remedy}"
                ),
                None,
            ));
        }

        let conn = self.open_db()?;

        // A handoff takes the same split `runner::finish_run` performs: the
        // artifact onto a run row (replayed in full, no cap), a derived
        // one-liner into the log (scannable, and what the pane shows).
        let recorded = if handoff {
            let task = db::tasks::get(&conn, &task_id)
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?
                .ok_or_else(|| {
                    McpError::invalid_params("that task no longer exists".to_string(), None)
                })?;
            Some(
                db::runs::record_foreground_handoff(
                    &conn,
                    &task_id,
                    task.bg_intent,
                    body,
                )
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?,
            )
        } else {
            None
        };

        let log_body = match &recorded {
            // Headed by the RUN's recorded intent, not the task's: when this
            // attaches to a dispatch row, that row is what the session was
            // actually framed as, and the task's `bg_intent` may have been
            // changed since. A verdict headed the same as the work it critiques
            // is the one distinction the chronology cannot afford to lose.
            Some(run) => {
                let noun = if run.intent.is_review() {
                    "Foreground review"
                } else {
                    "Foreground handoff"
                };
                format!(
                    "{noun}\n\n{}\n\n_Full handoff on the run._",
                    crate::runner::handoff_summary(body)
                )
            }
            None => body.to_string(),
        };

        let c = db::comments::add(&conn, &task_id, self.author(), &log_body)
            .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
        notify(&Envelope::new("task-updated", &task_id).with_tool("comment_task")).await;
        let n = db::comments::count_for_task(&conn, &task_id).unwrap_or(0);
        let render = crate::prose::render_note(body, crate::prose::Channel::Comment);
        match recorded {
            Some(_) => Self::text(format!(
                "Handoff recorded: {len} characters kept in full on the run and replayed to \
                 the next session; the log shows the summary below ({n} comment(s) on this \
                 task).\n{}{render}",
                c.body
            )),
            None => Self::text(format!("Comment added ({n} on this task).\n{}{render}", c.body)),
        }
    }

    #[tool(
        name = "file_followup",
        description = "File a follow-up you discovered while working this task. Use it for anything real that is OUT OF SCOPE for what you were asked to do — a bug you found on the way, a prerequisite, the next implementation pass. It lands as a separate untriaged task linked to your task, visible in Open and its project, inherits its project and repo, and a human triages it: it will NOT be run unattended, so write the body for someone who has none of your context. Filing three specific follow-ups is far more useful than one that says 'various issues'. If the work IS in scope, just do it (or explain why you couldn't) instead of filing it."
    )]
    async fn file_followup(
        &self,
        Parameters(params): Parameters<FileFollowupParams>,
    ) -> Result<CallToolResult, McpError> {
        let parent_id = self.resolve_task_id(&params.task_id)?;
        let (parent, task) = {
            let mut conn = self.open_db()?;
            // The task and its context link commit together. An unlinked task
            // would lose the investigation that explains why it was filed.
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
            let parent = db::tasks::get(&tx, &parent_id)
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?
                .ok_or_else(|| {
                    McpError::invalid_params("that task no longer exists".to_string(), None)
                })?;

            let new = db::tasks::NewTask {
                title: params.title,
                body: params.body,
                // Untriaged by definition — a machine-proposed task that arrived on
                // the board as `todo` would be claiming a triage decision nobody made.
                status: Some("inbox".into()),
                priority: params.priority,
                // Inherited, not asked for. The follow-up belongs to the same work
                // as its parent, and letting an unattended run name a project is how
                // you get a phantom one (invariant 17).
                project: parent.project.clone(),
                repo_path: parent.repo_path.clone(),
                // Out-of-scope work stays visible when the source is archived.
                parent_id: None,
                // **Forced, not defaulted.** A follow-up that could itself run
                // unattended closes the loop: the runner drains it, that run files
                // more, and nothing in the system stops it. A human opens the gate
                // or it stays shut.
                bg_allowed: Some(false),
                bg_writes_allowed: Some(false),
                bg_shell_allowed: Some(false),
                ..Default::default()
            };

            let task = db::tasks::create(&tx, &new)
                .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
            db::relations::add(
                &tx,
                &task.id,
                &parent.id,
                db::relations::RelationKind::RelatesTo,
            )
            .map_err(|e| McpError::invalid_params(format!("{e}"), None))?;
            tx.commit()
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
            (parent, task)
        };

        notify(
            &Envelope::new("task-created", &task.id)
                .with_tool("file_followup")
                .with_summary(&task.title),
        )
        .await;
        notify(&Envelope::new("task-updated", &parent.id).with_tool("file_followup")).await;

        Self::text(format!(
            "Filed follow-up {} linked to {}: {}\n\nIt is a separate task visible in Open, untriaged, and will not run \
             unattended. Keep going with your own task — mention this in your final \
             summary so the reviewer knows why it exists.{}",
            short_ref(&task).unwrap_or_else(|| task.id.clone()),
            short_ref(&parent).unwrap_or_else(|| parent.id.clone()),
            task.title,
            task.body
                .as_deref()
                .map(|b| crate::prose::render_note(b, crate::prose::Channel::Followup))
                .unwrap_or_default()
        ))
    }

    #[tool(
        name = "project_board",
        description = "The GSD board: one line per node showing health, open/blocked/done-this-week counts, the next dated milestone, the last written status update and how old it is, and what to work on next. AREAS come first with their nested PROJECTS indented under them; projects belonging to no area follow at the end. An area's counts INCLUDE its nested projects, so the top line is the whole picture and the indented lines say what inside it is finite. The activity word differs by kind on purpose — a project untouched for a month is `stalled`, an area untouched for a month is `quiet`, which is not a problem. Read-only. Pass `project` (an area name works too) to drill into one node's full update log, milestones and nested projects. Use this to answer 'what's on track and what's rotting'."
    )]
    async fn project_board(
        &self,
        Parameters(params): Parameters<ProjectBoardParams>,
    ) -> Result<CallToolResult, McpError> {
        let conn = self.open_db()?;
        let rows = db::projects::board(&conn)
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;

        if let Some(name) = params.project.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            let Some(row) = rows.iter().find(|r| r.project.eq_ignore_ascii_case(name)) else {
                return Self::text(format!("No project '{name}' on the board."));
            };
            let updates =
                db::project_updates::for_project(&conn, &row.project, params.limit.unwrap_or(5).min(50))
                    .unwrap_or_default();
            let sections = db::projects::list_sections(&conn, Some(&row.project)).unwrap_or_default();
            let children: Vec<&db::projects::BoardRow> = rows
                .iter()
                .filter(|c| {
                    c.parent
                        .as_deref()
                        .is_some_and(|p| p.eq_ignore_ascii_case(&row.project))
                })
                .collect();
            return Self::text(format_project_detail(row, &updates, &sections, &children));
        }

        if rows.is_empty() {
            return Self::text("No projects yet.".to_string());
        }
        Self::text(format_board(&conn, &rows))
    }

    #[tool(
        name = "post_project_update",
        description = "Post a written status update on a project, optionally moving its health (on_track | at_risk | off_track | paused). THIS IS THE ONLY WAY HEALTH CHANGES — there is deliberately no bare setter, so a status on the board always has an explanation behind it. Omit `health` to file a note without moving the dot. Signed as the agent calling it ('claude' or 'codex'), distinguishable from Chris's own updates."
    )]
    async fn post_project_update(
        &self,
        Parameters(params): Parameters<PostProjectUpdateParams>,
    ) -> Result<CallToolResult, McpError> {
        let update = {
            let conn = self.open_db()?;
            db::project_updates::post(
                &conn,
                &params.project,
                self.author(),
                &params.body,
                params.health.as_deref(),
            )
            .map_err(|e| McpError::invalid_params(format!("{e}"), None))?
        };

        // Empty task_id: this is an app-wide refresh hint, not a row that should
        // flash. See `bridge::Envelope`.
        notify(
            &Envelope::new("project-updated", "")
                .with_tool("post_project_update")
                .with_summary(&update.project),
        )
        .await;

        Self::text(format!(
            "Posted on {}{}.\n{}",
            update.project,
            update
                .health
                .as_deref()
                .map(|h| format!(" — health now {h}"))
                .unwrap_or_else(|| " (health unchanged)".to_string()),
            update.body
        ))
    }

    #[tool(
        name = "upsert_section",
        description = "Get or create a section (a phase) within a project, optionally giving it a target date. A section WITH a target date is a milestone and shows on the GSD board; without one it's a plain grouping. Safe to call repeatedly — it reuses an existing section and will NOT overwrite a date that's already set, so re-running a plan can't stomp a date someone moved by hand."
    )]
    async fn upsert_section(
        &self,
        Parameters(params): Parameters<UpsertSectionParams>,
    ) -> Result<CallToolResult, McpError> {
        let section = {
            let conn = self.open_db()?;
            db::projects::upsert_section(
                &conn,
                Some(&params.project),
                &params.name,
                params.target_date.as_deref(),
            )
            .map_err(|e| McpError::invalid_params(format!("{e}"), None))?
        };
        notify(
            &Envelope::new("project-updated", "")
                .with_tool("upsert_section")
                .with_summary(&section.name),
        )
        .await;
        Self::text(format!(
            "Section '{}' in {}{}.\n  section_id: {}",
            section.name,
            params.project,
            section
                .target_date
                .as_deref()
                .map(|d| format!(" — milestone due {d}"))
                .unwrap_or_default(),
            section.id
        ))
    }

    #[tool(
        name = "list_tags",
        description = "Every tag with how many open tasks carry it."
    )]
    async fn list_tags(&self) -> Result<CallToolResult, McpError> {
        let conn = self.open_db()?;
        let tags = db::tags::list_with_counts(&conn)
            .map_err(|e| McpError::internal_error(format!("{e}"), None))?;
        if tags.is_empty() {
            return Self::text("No tags yet.".to_string());
        }
        // People first, under their own heading: they are a different axis
        // from labels, and an agent scanning for "who is this work with"
        // should not have to pick `@` names out of a list of kinds.
        let (people, labels): (Vec<_>, Vec<_>) =
            tags.into_iter().partition(|t| db::tags::is_person(&t.name));
        let mut out = String::new();
        if !people.is_empty() {
            out.push_str("PEOPLE (tags spelled @name — search_tasks(people: [...]) finds their work)\n");
            for t in &people {
                out.push_str(&format!("{} — {} open\n", t.name, t.open_count));
            }
            out.push('\n');
        }
        out.push_str(
            &labels
                .iter()
                .map(|t| format!("#{} — {} open", t.name, t.open_count))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        Self::text(out)
    }
}

// **`router = self.tool_router` is load-bearing — the default is not what it
// looks like.** `#[tool_handler]` with no argument expands to
// `Self::tool_router()`, the STATIC constructor, in both `list_tools` and
// `call_tool`. So every request rebuilds the full router and whatever `new()`
// stored on the instance is ignored — which means the reduced background
// surface was served in full while the field-level test passed happily. The
// pre-existing "field `tool_router` is never read" warning was this, saying so.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for CxTasksMcp {
    fn get_info(&self) -> ServerInfo {
        let info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        // A background run has three tools and `file_task` is not one of them,
        // so filing discipline is dead weight in its context — and
        // `file_followup` already carries its own bar in its description
        // ("If the work IS in scope, just do it instead of filing it").
        if self.mode.is_unattended() {
            info
        } else {
            info.with_instructions(INTERACTIVE_INSTRUCTIONS)
        }
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        self.claimed.store(true, Ordering::Relaxed);
        if let Ok(mut name) = self.client_name.lock() {
            *name = Some(request.client_info.name.clone());
        }
        if context.peer.peer_info().is_none() {
            context.peer.set_peer_info(request);
        }
        Ok(self.get_info())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::tasks::{create, NewTask};

    /// T238: everything used to sign as "claude", Codex included.
    #[test]
    fn an_agent_signs_as_itself_not_as_claude() {
        // The runner's declaration wins over whatever the client calls itself.
        assert_eq!(author_for(Some("codex"), Some("claude-code")), "codex");
        assert_eq!(author_for(Some("claude"), Some("codex-mcp-client")), "claude");
        // Undeclared: the client's initialize name decides.
        assert_eq!(author_for(None, Some("codex-mcp-client")), "codex");
        assert_eq!(author_for(None, Some("claude-code")), "claude");
        // Unknown or absent is the foreground agent, and a garbled
        // declaration is not trusted over the client name.
        assert_eq!(author_for(None, None), "claude");
        assert_eq!(author_for(Some("gpt"), Some("codex-mcp-client")), "codex");
    }

    fn task_with(title: &str) -> db::tasks::Task {
        let conn = crate::db::schema::test_conn();
        create(
            &conn,
            &NewTask {
                title: title.into(),
                ..Default::default()
            },
        )
        .unwrap()
    }

    // ── the background tool surface ───────────────────────────────────────

    fn tool_names(mode: McpMode) -> Vec<String> {
        CxTasksMcp::with_mode(":memory:".into(), mode)
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect()
    }

    /// **The tools a background run doesn't get are ABSENT, not denied.**
    ///
    /// Both halves matter and they fail differently. Absent means the agent is
    /// never told the capability is nearby — the same reasoning
    /// `--strict-mcp-config` already applies to the user's other MCP servers,
    /// one level down. And absent means `complete_task` cannot be reached by an
    /// agent that decides the instructions don't apply to it, which is not
    /// hypothetical: a background task marked itself done and left the run row
    /// dangling before any of this existed.
    /// A check is narrower still: ONE tool. It can re-read the task it is
    /// evaluating and nothing else — no notes (a "not yet" every hour would
    /// bury the log), no follow-ups (a yes/no question must not become work).
    #[test]
    fn a_check_run_gets_exactly_get_task() {
        let check = tool_names(McpMode::Check);
        assert_eq!(check, vec!["get_task".to_string()], "{check:?}");
        for name in CHECK_TOOLS {
            assert!(BACKGROUND_TOOLS.contains(name), "a check tool must be a subset of the background surface");
        }
        // And the grant names exactly what is served, `mcp__cxtasks__`-prefixed.
        let granted: Vec<&str> = CHECK_TOOLS_CSV.split(',').collect();
        assert_eq!(granted.len(), CHECK_TOOLS.len());
        for name in CHECK_TOOLS {
            assert!(granted.contains(&format!("mcp__cxtasks__{name}").as_str()), "{granted:?}");
        }
        assert!(!CxTasksMcp::with_mode(":memory:".into(), McpMode::Check)
            .get_info()
            .instructions
            .is_some(), "a check gets no filing instructions");
    }

    #[test]
    fn a_background_run_gets_three_tools_and_no_way_to_close_a_task() {
        let bg = tool_names(McpMode::Background);
        assert_eq!(bg.len(), BACKGROUND_TOOLS.len(), "{bg:?}");
        for want in BACKGROUND_TOOLS {
            assert!(bg.iter().any(|n| n == want), "missing {want}: {bg:?}");
        }
        // `search_tasks` included deliberately: it grew the filter facets, and
        // growing a tool must never grow the background surface. It is not in
        // BACKGROUND_TOOLS, so `with_mode` removes it — this asserts the facets
        // rode along on a tool that is still gone, rather than reintroducing
        // an unattended run's ability to enumerate the whole board.
        //
        // `create_project` for a sharper reason: it is the only tool that
        // creates a *grouping* rather than a task, so a confused unattended run
        // reaching it doesn't produce a bad row someone triages — it produces a
        // second board, splitting the project's work in a way nothing on screen
        // explains (invariant 17). The allowlist already excludes it; this is
        // what stops a later "the runner needs to file into a new project" from
        // quietly adding it.
        for forbidden in [
            "complete_task",
            "update_task",
            "file_task",
            "block_task",
            "search_tasks",
            "list_tasks",
            "create_project",
        ] {
            assert!(
                !bg.iter().any(|n| n == forbidden),
                "{forbidden} must not exist in a background run: {bg:?}"
            );
        }
        // **A parameter is not a tool, and this test must not grow to cover
        // one.** `comment_task` gained `handoff`, which a background run is
        // refused — but a router removes routes, never fields, so that gate
        // lives in the handler and is pinned by
        // `a_background_run_cannot_reroute_its_handoff_through_comment_task`.
        // The count above stays 3.
    }

    /// **Instructions are served to an interactive session and withheld from a
    /// background run.** Same reasoning as the tool surface one level up: an
    /// unattended run cannot reach `file_task` at all, so filing discipline is
    /// context it pays for and can never act on.
    ///
    /// The `contains` assertions pin the two halves that carry the whole point
    /// — the unconditional path when Chris asks, and the bar that applies only
    /// when the model decides on its own. A rewrite that drops either one has
    /// changed the meaning, not the wording.
    #[test]
    fn instructions_are_interactive_only_and_keep_both_filing_paths() {
        let interactive = CxTasksMcp::with_mode(":memory:".into(), McpMode::Interactive).get_info();
        let background = CxTasksMcp::with_mode(":memory:".into(), McpMode::Background).get_info();

        assert_eq!(
            background.instructions, None,
            "a background run must not be told how to file"
        );

        let text = interactive
            .instructions
            .expect("an interactive session must get instructions");
        assert!(
            text.contains("WHEN CHRIS ASKS, FILE IT"),
            "the unconditional path is the common case and must stay stated: {text}"
        );
        assert!(
            text.contains("WHEN YOU DECIDE ON YOUR OWN"),
            "the bar must stay scoped to self-initiated filing: {text}"
        );
        assert!(
            text.contains("WHY NOT NOW"),
            "the bar's test is one question and it must stay named: {text}"
        );
    }

    /// The grant string and the router must name the same tools. They are
    /// declared separately — one is a CSV for `--allowedTools`, the other is
    /// what the server actually serves — and a name in only one of them fails
    /// silently: an unlisted tool becomes a permission prompt nobody can answer
    /// (invariant 7), and a listed-but-absent one is a deny rule the CLI warns
    /// about on stderr and ignores.
    #[test]
    fn the_allowed_tools_grant_matches_the_router_exactly() {
        let granted: Vec<&str> = BACKGROUND_TOOLS_CSV.split(',').collect();
        assert_eq!(granted.len(), BACKGROUND_TOOLS.len());
        for name in BACKGROUND_TOOLS {
            let qualified = format!("mcp__cxtasks__{name}");
            assert!(
                granted.contains(&qualified.as_str()),
                "{qualified} is served but not granted: {BACKGROUND_TOOLS_CSV}"
            );
        }
    }

    /// The filter facets rode onto `search_tasks` rather than arriving as a
    /// third listing tool, so the tool COUNT must not have moved. Invariant
    /// 18's measured argument applies here: every tool costs context on every
    /// background-adjacent call, and `list_tasks` + `search_tasks` +
    /// `filter_tasks` would make picking one a coin flip an agent sometimes
    /// loses. A drifting number here means someone added the third tool.
    #[test]
    fn the_facets_did_not_add_a_third_way_to_list_tasks() {
        let names = tool_names(McpMode::Interactive);
        assert!(names.iter().any(|n| n == "search_tasks"));
        assert!(
            !names.iter().any(|n| n == "filter_tasks"),
            "facets belong ON search_tasks: {names:?}"
        );
        let listers: Vec<&String> = names
            .iter()
            .filter(|n| n.contains("list_tasks") || n.contains("search_tasks"))
            .collect();
        assert_eq!(listers.len(), 2, "exactly two ways to list tasks: {listers:?}");
    }

    /// An empty result must say what it searched FOR.
    ///
    /// "no task is due before Aug 3" and "you typed a project that doesn't
    /// exist" otherwise produce byte-identical output, and an agent that reads
    /// the second as the first reports "nothing found" about a filter which
    /// never had a chance to match.
    #[test]
    fn the_result_line_names_every_filter_it_applied() {
        let params = SearchTasksParams {
            query: Some("webhook".into()),
            projects: Some(vec!["acme".into(), "initech".into()]),
            statuses: Some(vec!["done".into()]),
            priorities: Some(vec![0]),
            tags: Some(vec!["billable".into()]),
            waiting: None,
            people: None,
            due_before: Some("2026-08-03".into()),
            due_after: None,
            created_before: None,
            created_after: Some("2026-07-01".into()),
            include_done: None,
            limit: None,
        };
        let label = describe_search("webhook", &params);
        for want in [
            "'webhook'",
            "project acme/initech",
            "status done",
            "tag billable",
            "priority P0",
            "due before 2026-08-03",
            "created after 2026-07-01",
        ] {
            assert!(label.contains(want), "missing {want} in {label}");
        }
        assert!(!label.contains("due after"), "absent bounds stay unmentioned: {label}");
    }

    /// Facets alone are a valid query — `query` became optional so "everything
    /// done in project X" needs no keyword to hang off.
    #[test]
    fn facets_are_detected_without_any_text() {
        let bare = SearchTasksParams {
            query: None,
            projects: None,
            statuses: None,
            priorities: None,
            tags: None,
            waiting: None,
            people: None,
            due_before: None,
            due_after: None,
            created_before: None,
            created_after: None,
            include_done: None,
            limit: None,
        };
        assert!(!bare.has_facets(), "nothing set is not a facet query");
        // An EMPTY vec is not a facet either — it would otherwise send a
        // no-op filter down the filtered() path and silently drop the
        // relevance ordering a plain text search returns.
        let empty_vecs = SearchTasksParams {
            projects: Some(vec![]),
            tags: Some(vec![]),
            ..bare
        };
        assert!(!empty_vecs.has_facets());

        let filtered_only = SearchTasksParams {
            statuses: Some(vec!["done".into()]),
            ..empty_vecs
        };
        assert!(filtered_only.has_facets());
        assert_eq!(filtered_only.spec().statuses, Some(vec!["done".to_string()]));
        // The spec carries facets only — text is folded in by the caller,
        // after the T-number branch has had its look at the raw query.
        assert!(filtered_only.spec().query.is_none());
    }

    /// An interactive session keeps everything except the background-only
    /// filer. Two tools that both file a task would be a coin flip.
    #[test]
    fn an_interactive_session_keeps_file_task_and_hides_file_followup() {
        let names = tool_names(McpMode::Interactive);
        assert!(names.iter().any(|n| n == "file_task"));
        assert!(names.iter().any(|n| n == "complete_task"));
        assert!(
            !names.iter().any(|n| n == "file_followup"),
            "file_followup is background-only: {names:?}"
        );
    }

    /// **The recursion guard.** A follow-up that could itself run unattended
    /// closes the loop: the runner drains it, that run files more, and nothing
    /// in the system stops it. The gates are forced in code rather than left to
    /// default, because "the caller didn't ask for it" and "the caller cannot
    /// have it" are different guarantees and only the second one holds when the
    /// caller is a model.
    ///
    /// Uses a real file DB — `:memory:` gives each `open_db` its own empty
    /// database, so the tool would file into one and the assertions would read
    /// another.
    #[tokio::test]
    async fn a_filed_followup_is_visible_linked_and_never_runs_unattended() {
        let dir = std::env::temp_dir().join(format!("cxtasks-followup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let parent = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            crate::db::schema::initialize(&conn).unwrap();
            create(
                &conn,
                &NewTask {
                    title: "the delegated task".into(),
                    project: Some("pocketbuddy".into()),
                    repo_path: Some("/tmp/cx-repo/finance-app".into()),
                    bg_allowed: Some(true),
                    bg_writes_allowed: Some(true),
                    ..Default::default()
                },
            )
            .unwrap()
        };

        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Background);
        server
            .file_followup(Parameters(FileFollowupParams {
                task_id: parent.id.clone(),
                title: "retry the webhook on 429".into(),
                body: Some("found while doing the other thing".into()),
                priority: None,
            }))
            .await
            .expect("file_followup should succeed");

        let conn = rusqlite::Connection::open(&db).unwrap();
        let related = db::relations::related(&conn, &parent.id).unwrap();
        assert_eq!(
            related.len(),
            1,
            "the follow-up must link to its source task"
        );
        let kid = db::tasks::get(&conn, &related[0].task_id).unwrap().unwrap();
        assert!(
            kid.parent_id.is_none(),
            "out-of-scope work is a separate task"
        );
        assert_eq!(
            db::relations::related(&conn, &kid.id).unwrap()[0].task_id,
            parent.id
        );
        assert!(db::tasks::list_children(&conn, &parent.id)
            .unwrap()
            .is_empty());
        for view in [db::tasks::View::Open, db::tasks::View::All] {
            let rows = db::tasks::list(
                &conn,
                &db::tasks::ListFilters {
                    view: Some(view),
                    project: parent.project.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(
                rows.iter().any(|t| t.id == kid.id),
                "the follow-up must be visible in {view:?}"
            );
        }
        assert!(
            !kid.bg_allowed,
            "a follow-up must never be queueable on its own"
        );
        assert!(!kid.bg_writes_allowed);
        assert!(!kid.bg_shell_allowed);
        assert_eq!(
            kid.status, "inbox",
            "machine-proposed work arrives untriaged"
        );
        // Inherited rather than asked for: letting an unattended run name a
        // project is how you manufacture a phantom one (invariant 17).
        assert_eq!(kid.project.as_deref(), Some("pocketbuddy"));
        assert_eq!(kid.repo_path.as_deref(), Some("/tmp/cx-repo/finance-app"));

        // Archiving an investigation must not bury its next
        // implementation pass with the source task.
        db::tasks::archive(&conn, &parent.id).unwrap();
        let open = db::tasks::list(
            &conn,
            &db::tasks::ListFilters {
                view: Some(db::tasks::View::All),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(open.iter().any(|t| t.id == kid.id));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_followup_is_not_saved_without_its_source_link() {
        let (dir, db) = test_db("followup-rollback");
        let parent = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            let parent = create(
                &conn,
                &NewTask {
                    title: "investigate capture latency".into(),
                    ..Default::default()
                },
            )
            .unwrap();
            conn.execute_batch(
                "CREATE TRIGGER refuse_link BEFORE INSERT ON task_relations
                BEGIN SELECT RAISE(ABORT, 'test link failure'); END;",
            )
            .unwrap();
            parent
        };
        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Background);
        let result = server
            .file_followup(Parameters(FileFollowupParams {
                task_id: parent.id.clone(),
                title: "bound capture enrichment".into(),
                body: None,
                priority: None,
            }))
            .await;
        assert!(
            result.is_err(),
            "a failed source link must refuse the whole filing"
        );
        let conn = rusqlite::Connection::open(&db).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "a failed filing must leave only the source task");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn an_unfiled_followup_is_visible_in_inbox() {
        let (dir, db) = test_db("followup-inbox");
        let parent = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            create(
                &conn,
                &NewTask {
                    title: "investigate a task with no project".into(),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Background);
        server
            .file_followup(Parameters(FileFollowupParams {
                task_id: parent.id.clone(),
                title: "implement the follow-up".into(),
                body: None,
                priority: None,
            }))
            .await
            .unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        let related = db::relations::related(&conn, &parent.id).unwrap();
        let inbox = db::tasks::list(
            &conn,
            &db::tasks::ListFilters {
                view: Some(db::tasks::View::Inbox),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(inbox.iter().any(|t| t.id == related[0].task_id));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn view_parsing_accepts_aliases_and_rejects_junk() {
        assert_eq!(parse_view(None).unwrap(), db::tasks::View::All);
        assert_eq!(parse_view(Some("Today")).unwrap(), db::tasks::View::Today);
        assert_eq!(parse_view(Some("review")).unwrap(), db::tasks::View::InReview);
        // The pre-v12 spelling still resolves — an older installed copy of the
        // plugin passes it verbatim.
        assert_eq!(
            parse_view(Some("needs_review")).unwrap(),
            db::tasks::View::InReview
        );
        assert!(parse_view(Some("yesterday")).is_err());
    }

    #[test]
    fn repo_path_must_be_absolute_and_exist() {
        assert!(validate_repo_path("Projects/cxventures").is_err());
        assert!(validate_repo_path("/definitely/not/here/12345").is_err());
        let tmp = std::env::temp_dir();
        let ok = validate_repo_path(&tmp.to_string_lossy()).unwrap();
        assert!(Path::new(&ok).is_dir());
    }

    // ── queueing through update_task ──────────────────────────────────────

    fn test_db(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("cxtasks-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            crate::db::schema::initialize(&conn).unwrap();
        }
        (dir, db)
    }

    /// An unknown key must be REFUSED, never dropped on the floor. Serde's
    /// default is to ignore one, and `Parameters<P>` is `#[serde(transparent)]`
    /// over these structs, so for as long as they lacked
    /// `deny_unknown_fields` every tool on this surface silently accepted
    /// arguments it then did nothing with.
    ///
    /// It cost a real write. On 2026-09-19 a session updating T93 called
    /// `update_task` with `notes` and `due` — the names this tool does NOT use
    /// (`body` and `due_at`) — and got back `"Updated."` having written
    /// neither; only the third key in the same call, `defer_until`, landed. The
    /// session believed the task's baseline had been rewritten, and the stale
    /// one would have been replayed to the next dispatch two weeks later. A
    /// refusal is the only safe answer here, because the caller's intent was to
    /// SET something and a partial write is indistinguishable from a whole one
    /// in the reply.
    ///
    /// Asserted in both directions: the two near-miss aliases are refused and
    /// name themselves, and the call is accepted once they are spelled
    /// correctly. The positive half is the one that stops a future
    /// `deny_unknown_fields` sweep from quietly breaking every valid call.
    #[test]
    fn update_task_refuses_an_unknown_field_instead_of_ignoring_it() {
        for bad in ["notes", "due"] {
            let err = serde_json::from_value::<UpdateTaskParams>(serde_json::json!({
                "task_id": "T93",
                bad: "some value",
            }))
            .expect_err("an unknown field must be refused, not ignored");
            let msg = err.to_string();
            assert!(
                msg.contains(bad),
                "the refusal must name the offending key, got: {msg}"
            );
        }

        let ok = serde_json::from_value::<UpdateTaskParams>(serde_json::json!({
            "task_id": "T93",
            "body": "the notes",
            "due_at": "2026-10-03T13:00:00.000Z",
        }))
        .expect("the correctly-spelled fields must still deserialize");
        assert_eq!(ok.body.as_deref(), Some("the notes"));
        assert_eq!(ok.due_at.as_deref(), Some("2026-10-03T13:00:00.000Z"));
    }

    /// The same rule, proven to be surface-wide rather than one struct's.
    /// `file_task` and `comment_task` are the two other tools a session writes
    /// through most, and a sweep that missed one would leave exactly the hole
    /// this fix exists to close.
    #[test]
    fn every_write_tool_refuses_an_unknown_field() {
        serde_json::from_value::<FileTaskParams>(serde_json::json!({
            "title": "x",
            "why": "chris-asked",
            "notes": "wrong name for body",
        }))
        .expect_err("file_task must refuse an unknown field");

        serde_json::from_value::<CommentTaskParams>(serde_json::json!({
            "task_id": "T93",
            "body": "x",
            "hand_off": true,
        }))
        .expect_err("comment_task must refuse an unknown field");

        serde_json::from_value::<CompleteTaskParams>(serde_json::json!({
            "task_id": "T93",
            "results": "typo for result",
        }))
        .expect_err("complete_task must refuse an unknown field");
    }

    fn update_params(task_id: &str) -> UpdateTaskParams {
        UpdateTaskParams {
            task_id: task_id.into(),
            title: None,
            body: None,
            status: None,
            blocked_reason: None,
            priority: None,
            project: None,
            due_at: None,
            defer_until: None,
            recurrence: None,
            repo_path: None,
            prompt: None,
            bg_allowed: None,
            bg_remote_allowed: None,
            bg_mcp_servers: None,
            bg_executor: None,
            bg_writes_allowed: None,
            bg_shell_allowed: None,
            model: None,
            bg_timeout_minutes: None,
            bg_run_token_cap: None,
            tripwire: None,
            tripwire_check_every: None,
            tripwire_on_trip: None,
            check_tripwire: None,
            rearm_tripwire: None,
        }
    }

    /// `status: "queued"` used to be an unguarded write: the DB layer accepts
    /// every real status and `USER_STATUSES` is enforced only by the app's
    /// dropdown, so the MCP was a third route into the drain queue that ran
    /// none of `queue_background`'s checks (gotcha #18's every-entry-point
    /// lesson) — and a task queued without the opt-in never matches the drain
    /// query, so it sat at "Queued" forever. This pins the guard that is
    /// deterministic on every machine: no repo, no write.
    #[tokio::test]
    async fn update_task_cannot_queue_a_task_without_a_repo() {
        let (dir, db) = test_db("mcp-queue-norepo");
        let t = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            create(
                &conn,
                &NewTask {
                    title: "no repo".into(),
                    ..Default::default()
                },
            )
            .unwrap()
        };

        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        let mut params = update_params(&t.id);
        params.status = Some("queued".into());
        let err = server.update_task(Parameters(params)).await.unwrap_err();
        assert!(format!("{err:?}").contains("repo path"), "{err:?}");

        let conn = rusqlite::Connection::open(&db).unwrap();
        let after = db::tasks::get(&conn, &t.id).unwrap().unwrap();
        assert_eq!(after.status, "inbox", "a refused queue must not move the status");
        assert!(!after.bg_allowed, "a refused queue must not opt the task in");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Edits in the same call land BEFORE the guards run — a `repo_path`
    /// passed alongside `status: "queued"` is what preflight sees. Tolerant of
    /// a machine without the claude CLI (the runner tests' pattern): there the
    /// queue half refuses, the edits half must STILL have landed, and the
    /// error must say both.
    #[tokio::test]
    async fn update_task_applies_field_edits_before_queueing() {
        let (dir, db) = test_db("mcp-queue-edits");
        let t = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            create(
                &conn,
                &NewTask {
                    title: "queue me".into(),
                    ..Default::default()
                },
            )
            .unwrap()
        };

        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        let mut params = update_params(&t.id);
        params.status = Some("queued".into());
        params.repo_path = Some(dir.to_string_lossy().to_string());
        let result = server.update_task(Parameters(params)).await;

        let conn = rusqlite::Connection::open(&db).unwrap();
        let after = db::tasks::get(&conn, &t.id).unwrap().unwrap();
        assert!(after.repo_path.is_some(), "the edit half must land either way");
        match result {
            Ok(res) => {
                assert_eq!(after.status, "queued");
                assert!(after.bg_allowed, "queueing IS the opt-in");
                assert!(format!("{res:?}").contains("Queued for"), "{res:?}");
            }
            Err(e) => {
                let msg = format!("{e:?}");
                assert!(msg.contains("claude"), "only a missing CLI may refuse here: {msg}");
                assert!(msg.contains("field edits were applied"), "{msg}");
                assert_eq!(after.status, "inbox");
                assert!(!after.bg_allowed);
            }
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── create_project ────────────────────────────────────────────────────

    fn create_project_params(name: &str, repo: Option<&str>) -> CreateProjectParams {
        CreateProjectParams {
            name: name.into(),
            repo_path: repo.map(str::to_string),
            kind: None,
            parent: None,
        }
    }

    /// The point of the tool is the MAPPING, not the row: a project record was
    /// always reachable from outside the app (`file_task` with an explicit
    /// `project` calls `ensure_exists`), while `project_repos` was not. So the
    /// assertion that matters is that `for_repo_path` — the lookup `file_task`
    /// runs on every filing — answers afterwards, including from a
    /// subdirectory, which is where an agent's cwd usually is.
    #[tokio::test]
    async fn create_project_registers_the_directory_it_was_called_from() {
        let (dir, db) = test_db("mcp-newproj");
        let repo = dir.join("cxtasks-repo");
        let nested = repo.join("src-tauri/src");
        std::fs::create_dir_all(&nested).unwrap();

        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        let res = server
            .create_project(Parameters(create_project_params(
                "cxtasks",
                Some(&repo.to_string_lossy()),
            )))
            .await
            .unwrap();
        let out = format!("{res:?}");
        assert!(out.contains("created"), "{out}");
        assert!(out.contains("primary repo"), "the first repo is the primary: {out}");

        let conn = rusqlite::Connection::open(&db).unwrap();
        assert_eq!(
            db::projects::for_repo_path(&conn, &repo.to_string_lossy())
                .unwrap()
                .as_deref(),
            Some("cxtasks")
        );
        assert_eq!(
            db::projects::for_repo_path(&conn, &nested.to_string_lossy())
                .unwrap()
                .as_deref(),
            Some("cxtasks"),
            "an agent's cwd is usually a subdirectory — it must resolve too"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Get-or-create, not create-or-fail. The call an agent makes to fix an
    /// unregistered repo names a project that very often already exists, and
    /// erroring there leaves it with a refusal it cannot act on. The stored
    /// spelling wins over the caller's, so 'CXTasks' can never fork 'cxtasks'
    /// into a second board.
    #[tokio::test]
    async fn create_project_reuses_an_existing_project_instead_of_duplicating_it() {
        let (dir, db) = test_db("mcp-reuseproj");
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            db::projects::create(&conn, "cxtasks", db::projects::ProjectKind::Area, None).unwrap();
        }

        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        let res = server
            .create_project(Parameters(create_project_params(
                "CXTasks",
                Some(&repo.to_string_lossy()),
            )))
            .await
            .unwrap();
        let out = format!("{res:?}");
        assert!(out.contains("already existed"), "the result must say which happened: {out}");

        let conn = rusqlite::Connection::open(&db).unwrap();
        let projects = db::projects::list(&conn, true).unwrap();
        assert_eq!(projects.len(), 1, "a second board is the failure: {projects:?}");
        assert_eq!(projects[0].name, "cxtasks", "the stored spelling wins");
        assert_eq!(
            db::projects::for_repo_path(&conn, &repo.to_string_lossy())
                .unwrap()
                .as_deref(),
            Some("cxtasks"),
            "reusing the project must still do the mapping that was asked for"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// **A refused mapping must leave nothing behind.** Creating the project
    /// first and validating the repo second would answer "that directory
    /// belongs to someone else" by manufacturing exactly the empty duplicate
    /// project the refusal exists to prevent.
    ///
    /// The fixture uses a SUBDIRECTORY of the owned repo on purpose:
    /// `add_repo`'s own guard is the UNIQUE index on `repo_path`, so it sees
    // ── update_project (PARA lifecycle) ───────────────────────────────────

    fn update_project_params(project: &str) -> UpdateProjectParams {
        UpdateProjectParams {
            project: project.into(),
            kind: None,
            parent: None,
            target_date: None,
            closed: None,
        }
    }

    fn text_of(r: &CallToolResult) -> String {
        r.content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// T269: `update_task` told agents "to drop a task, archive it" and no tool
    /// could. Both directions, by T-number, through the shared verb.
    #[tokio::test]
    async fn a_task_can_be_archived_and_unarchived_from_a_session() {
        let (dir, db) = test_db("mcp-archive");
        let (id, short) = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            let t = create(&conn, &NewTask { title: "drop me".into(), ..Default::default() }).unwrap();
            (t.id, t.short_id)
        };
        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        let tref = format!("T{}", short.expect("a created task gets a T-number"));
        let archived_at = || -> Option<String> {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.query_row("SELECT archived_at FROM tasks WHERE id = ?1", [&id], |r| r.get(0))
                .unwrap()
        };

        let reply = text_of(
            &server
                .archive_task(Parameters(GetTaskParams { task_id: tref.clone() }))
                .await
                .unwrap(),
        );
        assert!(reply.starts_with("Archived: drop me"), "{reply}");
        assert!(archived_at().is_some());

        let reply = text_of(
            &server
                .unarchive_task(Parameters(GetTaskParams { task_id: tref }))
                .await
                .unwrap(),
        );
        assert!(reply.starts_with("Unarchived: drop me"), "{reply}");
        assert!(archived_at().is_none());

        // And neither is reachable from an unattended run.
        for mode in [McpMode::Background, McpMode::Check] {
            let names = tool_names(mode);
            assert!(!names.iter().any(|n| n.contains("archive")), "{names:?}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The reason this tool exists: a wave of work filed from a session could be
    /// started and never ended, because closing lived only in the app's menu.
    #[tokio::test]
    async fn a_finished_project_can_be_closed_from_a_session() {
        let (_dir, db) = test_db("mcp-projclose");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            db::projects::create(&conn, "cxventures", db::projects::ProjectKind::Area, None)
                .unwrap();
            db::projects::create(
                &conn,
                "gov contracting",
                db::projects::ProjectKind::Project,
                Some("cxventures"),
            )
            .unwrap();
        }
        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        let out = server
            .update_project(Parameters(UpdateProjectParams {
                closed: Some(true),
                ..update_project_params("gov contracting")
            }))
            .await
            .unwrap();
        assert!(text_of(&out).contains("CLOSED"), "{}", text_of(&out));

        let conn = rusqlite::Connection::open(&db).unwrap();
        let p = db::projects::get_by_name(&conn, "gov contracting").unwrap().unwrap();
        assert!(p.closed_at.is_some());
        assert!(p.archived, "closing files it away too");
    }

    /// Closing around unfinished work is REPORTED, never refused — an agent
    /// finishing the last task of a wave must be able to close it, and one
    /// closing a wave with six tasks left must be told what it walked away from.
    #[tokio::test]
    async fn closing_a_project_with_open_tasks_says_so_instead_of_refusing() {
        let (_dir, db) = test_db("mcp-projcloseopen");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            db::projects::create(&conn, "gov contracting", db::projects::ProjectKind::Project, None)
                .unwrap();
            crate::db::tasks::create(
                &conn,
                &crate::db::tasks::NewTask {
                    title: "file EBO".into(),
                    project: Some("gov contracting".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        let out = text_of(
            &server
                .update_project(Parameters(UpdateProjectParams {
                    closed: Some(true),
                    ..update_project_params("gov contracting")
                }))
                .await
                .unwrap(),
        );
        assert!(out.contains("CLOSED"), "{out}");
        assert!(out.contains("still open"), "{out}");

        let conn = rusqlite::Connection::open(&db).unwrap();
        assert!(
            crate::db::tasks::get(&conn, &crate::db::tasks::list(
                &conn,
                &crate::db::tasks::ListFilters::default()
            )
            .unwrap()[0]
                .id)
            .unwrap()
            .is_some(),
            "closing a project must not touch its tasks"
        );
    }

    /// An empty string is the only way to say "no area" — JSON `null` and an
    /// omitted key both arrive as `None`, so without the sentinel a project
    /// nested from a session could never be un-nested from one.
    #[tokio::test]
    async fn an_empty_parent_lifts_a_project_out_and_an_absent_one_leaves_it() {
        let (_dir, db) = test_db("mcp-projparent");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            db::projects::create(&conn, "cxventures", db::projects::ProjectKind::Area, None)
                .unwrap();
            db::projects::create(
                &conn,
                "gov contracting",
                db::projects::ProjectKind::Project,
                Some("cxventures"),
            )
            .unwrap();
        }
        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);

        // Absent: untouched.
        server
            .update_project(Parameters(UpdateProjectParams {
                target_date: Some("2026-10-02T16:00:00.000Z".into()),
                ..update_project_params("gov contracting")
            }))
            .await
            .unwrap();
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            let p = db::projects::get_by_name(&conn, "gov contracting").unwrap().unwrap();
            assert_eq!(p.parent.as_deref(), Some("cxventures"));
            assert!(p.target_date.is_some());
        }

        // Empty: lifted out.
        server
            .update_project(Parameters(UpdateProjectParams {
                parent: Some(String::new()),
                ..update_project_params("gov contracting")
            }))
            .await
            .unwrap();
        let conn = rusqlite::Connection::open(&db).unwrap();
        assert!(db::projects::get_by_name(&conn, "gov contracting")
            .unwrap()
            .unwrap()
            .parent
            .is_none());
    }

    /// The depth cap and "an area has no finish line" are enforced in
    /// `db::projects`; this asserts the tool REPORTS them rather than
    /// re-implementing a second, drifting copy.
    #[tokio::test]
    async fn the_para_rules_come_back_as_named_refusals() {
        let (_dir, db) = test_db("mcp-pararules");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            db::projects::create(&conn, "cxventures", db::projects::ProjectKind::Area, None)
                .unwrap();
            db::projects::create(&conn, "artist advisory", db::projects::ProjectKind::Area, None)
                .unwrap();
        }
        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);

        // An area has no finish line.
        let err = server
            .update_project(Parameters(UpdateProjectParams {
                closed: Some(true),
                ..update_project_params("cxventures")
            }))
            .await
            .unwrap_err();
        assert!(format!("{err:?}").contains("areas don't end"), "{err:?}");

        // Areas nest in areas now — but never in a loop.
        server
            .update_project(Parameters(UpdateProjectParams {
                parent: Some("cxventures".into()),
                ..update_project_params("artist advisory")
            }))
            .await
            .unwrap();
        let err = server
            .update_project(Parameters(UpdateProjectParams {
                parent: Some("artist advisory".into()),
                ..update_project_params("cxventures")
            }))
            .await
            .unwrap_err();
        assert!(format!("{err:?}").contains("out first"), "{err:?}");

        // And a name that isn't there says where to look, rather than 404ing.
        let err = server
            .update_project(Parameters(update_project_params("nope")))
            .await
            .unwrap_err();
        assert!(format!("{err:?}").contains("project_board"), "{err:?}");
    }

    /// Areas nest to any depth, so the board is a tree: each node once, one
    /// step further in per level. The one-level version printed a nested area
    /// twice — at the top, and again under its parent.
    #[test]
    fn the_board_prints_a_nested_tree_with_each_node_once() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::initialize(&conn).unwrap();
        use db::projects::{create, ProjectKind};
        create(&conn, "products", ProjectKind::Area, None).unwrap();
        create(&conn, "artist advisory", ProjectKind::Area, Some("products")).unwrap();
        create(&conn, "retitle", ProjectKind::Project, Some("artist advisory")).unwrap();
        let out = format_board(&conn, &db::projects::board(&conn).unwrap());
        let line = |name: &str| out.lines().filter(|l| l.contains(name)).collect::<Vec<_>>();
        assert_eq!(line("artist advisory").len(), 1, "{out}");
        assert!(line("artist advisory")[0].starts_with("    └ "), "{out}");
        assert!(line("retitle")[0].starts_with("        └ "), "{out}");
    }

    /// Kind is applied BEFORE the rest, so one call can demote a node and date
    /// it. Ordered the other way, promoting to an area would silently discard
    /// the date the same call just asked for.
    #[tokio::test]
    async fn kind_is_applied_before_the_fields_only_a_project_may_carry() {
        let (_dir, db) = test_db("mcp-projorder");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            db::projects::create(&conn, "wave 0", db::projects::ProjectKind::Area, None).unwrap();
        }
        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        server
            .update_project(Parameters(UpdateProjectParams {
                kind: Some("project".into()),
                target_date: Some("2026-10-02T16:00:00.000Z".into()),
                ..update_project_params("wave 0")
            }))
            .await
            .unwrap();

        let conn = rusqlite::Connection::open(&db).unwrap();
        let p = db::projects::get_by_name(&conn, "wave 0").unwrap().unwrap();
        assert_eq!(p.kind, db::projects::ProjectKind::Project);
        assert!(p.target_date.is_some(), "the date set in the same call survived");
    }

    /// only an exact collision and this case sails straight past it into a
    /// competing row that then wins every lookup beneath itself (resolution is
    /// longest-prefix). Only the `for_repo_path` pre-check catches it.
    #[tokio::test]
    async fn a_refused_repo_mapping_leaves_no_project_behind() {
        let (dir, db) = test_db("mcp-projconflict");
        let repo = dir.join("finance-app");
        let inside = repo.join("src");
        std::fs::create_dir_all(&inside).unwrap();
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            db::projects::add_repo(&conn, "pocketbuddy", &repo.to_string_lossy(), true).unwrap();
        }

        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        let err = server
            .create_project(Parameters(create_project_params(
                "finance-app",
                Some(&inside.to_string_lossy()),
            )))
            .await
            .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("pocketbuddy"), "the refusal must name the owner: {msg}");

        let conn = rusqlite::Connection::open(&db).unwrap();
        assert!(
            db::projects::get_by_name(&conn, "finance-app").unwrap().is_none(),
            "a refused mapping that still creates the project is the duplicate it was refusing"
        );
        assert_eq!(
            db::projects::list_repos(&conn, "pocketbuddy").unwrap().len(),
            1,
            "and no competing row under the owner's repo"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `-1` clears an override; a plain value sets it; omission leaves it
    /// alone. The three cases are the whole contract of an integer field on a
    /// patch tool, and the middle one is the only one a plain `Option<i64>`
    /// expresses on its own — without the sentinel an override could be set
    /// from a session and never removed from one.
    #[test]
    fn the_clear_sentinel_is_the_only_way_to_restore_a_default() {
        assert_eq!(clear_or_set(None), None, "omitted = leave alone");
        assert_eq!(clear_or_set(Some(60)), Some(Some(60)), "a value sets it");
        assert_eq!(clear_or_set(Some(-1)), Some(None), "-1 restores the default");
        // 0 must survive as a VALUE: it disables the token ceiling, and folding
        // it into the clear case would silently re-impose the 25M default on a
        // task that asked for none.
        assert_eq!(clear_or_set(Some(0)), Some(Some(0)), "0 is a value, not a clear");
    }

    /// An override is only worth setting if a session can see it was set. It is
    /// printed ONLY when overridden — rendering the effective value on every
    /// task would make the global default read as a per-task decision.
    #[tokio::test]
    async fn a_budget_override_is_visible_in_the_task_detail() {
        let (dir, db) = test_db("mcp-budget-detail");
        let t = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            create(
                &conn,
                &NewTask {
                    title: "big refactor".into(),
                    repo_path: Some(dir.to_string_lossy().to_string()),
                    bg_allowed: Some(true),
                    bg_timeout_minutes: Some(60),
                    bg_run_token_cap: Some(60_000_000),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let detail = format_detail(&t, &[], &[], None);
        assert!(detail.contains("budget:"), "{detail}");
        assert!(detail.contains("60 min"), "{detail}");
        assert!(detail.contains("60M tokens"), "{detail}");

        let plain = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            create(
                &conn,
                &NewTask {
                    title: "ordinary".into(),
                    repo_path: Some(dir.to_string_lossy().to_string()),
                    bg_allowed: Some(true),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        assert!(
            !format_detail(&plain, &[], &[], None).contains("budget:"),
            "a task on the defaults must not advertise a budget line"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Filing straight into the queue is refused with the remedy named — the
    /// `cancelled` precedent. Nothing is created: a half-filed task would be
    /// worse than the refusal.
    #[tokio::test]
    async fn file_task_refuses_to_file_straight_into_the_queue() {
        let (dir, db) = test_db("mcp-file-queued");

        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        let err = server
            .file_task(Parameters(FileTaskParams {
                title: "straight to the queue".into(),
                why: Some("chris-asked".into()),
                decision: None,
                body: None,
                project: None,
                priority: None,
                due_at: None,
                defer_until: None,
                recurrence: None,
                repo_path: Some(dir.to_string_lossy().to_string()),
                prompt: None,
                bg_allowed: Some(true),
                detached: false,
                bg_mcp_servers: None,
                bg_executor: None,
                bg_shell_allowed: None,
                bg_writes_allowed: None,
                status: Some("queued".into()),
                model: None,
                tags: None,
                new_tags: None,
                blocked_by: None,
                section: None,
                bg_timeout_minutes: None,
                bg_run_token_cap: None,
                tripwire: None,
                tripwire_check_every: None,
                tripwire_on_trip: None,
            }))
            .await
            .unwrap_err();
        assert!(format!("{err:?}").contains("update_task"), "{err:?}");

        let conn = rusqlite::Connection::open(&db).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "a refused filing must create nothing");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The smallest legal filing: a title and a `why`. Everything else is what
    /// `file_task` treats as absent.
    fn plain_filing(title: &str, why: Option<&str>) -> FileTaskParams {
        FileTaskParams {
            title: title.into(),
            why: why.map(str::to_string),
            decision: None,
            body: None,
            project: None,
            priority: None,
            due_at: None,
            defer_until: None,
            recurrence: None,
            repo_path: None,
            prompt: None,
            bg_allowed: None,
            detached: false,
            bg_mcp_servers: None,
            bg_executor: None,
            bg_shell_allowed: None,
            bg_writes_allowed: None,
            status: None,
            model: None,
            tags: None,
            new_tags: None,
            blocked_by: None,
            section: None,
            bg_timeout_minutes: None,
            bg_run_token_cap: None,
            tripwire: None,
            tripwire_check_every: None,
            tripwire_on_trip: None,
        }
    }

    fn task_count(db: &Path) -> i64 {
        rusqlite::Connection::open(db)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap()
    }

    /// Invariant 28: a filing that does not say why it isn't being done now is
    /// refused — with the four values AND the question named, which is the
    /// whole reason `why` is validated here and not by the deserializer. An
    /// empty string passes the schema and must land in the same place.
    #[tokio::test]
    async fn file_task_refuses_an_undeclared_why() {
        let (dir, db) = test_db("mcp-file-why-missing");
        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);

        for why in [None, Some(""), Some("   ")] {
            let err = server
                .file_task(Parameters(plain_filing("undeclared", why)))
                .await
                .unwrap_err();
            let text = format!("{err:?}");
            for value in FILING_REASONS {
                assert!(text.contains(value), "why={why:?}: must name {value}: {text}");
            }
            assert!(text.contains("why not now"), "why={why:?}: {text}");
        }
        assert_eq!(task_count(&db), 0, "a refused filing must create nothing");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A value outside the four is the same refusal, echoing what was sent so
    /// a near-miss ("wont_fit", "because") is diagnosable from the error alone.
    #[tokio::test]
    async fn file_task_refuses_an_unknown_why() {
        let (dir, db) = test_db("mcp-file-why-unknown");
        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);

        let err = server
            .file_task(Parameters(plain_filing("unknown reason", Some("because"))))
            .await
            .unwrap_err();
        let text = format!("{err:?}");
        assert!(text.contains("because"), "{text}");
        for value in FILING_REASONS {
            assert!(text.contains(value), "must name {value}: {text}");
        }
        assert_eq!(task_count(&db), 0, "a refused filing must create nothing");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The persisted half of invariant 28: a self-initiated filing carries the
    /// `agent-filed` tag and says so in its reply; Chris asking carries nothing,
    /// because that path has no bar to leave a trace of.
    /// The tag vocabulary guard, end to end through both tools. A refused
    /// `file_task` must create NO row — otherwise the agent's retry files the
    /// task twice — and the refusal must name the existing tag to reuse.
    #[tokio::test]
    async fn new_tags_are_vetted_before_anything_is_written() {
        let (dir, db) = test_db("mcp-tag-vet");
        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);

        // Seed the vocabulary deliberately.
        let mut first = plain_filing("seed", Some(WHY_CHRIS_ASKED));
        first.tags = Some(vec!["certification".into()]);
        first.new_tags = Some(true);
        server.file_task(Parameters(first)).await.unwrap();

        // A near-duplicate is refused, names the existing tag, and files nothing.
        let mut dup = plain_filing("dup", Some(WHY_CHRIS_ASKED));
        dup.tags = Some(vec!["certs".into()]);
        let err = format!("{:?}", server.file_task(Parameters(dup)).await.unwrap_err());
        assert!(err.contains("#certification"), "{err}");
        assert_eq!(task_count(&db), 1, "a refused filing must create nothing");

        // An unknown tag is refused on tag_task, and allowed with new_tags.
        let tag = |tags: &[&str], new_tags: Option<bool>| TagTaskParams {
            task_id: "T1".into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            remove: None,
            new_tags,
        };
        let err = format!("{:?}", server.tag_task(Parameters(tag(&["roth"], None))).await.unwrap_err());
        assert!(err.contains("new_tags: true"), "{err}");
        assert!(err.contains("#certification"), "the reply lists tags in use: {err}");
        server.tag_task(Parameters(tag(&["roth"], Some(true)))).await.unwrap();

        // Existing, person and decision tags pass with no flag.
        server
            .tag_task(Parameters(tag(&["certification", "@sam", "decision"], None)))
            .await
            .unwrap();

        // A field name is refused even with the override.
        let err = format!(
            "{:?}",
            server.tag_task(Parameters(tag(&["tripwire"], Some(true)))).await.unwrap_err()
        );
        assert!(err.contains("restates"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn file_task_tags_a_self_initiated_filing() {
        let (dir, db) = test_db("mcp-file-why-tag");
        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);

        let tag_names = |title: &str| -> Vec<String> {
            let conn = rusqlite::Connection::open(&db).unwrap();
            let id: String = conn
                .query_row("SELECT id FROM tasks WHERE title = ?1", [title], |r| r.get(0))
                .unwrap();
            db::tags::for_task(&conn, &id)
                .unwrap()
                .into_iter()
                .map(|t| t.name)
                .collect()
        };

        let out = server
            .file_task(Parameters(plain_filing("on my own", Some("wont-fit"))))
            .await
            .unwrap();
        let reply = format!("{out:?}");
        assert!(reply.contains("why: wont-fit"), "{reply}");
        assert!(reply.contains("search_tasks"), "{reply}");
        assert_eq!(tag_names("on my own"), vec![AGENT_FILED_TAG.to_string()]);

        let out = server
            .file_task(Parameters(plain_filing("he asked", Some(WHY_CHRIS_ASKED))))
            .await
            .unwrap();
        let reply = format!("{out:?}");
        assert!(!reply.contains("Self-filed"), "{reply}");
        assert!(tag_names("he asked").is_empty(), "Chris asking leaves no mark");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every write that puts prose on the pane says, in its reply, what the
    /// pane will drop — and says nothing when everything renders, so the line
    /// stays worth reading (`crate::prose`).
    #[tokio::test]
    async fn write_replies_name_what_the_app_will_not_render() {
        let (dir, db) = test_db("mcp-render-check");
        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        let id_of = |title: &str| -> String {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.query_row("SELECT id FROM tasks WHERE title = ?1", [title], |r| r.get(0))
                .unwrap()
        };

        // A clean, structured note — diagram included — earns no warning.
        let mut clean = plain_filing("clean note", Some(WHY_CHRIS_ASKED));
        clean.body = Some(
            "## State\n\n- [ ] step `Vec<String>`\n\n<svg viewBox=\"0 0 10 10\"><path d=\"M0,0\"/></svg>"
                .into(),
        );
        let reply = text_of(&server.file_task(Parameters(clean)).await.unwrap());
        assert!(!reply.contains("RENDER CHECK"), "{reply}");

        // file_task: a generic outside backticks.
        let mut dirty = plain_filing("dirty note", Some(WHY_CHRIS_ASKED));
        dirty.body = Some("returns Vec<String> now".into());
        let reply = text_of(&server.file_task(Parameters(dirty)).await.unwrap());
        assert!(reply.contains("RENDER CHECK") && reply.contains("<String>"), "{reply}");
        assert!(reply.contains("update_task(body)"), "notes can be rewritten: {reply}");
        let id = id_of("dirty note");

        // The original complaint: one long unbroken paragraph.
        let mut wall = plain_filing("wall note", Some(WHY_CHRIS_ASKED));
        wall.body = Some("This sentence keeps the paragraph going a while longer. ".repeat(14));
        let reply = text_of(&server.file_task(Parameters(wall)).await.unwrap());
        assert!(reply.contains("RENDER CHECK") && reply.contains("unbroken"), "{reply}");

        // update_task: only a body THIS call wrote is checked.
        let mut patch = update_params(&id);
        patch.priority = Some(1);
        let reply = text_of(&server.update_task(Parameters(patch)).await.unwrap());
        assert!(!reply.contains("RENDER CHECK"), "a status edit re-checked old notes: {reply}");
        let mut patch = update_params(&id);
        patch.body = Some("<div onclick=\"x()\">hi</div>".into());
        let reply = text_of(&server.update_task(Parameters(patch)).await.unwrap());
        assert!(reply.contains("RENDER CHECK") && reply.contains("onclick"), "{reply}");

        // comment_task and complete_task.
        let reply = text_of(
            &server
                .comment_task(Parameters(comment_params(&id, "<script>x</script> done")))
                .await
                .unwrap(),
        );
        assert!(reply.contains("RENDER CHECK") && reply.contains("<script>"), "{reply}");
        assert!(
            !reply.contains("update_task") && reply.contains("can't be edited"),
            "a comment cannot be fixed with update_task: {reply}"
        );
        let reply = text_of(
            &server
                .complete_task(Parameters(CompleteTaskParams {
                    task_id: id.clone(),
                    result: Some("Fixed it.\n\n- see <a href=\"javascript:x\">here</a>".into()),
                }))
                .await
                .unwrap(),
        );
        assert!(reply.contains("RENDER CHECK") && reply.contains("javascript:"), "{reply}");
        assert!(!reply.contains("update_task"), "a result cannot be fixed with update_task: {reply}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Gotcha #48: the client shows only the first 2,048 characters. A rule
    /// past that point is not a rule, however green the tests that read the
    /// server's side are.
    #[test]
    fn instructions_fit_the_client_cap() {
        let n = INTERACTIVE_INSTRUCTIONS.chars().count();
        assert!(
            n <= CLIENT_INSTRUCTIONS_CAP,
            "INTERACTIVE_INSTRUCTIONS is {n} characters; Claude Code shows the model only \
             the first {CLIENT_INSTRUCTIONS_CAP}. Move detail into a parameter description."
        );
    }

    /// The formatting rules live on the fields they govern, because field
    /// descriptions reach the model whole and the instructions do not.
    #[test]
    fn every_prose_field_carries_the_formatting_rules() {
        fn doc<T: schemars::JsonSchema>(field: &str) -> String {
            let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
            schema["properties"][field]["description"]
                .as_str()
                .unwrap_or_else(|| panic!("no description on {field}"))
                .to_string()
        }
        for (name, text) in [
            ("file_task.body", doc::<FileTaskParams>("body")),
            ("update_task.body", doc::<UpdateTaskParams>("body")),
        ] {
            for needle in ["markdown", "backticks", "inline SVG", "var(--clay)", "RENDER CHECK", "never one long block"] {
                assert!(text.contains(needle), "{name} is missing {needle:?}: {text}");
            }
        }
        for (name, text) in [
            ("complete_task.result", doc::<CompleteTaskParams>("result")),
            ("comment_task.body", doc::<CommentTaskParams>("body")),
            ("file_followup.body", doc::<FileFollowupParams>("body")),
        ] {
            for needle in ["markdown", "backticks"] {
                assert!(text.contains(needle), "{name} is missing {needle:?}: {text}");
            }
        }
        assert!(doc::<UpdateTaskParams>("body").contains("there is no `notes` parameter"));
        assert!(notes_format!().len() < 1_200, "the rules ride on two fields; keep them tight");
    }

    #[test]
    fn repo_path_expands_tilde() {
        let home = dirs::home_dir().unwrap();
        let resolved = validate_repo_path("~/").unwrap();
        assert!(
            Path::new(&resolved).starts_with(&home),
            "expected {resolved} under {home:?}"
        );
    }

    #[test]
    fn format_line_carries_the_id_so_followups_can_address_it() {
        let t = task_with("do the thing");
        let line = format_line(&t);
        assert!(line.contains("do the thing"));
        assert!(line.contains(&t.id), "id must be printed: {line}");
        assert!(line.contains("[inbox]"));
        // Both references, for two different audiences: the T-number is what a
        // human says out loud, the UUID is what follow-up tool calls have always
        // used. Dropping either breaks one of them.
        assert!(
            line.contains(&format!("[T{}]", t.short_id.unwrap())),
            "the speakable T-number must lead the line: {line}"
        );
    }

    #[test]
    fn format_detail_shows_the_speakable_reference() {
        let t = task_with("do the thing");
        let detail = format_detail(&t, &[], &[], None);
        assert!(
            detail.contains(&format!("ref:        T{}", t.short_id.unwrap())),
            "{detail}"
        );
        assert!(detail.contains(&format!("id:         {}", t.id)));
    }

    #[test]
    fn short_ref_parsing_round_trips_through_format_line() {
        let t = task_with("x");
        let n = t.short_id.unwrap();
        assert_eq!(
            db::tasks::parse_short_ref(&short_ref(&t).unwrap()),
            Some(n),
            "what we print must be what we accept"
        );
    }

    /// Collect the `label:` tokens from an MCP output's HEADER region.
    ///
    /// Bounded at the first blank line on purpose: everything after it is the
    /// task's own prose (Notes, seed prompt, run results), which is arbitrary
    /// user text that routinely contains `word:` and would otherwise be read
    /// as a label. T93's own notes contain the line `due:` in a table, so a
    /// naive whole-output scan fails against real data.
    fn header_labels(out: &str) -> Vec<String> {
        let mut labels = Vec::new();
        for line in out.lines().skip(1) {
            if line.trim().is_empty() {
                break;
            }
            let Some(rest) = line.strip_prefix("  ") else {
                continue;
            };
            let Some((label, _)) = rest.split_once(':') else {
                continue;
            };
            let label = label.trim();
            if !label.is_empty() {
                labels.push(label.to_string());
            }
        }
        labels
    }

    /// The property names of a params struct, read from the schema so these
    /// checks cannot drift away from the struct they guard.
    fn schema_fields<T: schemars::JsonSchema>() -> Vec<String> {
        let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
        schema["properties"]
            .as_object()
            .expect("params struct must expose properties")
            .keys()
            .cloned()
            .collect()
    }

    fn assert_labels_declared(
        out: &str,
        writable: &[String],
        read_only: &[&str],
        forbidden: &[&str],
        surface: &str,
    ) {
        for label in header_labels(out) {
            // Checked FIRST, and kept out of `read_only`: a label listed as
            // read-only is ACCEPTED by the check below, so filing the old
            // broken labels there would have made the guard permit exactly
            // what it exists to forbid. They are their own list for that
            // reason.
            assert!(
                !forbidden.contains(&label.as_str()),
                "{surface} prints `{label}:` — that is a label this fix \
                 removed, and it names no writable field. Someone reverted a \
                 fix, or added a new surface carrying the old vocabulary."
            );
            assert!(
                writable.iter().any(|f| *f == label) || read_only.contains(&label.as_str()),
                "{surface} prints `{label}:` but that is not a writable field and is not \
                 declared read-only. Either name the field an agent writes it back with, \
                 or add `{label}` to the read-only list with the reason it cannot be \
                 written.\nAll labels on this surface: {:?}",
                header_labels(out)
            );
        }
    }

    /// The READ surface must not teach a name the WRITE surface does not have.
    ///
    /// Root cause of the T93 incident (2026-09-19), and the half that
    /// `deny_unknown_fields` does not address: a refusal catches a wrong name,
    /// but an agent only guesses one because something showed it. `get_task`
    /// had printed `due:`, `deferred:`, `Notes:` and `repo:` for fields
    /// written as `due_at`, `defer_until`, `body` and `repo_path`, so reading
    /// a task and writing straight back was wrong four times out of five —
    /// and `deferred:` printing in the same aligned column as `status:`
    /// supplied the fifth error, `status: "deferred"`.
    ///
    /// **The fixture must set every optional field.** The first version of
    /// this test passed while `background:` and `budget:` were undeclared,
    /// purely because its task never populated the fields that emit those
    /// lines — a label check is only worth the coverage of the row it renders.
    #[test]
    fn every_printed_label_is_a_writable_field_or_declared_read_only() {
        /// Facts no `update_task` call can set. An entry here is a claim that
        /// the thing genuinely cannot be written.
        const READ_ONLY: &[&str] = &[
            "ref",       // short_id — permanent, never reissued (invariant 14)
            "id",        // the UUID
            "created",   // bookkeeping, written by SQLite
            "completed", // moves via complete_task, never a field write
            // Derived summaries of several fields at once. They are not names
            // to write back, and each is multi-word or plainly a summary, so
            // no caller mistakes them for parameters.
            "background",
            "budget",
            "next run",
            // Tripwire sub-block: observed facts from tripwire_runtime.
            "last check",
            "failures",
            "tripped at",
            "on trip",
        ];

        /// The labels that caused the incident. NOT in `READ_ONLY` — an entry
        /// there is permitted, which is the opposite of a guard.
        const FORBIDDEN: &[&str] = &["due", "deferred", "repo", "target"];

        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "every field set".into(),
                body: Some("some notes".into()),
                project: Some("cxtasks".into()),
                due_at: Some("2026-10-03T13:00:00.000Z".into()),
                defer_until: Some("2026-10-03T13:00:00.000Z".into()),
                recurrence: Some("weekly:MO".into()),
                repo_path: Some("/tmp".into()),
                prompt: Some("do the thing".into()),
                bg_allowed: Some(true),
                bg_writes_allowed: Some(true),
                bg_timeout_minutes: Some(45),
                bg_run_token_cap: Some(30_000_000),
                ..Default::default()
            },
        )
        .unwrap();
        let detail = format_detail(&t, &[], &[], None);

        let writable = schema_fields::<UpdateTaskParams>();
        assert!(
            writable.iter().any(|f| f == "body"),
            "sanity: the schema should carry `body`, got {writable:?}"
        );
        // The fixture must actually exercise the conditional rows, or this
        // test silently shrinks back to what it covered before.
        for expected in ["background", "budget"] {
            assert!(
                header_labels(&detail).iter().any(|l| l == expected),
                "fixture no longer emits `{expected}:` — the check has lost \
                 that row's coverage. Set the fields that render it."
            );
        }
        assert_labels_declared(&detail, &writable, READ_ONLY, FORBIDDEN, "get_task");

        // The ones that actually bit, by name, so a reshuffle of the helper
        // cannot quietly stop checking them.
        assert!(detail.contains("due_at:"), "due_at must print its own name");
        assert!(
            detail.contains("defer_until:"),
            "defer_until must print its own name"
        );
        assert!(
            !detail.contains("deferred:   until"),
            "the old `deferred: until` label is what taught `status: deferred`"
        );
        assert!(
            detail.contains("`body`"),
            "the Notes heading must name the field it is written with"
        );
    }

    /// `format_project_detail` is the same surface rule, and it was MISSED by the
    /// first pass of this fix: `format_detail` was swept while `format_board`
    /// went on printing `target:` for a field `update_project` calls
    /// `target_date`. Found by an adversarial review, not by the test above —
    /// which is exactly why the check now covers both outputs rather than the
    /// one that happened to be under the nose at the time.
    #[test]
    fn the_project_board_prints_writable_project_field_names() {
        /// Derived counts and pointers, settable nowhere.
        const READ_ONLY: &[&str] = &["tasks", "do next"];
        /// `target` is the broken label this test exists to catch, so it must
        /// be forbidden rather than declared read-only.
        const FORBIDDEN: &[&str] = &["target"];

        let conn = crate::db::schema::test_conn();
        crate::db::projects::create(
            &conn,
            "BoardProj",
            crate::db::projects::ProjectKind::Project,
            None,
        )
        .unwrap();
        // A dated project, or the `target_date:` row never renders and this
        // check silently loses the very label it exists to guard.
        crate::db::projects::set_target_date(&conn, "BoardProj", Some("2026-12-01")).unwrap();
        let rows = crate::db::projects::board(&conn).unwrap();
        let row = rows
            .iter()
            .find(|r| r.project == "BoardProj")
            .expect("the fixture project must be on the board");
        let board = format_project_detail(row, &[], &[], &[]);
        assert!(
            board.contains("target_date:"),
            "fixture must render the dated row: {board}"
        );

        let mut writable = schema_fields::<UpdateProjectParams>();
        writable.extend(schema_fields::<PostProjectUpdateParams>());
        assert_labels_declared(&board, &writable, READ_ONLY, FORBIDDEN, "project_board");
        assert!(
            !board.contains("  target:"),
            "`target:` is not an update_project field — it is `target_date`"
        );
    }

    #[test]
    fn format_detail_omits_absent_optional_fields() {
        let t = task_with("bare task");
        let detail = format_detail(&t, &[], &[], None);
        assert!(!detail.contains("recurrence:"));
        assert!(!detail.contains("repo:"));
        assert!(!detail.contains("Subtasks:"));
        assert!(detail.contains("status:"));
    }

    #[test]
    fn format_detail_shows_execution_context_and_runs() {
        let conn = crate::db::schema::test_conn();
        let t = create(
            &conn,
            &NewTask {
                title: "fix retry".into(),
                repo_path: Some("/tmp".into()),
                bg_allowed: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        let run = db::runs::start(&conn, &t.id, "background", None, crate::executor::BackgroundExecutor::Claude, crate::intent::BackgroundIntent::Work, None).unwrap();
        db::runs::finish(
            &conn,
            &run.id,
            &db::runs::RunOutcome {
                status: "succeeded".into(),
                result_text: Some("patched the backoff".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let runs = db::runs::list_for_task(&conn, &t.id).unwrap();

        let detail = format_detail(&t, &runs, &[], None);
        // `repo_path:`, not `repo:` — the label names the field you write it
        // back with (see every_printed_label_is_a_writable_field_or_declared_read_only).
        assert!(detail.contains("repo_path:  /tmp"));
        assert!(detail.contains("background: Claude Code (investigation only)"));
        assert!(detail.contains("patched the backoff"));
    }

    // ── the comment channel: bounded on read AND on write ─────────────────

    fn comment_params(task_id: &str, body: &str) -> CommentTaskParams {
        CommentTaskParams {
            task_id: task_id.into(),
            body: body.into(),
            handoff: None,
        }
    }

    /// `get_task` is one of `BACKGROUND_TOOLS`, so this output reaches
    /// unattended runs, and the comment loop was its only unbounded region —
    /// one real task returned 33k characters here. Both bounds are asserted
    /// together because they fail differently: dropping the window keeps every
    /// comment, dropping the per-comment cap keeps every character of one.
    ///
    /// The "says what it dropped" half is not decoration: silent truncation
    /// reads as "that's all there is", which is exactly the wrong thing to tell
    /// an agent deciding whether work has already been done.
    #[tokio::test]
    async fn get_task_bounds_the_comment_log_and_says_what_it_dropped() {
        let (dir, db) = test_db("mcp-get-task-comments");
        let t = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            let t = create(&conn, &NewTask { title: "chatty".into(), ..Default::default() })
                .unwrap();
            for i in 0..(DETAIL_COMMENTS + 3) {
                db::comments::add(&conn, &t.id, "claude", &format!("note number {i}")).unwrap();
            }
            db::comments::add(&conn, &t.id, "claude", &"z".repeat(DETAIL_COMMENT_CHARS * 2))
                .unwrap();
            t
        };

        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        let out = server
            .get_task(Parameters(GetTaskParams { task_id: t.id.clone() }))
            .await
            .unwrap();
        let text = format!("{:?}", out.content);

        assert!(
            text.contains(&format!("note number {}", DETAIL_COMMENTS + 2)),
            "the newest notes must survive: {text}"
        );
        assert!(!text.contains("note number 0"), "the oldest is what gets dropped: {text}");
        assert!(
            text.contains("4 older comment(s) omitted"),
            "silent truncation reads as 'that's all there is': {text}"
        );
        assert!(
            text.contains("the comment continues in the app"),
            "one enormous comment is truncated, not dropped — losing it loses that it \
             happened at all: {text}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The write-side bound, and the half that makes it a redirection rather
    /// than a wall: the refusal has to name the channel the text belongs in, or
    /// an agent's only move is to file the same thing in two halves.
    ///
    /// "Zero rows written" is the assertion that matters — a cap applied after
    /// the insert would leave the dump in the log and merely report an error.
    #[tokio::test]
    async fn an_oversized_comment_is_refused_with_the_handoff_channel_named() {
        let (dir, db) = test_db("mcp-comment-cap");
        let t = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            create(&conn, &NewTask { title: "target".into(), ..Default::default() }).unwrap()
        };

        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        let err = server
            .comment_task(Parameters(comment_params(
                &t.id,
                &"x".repeat(COMMENT_MAX_CHARS + 1),
            )))
            .await
            .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains(&COMMENT_MAX_CHARS.to_string()), "name the limit: {msg}");
        assert!(msg.contains("handoff: true"), "name the escape hatch: {msg}");
        assert!(msg.contains("complete_task"), "and the other one: {msg}");

        let conn = rusqlite::Connection::open(&db).unwrap();
        assert_eq!(
            db::comments::count_for_task(&conn, &t.id).unwrap(),
            0,
            "a refused comment must write nothing"
        );

        // Exactly at the cap is fine — an off-by-one here refuses text the
        // error message says is acceptable.
        server
            .comment_task(Parameters(comment_params(&t.id, &"x".repeat(COMMENT_MAX_CHARS))))
            .await
            .expect("the cap is inclusive");
        assert_eq!(db::comments::count_for_task(&conn, &t.id).unwrap(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `handoff: true` takes the same split `runner::finish_run` performs, so a
    /// foreground session leaving mid-way hands the next one the same artifact
    /// a delegated run would: full text on the run row (replayed verbatim by
    /// `dispatch::history_block`), a derived one-liner in the log.
    #[tokio::test]
    async fn a_handoff_comment_lands_on_the_run_row_with_a_summary_in_the_log() {
        let (dir, db) = test_db("mcp-comment-handoff");
        let t = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            create(&conn, &NewTask { title: "half done".into(), ..Default::default() }).unwrap()
        };

        let handoff = format!(
            "## Handoff\n\nGot as far as the parser: the 429 is thrown in lib/webhook.ts:88.\n\n\
             {}\n\nNext: add the backoff.",
            "Ruled out the proxy. ".repeat(150)
        );
        assert!(
            handoff.chars().count() > COMMENT_MAX_CHARS,
            "the fixture must be one the cap would refuse, or this proves nothing"
        );

        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        server
            .comment_task(Parameters(CommentTaskParams {
                task_id: t.id.clone(),
                body: handoff.clone(),
                handoff: Some(true),
            }))
            .await
            .expect("a handoff is the escape hatch from the cap, not subject to it");

        let conn = rusqlite::Connection::open(&db).unwrap();
        let runs = db::runs::list_for_task(&conn, &t.id).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].mode, "foreground");
        assert_eq!(runs[0].status, db::runs::FOREGROUND_STATUS);
        assert_eq!(
            runs[0].result_text.as_deref().map(str::trim),
            Some(handoff.trim()),
            "the run row holds the handoff in FULL"
        );

        let comments = db::comments::for_task(&conn, &t.id).unwrap();
        assert_eq!(comments.len(), 1);
        let body = &comments[0].body;
        assert!(body.starts_with("Foreground handoff"), "{body}");
        assert!(body.contains("lib/webhook.ts:88"), "the outcome line survives: {body}");
        assert!(body.contains("_Full handoff on the run._"), "{body}");
        assert!(
            !body.contains("Ruled out the proxy."),
            "the log gets a summary, not a second copy: {body}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// **A background run is refused `handoff: true`, and the reason is
    /// ordering.** Its handoff is captured from the CLI's final message when
    /// the run ends; a mid-run foreground row would carry a LATER `started_at`
    /// than the background run's own, so `history_block` — which replays
    /// `finished.first()` — would hand the next agent this note and call it the
    /// handoff, permanently.
    ///
    /// Gated in the handler rather than by the router, because a router removes
    /// tools and `comment_task` has to stay: notes-as-you-go are the reason an
    /// unattended run has it at all.
    #[tokio::test]
    async fn a_background_run_cannot_reroute_its_handoff_through_comment_task() {
        let (dir, db) = test_db("mcp-comment-bg-handoff");
        let t = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            create(&conn, &NewTask { title: "delegated".into(), ..Default::default() }).unwrap()
        };

        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Background);
        let err = server
            .comment_task(Parameters(CommentTaskParams {
                task_id: t.id.clone(),
                body: "the whole handoff".into(),
                handoff: Some(true),
            }))
            .await
            .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("FINAL MESSAGE"), "name the channel it DOES have: {msg}");

        let conn = rusqlite::Connection::open(&db).unwrap();
        assert!(db::runs::list_for_task(&conn, &t.id).unwrap().is_empty(), "no run row");
        assert_eq!(
            db::comments::count_for_task(&conn, &t.id).unwrap(),
            0,
            "and no comment either — a refusal that half-wrote would be worse than both"
        );

        // The tool itself still works there. Removing it would take the
        // notes-as-you-go channel with it, which is what it is FOR.
        server
            .comment_task(Parameters(comment_params(&t.id, "ruled out the proxy")))
            .await
            .expect("a background run keeps plain comments");
        assert_eq!(db::comments::count_for_task(&conn, &t.id).unwrap(), 1);

        // And the over-cap refusal points a background run at its own channel,
        // never at `handoff: true` — advice it cannot act on is worse than none.
        let err = server
            .comment_task(Parameters(comment_params(&t.id, &"y".repeat(COMMENT_MAX_CHARS + 1))))
            .await
            .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("FINAL MESSAGE"), "{msg}");
        assert!(!msg.contains("handoff: true"), "it cannot use that: {msg}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn relation_and_retrospective_tools_preserve_and_label_both_sides() {
        let (dir, db) = test_db("mcp-continuity");
        let (old, replacement) = {
            let conn = rusqlite::Connection::open(&db).unwrap();
            let old = create(
                &conn,
                &NewTask {
                    title: "old integration plan".into(),
                    project: Some("CXTasks".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            let replacement = create(
                &conn,
                &NewTask {
                    title: "replacement integration plan".into(),
                    project: Some("CXTasks".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            db::comments::add(
                &conn,
                &old.id,
                db::comments::AUTHOR_ME,
                "Ruled out: the vendor API cannot preserve event ordering",
            )
            .unwrap();
            (old, replacement)
        };

        let server = CxTasksMcp::with_mode(db.to_string_lossy().to_string(), McpMode::Interactive);
        server
            .relate_task(Parameters(RelateTaskParams {
                task_id: format!("T{}", replacement.short_id.unwrap()),
                other_task_id: format!("T{}", old.short_id.unwrap()),
                relation: "supersedes".into(),
                remove: None,
            }))
            .await
            .unwrap();

        let older_detail = server
            .get_task(Parameters(GetTaskParams {
                task_id: old.id.clone(),
            }))
            .await
            .unwrap();
        let newer_detail = server
            .get_task(Parameters(GetTaskParams {
                task_id: replacement.id.clone(),
            }))
            .await
            .unwrap();
        let older_text = format!("{:?}", older_detail.content);
        let newer_text = format!("{:?}", newer_detail.content);
        assert!(older_text.contains("Superseded by:"), "{older_text}");
        assert!(older_text.contains("replacement; this task is historical"), "{older_text}");
        assert!(newer_text.contains("Supersedes:"), "{newer_text}");

        let report = server
            .retrospective(Parameters(RetrospectiveParams {
                project: Some("cxtasks".into()),
                from: None,
                to: None,
                limit: None,
            }))
            .await
            .unwrap();
        let report_text = format!("{:?}", report.content);
        assert!(report_text.contains("ALREADY RULED OUT"), "{report_text}");
        assert!(report_text.contains("superseded"), "{report_text}");
        assert!(
            report_text.contains(&format!("T{}", old.short_id.unwrap())),
            "every conclusion needs a task citation: {report_text}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
