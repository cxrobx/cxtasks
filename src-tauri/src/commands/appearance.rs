//! Keep native chrome in step with the UI's theme toggle.
//!
//! See `appearance_macos` for why this has to exist at all. The one thing this
//! layer adds is the **main-thread hop**: `#[tauri::command]` bodies run on
//! Tokio workers, and `-[NSApplication setAppearance:]` is a main-thread-only
//! AppKit setter, so calling it directly from here would be a threading
//! violation (cxmail gotcha #20).

use tauri::AppHandle;

use crate::error::AppError;

/// Set native chrome to dark or light. Called by the frontend whenever the
/// theme changes, and once on hydrate so a persisted light theme is applied
/// even though the window booted dark.
#[tauri::command]
pub async fn set_native_appearance(app: AppHandle, dark: bool) -> Result<(), AppError> {
    #[cfg(target_os = "macos")]
    {
        // `run_on_main_thread` returns as soon as the closure is *queued*, not
        // when it completes. That's fine here: this is a fire-and-forget visual
        // change with no return value, and the frontend has already repainted.
        // Mapped rather than `?`-d. The `?` relied on an
        // `AppError: From<tauri::Error>` impl, and that impl was the only
        // reference to tauri in `error.rs` — which has to build for Linux.
        // Nothing is lost: the message is more accurate than the "Menu error"
        // the old variant printed.
        app.run_on_main_thread(move || {
            crate::appearance_macos::set_appearance(dark);
        })
        .map_err(|e| {
            AppError::General(format!("could not reach the main thread to set appearance: {e}"))
        })?;
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, dark);
    }

    Ok(())
}
