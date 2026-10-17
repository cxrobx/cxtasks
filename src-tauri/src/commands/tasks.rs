//! Tauri IPC commands for the task board.
//!
//! **Every command here is a wrapper.** The work happens in
//! `cxtasks_core::api::dispatch`, the one verb table, and these functions exist
//! only to be the IPC boundary: they give the frontend the names and types it
//! already invokes, and they satisfy the capability ACL, which is keyed on
//! command names.
//!
//! Routing the UI through the same table the sync server will use is what makes
//! drift impossible rather than merely discouraged. Before this, a command body
//! and a server handler would have been two implementations of "complete a
//! task", and the second one forgets that completion materializes the next
//! recurrence instance.
//!
//! The lock scope stays small (cxmail gotcha #11) — take the guard, dispatch,
//! drop it. Nothing holds the mutex across an `await`.

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::db::{self, meta::BudgetSettings, runs::TaskRun, tasks::*};
use crate::error::AppError;
use crate::{AppState, LockExt};
use tauri::State;

// Defined in core so the table and the UI cannot disagree about their shape.
pub use cxtasks_core::api::{CompletionResult, TaskRelations};
pub use cxtasks_core::db::retrospective::{Retrospective, RetrospectiveSpec};
pub use cxtasks_core::jev::{CaptureAssessment, CaptureRequest, ReadinessAssessment, SaveSettings as JevSaveSettings, Status as JevStatus};

/// Directory names under `~/Projects` — the autocomplete source for a task's
/// `project` field and for picking a `repo_path`.
#[derive(serde::Serialize)]
pub struct RepoSuggestion {
    pub name: String,
    pub path: String,
}

/// Take the lock, run one verb, decode the answer.
///
/// The extra JSON hop is deliberate and cheap. Tauri already serializes across
/// the IPC boundary, so this adds one encode/decode of a result that was about
/// to be encoded anyway — and it buys a single implementation of every verb.
fn call<T: DeserializeOwned>(
    state: &State<'_, AppState>,
    cmd: &str,
    args: Value,
) -> Result<T, AppError> {
    let mut conn = state.db.safe_lock();
    // `dispatch_synced`, not `dispatch`: with sync on, the change and the record
    // of it must commit together. With sync off it is the same call it always
    // was — one indexed `app_meta` read and then straight through.
    let value = cxtasks_core::api::dispatch_synced(&mut conn, cmd, &args)?;
    serde_json::from_value(value)
        .map_err(|e| AppError::General(format!("could not decode '{cmd}' result: {e}")))
}

#[tauri::command]
pub async fn list_tasks(
    state: State<'_, AppState>,
    filters: ListFilters,
) -> Result<Vec<Task>, AppError>{
    call(&state, "list_tasks", json!({"filters": filters}))
}

#[tauri::command]
pub async fn get_task(
    state: State<'_, AppState>,
    id: String,
) -> Result<Option<Task>, AppError>{
    call(&state, "get_task", json!({"id": id}))
}

#[tauri::command]
pub async fn list_subtasks(
    state: State<'_, AppState>,
    parent_id: String,
) -> Result<Vec<Task>, AppError>{
    call(&state, "list_subtasks", json!({"parent_id": parent_id}))
}

#[tauri::command]
pub async fn create_task(
    state: State<'_, AppState>,
    task: NewTask,
) -> Result<Task, AppError>{
    call(&state, "create_task", json!({"task": task}))
}

#[tauri::command]
pub async fn update_task(
    state: State<'_, AppState>,
    id: String,
    patch: TaskPatch,
) -> Result<Task, AppError>{
    call(&state, "update_task", json!({"id": id, "patch": patch}))
}

#[tauri::command]
pub async fn complete_task(
    state: State<'_, AppState>,
    id: String,
    result: Option<String>,
) -> Result<CompletionResult, AppError>{
    call(&state, "complete_task", json!({"id": id, "result": result}))
}

#[tauri::command]
pub async fn trash_task(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError>{
    call(&state, "trash_task", json!({"id": id}))
}

#[tauri::command]
pub async fn restore_task(
    state: State<'_, AppState>,
    id: String,
) -> Result<Task, AppError>{
    call(&state, "restore_task", json!({"id": id}))
}

#[tauri::command]
pub async fn archive_task(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError>{
    call(&state, "archive_task", json!({"id": id}))
}

#[tauri::command]
pub async fn unarchive_task(
    state: State<'_, AppState>,
    id: String,
) -> Result<Task, AppError>{
    call(&state, "unarchive_task", json!({"id": id}))
}

#[tauri::command]
pub async fn destroy_task(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError>{
    call(&state, "destroy_task", json!({"id": id}))
}

#[tauri::command]
pub async fn empty_trash(state: State<'_, AppState>) -> Result<usize, AppError>{
    call(&state, "empty_trash", json!({}))
}

#[tauri::command]
pub async fn trash_countdowns(state: State<'_, AppState>) -> Result<std::collections::HashMap<String, i64>, AppError>{
    call(&state, "trash_countdowns", json!({}))
}

#[tauri::command]
pub async fn reorder_tasks(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<(), AppError>{
    call(&state, "reorder_tasks", json!({"ids": ids}))
}

#[tauri::command]
pub async fn list_projects(state: State<'_, AppState>) -> Result<Vec<ProjectSummary>, AppError>{
    call(&state, "list_projects", json!({}))
}

#[tauri::command]
pub async fn view_counts(state: State<'_, AppState>) -> Result<ViewCounts, AppError>{
    call(&state, "view_counts", json!({}))
}

#[tauri::command]
pub async fn search_tasks(
    state: State<'_, AppState>,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<Task>, AppError>{
    let (mut tasks, rerank) = {
        let conn = state.db.safe_lock();
        let settings = cxtasks_core::jev::settings(&conn);
        (
            db::search::search(&conn, &query, limit.unwrap_or(50))?,
            settings.enabled && settings.recall,
        )
    };
    if !rerank {
        return Ok(tasks);
    }
    // Do not hold AppState's shared connection over HTTPS. A separate reader
    // lets task edits continue while Jev ranks the bounded shortlist.
    let conn = cxtasks_core::runner::open_db()?;
    if let Err(e) = cxtasks_core::jev::rerank_tasks(&conn, &query, &mut tasks) {
        log::warn!("Jev search rerank failed; keeping lexical order: {e}");
    }
    Ok(tasks)
}

#[tauri::command]
pub async fn filter_tasks(
    state: State<'_, AppState>,
    spec: db::search::FilterSpec,
) -> Result<Vec<Task>, AppError>{
    call(&state, "filter_tasks", json!({"spec": spec}))
}

#[tauri::command]
pub async fn count_filtered(
    state: State<'_, AppState>,
    specs: Vec<db::search::FilterSpec>,
) -> Result<Vec<i64>, AppError> {
    call(&state, "count_filtered", json!({"specs": specs}))
}

#[tauri::command]
pub async fn list_runs(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<TaskRun>, AppError>{
    call(&state, "list_runs", json!({"task_id": task_id}))
}

#[tauri::command]
pub async fn list_run_events(
    state: State<'_, AppState>,
    run_id: String,
    after_seq: Option<i64>,
) -> Result<Vec<db::run_events::RunEvent>, AppError>{
    call(&state, "list_run_events", json!({"run_id": run_id, "after_seq": after_seq}))
}

#[tauri::command]
pub async fn recent_runs(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<db::runs::RunWithTask>, AppError>{
    call(&state, "recent_runs", json!({"limit": limit}))
}

#[tauri::command]
pub async fn budget_status(state: State<'_, AppState>) -> Result<db::meta::BudgetStatus, AppError>{
    call(&state, "budget_status", json!({}))
}

#[tauri::command]
pub async fn save_budget_settings(
    state: State<'_, AppState>,
    settings: BudgetSettings,
) -> Result<db::meta::BudgetStatus, AppError>{
    call(&state, "save_budget_settings", json!({"settings": settings}))
}

#[tauri::command]
pub async fn reset_budget_window(state: State<'_, AppState>) -> Result<db::meta::BudgetStatus, AppError>{
    call(&state, "reset_budget_window", json!({}))
}

#[tauri::command]
pub async fn set_background_paused(
    state: State<'_, AppState>,
    paused: bool,
) -> Result<bool, AppError>{
    call(&state, "set_background_paused", json!({"paused": paused}))
}

#[tauri::command]
pub async fn suggest_repos() -> Result<Vec<RepoSuggestion>, AppError>{
    let root = match dirs::home_dir() {
        Some(h) => h.join("Projects"),
        None => return Ok(Vec::new()),
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<RepoSuggestion> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                return None;
            }
            Some(RepoSuggestion {
                name,
                path: e.path().to_string_lossy().to_string(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

#[tauri::command]
pub async fn list_tags(state: State<'_, AppState>) -> Result<Vec<db::tags::TagWithCount>, AppError>{
    call(&state, "list_tags", json!({}))
}

#[tauri::command]
pub async fn task_tags(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<db::tags::Tag>, AppError>{
    call(&state, "task_tags", json!({"task_id": task_id}))
}

#[tauri::command]
pub async fn set_task_tags(
    state: State<'_, AppState>,
    task_id: String,
    tags: Vec<String>,
) -> Result<Vec<db::tags::Tag>, AppError>{
    call(&state, "set_task_tags", json!({"task_id": task_id, "tags": tags}))
}

#[tauri::command]
pub async fn rename_tag(
    state: State<'_, AppState>,
    from: String,
    to: String,
) -> Result<db::tags::Tag, AppError>{
    call(&state, "rename_tag", json!({"from": from, "to": to}))
}

#[tauri::command]
pub async fn task_tag_map(state: State<'_, AppState>) -> Result<std::collections::HashMap<String, Vec<String>>, AppError>{
    call(&state, "task_tag_map", json!({}))
}

#[tauri::command]
pub async fn tag_usage_count(
    state: State<'_, AppState>,
    name: String,
) -> Result<i64, AppError>{
    call(&state, "tag_usage_count", json!({"name": name}))
}

#[tauri::command]
pub async fn delete_tag(
    state: State<'_, AppState>,
    name: String,
) -> Result<(), AppError>{
    call(&state, "delete_tag", json!({"name": name}))
}

#[tauri::command]
pub async fn task_relations(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<TaskRelations, AppError>{
    call(&state, "task_relations", json!({"task_id": task_id}))
}

#[tauri::command]
pub async fn add_dependency(
    state: State<'_, AppState>,
    task_id: String,
    blocked_by_id: String,
) -> Result<(), AppError>{
    call(&state, "add_dependency", json!({"task_id": task_id, "blocked_by_id": blocked_by_id}))
}

#[tauri::command]
pub async fn remove_dependency(
    state: State<'_, AppState>,
    task_id: String,
    blocked_by_id: String,
) -> Result<(), AppError>{
    call(&state, "remove_dependency", json!({"task_id": task_id, "blocked_by_id": blocked_by_id}))
}

#[tauri::command]
pub async fn add_relation(
    state: State<'_, AppState>,
    from_task_id: String,
    to_task_id: String,
    kind: String,
) -> Result<(), AppError>{
    call(&state, "add_relation", json!({
        "from_task_id": from_task_id,
        "to_task_id": to_task_id,
        "kind": kind
    }))
}

#[tauri::command]
pub async fn remove_relation(
    state: State<'_, AppState>,
    from_task_id: String,
    to_task_id: String,
    kind: String,
) -> Result<(), AppError>{
    call(&state, "remove_relation", json!({
        "from_task_id": from_task_id,
        "to_task_id": to_task_id,
        "kind": kind
    }))
}

#[tauri::command]
pub async fn blocked_task_ids(state: State<'_, AppState>) -> Result<Vec<String>, AppError>{
    call(&state, "blocked_task_ids", json!({}))
}

#[tauri::command]
pub async fn retrospective(
    state: State<'_, AppState>,
    spec: RetrospectiveSpec,
) -> Result<Retrospective, AppError>{
    let (mut report, rerank) = {
        let conn = state.db.safe_lock();
        let settings = cxtasks_core::jev::settings(&conn);
        (
            db::retrospective::read(&conn, &spec)?,
            settings.enabled && settings.recall,
        )
    };
    if !rerank {
        return Ok(report);
    }
    let conn = cxtasks_core::runner::open_db()?;
    if let Err(e) = cxtasks_core::jev::rerank_retrospective(&conn, &mut report) {
        log::warn!("Jev retrospective rerank failed; keeping evidence order: {e}");
    }
    Ok(report)
}

#[tauri::command]
pub async fn jev_status(_state: State<'_, AppState>) -> Result<JevStatus, AppError> {
    let conn = cxtasks_core::runner::open_db()?;
    Ok(cxtasks_core::jev::status(&conn))
}

#[tauri::command]
pub async fn save_jev_settings(
    _state: State<'_, AppState>,
    settings: JevSaveSettings,
) -> Result<JevStatus, AppError> {
    let conn = cxtasks_core::runner::open_db()?;
    cxtasks_core::jev::save_settings(&conn, &settings)
}

#[tauri::command]
pub async fn jev_assess_capture(
    _state: State<'_, AppState>,
    request: CaptureRequest,
) -> Result<Option<CaptureAssessment>, AppError> {
    let conn = cxtasks_core::runner::open_db()?;
    cxtasks_core::jev::assess_capture(&conn, &request)
}

#[tauri::command]
pub async fn jev_assess_readiness(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Option<ReadinessAssessment>, AppError> {
    let task = {
        let conn = state.db.safe_lock();
        db::tasks::resolve_ref(&conn, &task_id)?
            .ok_or_else(|| AppError::NotFound(format!("task {task_id} not found")))?
    };
    let conn = cxtasks_core::runner::open_db()?;
    cxtasks_core::jev::assess_readiness(&conn, &task)
}

/// Every live tripwire task's volatile row, keyed by task id — one call for
/// the board, the `task_tag_map` shape.
#[tauri::command]
pub async fn list_tripwire_runtime(
    state: State<'_, AppState>,
) -> Result<std::collections::HashMap<String, db::tripwires::Runtime>, AppError>{
    call(&state, "list_tripwire_runtime", json!({}))
}

/// The runnable scripts in THIS machine's tripwires directory — what the
/// picker offers. `HOST_LOCAL`, like `suggest_repos`, and implemented here
/// rather than in the verb table for the same reason: it reads local disk,
/// and a server answering it would describe the wrong machine.
#[tauri::command]
pub async fn tripwire_scripts() -> Result<Vec<String>, AppError>{
    Ok(cxtasks_core::tripwire::list_scripts_in(&cxtasks_core::paths::tripwires_dir()))
}

#[tauri::command]
pub async fn list_comments(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<db::comments::Comment>, AppError>{
    call(&state, "list_comments", json!({"task_id": task_id}))
}

#[tauri::command]
pub async fn add_comment(
    state: State<'_, AppState>,
    task_id: String,
    body: String,
) -> Result<db::comments::Comment, AppError>{
    call(&state, "add_comment", json!({"task_id": task_id, "body": body}))
}

#[tauri::command]
pub async fn edit_comment(
    state: State<'_, AppState>,
    id: String,
    body: String,
) -> Result<db::comments::Comment, AppError>{
    call(&state, "edit_comment", json!({"id": id, "body": body}))
}

#[tauri::command]
pub async fn delete_comment(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError>{
    call(&state, "delete_comment", json!({"id": id}))
}

#[tauri::command]
pub async fn list_all_projects(
    state: State<'_, AppState>,
    include_archived: Option<bool>,
) -> Result<Vec<db::projects::Project>, AppError>{
    call(&state, "list_all_projects", json!({"include_archived": include_archived}))
}

#[tauri::command]
pub async fn create_project(
    state: State<'_, AppState>,
    name: String,
    kind: Option<String>,
    parent: Option<String>,
) -> Result<db::projects::Project, AppError>{
    call(&state, "create_project", json!({"name": name, "kind": kind, "parent": parent}))
}

#[tauri::command]
pub async fn set_project_parent(
    state: State<'_, AppState>,
    name: String,
    parent: Option<String>,
) -> Result<(), AppError>{
    call(&state, "set_project_parent", json!({"name": name, "parent": parent}))
}

#[tauri::command]
pub async fn set_project_kind(
    state: State<'_, AppState>,
    name: String,
    kind: String,
) -> Result<(), AppError>{
    call(&state, "set_project_kind", json!({"name": name, "kind": kind}))
}

#[tauri::command]
pub async fn set_project_target_date(
    state: State<'_, AppState>,
    name: String,
    target_date: Option<String>,
) -> Result<(), AppError>{
    call(&state, "set_project_target_date", json!({"name": name, "target_date": target_date}))
}

#[tauri::command]
pub async fn set_project_closed(
    state: State<'_, AppState>,
    name: String,
    closed: bool,
) -> Result<(), AppError>{
    call(&state, "set_project_closed", json!({"name": name, "closed": closed}))
}

#[tauri::command]
pub async fn set_project_appearance(
    state: State<'_, AppState>,
    name: String,
    color: Option<String>,
    icon: Option<String>,
) -> Result<(), AppError> {
    call(&state, "set_project_appearance", json!({"name": name, "color": color, "icon": icon}))
}

#[tauri::command]
pub async fn set_tag_appearance(
    state: State<'_, AppState>,
    name: String,
    color: Option<String>,
    icon: Option<String>,
) -> Result<(), AppError> {
    call(&state, "set_tag_appearance", json!({"name": name, "color": color, "icon": icon}))
}

#[tauri::command]
pub async fn rename_project(
    state: State<'_, AppState>,
    from: String,
    to: String,
) -> Result<db::projects::Project, AppError>{
    call(&state, "rename_project", json!({"from": from, "to": to}))
}

#[tauri::command]
pub async fn archive_project(
    state: State<'_, AppState>,
    name: String,
    archived: bool,
) -> Result<(), AppError>{
    call(&state, "archive_project", json!({"name": name, "archived": archived}))
}

#[tauri::command]
pub async fn reorder_projects(
    state: State<'_, AppState>,
    names: Vec<String>,
) -> Result<(), AppError>{
    call(&state, "reorder_projects", json!({"names": names}))
}

#[tauri::command]
pub async fn delete_project(
    state: State<'_, AppState>,
    name: String,
    clear_from_tasks: Option<bool>,
) -> Result<(), AppError>{
    call(&state, "delete_project", json!({"name": name, "clear_from_tasks": clear_from_tasks}))
}

#[tauri::command]
pub async fn list_project_repos(state: State<'_, AppState>) -> Result<Vec<db::projects::ProjectRepo>, AppError>{
    call(&state, "list_project_repos", json!({}))
}

#[tauri::command]
pub async fn add_project_repo(
    state: State<'_, AppState>,
    project: String,
    repo_path: String,
    make_primary: Option<bool>,
) -> Result<db::projects::ProjectRepo, AppError>{
    call(&state, "add_project_repo", json!({"project": project, "repo_path": repo_path, "make_primary": make_primary}))
}

#[tauri::command]
pub async fn remove_project_repo(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError>{
    call(&state, "remove_project_repo", json!({"id": id}))
}

#[tauri::command]
pub async fn set_primary_project_repo(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError>{
    call(&state, "set_primary_project_repo", json!({"id": id}))
}

#[tauri::command]
pub async fn project_for_repo_path(
    state: State<'_, AppState>,
    repo_path: String,
) -> Result<Option<String>, AppError>{
    call(&state, "project_for_repo_path", json!({"repo_path": repo_path}))
}

#[tauri::command]
pub async fn list_sections(
    state: State<'_, AppState>,
    project: Option<String>,
) -> Result<Vec<db::projects::Section>, AppError>{
    call(&state, "list_sections", json!({"project": project}))
}

#[tauri::command]
pub async fn create_section(
    state: State<'_, AppState>,
    project: Option<String>,
    name: String,
) -> Result<db::projects::Section, AppError>{
    call(&state, "create_section", json!({"project": project, "name": name}))
}

#[tauri::command]
pub async fn rename_section(
    state: State<'_, AppState>,
    id: String,
    name: String,
) -> Result<(), AppError>{
    call(&state, "rename_section", json!({"id": id, "name": name}))
}

#[tauri::command]
pub async fn delete_section(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError>{
    call(&state, "delete_section", json!({"id": id}))
}

#[tauri::command]
pub async fn reorder_sections(
    state: State<'_, AppState>,
    project: Option<String>,
    ids: Vec<String>,
) -> Result<(), AppError> {
    call(&state, "reorder_sections", json!({"project": project, "ids": ids}))
}

#[tauri::command]
pub async fn project_board(state: State<'_, AppState>) -> Result<Vec<db::projects::BoardRow>, AppError>{
    call(&state, "project_board", json!({}))
}

#[tauri::command]
pub async fn post_project_update(
    state: State<'_, AppState>,
    project: String,
    body: String,
    health: Option<String>,
) -> Result<db::project_updates::ProjectUpdate, AppError>{
    call(&state, "post_project_update", json!({"project": project, "body": body, "health": health}))
}

#[tauri::command]
pub async fn list_project_updates(
    state: State<'_, AppState>,
    project: String,
    limit: Option<u32>,
) -> Result<Vec<db::project_updates::ProjectUpdate>, AppError>{
    call(&state, "list_project_updates", json!({"project": project, "limit": limit}))
}

#[tauri::command]
pub async fn set_section_target_date(
    state: State<'_, AppState>,
    id: String,
    target_date: Option<String>,
) -> Result<(), AppError>{
    call(&state, "set_section_target_date", json!({"id": id, "target_date": target_date}))
}

#[tauri::command]
pub async fn set_section_completed(
    state: State<'_, AppState>,
    id: String,
    done: bool,
) -> Result<(), AppError>{
    call(&state, "set_section_completed", json!({"id": id, "done": done}))
}

#[tauri::command]
pub async fn move_task_to_section(
    state: State<'_, AppState>,
    task_id: String,
    section_id: Option<String>,
) -> Result<(), AppError>{
    call(&state, "move_task_to_section", json!({"task_id": task_id, "section_id": section_id}))
}
