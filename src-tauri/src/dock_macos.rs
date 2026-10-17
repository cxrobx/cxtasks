//! A live, native Dock menu. AppKit asks the existing Tao application delegate
//! for this menu on every right click, so changes made by the runner or MCP
//! appear without polling or rebuilding a stale menu in the webview.

use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::NSString;
use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::OnceLock;
use tauri::{Emitter, Manager};

use crate::db::tasks::{self, ListFilters, Task, View};
use crate::{AppState, LockExt};

extern "C" {
    fn object_getClass(object: *const AnyObject) -> *mut AnyClass;
    fn class_addMethod(
        cls: *mut AnyClass,
        selector: Sel,
        implementation: *const std::ffi::c_void,
        types: *const std::ffi::c_char,
    ) -> Bool;
}

static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DockMenuSettings {
    pub show_today: bool,
    pub show_inbox: bool,
    pub show_quick_add: bool,
    pub max_tasks: u8,
}

impl Default for DockMenuSettings {
    fn default() -> Self {
        Self {
            show_today: true,
            show_inbox: false,
            show_quick_add: true,
            max_tasks: 6,
        }
    }
}

fn settings_path() -> std::path::PathBuf {
    crate::paths::app_data_dir().join("dock-menu.json")
}

pub fn settings() -> DockMenuSettings {
    fs::read(settings_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<DockMenuSettings>(&bytes).ok())
        .map(|mut value| {
            value.max_tasks = value.max_tasks.clamp(1, 10);
            value
        })
        .unwrap_or_default()
}

pub fn save_settings(mut value: DockMenuSettings) -> Result<DockMenuSettings, std::io::Error> {
    value.max_tasks = value.max_tasks.clamp(1, 10);
    let path = settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(&value).map_err(std::io::Error::other)?;
    fs::write(&temporary, data)?;
    fs::rename(temporary, path)?;
    Ok(value)
}

/// Add only new selectors to Tao's current delegate. Replacing that delegate
/// would break reopen, URL handling, and termination callbacks owned by Tauri.
pub fn install(app: &tauri::AppHandle) {
    let _ = APP.set(app.clone());
    unsafe {
        let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if ns_app.is_null() {
            return;
        }
        let delegate: *mut AnyObject = msg_send![ns_app, delegate];
        if delegate.is_null() {
            return;
        }
        let cls = object_getClass(delegate);
        if cls.is_null() {
            return;
        }
        let menu_added = class_addMethod(
            cls,
            sel!(applicationDockMenu:),
            dock_menu as *const std::ffi::c_void,
            c"@@:@".as_ptr(),
        );
        let task_added = class_addMethod(
            cls,
            sel!(cxTasksOpenDockTask:),
            open_task as *const std::ffi::c_void,
            c"v@:@".as_ptr(),
        );
        let action_added = class_addMethod(
            cls,
            sel!(cxTasksDockAction:),
            action as *const std::ffi::c_void,
            c"v@:@".as_ptr(),
        );
        if !menu_added.as_bool() || !task_added.as_bool() || !action_added.as_bool() {
            log::warn!("dock menu: could not install all delegate selectors");
        }
    }
}

/// The return value must be autoreleased: AppKit owns it only for this popup.
extern "C" fn dock_menu(delegate: &AnyObject, _: Sel, _: &AnyObject) -> *mut AnyObject {
    unsafe {
        let menu: *mut AnyObject = msg_send![class!(NSMenu), new];
        if menu.is_null() {
            return std::ptr::null_mut();
        }
        let prefs = settings();
        if prefs.show_quick_add {
            add_item(
                menu,
                delegate,
                "New Task…",
                sel!(cxTasksDockAction:),
                Some("quick-add"),
                true,
            );
            separator(menu);
        }
        if prefs.show_today {
            add_tasks(menu, delegate, View::Today, "Today", prefs.max_tasks);
        }
        if prefs.show_inbox {
            add_tasks(menu, delegate, View::Inbox, "Inbox", prefs.max_tasks);
        }
        add_item(
            menu,
            delegate,
            "Open CXTasks",
            sel!(cxTasksDockAction:),
            Some("open"),
            true,
        );
        add_item(
            menu,
            delegate,
            "Dock Menu Settings…",
            sel!(cxTasksDockAction:),
            Some("settings"),
            true,
        );
        let result: *mut AnyObject = msg_send![menu, autorelease];
        result
    }
}

unsafe fn add_tasks(
    menu: *mut AnyObject,
    delegate: &AnyObject,
    view: View,
    heading: &str,
    limit: u8,
) {
    let tasks = APP.get().and_then(|app| {
        let state = app.try_state::<AppState>()?;
        let conn = state.db.safe_lock();
        match tasks::list(
            &conn,
            &ListFilters {
                view: Some(view),
                limit: Some(u32::from(limit) + 1),
                ..Default::default()
            },
        ) {
            Ok(tasks) => Some(tasks),
            Err(error) => {
                log::warn!("dock menu: task query failed: {error}");
                None
            }
        }
    });
    let Some(tasks) = tasks else {
        return;
    };
    let more = tasks.len() > usize::from(limit);
    let label = format!(
        "{}  ·  {}{}",
        heading,
        tasks.len().min(usize::from(limit)),
        if more { "+" } else { "" }
    );
    add_item(
        menu,
        delegate,
        &label,
        sel!(cxTasksDockAction:),
        None,
        false,
    );
    if tasks.is_empty() {
        add_item(
            menu,
            delegate,
            "  All clear",
            sel!(cxTasksDockAction:),
            None,
            false,
        );
    } else {
        for task in tasks.iter().take(usize::from(limit)) {
            let compact = task.title.split_whitespace().collect::<Vec<_>>().join(" ");
            let title: String = compact.chars().take(62).collect();
            let suffix = if compact.chars().count() > 62 {
                "…"
            } else {
                ""
            };
            let reference = task
                .short_id
                .map(|number| format!("T{number}  "))
                .unwrap_or_default();
            let label = format!("{reference}{}  {}{}", task_symbol(task), title, suffix);
            add_item(
                menu,
                delegate,
                &label,
                sel!(cxTasksOpenDockTask:),
                Some(&task.id),
                true,
            );
        }
    }
    let route = if view == View::Today {
        "today"
    } else {
        "inbox"
    };
    add_item(
        menu,
        delegate,
        &format!("Show all {heading}…"),
        sel!(cxTasksDockAction:),
        Some(route),
        true,
    );
    separator(menu);
}

fn task_symbol(task: &Task) -> &'static str {
    match task.status.as_str() {
        "in_progress" => "●",
        "in_review" => "◆",
        "queued" => "◷",
        _ => "○",
    }
}

unsafe fn add_item(
    menu: *mut AnyObject,
    target: &AnyObject,
    title: &str,
    action: Sel,
    value: Option<&str>,
    enabled: bool,
) {
    let title = NSString::from_str(title);
    let empty = NSString::from_str("");
    let item: *mut AnyObject = msg_send![class!(NSMenuItem), alloc];
    let item: *mut AnyObject =
        msg_send![item, initWithTitle: &*title, action: action, keyEquivalent: &*empty];
    let _: () = msg_send![item, setTarget: target];
    let _: () = msg_send![item, setEnabled: enabled];
    if let Some(value) = value {
        let value = NSString::from_str(value);
        let _: () = msg_send![item, setRepresentedObject: &*value];
    }
    let _: () = msg_send![menu, addItem: item];
    let _: () = msg_send![item, release];
}

unsafe fn separator(menu: *mut AnyObject) {
    let item: *mut AnyObject = msg_send![class!(NSMenuItem), separatorItem];
    let _: () = msg_send![menu, addItem: item];
}

extern "C" fn open_task(_: &AnyObject, _: Sel, sender: &AnyObject) {
    unsafe {
        let value: *mut NSString = msg_send![sender, representedObject];
        if !value.is_null() {
            crate::notify_macos::open_task(&(*value).to_string());
        }
    }
}

extern "C" fn action(_: &AnyObject, _: Sel, sender: &AnyObject) {
    let Some(app) = APP.get() else {
        return;
    };
    let value = unsafe {
        let value: *mut NSString = msg_send![sender, representedObject];
        if value.is_null() {
            return;
        }
        (*value).to_string()
    };
    let _ = app.show();
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
    let _ = app.emit("dock-action", value);
}
