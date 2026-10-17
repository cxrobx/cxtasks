//! The FALLBACK blur, for machines where the real one is missing.
//!
//! # Not the primary path any more
//!
//! `glass_macos` is. `NSVisualEffectView` is a *material*, not a blur — Apple's
//! radius, Apple's tint and Apple's saturation boost welded into one enum case
//! with no dial on any of it — and in dark mode `UnderWindowBackground` reads
//! as flat milky grey that destroys the colour and shape of the wallpaper
//! behind it. That is a look, and it is not the one this app wants.
//!
//! This module stays because `glass_macos` resolves a *private* symbol that a
//! future macOS may not export. When that lookup comes back empty, a material
//! is much better than nothing: at `--alpha-pane: 0.25` a window with no blur
//! at all is illegible, not merely plainer. So this is the safety net, called
//! from `glass_macos::enable` and from nowhere else.
//!
//! # Why this can't be done in CSS
//!
//! The obvious approach — `backdrop-filter: blur()` on the app's root element —
//! does nothing useful here, and it fails in a way that looks like a tuning
//! problem rather than a category error. `backdrop-filter` blurs the element's
//! *backdrop root*, which for anything inside the page is the page itself. It
//! has no access to what is behind the **window**. So on a transparent window
//! it blurs a transparent nothing and you get a crisp, unblurred desktop
//! showing through, with the filter correctly applied to zero pixels.
//!
//! The desktop behind a window can only be blurred by AppKit, by putting an
//! `NSVisualEffectView` behind the webview. That is what this module does, via
//! the `window-vibrancy` crate.
//!
//! # The two halves have to agree
//!
//! This is only half of the effect. It supplies the blur; the *tint* on top of
//! it is the frontend's `--alpha-*` tokens (`globals.css`). Either half alone
//! looks broken — blur with no tint is an unreadable smear of wallpaper, tint
//! with no blur is a flat translucent panel with the desktop legible through
//! it. `tauri.conf.json` must also carry `"transparent": true`, or the webview
//! paints an opaque background over this view and the blur is invisible while
//! being perfectly correctly configured.
//!
//! # Main thread
//!
//! `NSVisualEffectView` construction and insertion are AppKit calls, so this is
//! main-thread-only for the same reason `appearance_macos` is (cxmail gotcha
//! #20). `.setup()` runs on the main thread inside `did_finish_launching`,
//! which is why this is called from there and not from a command.

#[cfg(target_os = "macos")]
use tauri::WebviewWindow;

/// Install the blur behind the main window's webview.
///
/// Deliberately called **once**, at setup, and never re-applied on theme
/// change. `UnderWindowBackground` derives its own light/dark rendering from
/// the window's `NSAppearance`, which `appearance_macos` already pins to follow
/// the UI theme — so the theme toggle moves this for free. Re-applying it per
/// theme change would stack a second effect view behind the first.
///
/// Nor is it cleared for the Solid level: at `--alpha-pane: 1` the panes cover
/// it completely, so leaving it installed costs nothing and makes switching
/// levels a pure CSS change with no native round-trip.
///
/// Failure is logged, never fatal. A window with no blur is a cosmetic
/// degradation; the app is entirely usable, and on a Mac too old for the
/// material this is the correct outcome rather than a failed launch.
#[cfg(target_os = "macos")]
pub fn apply(window: &WebviewWindow) {
    use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};

    // `UnderWindowBackground` over `HudWindow`: it is the material AppKit uses
    // for full-window backgrounds, so it picks up the desktop tint the way a
    // native translucent window does. HudWindow is tuned for small floating
    // panels and reads noticeably flatter and darker across a 1400pt window.
    //
    // `Active` rather than `FollowsWindowActiveState`: a task board spends most
    // of its life as the window you glance at while working in another one, and
    // the "follows" state drops the blur to flat grey the moment focus leaves.
    // That would make the effect visible almost exclusively when it isn't
    // being looked at.
    //
    // Radius `None` — the NSWindow already clips its content view to the system
    // corner radius. Passing a value here rounds the *effect view* separately,
    // and any mismatch with the system radius shows as a hairline of desktop
    // caught between the two curves at each corner.
    match apply_vibrancy(
        window,
        NSVisualEffectMaterial::UnderWindowBackground,
        Some(NSVisualEffectState::Active),
        None,
    ) {
        Ok(()) => log::info!("vibrancy: UnderWindowBackground applied"),
        Err(e) => log::warn!("vibrancy: not applied ({e}); window stays opaque"),
    }
}

