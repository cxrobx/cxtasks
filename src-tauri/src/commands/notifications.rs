//! The frontend's half of notification click routing.
//!
//! Clicking a banner is handled natively (`notify_macos::open_task`): the app
//! comes to the front and parks the task id. This is how the webview collects
//! it. See `notify_macos::PENDING_TASK` for why the id travels in a latch
//! rather than in the event payload — the short version is that a click on a
//! CLOSED app emits before React has mounted a listener, so the event alone
//! loses exactly the click the user cared most about.

use crate::error::AppError;

/// Collect and clear the task a notification click asked to open.
///
/// Called twice on two different triggers, both draining the same slot: once
/// when the app mounts (which is what catches a cold launch), and again from
/// the `open-task-from-notification` event (which is what makes a click on an
/// already-running app land immediately). Returns `None` when there is nothing
/// pending, which is the normal case for both.
#[tauri::command]
pub async fn take_pending_notification_task() -> Result<Option<String>, AppError> {
    #[cfg(target_os = "macos")]
    {
        Ok(crate::notify_macos::take_pending_task())
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(None)
    }
}
