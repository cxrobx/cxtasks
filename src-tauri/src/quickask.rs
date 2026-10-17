//! Current selection plus the separate host-reviewed task handoff for Quick Ask.
#[path = "quickask_handoff.rs"]
mod handoff;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{Manager, State, WebviewWindow};
use crate::LockExt;

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionInput {
    task_id: Option<String>,
    account_id: Option<String>,
    folder: Option<String>,
    uid: Option<u32>,
    #[serde(default)]
    view: String,
    project: Option<String>,
    #[serde(default)]
    composing: bool,
}
#[derive(Default)]
pub struct SelectionState(Mutex<(Option<SelectionInput>, u32)>);

#[tauri::command]
pub fn publish_quickask_selection(window: WebviewWindow, state: State<'_, SelectionState>, selection: SelectionInput) -> Result<(), String> {
    if window.label() != "main" { return Err("Selection must come from the main window".into()); }
    if [&selection.task_id, &selection.account_id, &selection.folder, &selection.project].iter().any(|v| v.as_ref().is_some_and(|s| s.len() > 512)) || selection.view.len() > 128 {
        return Err("Invalid selection".into());
    }
    #[cfg(target_os = "macos")]
    let number: isize = unsafe {
        let ptr = window.ns_window().map_err(|_| "Window unavailable")?;
        objc2::msg_send![ptr as *mut objc2::runtime::AnyObject, windowNumber]
    };
    #[cfg(not(target_os = "macos"))]
    let number: isize = 0;
    *state.0.lock().map_err(|_| "Selection unavailable")? = (Some(selection), number as u32);
    Ok(())
}

pub fn start(app: tauri::AppHandle) {
    app.manage(SelectionState::default());
    // Only the local account can connect. No network port and no action endpoint.
    let Some(home) = std::env::var_os("HOME") else { return };
    let directory = std::path::PathBuf::from(home).join("Library/Application Support/QuickAsk/Context");
    if directory.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) { return; }
    if std::fs::create_dir_all(&directory).is_err() { return; }
    if std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).is_err() { return; }
    let path = directory.join("cxtasks.sock");
    if UnixStream::connect(&path).is_ok() { return; }
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = UnixListener::bind(&path) else { return };
    if std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).is_err() { return; }
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let app = app.clone();
            std::thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                let mut query = String::new();
                if BufReader::new((&stream).take(64_001)).read_line(&mut query).is_err() || query.len() > 64_000 || !query.ends_with('\n') { return; }
                let Ok(value) = serde_json::from_str::<Value>(&query) else { return; };
                let result = match value["op"].as_str() {
                    Some("context") if value.as_object().is_some_and(|m| m.len() == 1) => snapshot(&app).unwrap_or(Value::Null),
                    Some("handoff") => handoff::handle(&app, &stream, value),
                    _ => return,
                };
                if let Ok(mut encoded) = serde_json::to_vec(&result) { encoded.push(b'\n'); let _ = stream.write_all(&encoded); }
            });
        }
    });
}

fn snapshot(app: &tauri::AppHandle) -> Option<Value> {
    let published = app.try_state::<SelectionState>()?;
    let (selected, window_id) = published.0.lock().ok()?.clone();
    let selected = selected?;
    let state = app.try_state::<crate::AppState>()?;
    let conn = state.db.safe_lock();
    let mut records = serde_json::Map::new();
    let mut label = selected.view.clone();
    let mut revision = format!("{}:{:?}:{:?}:{}", selected.view, selected.project, selected.task_id, selected.composing);
    if let Some(id) = selected.task_id.as_deref() {
        let task = crate::db::tasks::get(&conn, id).ok()??;
        records.insert("task_id".into(), json!(task.id));
        if let Some(short) = task.short_id { records.insert("task_ref".into(), json!(format!("T{short}"))); }
        if let Some(project) = &task.project { records.insert("project".into(), json!(project)); }
        label = task.title;
        revision = format!("{}:{}:{}", task.id, task.updated_at, selected.view);
    }

    Some(json!({"pid":std::process::id(), "bundleID":"com.cxtasks.app", "windowLabel":"main", "windowID":window_id, "revision":revision, "label":label.chars().take(1000).collect::<String>(), "records":records}))
}
