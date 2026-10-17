//! Let the frontend drive the window's desktop blur.
//!
//! Two calls rather than one, because they answer different questions and run
//! at wildly different rates. `set_window_glass` is the *state* change — glass
//! on or off, once on hydrate and again on every theme toggle. `set_blur_radius`
//! is the *amount*, driven by the transparency slider, and is called whenever
//! the rounded radius changes during a drag.
//!
//! Both hop to the main thread for the reason `commands::appearance` documents:
//! `#[tauri::command]` bodies run on Tokio workers and every AppKit setter
//! underneath is main-thread-only (cxmail gotcha #20).

use tauri::AppHandle;

use crate::error::AppError;

/// Turn the glass on (with a starting radius) or off (repainting the window
/// opaque in the theme's base colour).
///
/// The RGB triple is required even when enabling, so that disabling has
/// somewhere to go without a second round trip — and so the colour is always
/// the frontend's, never a constant in here that drifts from `globals.css`.
#[tauri::command]
pub async fn set_window_glass(
    app: AppHandle,
    enabled: bool,
    radius: u8,
    r: u8,
    g: u8,
    b: u8,
) -> Result<(), AppError> {
    #[cfg(target_os = "macos")]
    {
        // Cloned for the closure: `run_on_main_thread` is called ON `app`, so
        // moving the same handle inside would borrow it and move it at once.
        let handle = app.clone();
        app.run_on_main_thread(move || {
            crate::glass_macos::with_main(&handle, |window| {
                crate::glass_macos::set_state(window, enabled, radius, r, g, b);
            });
        })
        .map_err(|e| {
            AppError::General(format!("could not reach the main thread to set glass: {e}"))
        })?;
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, enabled, radius, r, g, b);
    }

    Ok(())
}

/// Change the blur radius without touching the window's glass state.
///
/// A no-op when glass is off: the radius is applied to a window whose opaque
/// background hides it, and the next `set_window_glass(true, …)` passes its own
/// radius anyway. Guarding that here would need this layer to track state the
/// frontend already owns.
#[tauri::command]
pub async fn set_blur_radius(app: AppHandle, radius: u8) -> Result<(), AppError> {
    #[cfg(target_os = "macos")]
    {
        let handle = app.clone();
        app.run_on_main_thread(move || {
            crate::glass_macos::with_main(&handle, |window| {
                crate::glass_macos::set_radius(window, radius);
            });
        })
        .map_err(|e| {
            AppError::General(format!("could not reach the main thread to set blur: {e}"))
        })?;
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, radius);
    }

    Ok(())
}

/// Is Reduce Transparency on right now?
///
/// The frontend needs it at hydrate so the CSS tint matches the window on the
/// first paint — the native side goes opaque on its own, but the panes would
/// otherwise stay translucent over it and the sidebar would come out a shade
/// off. Changes after that arrive as a `reduce-transparency-changed` event.
#[tauri::command]
pub async fn get_reduce_transparency() -> Result<bool, AppError> {
    #[cfg(target_os = "macos")]
    {
        return Ok(crate::glass_macos::reduce_transparency());
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(false)
    }
}
