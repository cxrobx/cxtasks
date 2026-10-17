//! Posting a banner from a process that is NOT the app.
//!
//! Gotcha #34: only the bundle's main executable may talk to
//! `UNUserNotificationCenter`. A helper — `cxtasks-runner`, `cxtasks-mcp` — that
//! tries is terminated outright. So helpers have always delegated by spawning
//! `cxtasks --notify` instead.
//!
//! That delegation is the reason this module can exist here at all: it is
//! `std::process::Command` and nothing else. No AppKit, no Objective-C, no
//! framework of any kind. Which means the helper binaries can post banners
//! while still living in the platform-neutral crate — and that is what lets
//! them build for the container.
//!
//! The app itself does NOT use this. It has an `NSApplication` and posts
//! directly; see `cxtasks_lib::notify_macos`.

use crate::notify::Notifier;

/// Ask the installed CXTasks.app to post on our behalf.
///
/// Returns whether it did. Moved here from `notify_macos` unchanged, including
/// the silenced streams and the reasoning for them.
pub fn delegate_to_installed_bundle(task_id: &str, title: &str, body: &str) -> bool {
    let Some(helper) = crate::paths::installed_binary("cxtasks") else {
        log::debug!("notify: no installed CXTasks.app to post through");
        return false;
    };
    match std::process::Command::new(&helper)
        // Silenced because the interesting failure is LOUD: a bundle macOS
        // cannot resolve makes `+currentNotificationCenter` raise, and an
        // uncaught ObjC exception aborts the process with a full backtrace on
        // stderr (gotcha #4). Running that call in a throwaway child is what
        // makes it survivable at all — but the caller doesn't need the dump in
        // its console every 30 seconds, only the exit status, which says the
        // same thing.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .arg("--notify")
        .arg("--task-id")
        .arg(task_id)
        .arg("--title")
        .arg(title)
        .arg("--body")
        .arg(body)
        .status()
    {
        Ok(status) if status.success() => true,
        Ok(status) => {
            log::warn!("notify: {} --notify exited {status}", helper.display());
            false
        }
        Err(e) => {
            log::warn!("notify: could not run {} --notify: {e}", helper.display());
            false
        }
    }
}

/// Last resort when there is no installed bundle to delegate to.
#[cfg(target_os = "macos")]
pub fn via_osascript(title: &str, body: &str) {
    // AppleScript string literals escape only `\` and `"`.
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        esc(&body.chars().take(240).collect::<String>()),
        esc(&title.chars().take(120).collect::<String>()),
    );
    match std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => log::warn!(
            "osascript notification failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => log::warn!("could not run osascript for notification: {e}"),
    }
}

/// The helper route, whole: delegate, then fall back.
pub fn post(task_id: &str, title: &str, body: &str) {
    if delegate_to_installed_bundle(task_id, title, body) {
        return;
    }
    #[cfg(target_os = "macos")]
    via_osascript(title, body);
    #[cfg(not(target_os = "macos"))]
    log::debug!("no installed bundle to notify through; dropping banner for {task_id}");
}

/// Install this in a helper binary — `cxtasks-runner`, `cxtasks-mcp`.
///
/// On Linux it degrades to silence on its own: there is no installed
/// CXTasks.app to delegate to and no osascript to fall back on, which is the
/// honest answer, since nobody is standing at the container.
pub struct HelperNotifier;

impl Notifier for HelperNotifier {
    fn post(&self, task_id: &str, title: &str, body: &str) {
        post(task_id, title, body);
    }
}
