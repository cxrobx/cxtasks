//! Native macOS notifications via UNUserNotificationCenter.
//!
//! Ported from cxmail's `notify_macos.rs`, trimmed to what CXTasks needs: post
//! a notification carrying a `task_id`, and when it's clicked, raise the app
//! and hand the frontend that task to reveal — see [`open_task`], which is
//! where the ordering and the latch both matter.
//!
//! Used for due reminders (Phase 5) and finished background runs (Phase 4).
//! The runner is a separate process with no Tauri app handle, so `init` is
//! optional — posting works without a delegate, you just don't get click
//! routing.

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::NSString;
use std::sync::{Mutex, OnceLock};

// Raw ObjC runtime FFI for class creation (bypasses MethodImplementation bounds).
extern "C" {
    fn objc_allocateClassPair(
        superclass: *const AnyClass,
        name: *const std::ffi::c_char,
        extra_bytes: usize,
    ) -> *mut AnyClass;
    fn objc_registerClassPair(cls: *mut AnyClass);
    fn class_addMethod(
        cls: *mut AnyClass,
        sel: Sel,
        imp: *const std::ffi::c_void,
        types: *const std::ffi::c_char,
    ) -> Bool;
    fn class_addProtocol(cls: *mut AnyClass, protocol: *const std::ffi::c_void) -> Bool;
    fn objc_getProtocol(name: *const std::ffi::c_char) -> *const std::ffi::c_void;
}

static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Category carrying the "Open in Claude" button on a task notification.
///
/// A notification shows action buttons only when its `categoryIdentifier`
/// names a category that has been REGISTERED with the notification centre; an
/// unregistered one renders as a plain banner with nothing logged anywhere. So
/// the identifier is set in two places that must agree — here, when
/// registering, and on every posted notification.
const TASK_CATEGORY: &str = "cxtasks.task";
/// The one action on that category. Deliberately one: macOS promotes a lone
/// action to a visible button and hides every action after the first behind an
/// "Options" menu, which on a banner that self-dismisses is the difference
/// between usable and theoretical (cxmail learned this the same way).
const ACTION_OPEN_IN_CLAUDE: &str = "cxtasks.open-in-claude";

/// The task a notification click asked for, held until the frontend collects it.
///
/// Clicking a banner while the app is CLOSED launches it, and macOS delivers the
/// response to the delegate as soon as `init` registers one — which happens in
/// `.setup()`, long before React has mounted and called `listen()`. The emit
/// therefore lands with nobody listening and the click is silently lost, which
/// is the one case where the user most expects it to work: they clicked because
/// the app wasn't in front of them.
///
/// So the id is also parked here, and the frontend drains it on mount via
/// `take_pending_notification_task`. Both routes are kept rather than replacing
/// the event with polling: the event is what makes a click land instantly on an
/// app that is already running, and this is what makes a cold launch land at
/// all. `take` empties the slot, so whichever gets there first, the other finds
/// nothing and the task is never revealed twice.
static PENDING_TASK: Mutex<Option<String>> = Mutex::new(None);

/// Collect and clear the task a notification click asked to open.
///
/// Returns `None` on the overwhelmingly common path where the app was launched
/// normally. A poisoned lock is treated as empty — failing to reveal a task is
/// not worth a panic in a startup path.
pub fn take_pending_task() -> Option<String> {
    PENDING_TASK.lock().ok().and_then(|mut slot| slot.take())
}

/// Park the clicked task for whichever route collects it first.
///
/// Last click wins: only one task can be on screen, so a second click arriving
/// before the frontend has drained the first is a change of mind, not a queue.
fn park_pending_task(task_id: &str) {
    if let Ok(mut slot) = PENDING_TASK.lock() {
        *slot = Some(task_id.to_string());
    }
}

/// Raise the app to the front and hand the frontend the task to reveal.
///
/// **The order is the whole function.** `set_focus` is a documented no-op while
/// the window is minimized or hidden — tao guards it with
/// `if !is_minimized && is_visible` — so calling it first, which is what this
/// used to do, meant a click on a banner did precisely nothing whenever CXTasks
/// was minimized. For a task app that lives in the background, that is most of
/// the time. Undo both kinds of hidden FIRST (`unminimize` for the yellow
/// button, `AppHandle::show` for an app-wide ⌘H, `Window::show` for a closed
/// window), and only then ask for focus.
///
/// The id goes into [`PENDING_TASK`] rather than into the event payload, and
/// the event is a bare ping. One source of truth means every route drains the
/// same slot: an app already running collects it from the event handler, a cold
/// launch collects it on mount, and neither can leave a stale id parked to be
/// revealed spuriously at some unrelated future launch.
pub(crate) fn open_task(task_id: &str) {
    park_pending_task(task_id);

    let Some(app) = APP_HANDLE.get() else {
        // No handle means `init` never ran, which means this delegate was never
        // installed and we cannot be here. Logged rather than ignored because
        // if it ever does happen, the click vanishing is the only symptom.
        log::warn!("notification click for {task_id} had no app handle to focus");
        return;
    };

    use tauri::{Emitter, Manager};
    // App-wide unhide first: while the app is hidden, its windows are not
    // considered visible and every per-window call below is a no-op.
    let _ = app.show();
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
    let _ = app.emit("open-task-from-notification", ());
}

/// Hand the task straight to a Claude session in Ghostty, from the banner.
///
/// The same `dispatch_foreground` the "Open in Claude" button in the app runs,
/// so the seed prompt, the run row and the move to `in_progress` are identical
/// whichever surface started it — a second implementation here would drift
/// within a month.
///
/// The window is deliberately NOT raised. The user asked for a terminal, and
/// the session opens in Ghostty; pulling CXTasks to the front would put the
/// wrong app over the one they are about to work in.
fn open_in_claude(task_id: &str) {
    let Some(app) = APP_HANDLE.get() else {
        // Only the app process registers this delegate, so there is no route
        // that reaches here without a handle. Logged rather than ignored: if it
        // ever does happen, the click vanishing is the only symptom.
        log::warn!("Open in Claude for {task_id} had no app handle to dispatch with");
        return;
    };
    let app = app.clone();
    let task_id = task_id.to_string();
    // This callback runs on the MAIN thread, and dispatch takes the database
    // mutex, writes a scratch dir and spawns Ghostty over AppleScript. Doing
    // that inline stalls the UI for as long as all of it takes.
    tauri::async_runtime::spawn(async move {
        use tauri::Manager;
        let state = app.state::<crate::AppState>();
        match crate::commands::dispatch::dispatch_foreground(state, task_id.clone()).await {
            Ok(result) => log::info!(
                "Open in Claude from notification landed in {} (run {})",
                result.repo_path,
                result.run_id
            ),
            Err(e) => {
                // A banner click leaves no window to paint an error into — the
                // banner is already gone — so this would otherwise fail in
                // total silence, which reads as "the button does nothing".
                // Reported as another banner on the same task: clicking it
                // reveals the task, and its own button is the retry.
                log::warn!("Open in Claude from notification failed: {e}");
                show_notification(&task_id, "Open in Claude failed", &e.to_string());
            }
        }
    });
}

/// True when this process is running from inside a `.app` bundle.
///
/// `UNUserNotificationCenter` requires a bundle identifier. From a bare CLI
/// binary, `+currentNotificationCenter` raises an **NSException** — and an
/// uncaught ObjC exception calls `std::terminate`, so it doesn't just fail the
/// notification, it aborts the process. `catch_unwind` does not help: it's not
/// a Rust panic. Found the hard way — `cxtasks-runner` died mid-batch and left
/// an in-flight run stuck at `running`, occupying a concurrency slot.
fn inside_app_bundle() -> bool {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
        .unwrap_or(false)
}


/// Register the delegate and request authorization. Call once from the Tauri
/// app's setup. The runner does NOT call this — it only posts.
///
/// **No-ops outside an app bundle.** `npx tauri dev` runs the bare binary at
/// `target/debug/cxtasks`, not a `.app`, so `+currentNotificationCenter`
/// raises `NSInternalInconsistencyException: bundleProxyForCurrentProcess is
/// nil` — which, being an uncaught ObjC exception, terminates the whole app at
/// startup rather than failing the notification. Dev builds fall through to
/// the osascript path in `show_notification`, so reminders still appear;
/// only click-to-open-task needs the bundle.
pub fn init(app: &tauri::AppHandle) {
    let _ = APP_HANDLE.set(app.clone());

    if !inside_app_bundle() {
        log::info!(
            "notify_macos: not running from an .app bundle — skipping \
             UNUserNotificationCenter setup; notifications will use osascript"
        );
        return;
    }

    unsafe {
        let cls = register_delegate_class();
        let delegate: Retained<AnyObject> = msg_send![cls, new];

        let center: Retained<AnyObject> =
            msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
        let _: () = msg_send![&*center, setDelegate: &*delegate];

        // Leak the delegate so it lives for the app's lifetime.
        std::mem::forget(delegate);

        register_notification_categories(&center);

        // UNAuthorizationOptions: badge 1<<0, sound 1<<1, alert 1<<2.
        let options: usize = 0x04 | 0x02 | 0x01;
        // Log the answer. Without this, "notifications don't work" is
        // indistinguishable from "the user never granted permission" — the
        // same diagnostic lesson as cxmail gotcha #27.
        let block = block2::RcBlock::new(move |granted: Bool, _error: *const AnyObject| {
            // Publish the answer for `ensure_authorized`, so the posting path
            // in this process never has to ask a second time.
            let _ = AUTHORIZED.set(granted.as_bool());
            if granted.as_bool() {
                log::info!("notify_macos: notification authorization GRANTED");
            } else {
                log::warn!(
                    "notify_macos: notification authorization DENIED — banners will not \
                     appear. Re-enable in System Settings › Notifications › CXTasks."
                );
            }
        });
        let _: () = msg_send![
            &*center,
            requestAuthorizationWithOptions: options,
            completionHandler: &*block
        ];
    }
}

/// Register the one category CXTasks uses: a task notification carrying an
/// "Open in Claude" button.
///
/// **Every process that POSTS registers it**, not just the app. A banner from a
/// background run is posted by a second process borrowing the bundle
/// (`cxtasks --notify`, see [`show_notification`]), and a category the posting
/// process has not registered gives a button-less banner — which is precisely
/// the notification where the button is worth the most, because the app was
/// closed. Registering the same single category from either process is safe:
/// `setNotificationCategories:` REPLACES the set rather than adding to it, so
/// two registrars are only a problem when they disagree about its contents.
///
/// No `.foreground` option on the action: the point of the handoff is landing
/// in Ghostty, not raising CXTasks.
unsafe fn register_notification_categories(center: &AnyObject) {
    let action_id = NSString::from_str(ACTION_OPEN_IN_CLAUDE);
    let action_title = NSString::from_str("Open in Claude");
    let action: Retained<AnyObject> = msg_send![
        class!(UNNotificationAction),
        actionWithIdentifier: &*action_id,
        title: &*action_title,
        options: 0usize
    ];

    let actions: Retained<AnyObject> = msg_send![class!(NSMutableArray), new];
    let _: () = msg_send![&*actions, addObject: &*action];
    let intents: Retained<AnyObject> = msg_send![class!(NSMutableArray), new];

    let category_id = NSString::from_str(TASK_CATEGORY);
    let category: Retained<AnyObject> = msg_send![
        class!(UNNotificationCategory),
        categoryWithIdentifier: &*category_id,
        actions: &*actions,
        intentIdentifiers: &*intents,
        options: 0usize
    ];

    let category_array: Retained<AnyObject> = msg_send![class!(NSMutableArray), new];
    let _: () = msg_send![&*category_array, addObject: &*category];
    let categories: Retained<AnyObject> = msg_send![class!(NSSet), setWithArray: &*category_array];
    let _: () = msg_send![center, setNotificationCategories: &*categories];
}

/// Register the categories and WAIT until the notification daemon has them.
///
/// For the app this would be pointless — `init` registers at startup, seconds
/// before anything is posted. For the `--notify` helper it is the whole point:
/// that process registers and posts in the same breath and then exits, and
/// `setNotificationCategories:` is one-way with no acknowledgement. Reading the
/// set back is a round trip through the same daemon, so a reply means the
/// registration has landed and the post that follows will find its category.
///
/// Bounded and best-effort: a daemon that never answers costs one second and
/// yields a banner without a button, which is what would have happened anyway.
fn register_categories_and_flush() {
    unsafe {
        let center: Retained<AnyObject> =
            msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
        register_notification_categories(&center);

        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let block = block2::RcBlock::new(move |_categories: *const AnyObject| {
            let _ = tx.send(());
        });
        let _: () = msg_send![
            &*center,
            getNotificationCategoriesWithCompletionHandler: &*block
        ];
        if rx.recv_timeout(std::time::Duration::from_secs(1)).is_err() {
            log::warn!(
                "notify_macos: the notification daemon did not confirm the category \
                 registration; the banner may arrive without its button"
            );
        }
    }
}

/// Whether this PROCESS has been cleared to post. See [`ensure_authorized`].
static AUTHORIZED: OnceLock<bool> = OnceLock::new();

/// Ask macOS for permission and WAIT for the answer, once per process.
///
/// **Authorization is per-process, not per-bundle**, and that is not obvious:
/// the user granted CXTasks permission once, in the app, months ago — but a
/// process that has never called `requestAuthorizationWithOptions` still gets
/// `UNErrorDomain error 1` (*notifications not allowed*) back from
/// `addNotificationRequest`. The request is what makes this process fetch the
/// settings; without it there is nothing to check against and the daemon
/// refuses.
///
/// This is why a background run that finished while the app was CLOSED never
/// put a banner on screen. The runner posted, the daemon refused, and the
/// completion block that received the error threw it away — so the failure had
/// no symptom at all beyond a missing notification, which reads as "the run
/// didn't finish".
///
/// Blocking, unlike the request in [`init`], and deliberately not shared with
/// it: on a machine that has never granted permission the completion handler
/// does not fire until the user answers the system dialog, and `init` runs on
/// the main thread inside `.setup()` — waiting there would freeze the window
/// behind a prompt. Every caller here is already off the main thread, and the
/// answer is cached, so at most one call per process ever waits.
fn ensure_authorized() -> bool {
    *AUTHORIZED.get_or_init(|| unsafe {
        let center: Retained<AnyObject> =
            msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        // Same bits as `init`: alert | sound | badge.
        let options: usize = 0x04 | 0x02 | 0x01;
        let block = block2::RcBlock::new(move |granted: Bool, _error: *const AnyObject| {
            let _ = tx.send(granted.as_bool());
        });
        let _: () = msg_send![
            &*center,
            requestAuthorizationWithOptions: options,
            completionHandler: &*block
        ];
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(granted) => granted,
            Err(_) => {
                // A timeout is a dialog nobody answered, not a denial. Reported
                // as "not authorized" because posting would fail anyway, and
                // said out loud because it is otherwise indistinguishable from
                // a silent drop.
                log::warn!("notify_macos: no answer to the authorization request within 5s");
                false
            }
        }
    })
}

/// Post a notification under the CXTasks name and icon, whoever is posting.
///
/// **macOS brands a banner with the bundle of the process that posted it**, and
/// there is no API to override that — no title, subtitle or attachment changes
/// who a notification appears to be from. So the routing is about finding a
/// bundle to post from, in descending order of directness:
///
/// 1. the CXTasks window process → `UNUserNotificationCenter` directly, with
///    `task_id` in userInfo so a click focuses the app and reveals the task;
/// 2. anyone else — the LaunchAgent runner, every `tauri dev` build, a helper
///    run by hand out of `target/release` → re-post by running the installed
///    app's own executable with `--notify`, which posts and exits inside a
///    second without ever starting Tauri;
/// 3. CXTasks isn't installed at all → `osascript`, which needs no bundle and
///    always shows, at the cost of the banner reading "Script Editor".
///
/// Route 2 covers two failures that looked unrelated and were the same thing.
/// A dev build's banners arrived wearing Script Editor's pencil icon, because
/// route 3 was the only one it could take. And a background run that finished
/// while the app was CLOSED posted nothing at all: the runner is inside the
/// bundle, so it *looked* like case 1, but a process that is not a registered
/// app is refused (see [`become_an_accessory_app`]) and the refusal was
/// swallowed. Both are fixed by borrowing a real app instance rather than
/// asserting that a path is good enough.
///
/// What route 2 cannot preserve is where a click lands: the notification
/// belongs to the installed bundle, so clicking one raised by a dev build
/// activates the INSTALLED app.
pub fn show_notification(task_id: &str, title: &str, body: &str) {
    if is_the_running_app() {
        show_notification_bundled(task_id, title, body, None);
    } else {
        // The helper route is `std::process::Command` and nothing else, so it
        // lives in `cxtasks_core::notify_helper` — which is what lets the
        // helper BINARIES post banners while building for Linux. Same code,
        // one copy, called from both sides.
        cxtasks_core::notify_helper::post(task_id, title, body);
    }
}

/// True in the CXTasks window process, and nowhere else.
///
/// [`init`] only runs from the Tauri app's `.setup()`, so the handle it parks
/// is the one thing that separates "the app" from its helpers — a path check
/// cannot, because the runner lives in the same `Contents/MacOS` directory.
/// The distinction matters because posting is only safe from a process that is
/// a registered GUI app: everyone else has to borrow one (see
/// [`delegate_to_installed_bundle`]).
fn is_the_running_app() -> bool {
    APP_HANDLE.get().is_some() && inside_app_bundle()
}

/// Post via `UNUserNotificationCenter` and wait until it has been accepted.
///
/// The entry point for `cxtasks-runner --notify`: that process exists only to
/// lend its bundle to the banner, so it must not exit before the request has
/// actually reached the notification daemon. `addNotificationRequest` is
/// asynchronous, so "posted" is the completion handler firing, not the call
/// returning. The timeout is a backstop — returning `false` after 3s means the
/// caller falls back to osascript, which is the right answer for a daemon that
/// never replied.
///
/// Returns false outside a bundle, where posting would raise an ObjC exception
/// and terminate the process rather than fail (see [`inside_app_bundle`]).
pub fn post_and_wait(task_id: &str, title: &str, body: &str) -> bool {
    if !inside_app_bundle() {
        log::warn!("notify_macos: --notify used outside an .app bundle; refusing to post");
        return false;
    }
    become_an_accessory_app();
    // This process has never registered anything with the notification daemon,
    // and the banner it is about to post is the one that most needs the button.
    register_categories_and_flush();
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    show_notification_bundled(task_id, title, body, Some(tx));
    rx.recv_timeout(std::time::Duration::from_secs(3)).is_ok()
}

/// Register this process with the window server as a UI-less app instance.
///
/// **Living inside the bundle is not enough.** A plain command-line process
/// there resolves its bundle identifier — so `currentNotificationCenter` works
/// — but `requestAuthorizationWithOptions` still answers *not granted*, and
/// every post after it fails with `UNErrorDomain error 1`. The permission the
/// user granted belongs to an APP, and until this process is one, macOS has
/// nothing to match it against.
///
/// `Accessory` rather than `Regular`: it registers with the window server
/// without bouncing a second CXTasks icon into the Dock for the half-second
/// this process exists. Nothing here starts a run loop — `sharedApplication`
/// is the registration, `run` would be the UI.
fn become_an_accessory_app() {
    unsafe {
        let app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
        // NSApplicationActivationPolicyAccessory = 1.
        let _: Bool = msg_send![&*app, setActivationPolicy: 1isize];
    }
}

/// Hand the post to the installed bundle's runner and wait for it to finish.
///
/// Waiting is deliberate: the child is bounded by its own 3s ceiling, and a
/// caller that returned immediately could not tell a successful post from a
/// missing install, which is the whole decision the return value carries. Every
/// call site is already off the UI thread (the reminder loop and the bridge
/// handler are both spawned tasks), so the wait costs nothing visible.

/// Notification for unbundled processes. Failure is logged and swallowed —
/// a missing banner must never take down a background run.

fn show_notification_bundled(
    task_id: &str,
    title: &str,
    body: &str,
    posted: Option<std::sync::mpsc::Sender<()>>,
) {
    if !ensure_authorized() {
        log::warn!(
            "notify_macos: not authorized to post — re-enable in \
             System Settings › Notifications › CXTasks"
        );
        // Signal anyway: the waiter is asking "is this handled?", and a refusal
        // IS an answer. Reporting it as a failure would send the caller off to
        // osascript, which posts under a different bundle and would sneak the
        // banner past the very denial being honoured here.
        if let Some(tx) = &posted {
            let _ = tx.send(());
        }
        return;
    }
    unsafe {
        let content: Retained<AnyObject> = msg_send![class!(UNMutableNotificationContent), new];
        let ns_title = NSString::from_str(title);
        let ns_body = NSString::from_str(body);
        let _: () = msg_send![&*content, setTitle: &*ns_title];
        let _: () = msg_send![&*content, setBody: &*ns_body];
        // Without a categoryIdentifier naming a REGISTERED category, macOS
        // renders the banner with no actions at all and says nothing about it.
        let ns_category = NSString::from_str(TASK_CATEGORY);
        let _: () = msg_send![&*content, setCategoryIdentifier: &*ns_category];

        let user_info: Retained<AnyObject> = msg_send![class!(NSMutableDictionary), new];
        let key = NSString::from_str("task_id");
        let val = NSString::from_str(task_id);
        let _: () = msg_send![&*user_info, setObject: &*val, forKey: &*key];
        let _: () = msg_send![&*content, setUserInfo: &*user_info];

        let default_sound: Retained<AnyObject> =
            msg_send![class!(UNNotificationSound), defaultSound];
        let _: () = msg_send![&*content, setSound: &*default_sound];

        let request_id = NSString::from_str(&format!(
            "{}-{}",
            task_id,
            uuid::Uuid::new_v4()
        ));
        let trigger: *const AnyObject = std::ptr::null();
        let request: Retained<AnyObject> = msg_send![
            class!(UNNotificationRequest),
            requestWithIdentifier: &*request_id,
            content: &*content,
            trigger: trigger
        ];

        let center: Retained<AnyObject> =
            msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
        // Fires once the notification daemon has taken the request. Only
        // `post_and_wait` supplies a channel; for everyone else this is the
        // no-op it always was.
        let error_block = block2::RcBlock::new(move |error: *const AnyObject| {
            if !error.is_null() {
                let desc: Retained<AnyObject> =
                    unsafe { msg_send![error, localizedDescription] };
                let desc: &NSString = unsafe { &*(Retained::as_ptr(&desc) as *const NSString) };
                log::warn!("notify_macos: the notification request was rejected: {desc}");
            }
            if let Some(tx) = &posted {
                let _ = tx.send(());
            }
        });
        let _: () = msg_send![
            &*center,
            addNotificationRequest: &*request,
            withCompletionHandler: &*error_block
        ];
    }
}

fn register_delegate_class() -> &'static AnyClass {
    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
    CLASS.get_or_init(|| unsafe {
        let superclass = class!(NSObject) as *const AnyClass;
        let cls = objc_allocateClassPair(superclass, c"CXTasksNotificationDelegate".as_ptr(), 0);
        assert!(!cls.is_null(), "failed to create notification delegate class");

        let proto = objc_getProtocol(c"UNUserNotificationCenterDelegate".as_ptr());
        if !proto.is_null() {
            class_addProtocol(cls, proto);
        }

        // ObjC type encoding: v@:@@@ (void, self, sel, id, id, id)
        class_addMethod(
            cls,
            sel!(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:),
            did_receive_response as *const std::ffi::c_void,
            c"v@:@@@".as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(userNotificationCenter:willPresentNotification:withCompletionHandler:),
            will_present_notification as *const std::ffi::c_void,
            c"v@:@@@".as_ptr(),
        );

        objc_registerClassPair(cls);
        &*cls
    })
}

/// Notification clicked → focus the window and tell the frontend which task.
extern "C" fn did_receive_response(
    _this: *const AnyObject,
    _cmd: Sel,
    _center: *const AnyObject,
    response: *const AnyObject,
    completion: *const AnyObject, // ObjC block
) {
    if response.is_null() || completion.is_null() {
        return;
    }

    unsafe {
        let notification: Retained<AnyObject> = msg_send![response, notification];
        let request: Retained<AnyObject> = msg_send![&*notification, request];
        let content: Retained<AnyObject> = msg_send![&*request, content];
        let user_info: Retained<AnyObject> = msg_send![&*content, userInfo];

        let key = NSString::from_str("task_id");
        let task_obj: *const AnyObject = msg_send![&*user_info, objectForKey: &*key];

        if !task_obj.is_null() {
            let task_nsstr: &NSString = &*(task_obj as *const NSString);
            let task_id = task_nsstr.to_string();

            // Which SURFACE was clicked. An empty or absent identifier is the
            // body, and so is `com.apple.UNNotificationDefaultActionIdentifier`
            // — both mean "show me the task", which is what this always did.
            let action_obj: *const AnyObject = msg_send![response, actionIdentifier];
            let action = if action_obj.is_null() {
                String::new()
            } else {
                (*(action_obj as *const NSString)).to_string()
            };

            if action == ACTION_OPEN_IN_CLAUDE {
                log::info!("notification: Open in Claude for task_id={task_id}");
                open_in_claude(&task_id);
            } else {
                log::info!("notification clicked: task_id={task_id}");
                open_task(&task_id);
            }
        }

        // Call the completion block. ObjC block layout puts the invoke pointer
        // at offset 16 ([isa:8][flags:4][reserved:4][invoke:8]).
        let invoke_ptr = *(completion.byte_add(16) as *const *const std::ffi::c_void);
        let invoke: extern "C" fn(*const AnyObject) = std::mem::transmute(invoke_ptr);
        invoke(completion);
    }
}

/// Arriving while the app is frontmost. Unlike cxmail (which suppresses these
/// because new mail is already visible in the inbox), a due reminder is worth
/// showing even with the window open — the user may be looking at a different
/// view. 0x04 = UNNotificationPresentationOptionBanner (+ sound 0x02).
extern "C" fn will_present_notification(
    _this: *const AnyObject,
    _cmd: Sel,
    _center: *const AnyObject,
    _notification: *const AnyObject,
    completion: *const AnyObject,
) {
    if completion.is_null() {
        return;
    }
    unsafe {
        let invoke_ptr = *(completion.byte_add(16) as *const *const std::ffi::c_void);
        let invoke: extern "C" fn(*const AnyObject, usize) = std::mem::transmute(invoke_ptr);
        // UNNotificationPresentationOptions — note these are NOT the same bits
        // as UNAuthorizationOptions:
        //   Badge 1<<0=1, Sound 1<<1=2, Alert 1<<2=4 (DEPRECATED in macOS 11),
        //   List 1<<3=8, Banner 1<<4=16.
        // Passing the old `Alert` bit silently shows nothing on macOS 11+ —
        // which is exactly how this presented: the scan logged "fired 1 due
        // reminder(s)" and no banner ever appeared.
        const BANNER: usize = 1 << 4;
        const LIST: usize = 1 << 3;
        const SOUND: usize = 1 << 1;
        invoke(completion, BANNER | LIST | SOUND);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The latch is a HANDOFF, not a mailbox: whoever reads it first empties it.
    ///
    /// Both triggers drain the same slot — the app's mount (which is what
    /// catches a click on a closed app) and the `open-task-from-notification`
    /// event (which is what makes a click on a running app land immediately).
    /// If a read left the id behind, the second drain would reveal the same
    /// task again, and worse, an id parked by a click that WAS handled would
    /// still be sitting there at some unrelated future launch, yanking the user
    /// to a task they dealt with days ago.
    /// The latch is process-wide, and `cargo test` runs these three in
    /// parallel threads of ONE process — so without serializing them, one
    /// test's `park` satisfies another's `take` and the failure lands on
    /// whichever lost the race. It presented as a flaky
    /// `a_parked_task_is_handed_out_exactly_once`, which is the test least
    /// related to the cause.
    ///
    /// A dedicated mutex rather than a test-only latch: what is under test IS
    /// the global, and swapping it for a per-test one would leave the real
    /// thing uncovered.
    static LATCH_TESTS: Mutex<()> = Mutex::new(());

    /// Take the test lock, surviving a previous test's panic.
    ///
    /// A poisoned mutex here means some other test failed while holding it;
    /// propagating that would turn one failure into three and hide the real
    /// one.
    fn latch_guard() -> std::sync::MutexGuard<'static, ()> {
        LATCH_TESTS.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn a_parked_task_is_handed_out_exactly_once() {
        let _lock = latch_guard();
        park_pending_task("task-abc");
        assert_eq!(take_pending_task().as_deref(), Some("task-abc"));
        assert_eq!(take_pending_task(), None);
    }

    /// Nothing pending is the normal case — every ordinary launch, and every
    /// drain after the first. It must read as "nothing to do", never as an id.
    #[test]
    fn an_untouched_latch_is_empty() {
        let _lock = latch_guard();
        let _ = take_pending_task();
        assert_eq!(take_pending_task(), None);
    }

    /// Two clicks before a drain: the second wins. Only one task can be on
    /// screen, so this is a change of mind rather than a queue to work through.
    #[test]
    fn the_last_click_wins() {
        let _lock = latch_guard();
        park_pending_task("first");
        park_pending_task("second");
        assert_eq!(take_pending_task().as_deref(), Some("second"));
        assert_eq!(take_pending_task(), None);
    }
}

/// Adapts this module to [`cxtasks_core::notify::Notifier`], so the runner can
/// post a banner without naming anything macOS.
///
/// Installed by whichever binary has a UI to speak for: the Tauri app in
/// `lib.rs`'s setup, and `cxtasks-runner` in its `main`. `cxtasks-mcp` does not
/// install one — it runs inside somebody else's Claude session and has no
/// business raising banners.
pub struct MacNotifier;

impl cxtasks_core::notify::Notifier for MacNotifier {
    fn post(&self, task_id: &str, title: &str, body: &str) {
        show_notification(task_id, title, body);
    }
}
