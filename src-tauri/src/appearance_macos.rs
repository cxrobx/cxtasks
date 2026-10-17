//! Pin the app's native appearance to match the UI's theme.
//!
//! # Why this is needed
//!
//! **Native** chrome — `NSMenu` popups, `<select>` popups, sheets, scrollbars,
//! the title bar — is drawn by AppKit using `NSApp.effectiveAppearance`, which
//! follows the *system* setting. Nothing in the web layer can influence it. So
//! on a Mac in Light mode, a dark web UI produces a light-grey menu erupting
//! out of a dark window.
//!
//! This is the real root cause of two separate symptoms that looked unrelated:
//! the `<select>` dropdown rendering light grey (worked around by replacing it
//! with a Radix menu), and the right-click `NSMenu` rendering light. Setting
//! `NSApp.appearance` explicitly fixes the whole class at once — every piece of
//! native chrome the app draws now matches the UI.
//!
//! The UI theme is user-selectable, so this is a **function of that choice**
//! rather than a constant: whichever way the web layer goes, native chrome must
//! follow it, not the system. Getting this backwards is worse than not having
//! it — a light UI with a dark right-click menu is the same bug inverted.
//!
//! **Main thread only.** AppKit setters like `-[NSApplication setAppearance:]`
//! are main-thread APIs (cxmail gotcha #20). Tauri's `.setup()` runs on the
//! main thread inside `did_finish_launching`, so calling it there is fine.
//! Calling it from a `#[tauri::command]` is NOT — those run on Tokio workers,
//! so the command must hop across with `AppHandle::run_on_main_thread`.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSString;

/// Pin the app to the dark or light system appearance.
///
/// Safe to call before any window exists; AppKit applies the appearance to
/// windows created later. Must be called on the main thread.
pub fn set_appearance(dark: bool) {
    // The two documented `NSAppearanceName` constants. Passing an unknown name
    // to `appearanceNamed:` returns nil rather than raising, which the null
    // check below turns into a warning instead of a process abort.
    let name = if dark {
        "NSAppearanceNameDarkAqua"
    } else {
        "NSAppearanceNameAqua"
    };

    // Guard the same way `notify_macos` does: an unbundled process still has a
    // valid NSApplication under Tauri, so this is safe either way, but keeping
    // the ObjC surface small is worth it — an uncaught ObjC exception aborts
    // the process rather than returning an error.
    unsafe {
        let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if ns_app.is_null() {
            log::warn!("appearance: NSApplication unavailable; leaving system appearance");
            return;
        }
        let ns_name = NSString::from_str(name);
        let appearance: *mut AnyObject = msg_send![class!(NSAppearance), appearanceNamed: &*ns_name];
        if appearance.is_null() {
            log::warn!("appearance: {name} not found");
            return;
        }
        let _: () = msg_send![ns_app, setAppearance: appearance];

        // Read it back so the log says what actually took effect rather than
        // what we asked for.
        let effective: *mut AnyObject = msg_send![ns_app, effectiveAppearance];
        if !effective.is_null() {
            let n: Retained<NSString> = msg_send![effective, name];
            log::info!("appearance: native chrome pinned to {n}");
        }
    }
}
