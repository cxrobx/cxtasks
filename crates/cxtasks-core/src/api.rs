//! One verb table.
//!
//! Every operation the app can perform, in a single `match`, keyed by the same
//! names the frontend already invokes. `#[tauri::command]` is RPC with named
//! arguments — `invoke(name, argsObject)` — so this is a transcription of a
//! shape that already existed, not a new design.
//!
//! # Why this exists
//!
//! A sync server needs to execute these verbs. Without one table it would need
//! its own implementation of every one, and two implementations of "complete a
//! task" drift within a month — the second copy forgets that completion
//! materializes the next recurrence instance, or that a `project` change
//! prefills `repo_path`. Routing the Tauri commands through here as well is
//! what makes drift impossible rather than merely discouraged: there is
//! nowhere for a second answer to live.
//!
//! # Why not just expose `db::*`
//!
//! Wrong grain. `complete_task` is `recurrence::complete_with_recurrence` —
//! two db calls plus materialization — and `create_task` validates the
//! recurrence rule before writing. Exposing the db layer means every caller
//! reimplements those compositions, which is the mistake invariant 17
//! documents in a different key.
//!
//! # Argument convention
//!
//! **snake_case, always.** Tauri accepts camelCase from JavaScript and converts
//! it, so `crate::commands` receives typed parameters and builds the JSON here
//! with snake_case keys. camelCase never reaches this module, and the wire
//! format matches the database and the rest of the API.

use rusqlite::Connection;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::db;
use crate::error::AppError;

/// What completing a task produced.
///
/// `next` is the materialized recurrence instance, if the task repeats — a
/// separate row with its own id and date, which is why the caller cannot derive
/// it and why `complete_task` cannot be `update(status = done)`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompletionResult {
    pub completed: db::tasks::Task,
    pub next: Option<db::tasks::Task>,
}

/// The serializable face of `dispatch::QueueOutcome`.
///
/// The runner facts travel back WITH the task rather than being left for the
/// user to discover: gotcha #18 is that queueing into a runner that isn't
/// running looks exactly like queueing into one that is, and the task then sits
/// at `queued` forever with nothing on screen to explain it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QueueResult {
    pub task: db::tasks::Task,
    /// Is a drain loop installed — i.e. is there a runner process at all?
    pub runner_installed: bool,
    /// Is the `STOP_BACKGROUND` kill switch tripped?
    pub paused: bool,
    /// Which agent will pick this up, so the UI can say so rather than making
    /// the user infer it from a checkbox two panes away.
    pub executor: crate::executor::BackgroundExecutor,
}

/// A task's dependency edges, both directions, plus the derived answer.
///
/// `is_blocked` is computed rather than stored (invariant 16), so it is
/// returned alongside the edges instead of being re-derived by every caller.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskRelations {
    pub blocked_by: Vec<db::deps::DepEdge>,
    pub blocking: Vec<db::deps::DepEdge>,
    pub is_blocked: bool,
    pub supersedes: Vec<db::relations::RelationEdge>,
    pub superseded_by: Vec<db::relations::RelationEdge>,
    pub related: Vec<db::relations::RelationEdge>,
    pub is_superseded: bool,
}

/// Verbs that mean nothing away from the machine that owns the files.
///
/// A sync server must refuse these rather than execute them, because executing
/// them would act on the SERVER's disk while the caller meant theirs. They fall
/// into three groups:
///
/// * **A human is at the machine** — `dispatch_foreground` opens a terminal
///   window for someone to watch (invariant 12).
/// * **It reads or writes local paths** — `suggest_repos` scans `~/Projects`;
///   `run_diff` and `restore_checkpoint` resolve a git ref in a local checkout
///   (and `restore_checkpoint` resets a working tree); the two attachment ADD
///   verbs take source paths on local disk, which is why upload needs its own
///   HTTP route rather than this table.
/// * **It is about this process or this OS** — the runner's launchd
///   registration, `run_queue_once`, native appearance, the notification latch.
pub const HOST_LOCAL: &[&str] = &[
    "dispatch_foreground",
    "suggest_repos",
    "install_runner",
    "uninstall_runner",
    "is_runner_installed",
    "run_queue_once",
    "run_diff",
    "restore_checkpoint",
    "set_native_appearance",
    "take_pending_notification_task",
    "add_task_attachments",
    "add_comment_with_attachments",
    // Lists the executables in THIS machine's tripwires directory. A picker on
    // the phone would be offering scripts the phone cannot see, so it stays
    // here and the phone types a name (which the Mac's runner then judges).
    "tripwire_scripts",
    // Both probe THIS machine. `executor_available` runs `Provider::preflight()`
    // against the local CLI; `get_dispatch_prompt` resolves a repo path through
    // `resolve_repo_or_home`. Until they were listed here a remote caller got
    // "unknown command" for two verbs that exist — the exact misleading refusal
    // this list is for.
    "executor_available",
    "get_dispatch_prompt",
    // Sync CONFIGURATION — who this machine talks to. Running one of these
    // through a server would configure the server instead, and `/rpc`'s board
    // scope refuses only three verbs by name, so a phone's token would
    // otherwise be able to unpair the Mac it syncs with. The protocol itself
    // stays on the server as `/sync/*`; this is how a node is told who to talk
    // to, which is nobody else's business.
    "sync_status",
    "sync_pair",
    "sync_now",
    "sync_set_enabled",
    "sync_unpair",
];

/// Is this a verb only the owning host may run?
pub fn is_host_local(cmd: &str) -> bool {
    HOST_LOCAL.contains(&cmd)
}

// ── argument plumbing ───────────────────────────────────────────────────────

/// A required argument. A missing key is a caller bug, so it says which one.
fn arg<T: DeserializeOwned>(args: &Value, key: &str) -> Result<T, AppError> {
    let raw = args
        .get(key)
        .ok_or_else(|| AppError::Validation(format!("missing argument '{key}'")))?;
    serde_json::from_value(raw.clone())
        .map_err(|e| AppError::Validation(format!("bad argument '{key}': {e}")))
}

/// An optional argument. Absent and `null` are the same thing here — which is
/// NOT the `TaskPatch` convention, and deliberately so: this is about whether
/// the caller supplied a parameter, not about clearing a column.
fn opt<T: DeserializeOwned>(args: &Value, key: &str) -> Result<Option<T>, AppError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(raw) => serde_json::from_value(raw.clone())
            .map(Some)
            .map_err(|e| AppError::Validation(format!("bad argument '{key}': {e}"))),
    }
}

fn ok<T: Serialize>(value: T) -> Result<Value, AppError> {
    serde_json::to_value(value).map_err(|e| AppError::General(format!("could not encode result: {e}")))
}

/// Validate a recurrence rule before it is stored.
///
/// Both `create_task` and `update_task` do this, and neither may skip it: an
/// unparseable rule stored on a task is a completion that silently fails to
/// materialize the next instance, discovered days later when the task simply
/// does not come back.
fn check_recurrence(rule: &str) -> Result<(), AppError> {
    if rule.trim().is_empty() {
        return Ok(());
    }
    crate::recurrence::Recurrence::parse(rule)
        .map(|_| ())
        .map_err(|e| AppError::Validation(format!("invalid recurrence '{rule}': {e}")))
}

// ── the table ───────────────────────────────────────────────────────────────

/// Execute one verb.
///
/// `&mut Connection` because a few verbs need a transaction. Callers holding a
/// `MutexGuard<Connection>` can pass `&mut *guard`.
pub fn dispatch(conn: &mut Connection, cmd: &str, args: &Value) -> Result<Value, AppError> {
    match cmd {
        // ── tasks ───────────────────────────────────────────────────────────
        "list_tasks" => ok(db::tasks::list(conn, &arg(args, "filters")?)?),
        // `resolve_ref`, not `get` — this is the one lookup that accepts a
        // T-number as well as a uuid, which is what makes `cxtasks://task/T42`
        // and "handle T69" resolve to the same row.
        "get_task" => ok(db::tasks::resolve_ref(conn, &arg::<String>(args, "id")?)?),
        "list_subtasks" => ok(db::tasks::list_children(conn, &arg::<String>(args, "parent_id")?)?),
        "create_task" => {
            let task: db::tasks::NewTask = arg(args, "task")?;
            if let Some(rule) = task.recurrence.as_deref() {
                check_recurrence(rule)?;
            }
            ok(db::tasks::create(conn, &task)?)
        }
        // Free text from a client that has no project picker — see
        // `crate::intake`. `intake_route` is READ-ONLY on purpose: it is the
        // half that may call Jev, and a write verb runs inside `BEGIN
        // IMMEDIATE`.
        "intake_route" => ok(crate::intake::assess(conn, &arg::<String>(args, "text")?)?),
        "intake_resolve" => ok(crate::intake::resolve(conn, &arg::<String>(args, "text")?)?),
        "intake_places" => ok(crate::intake::places(conn)?),
        "intake" => ok(crate::intake::intake(
            conn,
            &arg::<crate::intake::IntakeRequest>(args, "request")?,
        )?),
        "update_task" => {
            let patch: db::tasks::TaskPatch = arg(args, "patch")?;
            if let Some(Some(rule)) = patch.recurrence.as_ref() {
                check_recurrence(rule)?;
            }
            ok(db::tasks::update(conn, &arg::<String>(args, "id")?, &patch)?)
        }
        "complete_task" => {
            let (completed, next) = crate::recurrence::complete_with_recurrence(
                conn,
                &arg::<String>(args, "id")?,
                opt::<String>(args, "result")?.as_deref(),
            )?;
            ok(CompletionResult { completed, next })
        }
        "trash_task" => ok(db::tasks::trash(conn, &arg::<String>(args, "id")?).map(|_| ())?),
        "restore_task" => ok(db::tasks::restore(conn, &arg::<String>(args, "id")?)?),
        "archive_task" => ok(db::tasks::archive(conn, &arg::<String>(args, "id")?).map(|_| ())?),
        "unarchive_task" => ok(db::tasks::unarchive(conn, &arg::<String>(args, "id")?)?),
        "destroy_task" => {
            let id: String = arg(args, "id")?;
            let attachments = db::attachments::for_task_tree(conn, &id)?;
            db::tasks::delete(conn, &id)?;
            crate::attachments::remove_managed_records(&attachments);
            ok(())
        }
        // **Replays as a QUERY, not as a list, and that is a real divergence.**
        //
        // The recorded intent carries `{}`, because that is what the caller
        // sent. Replayed on the peer it runs the same `DELETE … WHERE
        // deleted_at IS NOT NULL` against the PEER's trash, which need not hold
        // the same rows: a task the phone trashed and this host has not pulled
        // yet is destroyed by an action the user took against a set that did
        // not include it.
        //
        // Left as-is deliberately rather than papered over. Fixing it properly
        // means recording the resolved ids, which means `dispatch_synced` being
        // able to rewrite an intent's arguments from a verb's result, and
        // `empty_trash` returning ids instead of the count the UI shows in its
        // toast. That is a real change to three things, and the right time to
        // make it is when the apply path exists to test it against — not
        // mid-phase, on a guess about what the applier will look like.
        //
        // Bounded meanwhile: everything it can over-destroy was already in a
        // trash, and both ends converge to the same empty trash afterwards.
        "empty_trash" => {
            let attachments = db::attachments::for_trashed_tasks(conn)?;
            let n = conn.execute("DELETE FROM tasks WHERE deleted_at IS NOT NULL", [])?;
            // Raw SQL rather than `tasks::delete`, so it does not inherit that
            // function's tag collection and needs its own (invariant 13's
            // second hard-delete path).
            db::tags::gc(conn)?;
            crate::attachments::remove_managed_records(&attachments);
            log::info!("emptied the trash: {n} task(s) destroyed");
            ok(n)
        }
        "trash_countdowns" => ok(trash_countdowns(conn)?),
        "reorder_tasks" => {
            let ids: Vec<String> = arg(args, "ids")?;
            // A SAVEPOINT, not a transaction. Identical semantics on its own —
            // SQLite starts a transaction for a savepoint taken outside one, and
            // rusqlite rolls both back on drop — but a savepoint NESTS, and a
            // transaction does not. Once sync wraps every verb so the local
            // write and its outbox entry commit together, an arm that opened
            // its own transaction would fail with "cannot start a transaction
            // within a transaction". Pinned by
            // `a_verb_that_opens_its_own_transaction_still_works_inside_an_outer_one`.
            let tx = conn.savepoint()?;
            for (i, id) in ids.iter().enumerate() {
                tx.execute(
                    "UPDATE tasks SET sort_order = ?1, updated_at = datetime('now') WHERE id = ?2",
                    rusqlite::params![i as i64, id],
                )?;
            }
            tx.commit()?;
            ok(())
        }
        "list_projects" => ok(db::tasks::list_projects(conn)?),
        "view_counts" => ok(db::tasks::view_counts(conn)?),
        "search_tasks" => {
            let query: String = arg(args, "query")?;
            let mut tasks = db::search::search(
                conn,
                &query,
                opt::<u32>(args, "limit")?.unwrap_or(50),
            )?;
            // Search must still work offline and during a provider outage. Jev
            // is a reranker over the lexical shortlist, never the retrieval
            // layer and never a new failure mode for the search box.
            if let Err(e) = crate::jev::rerank_tasks(conn, &query, &mut tasks) {
                log::warn!("Jev search rerank failed; keeping lexical order: {e}");
            }
            ok(tasks)
        }
        "filter_tasks" => ok(db::search::filtered(conn, &arg(args, "spec")?)?),
        // The sidebar's node badges: one count per spec, each the SAME WHERE
        // clause `filter_tasks` runs, so a badge and the list it opens agree.
        "count_filtered" => {
            let specs: Vec<db::search::FilterSpec> = arg(args, "specs")?;
            ok(db::search::count_many(conn, &specs)?)
        }
        "retrospective" => {
            let mut report = db::retrospective::read(conn, &arg(args, "spec")?)?;
            if let Err(e) = crate::jev::rerank_retrospective(conn, &mut report) {
                log::warn!("Jev retrospective rerank failed; keeping evidence order: {e}");
            }
            ok(report)
        }

        // ── Jev judgments ─────────────────────────────────────────────────
        "jev_status" => ok(crate::jev::status(conn)),
        "save_jev_settings" => ok(crate::jev::save_settings(
            conn,
            &arg::<crate::jev::SaveSettings>(args, "settings")?,
        )?),
        "jev_assess_capture" => ok(crate::jev::assess_capture(
            conn,
            &arg::<crate::jev::CaptureRequest>(args, "request")?,
        )?),
        "jev_assess_readiness" => {
            let id: String = arg(args, "task_id")?;
            let task = db::tasks::resolve_ref(conn, &id)?
                .ok_or_else(|| AppError::NotFound(format!("task {id} not found")))?;
            ok(crate::jev::assess_readiness(conn, &task)?)
        }

        // ── runs & budget ───────────────────────────────────────────────────
        "list_runs" => ok(db::runs::list_for_task(conn, &arg::<String>(args, "task_id")?)?),

        // ── hosts ──────────────────────────────────────────────────────────
        // A READ. It does not register this machine, deliberately: a host row
        // means "a machine that runs tasks", and the only process entitled to
        // write one is the runner. `cxtasks-server` calls this too, and it
        // cannot execute anything — a row from it would claim to do everything
        // and drain nothing.
        "list_hosts" => {
            let hosts = db::hosts::list(conn)?;
            let mut out = Vec::with_capacity(hosts.len());
            for h in hosts {
                let mirrors = db::hosts::mirrors(conn, &h.id)?;
                let remote = db::hosts::mirror_remote(conn, &h.id)?;
                out.push(serde_json::json!({
                    "capability": h,
                    // Alongside rather than inside `HostCapability`, because
                    // `satisfies` takes it separately and putting it on the
                    // struct would invite somebody to read it from there and
                    // skip the parameter.
                    "mirrors": mirrors,
                    "mirror_remote": remote,
                    "is_this_host": h.id == crate::host::host_id(),
                    "alive": h.is_alive(db::hosts::HOST_STALE_MINUTES),
                }));
            }
            ok(out)
        }
        "set_host_policy" => ok(db::hosts::set_policy(
            conn,
            &arg::<String>(args, "host_id")?,
            &arg::<db::hosts::HostPolicy>(args, "policy")?,
        )?),
        "set_host_mirror_remote" => ok(db::hosts::set_mirror_remote(
            conn,
            &arg::<String>(args, "host_id")?,
            opt::<String>(args, "remote")?.as_deref(),
        )?),
        "remove_host" => ok(db::hosts::remove(conn, &arg::<String>(args, "host_id")?)?),
        // "4 runs finished on the NAS while you were away." No outbox table:
        // `task_runs` already replicates whole-row and carries `host` and
        // `finished_at`, so a cursor turns it into the same feature with
        // nothing new to classify, sweep, purge and keep in step.
        "runs_finished_elsewhere" => ok(db::runs::finished_elsewhere_since(
            conn,
            &arg::<String>(args, "since")?,
            opt::<u32>(args, "limit")?.unwrap_or(20),
        )?),
        "list_run_events" => ok(db::run_events::list_after(
            conn,
            &arg::<String>(args, "run_id")?,
            opt::<i64>(args, "after_seq")?.unwrap_or(0),
        )?),
        "recent_runs" => ok(db::runs::recent_with_tasks(
            conn,
            opt::<u32>(args, "limit")?.unwrap_or(24),
        )?),
        // A lookup, not a list. `recent_runs` was the only verb returning runs,
        // so `cxtasks://run/<id>` could not resolve anything older than its
        // window — and the fix is a verb that takes an id, not a bigger window.
        // Returns the same `RunWithTask` shape the list does, so a deep link
        // and a tapped row render through one code path on the client.
        "get_run" => ok(db::runs::get_with_task(conn, &arg::<String>(args, "run_id")?)?),
        "budget_status" => ok(db::meta::budget_status(conn)?),
        "save_budget_settings" => {
            db::meta::save_budget_settings(conn, &arg(args, "settings")?)?;
            ok(db::meta::budget_status(conn)?)
        }
        "reset_budget_window" => {
            db::meta::reset_budget_window(conn)?;
            ok(db::meta::budget_status(conn)?)
        }
        // Acts on the host that EXECUTES it — the kill switch is a file in that
        // host's data dir. That is the right semantics for a runner pausing
        // itself, and the reason per-host pause needs its own surface rather
        // than this verb reaching across hosts.
        "set_background_paused" => {
            let paused: bool = arg(args, "paused")?;
            let path = crate::paths::kill_switch_path();
            if paused {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(
                    &path,
                    "CXTasks background runs are paused.\nDelete this file (or toggle it in the app) to resume.\n",
                )?;
            } else if path.exists() {
                std::fs::remove_file(&path)?;
            }
            ok(crate::paths::background_paused())
        }
        "cancel_run" => ok(cancel_run(conn, &arg::<String>(args, "run_id")?)?),
        // Canonical, unlike its foreground sibling: queueing is a database
        // write plus a set of guards, none of which need a human at the
        // machine. `queue_for_background` takes a connection-lending closure
        // because the Tauri caller re-locks per access; here there is one
        // connection, so the closure just hands it over.
        "queue_background" => {
            let task_id: String = arg(args, "task_id")?;
            let outcome = crate::dispatch::queue_for_background(
                |f| f(conn),
                &task_id,
            )?;
            ok(QueueResult {
                executor: outcome.task.bg_executor,
                task: outcome.task,
                runner_installed: outcome.runner_installed,
                paused: outcome.paused,
            })
        }

        // ── tags ────────────────────────────────────────────────────────────
        "list_tags" => ok(db::tags::list_with_counts(conn)?),
        "task_tags" => ok(db::tags::for_task(conn, &arg::<String>(args, "task_id")?)?),
        "set_task_tags" => {
            let task_id: String = arg(args, "task_id")?;
            let tags: Vec<String> = arg(args, "tags")?;
            db::tags::set_for_task(conn, &task_id, &tags)?;
            ok(db::tags::for_task(conn, &task_id)?)
        }
        "rename_tag" => ok(db::tags::rename(
            conn,
            &arg::<String>(args, "from")?,
            &arg::<String>(args, "to")?,
        )?),
        "task_tag_map" => ok(db::tags::by_task(conn)?),
        "tag_usage_count" => ok(db::tags::usage_count(conn, &arg::<String>(args, "name")?)?),
        "delete_tag" => ok(db::tags::delete(conn, &arg::<String>(args, "name")?)?),

        // ── dependencies ────────────────────────────────────────────────────
        "task_relations" => {
            let task_id: String = arg(args, "task_id")?;
            ok(TaskRelations {
                blocked_by: db::deps::blocked_by(conn, &task_id)?,
                blocking: db::deps::blocking(conn, &task_id)?,
                is_blocked: db::deps::is_blocked(conn, &task_id)?,
                supersedes: db::relations::supersedes(conn, &task_id)?,
                superseded_by: db::relations::superseded_by(conn, &task_id)?,
                related: db::relations::related(conn, &task_id)?,
                is_superseded: db::relations::is_superseded(conn, &task_id)?,
            })
        }
        "add_dependency" => ok(db::deps::add(
            conn,
            &arg::<String>(args, "task_id")?,
            &arg::<String>(args, "blocked_by_id")?,
        )?),
        "remove_dependency" => ok(db::deps::remove(
            conn,
            &arg::<String>(args, "task_id")?,
            &arg::<String>(args, "blocked_by_id")?,
        )?),
        "blocked_task_ids" => ok(db::deps::blocked_task_ids(conn)?),
        "add_relation" => ok(db::relations::add(
            conn,
            &arg::<String>(args, "from_task_id")?,
            &arg::<String>(args, "to_task_id")?,
            db::relations::RelationKind::parse(&arg::<String>(args, "kind")?)?,
        )?),
        "remove_relation" => ok(db::relations::remove(
            conn,
            &arg::<String>(args, "from_task_id")?,
            &arg::<String>(args, "to_task_id")?,
            db::relations::RelationKind::parse(&arg::<String>(args, "kind")?)?,
        )?),

        // ── tripwires ───────────────────────────────────────────────────────
        // Read-only: the volatile row for every live tripwire task, keyed by
        // task id — the `task_tag_map` shape, one call for the whole board.
        // Setting, re-arming and "check now" all go through `update_task`;
        // evaluating goes through the runner and nowhere else.
        "list_tripwire_runtime" => ok(db::tripwires::runtime_map(conn)?),

        // ── comments ────────────────────────────────────────────────────────
        "list_comments" => ok(db::comments::for_task(conn, &arg::<String>(args, "task_id")?)?),
        "add_comment" => ok(db::comments::add(
            conn,
            &arg::<String>(args, "task_id")?,
            db::comments::AUTHOR_ME,
            &arg::<String>(args, "body")?,
        )?),
        "edit_comment" => ok(db::comments::edit(
            conn,
            &arg::<String>(args, "id")?,
            &arg::<String>(args, "body")?,
        )?),
        "delete_comment" => {
            // The DB cascade removes attachment metadata, so capture the
            // managed filenames first or the image copies are orphaned.
            let id: String = arg(args, "id")?;
            let attachments = db::attachments::for_comment(conn, &id)?;
            db::comments::delete(conn, &id)?;
            crate::attachments::remove_managed_records(&attachments);
            ok(())
        }

        // ── attachments (read/delete only; ADD is host-local) ────────────────
        "list_task_attachments" => ok(crate::attachments::list_task_attachments(
            conn,
            arg::<String>(args, "task_id")?,
        )?),
        "delete_task_attachment" => ok(crate::attachments::delete_task_attachment(
            conn,
            arg::<String>(args, "id")?,
        )?),

        // ── projects ────────────────────────────────────────────────────────
        "list_all_projects" => ok(db::projects::list(
            conn,
            opt::<bool>(args, "include_archived")?.unwrap_or(false),
        )?),
        "create_project" => {
            let kind = match opt::<String>(args, "kind")?
                .as_deref()
                .map(str::trim)
                .filter(|k| !k.is_empty())
            {
                Some(raw) => db::projects::ProjectKind::parse(raw)?,
                None => db::projects::ProjectKind::default(),
            };
            ok(db::projects::create(
                conn,
                &arg::<String>(args, "name")?,
                kind,
                opt::<String>(args, "parent")?.as_deref(),
            )?)
        }
        "set_project_parent" => ok(db::projects::set_parent(
            conn,
            &arg::<String>(args, "name")?,
            opt::<String>(args, "parent")?.as_deref(),
        )?),
        "set_project_kind" => ok(db::projects::set_kind(
            conn,
            &arg::<String>(args, "name")?,
            db::projects::ProjectKind::parse(&arg::<String>(args, "kind")?)?,
        )?),
        "set_project_target_date" => ok(db::projects::set_target_date(
            conn,
            &arg::<String>(args, "name")?,
            opt::<String>(args, "target_date")?.as_deref(),
        )?),
        "set_project_closed" => ok(db::projects::set_closed(
            conn,
            &arg::<String>(args, "name")?,
            arg::<bool>(args, "closed")?,
        )?),
        "set_project_appearance" => ok(db::appearance::set_project(
            conn,
            &arg::<String>(args, "name")?,
            opt::<String>(args, "color")?.as_deref(),
            opt::<String>(args, "icon")?.as_deref(),
        )?),
        "set_tag_appearance" => ok(db::appearance::set_tag(
            conn,
            &arg::<String>(args, "name")?,
            opt::<String>(args, "color")?.as_deref(),
            opt::<String>(args, "icon")?.as_deref(),
        )?),
        "rename_project" => ok(db::projects::rename(
            conn,
            &arg::<String>(args, "from")?,
            &arg::<String>(args, "to")?,
        )?),
        "archive_project" => ok(db::projects::set_archived(
            conn,
            &arg::<String>(args, "name")?,
            arg::<bool>(args, "archived")?,
        )?),
        "reorder_projects" => ok(db::projects::reorder(conn, &arg::<Vec<String>>(args, "names")?)?),
        "delete_project" => ok(db::projects::delete(
            conn,
            &arg::<String>(args, "name")?,
            opt::<bool>(args, "clear_from_tasks")?.unwrap_or(false),
        )?),
        "project_board" => ok(db::projects::board(conn)?),

        // ── project repos ───────────────────────────────────────────────────
        "list_project_repos" => ok(db::projects::all_repos(conn)?),
        "add_project_repo" => ok(db::projects::add_repo(
            conn,
            &arg::<String>(args, "project")?,
            &arg::<String>(args, "repo_path")?,
            opt::<bool>(args, "make_primary")?.unwrap_or(false),
        )?),
        "remove_project_repo" => ok(db::projects::remove_repo(conn, &arg::<String>(args, "id")?)?),
        "set_primary_project_repo" => {
            ok(db::projects::set_primary_repo(conn, &arg::<String>(args, "id")?)?)
        }
        "project_for_repo_path" => ok(db::projects::for_repo_path(
            conn,
            &arg::<String>(args, "repo_path")?,
        )?),

        // ── sections ────────────────────────────────────────────────────────
        "list_sections" => ok(db::projects::list_sections(
            conn,
            opt::<String>(args, "project")?.as_deref(),
        )?),
        "create_section" => ok(db::projects::create_section(
            conn,
            opt::<String>(args, "project")?.as_deref(),
            &arg::<String>(args, "name")?,
        )?),
        "rename_section" => ok(db::projects::rename_section(
            conn,
            &arg::<String>(args, "id")?,
            &arg::<String>(args, "name")?,
        )?),
        "delete_section" => ok(db::projects::delete_section(conn, &arg::<String>(args, "id")?)?),
        "reorder_sections" => ok(db::projects::reorder_sections(
            conn,
            opt::<String>(args, "project")?.as_deref(),
            &arg::<Vec<String>>(args, "ids")?,
        )?),
        "set_section_target_date" => ok(db::projects::set_section_target_date(
            conn,
            &arg::<String>(args, "id")?,
            opt::<String>(args, "target_date")?.as_deref(),
        )?),
        "set_section_completed" => ok(db::projects::set_section_completed(
            conn,
            &arg::<String>(args, "id")?,
            arg::<bool>(args, "done")?,
        )?),
        "move_task_to_section" => ok(db::projects::move_task_to_section(
            conn,
            &arg::<String>(args, "task_id")?,
            opt::<String>(args, "section_id")?.as_deref(),
        )?),

        // ── project updates ─────────────────────────────────────────────────
        "post_project_update" => ok(db::project_updates::post(
            conn,
            &arg::<String>(args, "project")?,
            db::comments::AUTHOR_ME,
            &arg::<String>(args, "body")?,
            opt::<String>(args, "health")?.as_deref(),
        )?),
        "list_project_updates" => ok(db::project_updates::for_project(
            conn,
            &arg::<String>(args, "project")?,
            opt::<u32>(args, "limit")?.unwrap_or(50),
        )?),

        // ── refusals ────────────────────────────────────────────────────────
        //
        // Named separately from "unknown verb" so a caller learns WHICH kind of
        // mistake it made. "That verb only runs on the machine that owns the
        // files" is actionable; "unknown command" sends you looking for a typo.
        other if is_host_local(other) => Err(AppError::Validation(format!(
            "'{other}' can only run on the host that owns the files, not through the verb table"
        ))),
        other => Err(AppError::NotFound(format!("unknown command '{other}'"))),
    }
}

/// Verbs that only READ.
///
/// Being wrong here is safe in exactly one direction, which is why the list can
/// exist at all. A mutating verb missing from it takes the transactional path —
/// correct, marginally slower. A read verb missing from it does the same. The
/// only harmful mistake is listing a verb that actually writes, and
/// [`tests::every_verb_called_read_only_really_changes_nothing`] runs each one
/// against a seeded database and fails if it changes a single row.
///
/// It exists purely so that reading a task does not take a write lock on a file
/// four processes share.
/// Every verb the table answers. Used only by tests, and kept beside the table
/// so the two are edited together.
pub const ALL_VERBS: &[&str] = &[
    "list_tasks", "get_task", "list_subtasks", "create_task", "update_task", "complete_task",
    "trash_task", "restore_task", "archive_task", "unarchive_task", "destroy_task", "empty_trash",
    "intake", "intake_route", "intake_resolve", "intake_places",
    "trash_countdowns", "reorder_tasks", "list_projects", "view_counts", "search_tasks",
    "filter_tasks", "count_filtered", "retrospective", "jev_status", "save_jev_settings", "jev_assess_capture",
    "jev_assess_readiness", "list_runs", "list_run_events", "recent_runs", "get_run", "budget_status",
    "save_budget_settings", "reset_budget_window", "set_background_paused", "cancel_run",
    "queue_background", "list_tags", "task_tags", "set_task_tags", "rename_tag", "task_tag_map",
    "tag_usage_count", "delete_tag", "task_relations", "add_dependency", "remove_dependency",
    "blocked_task_ids", "add_relation", "remove_relation", "list_tripwire_runtime",
    "list_comments", "add_comment", "edit_comment", "delete_comment",
    "list_task_attachments", "delete_task_attachment", "list_all_projects", "create_project",
    "set_project_parent", "set_project_kind", "set_project_target_date", "set_project_closed",
    "rename_project", "archive_project", "reorder_projects", "delete_project", "project_board",
    "set_project_appearance", "set_tag_appearance",
    "list_project_repos", "add_project_repo", "remove_project_repo", "set_primary_project_repo",
    "project_for_repo_path", "list_sections", "create_section", "rename_section", "delete_section",
    "reorder_sections", "set_section_target_date", "set_section_completed", "move_task_to_section",
    "post_project_update", "list_project_updates",
    "list_hosts", "set_host_policy", "set_host_mirror_remote", "remove_host",
    "runs_finished_elsewhere",
];

const READ_ONLY: &[&str] = &[
    "list_tasks", "get_task", "list_subtasks", "trash_countdowns", "list_projects",
    "intake_route", "intake_resolve", "intake_places",
    "view_counts", "search_tasks", "filter_tasks", "count_filtered", "retrospective", "jev_status",
    "jev_assess_capture", "jev_assess_readiness",
    "list_runs", "list_run_events", "recent_runs", "get_run", "budget_status",
    "list_tags", "task_tags", "task_tag_map", "tag_usage_count",
    "task_relations", "blocked_task_ids", "list_tripwire_runtime",
    "list_comments",
    "list_task_attachments",
    "list_all_projects", "project_board",
    "list_project_repos", "project_for_repo_path",
    "list_sections",
    "list_project_updates",
    "list_hosts",
    "runs_finished_elsewhere",
];

/// Verbs whose effect belongs to the machine that ran them, so replaying them
/// elsewhere would be wrong even though they change rows.
///
/// Short by construction: most host-local effects are files rather than rows
/// (`set_background_paused`, `cancel_run`), and a verb that changes nothing is
/// never recorded in the first place.
/// Verbs that REMOVE rows, and therefore the only ones recorded as intents.
///
/// # Why state does not travel as a verb
///
/// Replaying a state-creating verb on the peer and sweeping its post-image are
/// not merely redundant — they conflict, and the conflicts are silent:
///
/// * `create_task` replayed on the peer makes the peer generate its OWN uuid
///   and claim its OWN T-number, so the swept post-image of the local row then
///   collides. It surfaces as `UNIQUE constraint failed: tasks.short_id`, which
///   is the lucky version.
/// * `complete_task` on a RECURRING task materializes the next instance. The
///   peer materializes its own, the sweep delivers the local one, and the
///   result is a duplicate future task that nothing reports.
///
/// A post-image can express every state change there is. The one thing it
/// cannot express is a row that is **gone** — a swept table reports rows that
/// exist. So deletions travel as verbs and everything else travels as state,
/// and the two mechanisms stop overlapping.
///
/// # Why this list is discovered rather than declared
///
/// [`tests::the_list_of_deleting_verbs_matches_what_the_verbs_actually_do`]
/// runs every verb against a seeded database and fails naming any that made a
/// synced table shrink without being listed here. A delete missing from this
/// list never reaches the peer, and the next pull resurrects what it removed.
const DELETES_ROWS: &[&str] = &[
    "delete_comment",
    // **Was missing, and the discovery test could not see it**: the fixture
    // below never created an attachment row, so this verb ran against a task
    // with nothing to delete, shrank no table and passed. The consequence was
    // that removing an attachment on one machine never travelled, and the next
    // pull resurrected the row — and then the byte transfer dutifully
    // re-downloaded the image. A delete that undoes itself on a timer.
    "delete_task_attachment",
    "delete_project",
    "delete_section",
    "delete_tag",
    "destroy_task",
    "empty_trash",
    "remove_dependency",
    "remove_relation",
    "remove_host",
    "remove_project_repo",
    // Replaces a task's whole tag set, so it removes the rows that fall out.
    "set_task_tags",
    // Renaming onto an EXISTING tag merges them (invariant 17), which removes a
    // row. Renaming onto a free name does not — but the verb is one verb, and a
    // list that tried to tell those apart would be guessing at runtime.
    "rename_tag",
];

fn is_read_only(cmd: &str) -> bool {
    READ_ONLY.contains(&cmd)
}

/// Execute one verb, and record it if sync is on and it changed anything.
///
/// # The one transaction
///
/// Applying the change and recording the intent must commit together. A crash
/// between them drops the change silently — the local database moves on, the
/// peer never hears, and the next pull quietly reverts it. That is the failure
/// the outbox exists to prevent, so it cannot be two statements.
///
/// # Why mutation is DETECTED, not declared
///
/// The alternative is a list of writing verbs, which is one more thing to
/// forget on every new verb — and forgetting means that verb's changes silently
/// stop syncing, with no call site to grep for. `total_changes()` asks the
/// database what actually happened, so a verb added next year is recorded
/// correctly by a function nobody edited.
///
/// # With sync off
///
/// Straight through to [`dispatch`], no transaction, no lookup beyond one
/// indexed `app_meta` read. The standalone Mac must behave byte-identically,
/// and this is the line that guarantees it.
pub fn dispatch_synced(conn: &mut Connection, cmd: &str, args: &Value) -> Result<Value, AppError> {
    // The canonical node keeps no outbox: it IS the thing peers reconcile
    // against, so an intent recorded there would be drained by nobody and the
    // table would grow forever. It still runs inside the transaction below, so
    // a verb that half-succeeds still rolls back.
    let record = crate::sync::state::enabled(conn) && !crate::sync::state::is_canonical(conn);
    if (!crate::sync::state::enabled(conn)) || is_read_only(cmd) {
        return dispatch(conn, cmd, args);
    }

    // IMMEDIATE, not deferred. A deferred transaction takes its write lock on
    // the first write, and an upgrade that collides with another writer fails
    // rather than waiting — which would surface as random `database is locked`
    // errors on ordinary edits, since four processes share this file.
    conn.execute_batch("BEGIN IMMEDIATE")?;

    // **A recorded verb is queued behind every row it refers to.** Only the
    // sweep ever queues a row written outside the verb table (the MCP, the
    // runner) or a create (which travels as state). Without these two sweeps,
    // the app retagging a task the MCP had just created put `set_task_tags` in
    // the outbox ahead of the task, the peer refused it as "not found", and a
    // push stops at its first failure, so sync stood still for hours behind one
    // intent (2026-10-02).
    //
    // BEFORE the verb: the rows it acts on, so a delete finds its target.
    // AFTER it: the rows it CREATED, so the replay reuses them rather than
    // minting its own. `set_task_tags` with a new name makes the tag on each
    // end under a different uuid, and the next sweep then collides on
    // `tags.name`. The queue then reads: prior state, new state, verb.
    let sweeps = record && DELETES_ROWS.contains(&cmd);
    if sweeps {
        if let Err(e) = crate::sync::sweep::run(conn) {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    // After the sweep: its own outbox and watermark writes are not the verb
    // changing anything.
    let before = conn.total_changes();

    let outcome = dispatch(conn, cmd, args);
    let value = match outcome {
        Ok(value) => value,
        Err(e) => {
            // Roll back even though the verb reported failure: it may have
            // written something before failing, and half of a refused verb is
            // worse than none of it.
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    };

    // Only deletions are recorded. Everything else reaches the peer as a
    // post-image through the sweep — see `DELETES_ROWS` for why replaying a
    // state-creating verb and sweeping its result cannot both happen.
    let changed = conn.total_changes() > before;
    if sweeps && changed {
        let host = crate::host::host_id();
        if let Err(e) = crate::sync::sweep::run(conn)
            .and_then(|_| crate::sync::outbox::append(conn, cmd, args, &host))
        {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    conn.execute_batch("COMMIT")?;
    Ok(value)
}

/// Days remaining before each trashed task is purged.
fn trash_countdowns(conn: &Connection) -> Result<std::collections::HashMap<String, i64>, AppError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, julianday(deleted_at, '+{} days') - julianday('now')
           FROM tasks WHERE deleted_at IS NOT NULL",
        db::tasks::RETENTION_DAYS
    ))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|(id, days)| (id, days.ceil().max(0.0) as i64))
        .collect())
}

/// Ask a run to stop — on this machine or on another one.
///
/// Writes BOTH: the row, which replicates and is how a request reaches a remote
/// host; and, when the run is local, the file, which is what `process.rs`
/// actually polls and which acts immediately rather than waiting on a
/// 3-second control loop. See `db::runs::request_cancel` for why the file
/// stays the interface.
fn cancel_run(conn: &Connection, run_id: &str) -> Result<bool, AppError> {
    let Some(run) = db::runs::get(conn, run_id)? else {
        return Err(AppError::NotFound(format!("run {run_id} not found")));
    };
    if run.status != "running" {
        return Ok(false);
    }
    db::runs::request_cancel(conn, run_id)?;

    // Local runs stop now. A row addressed to another host is picked up by
    // that host's control thread; there is nothing useful to touch here.
    let mine = run
        .host
        .as_deref()
        .map(|h| h == crate::host::host_id())
        .unwrap_or(true);
    if mine {
        // A failed write means the run will NOT stop, so this must not be
        // ignored: the UI would say "asked it to stop" about a request that was
        // never made.
        crate::paths::request_cancel(run_id)
            .map_err(|e| AppError::General(format!("could not request cancellation: {e}")))?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conn() -> Connection {
        crate::db::schema::test_conn()
    }

    fn call(conn: &mut Connection, cmd: &str, args: Value) -> Result<Value, AppError> {
        dispatch(conn, cmd, &args)
    }

    /// The table can do the whole life of a task, through the same names the
    /// frontend invokes.
    #[test]
    fn a_task_can_be_created_read_updated_and_completed_through_the_table() {
        let mut c = conn();
        let created = call(&mut c, "create_task", json!({ "task": { "title": "write it down" } })).unwrap();
        let id = created["id"].as_str().unwrap().to_string();

        let got = call(&mut c, "get_task", json!({ "id": &id })).unwrap();
        assert_eq!(got["title"], "write it down");

        call(&mut c, "update_task", json!({ "id": &id, "patch": { "priority": 0 } })).unwrap();
        let got = call(&mut c, "get_task", json!({ "id": &id })).unwrap();
        assert_eq!(got["priority"], 0);

        let done = call(&mut c, "complete_task", json!({ "id": &id })).unwrap();
        assert_eq!(done["completed"]["status"], "done");
        assert!(done["next"].is_null(), "a non-recurring task has no next instance");
    }

    /// A replayed create returns the SAME row instead of a second one.
    ///
    /// This is what makes offline capture safe. A phone that queues a create
    /// and retries after a timeout cannot know whether the first attempt
    /// landed — so without a client-supplied id, every retry makes a duplicate
    /// and nothing afterwards can tell the two apart.
    ///
    /// Found by running the real app against the real server: an outbox that
    /// drained twice produced two identical tasks, and the client id was being
    /// dropped by serde because `NewTask` had no such field.
    #[test]
    fn creating_with_the_same_client_id_twice_makes_one_task() {
        let mut c = conn();
        let args = json!({ "task": { "id": "captured-offline-1", "title": "on the train" } });

        let first = call(&mut c, "create_task", args.clone()).unwrap();
        let second = call(&mut c, "create_task", args).unwrap();

        assert_eq!(first["id"], "captured-offline-1", "the client's id must be honoured");
        assert_eq!(second["id"], first["id"], "a replay returns the same row");

        let all = call(&mut c, "list_tasks", json!({ "filters": {} })).unwrap();
        let matching = all.as_array().unwrap().iter()
            .filter(|t| t["title"] == "on the train")
            .count();
        assert_eq!(matching, 1, "a replayed create must not duplicate");
    }

    /// And the id stays optional, because every existing caller omits it.
    #[test]
    fn a_create_without_an_id_still_gets_one() {
        let mut c = conn();
        let made = call(&mut c, "create_task", json!({ "task": { "title": "x" } })).unwrap();
        assert!(!made["id"].as_str().unwrap().is_empty());
    }

    /// A T-number resolves, because `get_task` uses `resolve_ref`.
    ///
    /// Worth pinning: swapping it for `db::tasks::get` would compile, pass
    /// every other test, and quietly break `cxtasks://task/T42` and every
    /// "handle T69" reference.
    #[test]
    fn get_task_accepts_a_t_number_as_well_as_a_uuid() {
        let mut c = conn();
        let created = call(&mut c, "create_task", json!({ "task": { "title": "x" } })).unwrap();
        let short = created["short_id"].as_i64().expect("a short id was claimed");

        let by_ref = call(&mut c, "get_task", json!({ "id": format!("T{short}") })).unwrap();
        assert_eq!(by_ref["id"], created["id"]);
    }

    /// Completion of a RECURRING task materializes the next instance.
    ///
    /// This is the composition the plan cites for why the table wraps verbs
    /// rather than exposing `db::*`: a caller reaching for `db::tasks::update`
    /// with `status = done` gets a task that never comes back.
    #[test]
    fn completing_a_recurring_task_materializes_the_next_one() {
        let mut c = conn();
        let created = call(
            &mut c,
            "create_task",
            json!({ "task": { "title": "water the plants", "recurrence": "daily",
                              "due_at": "2026-01-01T09:00:00.000Z" } }),
        )
        .unwrap();
        let done = call(&mut c, "complete_task", json!({ "id": created["id"].as_str().unwrap() })).unwrap();
        assert!(!done["next"].is_null(), "a daily task must come back");
        assert_ne!(done["next"]["id"], created["id"], "and as a NEW row");
    }

    /// An invalid recurrence is refused at the door, on both writing verbs.
    ///
    /// Storing one is a completion that silently fails to materialize the next
    /// instance — discovered days later, when the task simply does not return.
    #[test]
    fn an_unparseable_recurrence_is_refused_by_create_and_by_update() {
        let mut c = conn();
        let err = call(&mut c, "create_task", json!({ "task": { "title": "x", "recurrence": "every so often" } }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid recurrence"), "{err}");

        let ok_task = call(&mut c, "create_task", json!({ "task": { "title": "y" } })).unwrap();
        let err = call(
            &mut c,
            "update_task",
            json!({ "id": ok_task["id"].as_str().unwrap(), "patch": { "recurrence": "every so often" } }),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("invalid recurrence"), "{err}");
    }

    /// `get_run` answers by id, which is what a deep link has.
    ///
    /// `recent_runs` is a window and cannot answer `cxtasks://run/<id>` for
    /// anything older than its limit. The two assertions are a pair: the window
    /// must NOT contain the run, or this proves nothing.
    #[test]
    fn get_run_resolves_a_run_the_recent_window_has_dropped() {
        let mut c = conn();
        let older = call(&mut c, "create_task", json!({ "task": { "title": "older" } })).unwrap();
        let older_run = db::runs::start(
            &c, older["id"].as_str().unwrap(), "background", None,
            crate::executor::BackgroundExecutor::Claude,
            crate::intent::BackgroundIntent::Work, None,
        ).unwrap();
        let newer = call(&mut c, "create_task", json!({ "task": { "title": "newer" } })).unwrap();
        db::runs::start(
            &c, newer["id"].as_str().unwrap(), "background", None,
            crate::executor::BackgroundExecutor::Claude,
            crate::intent::BackgroundIntent::Work, None,
        ).unwrap();

        let window = call(&mut c, "recent_runs", json!({ "limit": 1 })).unwrap();
        assert_eq!(window.as_array().unwrap().len(), 1);
        assert_ne!(window[0]["id"], older_run.id.as_str(), "the window must have dropped it");

        let got = call(&mut c, "get_run", json!({ "run_id": &older_run.id })).unwrap();
        assert_eq!(got["id"], older_run.id.as_str());
        assert_eq!(got["task_title"], "older", "same RunWithTask shape as the list");
    }

    /// An unknown run id is `null`, not an error — a deep link to a run that no
    /// longer exists should render "not found", not an error dialog.
    #[test]
    fn get_run_returns_null_for_an_unknown_id() {
        let mut c = conn();
        assert!(call(&mut c, "get_run", json!({ "run_id": "nope" })).unwrap().is_null());
    }

    /// A host-local verb is refused DIFFERENTLY from an unknown one.
    ///
    /// The distinction is the whole value of `HOST_LOCAL`: "that only runs on
    /// the machine that owns the files" is actionable, and "unknown command"
    /// sends the caller hunting for a typo that isn't there.
    #[test]
    fn a_host_local_verb_is_refused_with_its_own_reason() {
        let mut c = conn();
        let local = call(&mut c, "restore_checkpoint", json!({ "run_id": "r" })).unwrap_err().to_string();
        assert!(local.contains("only run on the host"), "{local}");

        let unknown = call(&mut c, "make_me_a_sandwich", json!({})).unwrap_err().to_string();
        assert!(unknown.contains("unknown command"), "{unknown}");
        assert!(!unknown.contains("only run on the host"));
    }

    /// A missing argument names itself.
    #[test]
    fn a_missing_argument_says_which_one() {
        let mut c = conn();
        let err = call(&mut c, "get_task", json!({})).unwrap_err().to_string();
        assert!(err.contains("missing argument 'id'"), "{err}");
    }

    /// Absent and null are the same for an OPTIONAL parameter — and that is
    /// deliberately not the `TaskPatch` convention, where explicit null means
    /// "clear this column". These are two different questions and the table
    /// must not conflate them.
    #[test]
    fn an_optional_argument_treats_absent_and_null_alike() {
        let mut c = conn();
        let a = call(&mut c, "recent_runs", json!({})).unwrap();
        let b = call(&mut c, "recent_runs", json!({ "limit": null })).unwrap();
        assert_eq!(a, b);
    }

    /// But a PATCH still distinguishes them, which is the point above.
    #[test]
    fn a_patch_still_clears_a_column_with_an_explicit_null() {
        let mut c = conn();
        let created = call(
            &mut c,
            "create_task",
            json!({ "task": { "title": "x", "due_at": "2026-03-01T09:00:00.000Z" } }),
        )
        .unwrap();
        let id = created["id"].as_str().unwrap().to_string();
        assert!(!created["due_at"].is_null());

        // Absent key: left alone.
        call(&mut c, "update_task", json!({ "id": &id, "patch": { "title": "y" } })).unwrap();
        let got = call(&mut c, "get_task", json!({ "id": &id })).unwrap();
        assert!(!got["due_at"].is_null(), "an absent key must not clear the column");

        // Explicit null: cleared.
        call(&mut c, "update_task", json!({ "id": &id, "patch": { "due_at": null } })).unwrap();
        let got = call(&mut c, "get_task", json!({ "id": &id })).unwrap();
        assert!(got["due_at"].is_null(), "an explicit null must clear it");
    }

    /// The three patch states survive a JSON round trip.
    ///
    /// This is the most dangerous thing in the verb table. `TaskPatch` means
    /// **absent = leave alone, explicit null = clear**, and routing patches
    /// through JSON puts that contract at the mercy of a serde derive. A plain
    /// `#[derive(Serialize)]` emits `null` for `None`, which the deserializer
    /// reads back as `Some(None)` — so every field the caller did not set would
    /// arrive as an instruction to clear it, and a patch touching one field
    /// would wipe the rest of the task.
    ///
    /// `skip_serializing_if` on every field is what prevents that, and this
    /// asserts it directly rather than through a symptom, because the symptom
    /// is silent data loss.
    #[test]
    fn a_patch_survives_a_json_round_trip_without_clearing_anything() {
        let patch = db::tasks::TaskPatch {
            title: Some("only this".into()),
            due_at: Some(None),   // explicit clear
            ..Default::default()  // everything else absent
        };

        let encoded = serde_json::to_value(&patch).unwrap();
        let obj = encoded.as_object().unwrap();

        assert!(obj.contains_key("title"), "a set field must be present");
        assert!(
            obj.contains_key("due_at") && obj["due_at"].is_null(),
            "an explicit clear must survive as null, got {encoded}"
        );
        assert!(
            !obj.contains_key("body") && !obj.contains_key("project"),
            "absent fields must be OMITTED, not null — got {encoded}"
        );

        // And decoding it back yields the same three states.
        let back: db::tasks::TaskPatch = serde_json::from_value(encoded).unwrap();
        assert_eq!(back.title.as_deref(), Some("only this"));
        assert_eq!(back.due_at, Some(None), "still an explicit clear");
        assert_eq!(back.body, None, "still absent");
    }

    /// The same thing, end to end through the table, against a real row.
    #[test]
    fn patching_one_field_through_the_table_leaves_the_others_alone() {
        let mut c = conn();
        let created = call(
            &mut c,
            "create_task",
            json!({ "task": { "title": "keep me", "body": "and my notes",
                              "project": "cxtasks", "due_at": "2026-05-01T09:00:00.000Z" } }),
        )
        .unwrap();
        let id = created["id"].as_str().unwrap().to_string();

        call(&mut c, "update_task", json!({ "id": &id, "patch": { "priority": 1 } })).unwrap();

        let got = call(&mut c, "get_task", json!({ "id": &id })).unwrap();
        assert_eq!(got["priority"], 1);
        assert_eq!(got["title"], "keep me", "title was not in the patch");
        assert_eq!(got["body"], "and my notes", "body was not in the patch");
        assert_eq!(got["project"], "cxtasks", "project was not in the patch");
        assert!(!got["due_at"].is_null(), "due_at was not in the patch");
    }

    /// Destroying a task removes its rows; the attachment sweep runs beside it.
    #[test]
    fn destroy_and_empty_trash_both_reach_the_attachment_cleanup() {
        let mut c = conn();
        let a = call(&mut c, "create_task", json!({ "task": { "title": "a" } })).unwrap();
        call(&mut c, "destroy_task", json!({ "id": a["id"].as_str().unwrap() })).unwrap();
        assert!(call(&mut c, "get_task", json!({ "id": a["id"].as_str().unwrap() })).unwrap().is_null());

        let b = call(&mut c, "create_task", json!({ "task": { "title": "b" } })).unwrap();
        call(&mut c, "trash_task", json!({ "id": b["id"].as_str().unwrap() })).unwrap();
        let n = call(&mut c, "empty_trash", json!({})).unwrap();
        assert_eq!(n, 1);
    }

    /// A verb that opens its own transaction still works INSIDE an outer one.
    ///
    /// This is the property sync depends on, and the reason `reorder_tasks` and
    /// `reorder_projects` use `savepoint()` rather than `transaction()`.
    ///
    /// The wrapper that lands with sync has to apply the verb and append its
    /// outbox entry in ONE transaction — a crash between them silently drops a
    /// change. rusqlite's `Transaction` is `Deref` only, never `DerefMut`, so a
    /// transaction can never be handed to `dispatch(&mut Connection, …)`; the
    /// wrapper must therefore open its own with `BEGIN IMMEDIATE` and keep the
    /// `&mut Connection`. At which point any arm holding a nested
    /// `conn.transaction()` fails with "cannot start a transaction within a
    /// transaction" — and it fails at RUNTIME, on two ordinary verbs, only once
    /// sync is switched on.
    #[test]
    fn a_verb_that_opens_its_own_transaction_still_works_inside_an_outer_one() {
        let mut c = conn();
        let a = call(&mut c, "create_task", json!({ "task": { "title": "a" } })).unwrap();
        let b = call(&mut c, "create_task", json!({ "task": { "title": "b" } })).unwrap();
        let (a_id, b_id) = (a["id"].as_str().unwrap().to_string(), b["id"].as_str().unwrap().to_string());
        call(&mut c, "create_project", json!({ "name": "one" })).unwrap();
        call(&mut c, "create_project", json!({ "name": "two" })).unwrap();
        let section_a = call(&mut c, "create_section", json!({ "name": "first" })).unwrap();
        let section_b = call(&mut c, "create_section", json!({ "name": "second" })).unwrap();

        // Exactly what the sync wrapper will do.
        c.execute_batch("BEGIN IMMEDIATE").unwrap();
        call(&mut c, "reorder_tasks", json!({ "ids": [&b_id, &a_id] }))
            .expect("reorder_tasks must survive an outer transaction");
        call(&mut c, "reorder_projects", json!({ "names": ["two", "one"] }))
            .expect("reorder_projects must survive an outer transaction");
        call(&mut c, "reorder_sections", json!({ "ids": [section_b["id"], section_a["id"]] }))
            .expect("reorder_sections must survive an outer transaction");
        c.execute_batch("COMMIT").unwrap();

        // And the writes actually landed, rather than being rolled back with
        // the savepoint.
        let order: i64 = c
            .query_row("SELECT sort_order FROM tasks WHERE id = ?1", [&b_id], |r| r.get(0))
            .unwrap();
        assert_eq!(order, 0, "b was moved to the front");
        let first: String = c
            .query_row("SELECT name FROM projects ORDER BY sort_order ASC LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(first, "two");
        let sections = call(&mut c, "list_sections", json!({})).unwrap();
        assert_eq!(sections[0]["id"], section_b["id"]);
    }

    /// And they still work with NO outer transaction, which is every call today.
    #[test]
    fn the_same_verbs_still_work_on_their_own() {
        let mut c = conn();
        let a = call(&mut c, "create_task", json!({ "task": { "title": "a" } })).unwrap();
        call(&mut c, "reorder_tasks", json!({ "ids": [a["id"].as_str().unwrap()] })).unwrap();
        call(&mut c, "create_project", json!({ "name": "solo" })).unwrap();
        call(&mut c, "reorder_projects", json!({ "names": ["solo"] })).unwrap();
    }

    // ── sync wrapper ────────────────────────────────────────────────────────

    fn synced_conn() -> Connection {
        let conn = crate::db::schema::test_conn();
        crate::sync::state::set_enabled(&conn, true).unwrap();
        conn
    }

    /// **Which verbs REMOVE rows** — discovered, not guessed.
    ///
    /// Runs every verb in the table against a seeded database and reports any
    /// whose execution made a synced table shrink. That set must equal
    /// `DELETES_ROWS`, because a delete that is not recorded as an intent can
    /// never travel: post-images describe rows that exist, and the whole point
    /// of a delete is that one does not.
    #[test]
    fn the_list_of_deleting_verbs_matches_what_the_verbs_actually_do() {
        let synced: Vec<&str> = crate::sync::tables::synced_tables().collect();
        let mut observed: Vec<&str> = Vec::new();

        for cmd in ALL_VERBS {
            if is_read_only(cmd) {
                continue;
            }
            let mut c = conn();
            // A fixture rich enough that most verbs have something to remove.
            let task = call(&mut c, "create_task", json!({ "task": { "title": "seed", "project": "p" } })).unwrap();
            let id = task["id"].as_str().unwrap().to_string();
            let other = call(&mut c, "create_task", json!({ "task": { "title": "other" } })).unwrap();
            let other_id = other["id"].as_str().unwrap().to_string();
            call(&mut c, "set_task_tags", json!({ "task_id": &id, "tags": ["a", "b"] })).unwrap();
            let comment = call(&mut c, "add_comment", json!({ "task_id": &id, "body": "hi" })).unwrap();
            call(&mut c, "add_dependency", json!({ "task_id": &id, "blocked_by_id": &other_id })).unwrap();
            // An attachment row, added through `db::attachments` rather than
            // the verb table because the two ADD verbs are `HOST_LOCAL` and
            // take paths on local disk. Without one, `delete_task_attachment`
            // deletes nothing and this test cannot tell a verb that removes
            // rows from one that does not — which is exactly how it missed it.
            let attachment = crate::db::attachments::add(
                &c, &id, None, "shot.png", "image/png",
                "00000000-1111-2222-3333-444444444444.png", None,
            )
            .unwrap();
            call(&mut c, "create_section", json!({ "project": "p", "name": "s" })).unwrap();
            call(&mut c, "add_project_repo", json!({ "project": "p", "repo_path": "/tmp/r" })).unwrap();
            call(&mut c, "trash_task", json!({ "id": &other_id })).unwrap();
            let section = call(&mut c, "list_sections", json!({ "project": "p" })).unwrap();
            let section_id = section[0]["id"].as_str().unwrap_or_default().to_string();
            let repo = call(&mut c, "list_project_repos", json!({})).unwrap();
            let repo_id = repo[0]["id"].as_str().unwrap_or_default().to_string();

            let before: Vec<i64> = synced
                .iter()
                .map(|t| c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap())
                .collect();

            let args = json!({
                "id": if *cmd == "delete_comment" { comment["id"].clone() }
                      else if *cmd == "delete_task_attachment" { json!(attachment.id) }
                      else if *cmd == "rename_section" || *cmd == "delete_section"
                           || *cmd == "set_section_target_date" || *cmd == "set_section_completed" { json!(section_id) }
                      else if *cmd == "remove_project_repo" || *cmd == "set_primary_project_repo" { json!(repo_id) }
                      else { json!(id) },
                "task_id": &id, "blocked_by_id": &other_id, "tags": [], "body": "x",
                // Tag verbs need a real TAG name, and `rename_tag` needs a
                // target that already exists — renaming onto an existing tag
                // MERGES (invariant 17), which removes a row, while renaming
                // onto a free name is a plain update.
                "name": if *cmd == "delete_tag" { json!("a") } else { json!("p") },
                "project": "p", "from": "a", "to": "b", "ids": [&id],
                "names": ["p"], "repo_path": "/tmp/r", "section_id": json!(null),
                "kind": "area", "closed": false, "archived": false, "done": false,
                "health": json!(null), "target_date": json!(null),
                "patch": {}, "settings": {}, "paused": false, "limit": 5, "spec": {}, "query": "x",
            });
            let _ = dispatch(&mut c, cmd, &args);

            let after: Vec<i64> = synced
                .iter()
                .map(|t| c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap())
                .collect();

            if before.iter().zip(&after).any(|(b, a)| a < b) {
                observed.push(cmd);
            }
        }

        let mut declared: Vec<&str> = DELETES_ROWS.to_vec();
        declared.sort_unstable();
        observed.sort_unstable();

        let undeclared: Vec<&&str> = observed.iter().filter(|c| !declared.contains(c)).collect();
        assert!(
            undeclared.is_empty(),
            "these verbs remove rows but are not in DELETES_ROWS, so their deletions would \
             never reach the peer and the next pull would resurrect what they removed: {undeclared:?}"
        );
    }

    /// **The guard that lets `READ_ONLY` exist at all.**
    ///
    /// Every verb the wrapper skips the transaction for is run against a seeded
    /// database and must change zero rows. Listing a verb that actually writes
    /// is the one harmful mistake available here, and it would be invisible:
    /// the change would apply locally, never be recorded, and be reverted by
    /// the next pull.
    #[test]
    fn every_verb_called_read_only_really_changes_nothing() {
        let mut c = conn();
        let task = call(&mut c, "create_task", json!({ "task": { "title": "seed", "project": "p" } })).unwrap();
        let id = task["id"].as_str().unwrap().to_string();
        // No explicit `create_project`: filing a task into "p" already creates
        // the project row, and asking twice is a refusal.

        for cmd in READ_ONLY {
            // Give each one whatever it needs; a verb that errors on a missing
            // argument still proves the point, since a refusal writes nothing.
            let args = json!({
                "id": &id, "task_id": &id, "parent_id": &id, "run_id": "none",
                "filters": {}, "spec": {}, "query": "seed", "name": "p", "project": "p",
                "repo_path": "/tmp", "limit": 5,
            });
            let before = c.total_changes();
            let _ = dispatch(&mut c, cmd, &args);
            assert_eq!(
                c.total_changes(),
                before,
                "'{cmd}' is listed READ_ONLY but changed rows — it would skip the outbox \
                 and its edits would be silently reverted by the next pull"
            );
        }
    }

    /// A DELETION is recorded, with its arguments, exactly once.
    #[test]
    fn a_deletion_lands_in_the_outbox_with_what_it_was_called_with() {
        let mut c = synced_conn();
        let made = dispatch_synced(&mut c, "create_task", &json!({ "task": { "title": "doomed" } })).unwrap();
        let id = made["id"].as_str().unwrap().to_string();
        dispatch_synced(&mut c, "destroy_task", &json!({ "id": &id })).unwrap();

        // The create travels as state — swept just ahead of the verb so the
        // peer holds the row the verb deletes — and only the delete is a verb.
        let queued = crate::sync::outbox::pending(&c, 10).unwrap();
        let verbs: Vec<_> =
            queued.iter().filter(|i| i.cmd != crate::sync::sweep::SYNC_UPSERT).collect();
        assert_eq!(verbs.len(), 1, "only the delete is a verb: {queued:?}");
        assert_eq!(verbs[0].cmd, "destroy_task");
        assert_eq!(verbs[0].args["id"], id.as_str());
        assert_eq!(verbs[0].host, crate::host::host_id());
        assert_eq!(queued.last().unwrap().cmd, "destroy_task", "the verb is queued last");
    }

    /// **A create is NOT recorded**, and that is the architecture rather than an
    /// omission.
    ///
    /// Replayed on the peer it would generate a different uuid and claim a
    /// different T-number, and the swept post-image of the local row would then
    /// collide — `UNIQUE constraint failed: tasks.short_id`, which is the
    /// version that announces itself. `complete_task` on a recurring task is
    /// the version that does not: both ends materialize a next instance and the
    /// duplicate is silent.
    #[test]
    fn a_create_travels_as_state_rather_than_as_a_replayed_verb() {
        let mut c = synced_conn();
        dispatch_synced(&mut c, "create_task", &json!({ "task": { "title": "x" } })).unwrap();
        dispatch_synced(&mut c, "create_task", &json!({ "task": { "title": "y", "recurrence": "daily", "due_at": "2030-01-01T09:00:00.000Z" } })).unwrap();

        assert_eq!(
            crate::sync::outbox::count(&c).unwrap(),
            0,
            "a create replayed on the peer makes a DIFFERENT row; the sweep sends the real one"
        );
    }

    /// A read records nothing, so the outbox does not fill up with questions.
    #[test]
    fn reads_are_never_recorded() {
        let mut c = synced_conn();
        dispatch_synced(&mut c, "create_task", &json!({ "task": { "title": "x" } })).unwrap();
        for cmd in ["list_tasks", "view_counts", "project_board", "budget_status"] {
            let args = json!({ "filters": {} });
            let _ = dispatch_synced(&mut c, cmd, &args);
        }
        assert_eq!(crate::sync::outbox::count(&c).unwrap(), 0, "neither reads nor creates queue a verb");
    }

    /// A verb that changes nothing is not recorded even when it is a "write".
    ///
    /// Detecting mutation rather than declaring it is what makes this true
    /// without a list: a no-op patch produces no rows and therefore no intent.
    #[test]
    fn a_write_that_changes_nothing_is_not_recorded() {
        let mut c = synced_conn();
        let made = dispatch_synced(&mut c, "create_task", &json!({ "task": { "title": "x" } })).unwrap();
        let id = made["id"].as_str().unwrap().to_string();
        let before = crate::sync::outbox::count(&c).unwrap();

        // `trash_task` on an id that does not exist: reaches the write path,
        // changes nothing.
        let _ = dispatch_synced(&mut c, "trash_task", &json!({ "id": "no-such-task" }));
        assert_eq!(crate::sync::outbox::count(&c).unwrap(), before);
        let _ = id;
    }

    /// A verb refused at the door writes nothing and records nothing.
    ///
    /// Deliberately named for what it proves. The recurrence check runs BEFORE
    /// any write, so this exercises the validation path — it does not prove the
    /// rollback, which is why the test below exists. (Found by removing the
    /// `ROLLBACK` and watching this one still pass.)
    #[test]
    fn a_verb_refused_before_writing_leaves_nothing_behind() {
        let mut c = synced_conn();
        let before_tasks: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();

        let err = dispatch_synced(
            &mut c,
            "create_task",
            &json!({ "task": { "title": "doomed", "recurrence": "every so often" } }),
        );
        assert!(err.is_err(), "an unparseable recurrence must be refused");

        let after: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(after, before_tasks, "a refused create must leave no task");
        assert_eq!(crate::sync::outbox::count(&c).unwrap(), 0, "and no intent");
    }

    /// **The atomicity guarantee, tested where it can actually fail.**
    ///
    /// If the change lands and the intent does not, the local database has
    /// moved on, the peer never hears about it, and the next pull silently
    /// reverts it — the exact data loss the outbox exists to prevent. Dropping
    /// the table is a blunt way to make the append fail, and it is the only
    /// failure that matters: everything else about recording an intent is one
    /// INSERT.
    #[test]
    fn a_change_whose_intent_cannot_be_recorded_does_not_survive() {
        let mut c = synced_conn();
        let made = dispatch_synced(&mut c, "create_task", &json!({ "task": { "title": "doomed" } })).unwrap();
        let id = made["id"].as_str().unwrap().to_string();
        let before: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();

        c.execute_batch("DROP TABLE sync_outbox").unwrap();

        let result = dispatch_synced(&mut c, "destroy_task", &json!({ "id": &id }));
        assert!(result.is_err(), "the verb must fail when its intent cannot be recorded");

        let after: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(
            after, before,
            "the task was destroyed but the deletion was never recorded — the peer would \
             never hear, and the next pull would resurrect it"
        );
    }

    /// **With sync off, nothing is recorded and nothing changes.**
    ///
    /// The standalone Mac is the majority case and must stay byte-identical.
    #[test]
    fn with_sync_off_the_outbox_stays_empty() {
        let mut c = conn(); // sync never enabled
        for i in 0..5 {
            dispatch_synced(&mut c, "create_task", &json!({ "task": { "title": format!("t{i}") } })).unwrap();
        }
        assert_eq!(crate::sync::outbox::count(&c).unwrap(), 0);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 5, "the writes still happened, they were just not recorded");
    }

    /// The verbs that must never be replayed are not recorded.
    #[test]
    fn a_host_local_effect_is_not_queued_for_the_peer() {
        let mut c = synced_conn();
        dispatch_synced(&mut c, "reset_budget_window", &json!({})).unwrap();
        assert_eq!(
            crate::sync::outbox::count(&c).unwrap(),
            0,
            "resetting THIS host's budget window must not reset the peer's"
        );
    }

    /// **The canonical node keeps no outbox.**
    ///
    /// It is what peers reconcile against, so intents recorded there would be
    /// drained by nobody and the table would grow without bound.
    #[test]
    fn the_canonical_node_records_nothing() {
        let mut c = synced_conn();
        crate::sync::state::set_canonical(&c, true).unwrap();

        for i in 0..3 {
            dispatch_synced(&mut c, "create_task", &json!({ "task": { "title": format!("t{i}") } })).unwrap();
        }

        assert_eq!(crate::sync::outbox::count(&c).unwrap(), 0);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3, "but the writes still happen — it is a real database, not a relay");
    }

    /// A canonical node still rolls back a verb that fails mid-write.
    #[test]
    fn the_canonical_node_still_rolls_back_a_failure() {
        let mut c = synced_conn();
        crate::sync::state::set_canonical(&c, true).unwrap();
        let before: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        let _ = dispatch_synced(
            &mut c,
            "create_task",
            &json!({ "task": { "title": "x", "recurrence": "nonsense" } }),
        );
        let after: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(after, before);
    }

    /// And the nesting property holds through the wrapper, not just in theory.
    #[test]
    fn a_verb_with_its_own_savepoint_works_through_the_sync_wrapper() {
        let mut c = synced_conn();
        let a = dispatch_synced(&mut c, "create_task", &json!({ "task": { "title": "a" } })).unwrap();
        let b = dispatch_synced(&mut c, "create_task", &json!({ "task": { "title": "b" } })).unwrap();
        dispatch_synced(
            &mut c,
            "reorder_tasks",
            &json!({ "ids": [b["id"].as_str().unwrap(), a["id"].as_str().unwrap()] }),
        )
        .expect("reorder must survive the wrapper's transaction");

        // It reaches the peer as state, not as a replayed verb, so the outbox
        // stays empty — what matters here is that the nested savepoint did not
        // collide with the wrapper's transaction.
        let order: i64 = c
            .query_row("SELECT sort_order FROM tasks WHERE id = ?1", [b["id"].as_str().unwrap()], |r| r.get(0))
            .unwrap();
        assert_eq!(order, 0, "b was moved to the front");
    }

    /// Every verb in `HOST_LOCAL` is genuinely absent from the table, so the
    /// list cannot rot into claiming something the table actually handles.
    #[test]
    fn the_host_local_list_and_the_table_do_not_overlap() {
        let mut c = conn();
        for cmd in HOST_LOCAL {
            let err = call(&mut c, cmd, json!({})).unwrap_err().to_string();
            assert!(
                err.contains("only run on the host"),
                "{cmd} is in HOST_LOCAL but the table answered it: {err}"
            );
        }
    }
}
