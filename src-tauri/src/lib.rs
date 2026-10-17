mod quickask;
// What is left in this crate is exactly what needs macOS: the Tauri commands,
// launchd, Ghostty, and the three `*_macos` modules. Everything else now lives
// in `cxtasks-core` and builds for Linux.
pub mod appearance_macos;
pub mod commands;
#[cfg(target_os = "macos")]
pub mod dock_macos;
pub mod ghostty_macos;
pub mod glass_macos;
pub mod links;
pub mod notify_macos;
pub mod vibrancy_macos;

// ── moved to cxtasks-core, re-exported under their original paths ──────────
//
// Re-exporting means every existing `crate::db::tasks::update(...)` and
// `crate::runner::gate(...)` in this crate keeps resolving unchanged, so the
// move touched no call site and no test body.
//
// That is the point: a refactor whose diff is almost entirely `git mv` can
// actually be reviewed. The `use` churn can come later, on its own, when it is
// the only thing in the diff.
pub use cxtasks_core::{
    checkpoint, db, dispatch, error, executor, intent, launchd, mcp, notify, notify_helper, paths,
    platform, recurrence, runner,
};

use std::fs;
use std::sync::Mutex;

use tauri::{Emitter, Manager};

pub struct AppState {
    pub db: Mutex<rusqlite::Connection>,
    pub db_path: std::path::PathBuf,
}

/// A poisoned DB mutex is not a reason to take the app down — the connection
/// itself is still usable, and the alternative is a hard crash on the next
/// click. Same posture as cxmail.
pub trait LockExt<T> {
    fn safe_lock(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn safe_lock(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| {
            log::warn!("recovered a poisoned DB mutex");
            poisoned.into_inner()
        })
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            quickask::publish_quickask_selection,
            commands::sync::sync_status,
            commands::sync::sync_pair,
            commands::sync::sync_now,
            commands::sync::sync_set_enabled,
            commands::sync::sync_unpair,
            commands::hosts::list_hosts,
            commands::hosts::set_host_policy,
            commands::hosts::set_host_mirror_remote,
            commands::hosts::remove_host,
            commands::hosts::runs_finished_elsewhere,
            commands::tasks::list_tasks,
            commands::tasks::get_task,
            commands::tasks::create_task,
            commands::tasks::update_task,
            commands::tasks::complete_task,
            commands::tasks::trash_task,
            commands::tasks::restore_task,
            commands::tasks::archive_task,
            commands::tasks::unarchive_task,
            commands::tasks::destroy_task,
            commands::tasks::empty_trash,
            commands::tasks::trash_countdowns,
            commands::tasks::reorder_tasks,
            commands::tasks::list_subtasks,
            commands::tasks::list_projects,
            commands::tasks::view_counts,
            commands::tasks::search_tasks,
            commands::tasks::filter_tasks,
            commands::tasks::count_filtered,
            commands::tasks::list_runs,
            commands::tasks::list_run_events,
            commands::tasks::recent_runs,
            commands::tasks::budget_status,
            commands::tasks::save_budget_settings,
            commands::tasks::reset_budget_window,
            commands::tasks::set_background_paused,
            commands::tasks::suggest_repos,
            commands::tasks::list_tags,
            commands::tasks::task_tags,
            commands::tasks::set_task_tags,
            commands::tasks::rename_tag,
            commands::tasks::delete_tag,
            commands::tasks::task_tag_map,
            commands::tasks::tag_usage_count,
            commands::tasks::task_relations,
            commands::tasks::add_dependency,
            commands::tasks::remove_dependency,
            commands::tasks::add_relation,
            commands::tasks::remove_relation,
            commands::tasks::blocked_task_ids,
            commands::tasks::retrospective,
            commands::tasks::jev_status,
            commands::tasks::save_jev_settings,
            commands::tasks::jev_assess_capture,
            commands::tasks::jev_assess_readiness,
            commands::tasks::list_tripwire_runtime,
            commands::tasks::tripwire_scripts,
            commands::tasks::list_comments,
            commands::tasks::add_comment,
            commands::tasks::edit_comment,
            commands::tasks::delete_comment,
            commands::attachments::list_task_attachments,
            commands::attachments::add_task_attachments,
            commands::attachments::paste_task_image,
            commands::attachments::read_clipboard_image,
            commands::attachments::try_read_clipboard_image,
            commands::attachments::add_comment_with_attachments,
            commands::attachments::delete_task_attachment,
            commands::tasks::list_all_projects,
            commands::tasks::create_project,
            commands::tasks::set_project_parent,
            commands::tasks::set_project_kind,
            commands::tasks::set_project_target_date,
            commands::tasks::set_project_closed,
            commands::tasks::rename_project,
            commands::tasks::set_project_appearance,
            commands::tasks::set_tag_appearance,
            commands::icons::repo_icon_candidates,
            commands::icons::icon_from_file,
            commands::tasks::archive_project,
            commands::tasks::reorder_projects,
            commands::tasks::delete_project,
            commands::tasks::list_project_repos,
            commands::tasks::add_project_repo,
            commands::tasks::remove_project_repo,
            commands::tasks::set_primary_project_repo,
            commands::tasks::project_for_repo_path,
            commands::tasks::list_sections,
            commands::tasks::create_section,
            commands::tasks::rename_section,
            commands::tasks::delete_section,
            commands::tasks::reorder_sections,
            commands::tasks::move_task_to_section,
            commands::tasks::project_board,
            commands::tasks::post_project_update,
            commands::tasks::list_project_updates,
            commands::tasks::set_section_target_date,
            commands::tasks::set_section_completed,
            commands::dispatch::dispatch_foreground,
            commands::dispatch::get_dispatch_prompt,
            commands::dispatch::queue_background,
            commands::dispatch::executor_available,
            commands::dispatch::run_queue_once,
            commands::dispatch::is_runner_installed,
            commands::dispatch::install_runner,
            commands::dispatch::uninstall_runner,
            commands::dispatch::cancel_run,
            commands::dispatch::run_diff,
            commands::dispatch::restore_checkpoint,
            commands::appearance::set_native_appearance,
            commands::glass::set_window_glass,
            commands::glass::set_blur_radius,
            commands::glass::get_reduce_transparency,
            commands::vault_look::fetch_vault_look,
            commands::notifications::take_pending_notification_task,
            commands::dock::get_dock_menu_settings,
            commands::dock::save_dock_menu_settings,
        ])
        .setup(|app| {
            quickask::start(app.handle().clone());
            let targets: Vec<tauri_plugin_log::Target> = if cfg!(debug_assertions) {
                vec![
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: None,
                    }),
                ]
            } else {
                vec![tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::LogDir { file_name: None },
                )]
            };
            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(log::LevelFilter::Info)
                    .max_file_size(5_000_000)
                    .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepSome(3))
                    .targets(targets)
                    .build(),
            )?;

            // The MCP binary and the runner resolve the DB through `paths`, not
            // through Tauri's app_data_dir — keep them identical or the app and
            // the tools end up on two different databases.
            let app_dir = paths::app_data_dir();
            fs::create_dir_all(&app_dir)?;

            let db_path = paths::db_path();
            let conn = rusqlite::Connection::open(&db_path)?;
            // Another process (MCP, runner) may hold a write lock; retry for 5s.
            conn.busy_timeout(std::time::Duration::from_secs(5))?;
            db::schema::initialize(&conn)?;
            db::search::verify_or_rebuild(&conn);
            match cxtasks_core::attachments::prune_orphaned_files(&conn) {
                Ok(n) if n > 0 => log::info!("removed {n} orphaned attachment file(s)"),
                Ok(_) => {}
                Err(error) => log::warn!("attachment cleanup failed: {error}"),
            }
            log::info!("Database initialized at {:?}", db_path);

            // Pin native chrome (NSMenu, <select> popups, sheets, scrollbars)
            // so it matches the UI instead of the system setting. `.setup()` is
            // the main thread, which AppKit requires.
            //
            // Dark is the *boot* default only, matching `tauri.conf.json`'s
            // window `"theme": "Dark"` — the real choice lives in the frontend's
            // localStorage, which Rust can't read from here. The frontend calls
            // `set_native_appearance` on hydrate to correct this. That leaves at
            // most a frame of native mismatch on a light-theme launch, and only
            // for chrome that isn't on screen yet.
            #[cfg(target_os = "macos")]
            appearance_macos::set_appearance(true);

            // Paint the window OPAQUE for launch, and let the frontend turn the
            // glass on once it has something to show (`glass_macos::enable`,
            // called from `applyGlass` on hydrate).
            //
            // The window is `"transparent": true` in `tauri.conf.json`, so
            // without this it boots as a clear pane: between window creation and
            // React's first paint there is nothing in it, and what the user sees
            // is bare wallpaper with three traffic lights floating on it. The
            // old `NSVisualEffectView` hid that gap by filling it with a
            // material; a plain CGS blur has nothing to fill it with, so the
            // launch state has to be opaque and the flip deferred.
            //
            // `--bg-primary` for dark, which is also `tauri.conf.json`'s window
            // `"theme": "Dark"` boot default. A light-theme launch is corrected
            // by the same frontend call that enables the glass, one frame later
            // — the same deal `set_native_appearance` already makes above.
            #[cfg(target_os = "macos")]
            glass_macos::with_main(&app.handle().clone(), |window| {
                glass_macos::set_launch_background(window, 24, 24, 24);
            });

            // Reduce Transparency used to be free — `NSVisualEffectView` went
            // opaque by itself. A raw CGS blur reads no such setting, so we now
            // watch for it. Registered once; the observer lives as long as the
            // process (see the module doc for why the block is leaked).
            #[cfg(target_os = "macos")]
            glass_macos::install_accessibility_observer(&app.handle().clone());

            // Notification delegate — only valid inside the app bundle.
            notify_macos::init(&app.handle().clone());
            // The runner half posts through the platform-neutral hook, so the
            // app has to tell it what platform it is on.
            cxtasks_core::notify::set_notifier(Box::new(notify_macos::MacNotifier));
            // `dispatch` can no longer ask launchd directly — it has to build
            // for Linux. Unprobed it stays quiet; probed it tells the truth.
            cxtasks_core::platform::set_drain_probe(Box::new(launchd::is_installed));

            app.manage(AppState {
                db: Mutex::new(conn),
                db_path: db_path.clone(),
            });
            #[cfg(target_os = "macos")]
            dock_macos::install(&app.handle().clone());

            // ── MCP/runner → app live bridge ───────────────────────────────
            // `cxtasks-mcp` writes one JSON Envelope per connection when it
            // mutates a task; we re-emit each as an `mcp-activity` frontend
            // event so the app live-refreshes without polling. This is what
            // makes `/task` in another terminal show up here within a second.
            {
                let socket = paths::socket_path();
                // Clear any stale socket from a previous (crashed) run.
                let _ = fs::remove_file(&socket);
                let bridge_handle = app.handle().clone();
                // CRITICAL: `UnixListener::bind` registers the fd with the Tokio
                // reactor, so it MUST run inside the runtime. `.setup()` runs on
                // the main thread OUTSIDE any runtime — binding here
                // synchronously panics ("there is no reactor running"), and
                // because setup runs inside the ObjC did_finish_launching
                // callback that panic can't unwind and aborts the whole app.
                tauri::async_runtime::spawn(async move {
                    let listener = match tokio::net::UnixListener::bind(&socket) {
                        Ok(l) => {
                            log::info!("live bridge listening at {:?}", socket);
                            l
                        }
                        Err(e) => {
                            // EADDRINUSE → another app instance owns the socket.
                            // Non-fatal by design: the app still works, just
                            // without live refresh.
                            log::warn!("live bridge bind {socket:?} failed (another instance?): {e}");
                            return;
                        }
                    };
                    loop {
                        match listener.accept().await {
                            Ok((mut stream, _addr)) => {
                                let conn_handle = bridge_handle.clone();
                                tauri::async_runtime::spawn(async move {
                                    use tokio::io::AsyncReadExt;
                                    let mut buf = Vec::new();
                                    if let Err(e) = stream.read_to_end(&mut buf).await {
                                        log::debug!("bridge: read failed: {e}");
                                        return;
                                    }
                                    match serde_json::from_slice::<mcp::bridge::Envelope>(&buf) {
                                        Ok(env) => {
                                            log::info!(
                                                "bridge: received {} (tool={:?}, task={}) → emitting mcp-activity",
                                                env.kind, env.tool, env.task_id
                                            );
                                            if let Err(e) = conn_handle.emit("mcp-activity", &env) {
                                                log::warn!("bridge: emit failed: {e}");
                                            }
                                            // The runner hands a finished run
                                            // to whoever is in front of the
                                            // user: we post, because we are the
                                            // side that can word it around
                                            // what's already on screen.
                                            if env.kind == "run-finished" {
                                                let summary = env
                                                    .summary
                                                    .as_deref()
                                                    .unwrap_or("finished");
                                                notify_macos::show_notification(
                                                    &env.task_id,
                                                    "CXTasks — background run",
                                                    summary,
                                                );
                                            }
                                            // A tripwire transition is the
                                            // same hand-off: the runner
                                            // recorded it, and whoever is in
                                            // front of the user says so.
                                            if env.kind == "tripwire-tripped"
                                                || env.kind == "tripwire-failed"
                                            {
                                                let summary = env
                                                    .summary
                                                    .as_deref()
                                                    .unwrap_or("tripwire");
                                                notify_macos::show_notification(
                                                    &env.task_id,
                                                    if env.kind == "tripwire-tripped" {
                                                        "CXTasks — tripwire tripped"
                                                    } else {
                                                        "CXTasks — tripwire check failed"
                                                    },
                                                    summary,
                                                );
                                            }
                                        }
                                        Err(e) => log::debug!("bridge: parse failed: {e}"),
                                    }
                                });
                            }
                            Err(e) => {
                                log::warn!("bridge: accept error: {e}");
                                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            }
                        }
                    }
                });
            }

            // ── due / reminder scan ────────────────────────────────────────
            // 30s cadence, mirroring cxmail's scheduled-send loop. Every
            // comparison inside `list_due_for_reminder` wraps BOTH operands in
            // datetime() — see gotcha #23. Without that a task due at 9am
            // fires at UTC midnight instead, which on this machine is ~8pm.
            //
            // Like cxmail's, this only runs while the app is open and the Mac
            // is awake; that's inherent to a local-first app with no server.
            // A missed reminder fires on the next wake rather than being lost.
            {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_secs(30));
                    loop {
                        interval.tick().await;

                        let Some(state) = app_handle.try_state::<AppState>() else {
                            continue;
                        };

                        // Small lock scope: collect, release, then notify.
                        let due = {
                            let conn = state.db.safe_lock();
                            db::tasks::list_due_for_reminder(&conn).unwrap_or_default()
                        };
                        if due.is_empty() {
                            continue;
                        }

                        for task in &due {
                            let body = match task.project.as_deref() {
                                Some(p) => format!("#{p} — due now"),
                                None => "Due now".to_string(),
                            };
                            notify_macos::show_notification(&task.id, &task.title, &body);

                            // Mark it BEFORE anything that could fail, so a
                            // hiccup can never re-notify the same task in a
                            // loop 30 seconds later.
                            let conn = state.db.safe_lock();
                            if let Err(e) = db::tasks::mark_reminded(&conn, &task.id) {
                                log::warn!("could not mark task {} reminded: {e}", task.id);
                            }
                        }

                        log::info!("fired {} due reminder(s)", due.len());
                        if let Err(e) = app_handle.emit("tasks-changed", ()) {
                            log::debug!("tasks-changed emit failed: {e}");
                        }
                    }
                });
            }

            // ── periodic sync ──────────────────────────────────────────────
            // A button is a fine way to say "now" and a terrible way to say
            // "keep up". Without this loop the Mac reconciled only when someone
            // opened Settings and clicked, so on 2026-08-25 `sync_last_pull_at`
            // was found four days stale with nothing erroring anywhere — and
            // the canonical node the phone reads was missing nine rows,
            // including every tripwire value. The argument and the cadence live
            // in `sync::auto`; this is only the clock.
            //
            // **A dedicated OS thread, not `async_runtime::spawn`**, and not
            // the frontend. Three reasons, each of which was the deciding one
            // for something:
            //
            //   * `HttpPeer` is BLOCKING with a 120-second timeout. On a Tokio
            //     worker that parks an executor thread for two minutes when the
            //     NAS is slow.
            //   * It opens its OWN connection rather than taking
            //     `AppState.db`. Every Tauri command locks that mutex, so
            //     holding it across an unreachable peer's connect timeout
            //     freezes the whole UI every five minutes — the loop's worst
            //     day would be indistinguishable from the app hanging. A second
            //     connection to a WAL database is the ordinary case here: the
            //     MCP and the runner have each had one, from other processes,
            //     since the first version.
            //   * A frontend `setInterval` would have been the smallest diff
            //     and is the wrong shape — WKWebView throttles timers hard in a
            //     hidden or occluded window, and Cmd+W HIDES this app's window
            //     rather than quitting. The state where sync stops would be
            //     exactly the state where the app looks closed and isn't, which
            //     is the bug wearing a new hat.
            //
            // Non-overlap is structural: the pass is synchronous and the sleep
            // follows it, so there is no tick to miss and no in-flight guard to
            // get wrong.
            {
                let app_handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(
                        cxtasks_core::sync::auto::STARTUP_DELAY_SECONDS,
                    ));
                    let mut conn = match rusqlite::Connection::open(paths::db_path()) {
                        Ok(c) => c,
                        Err(e) => {
                            // Fatal to the loop, not to the app — same posture
                            // as the runner's control thread. Sync still works
                            // from the button; what is lost is it being
                            // automatic, and saying so beats a silently
                            // half-working feature.
                            log::error!(
                                "periodic sync could not open the database ({e}); \
                                 automatic reconcile is OFF for this session"
                            );
                            return;
                        }
                    };
                    // Another process (the app itself, the MCP, the runner) may
                    // hold the write lock while a snapshot is being applied.
                    if let Err(e) = conn.busy_timeout(std::time::Duration::from_secs(10)) {
                        log::warn!("periodic sync: busy_timeout failed: {e}");
                    }
                    loop {
                        match cxtasks_core::sync::auto::pass(&mut conn) {
                            // Not paired, paused, or canonical. Says nothing,
                            // every five minutes, deliberately.
                            Ok(None) => {}
                            Ok(Some(report)) => {
                                log::info!(
                                    "sync: swept {} pushed {} failed {} pulled {:?} held_back {}",
                                    report.swept,
                                    report.pushed,
                                    report.failed.len(),
                                    report.pulled,
                                    report.held_back
                                );
                                // ONLY on `changed()` — a redraw resets
                                // in-progress edits (gotcha #24), so a quiet
                                // reconcile that emitted anyway would discard
                                // characters while you type, on a timer.
                                //
                                // Measured against the live pairing, that is
                                // less quiet than it sounds: the NAS heartbeats
                                // its own `hosts` row every 60s, so most passes
                                // legitimately pull one row and do emit. It is
                                // still safe, because the draft reset keys on
                                // the SELECTED TASK's `updated_at` — a host row
                                // moving refetches the board and touches no
                                // task. Don't narrow this to "tasks only":
                                // Mission Control is drawn from `hosts`.
                                if report.changed() {
                                    if let Err(e) = app_handle.emit("tasks-changed", ()) {
                                        log::debug!("tasks-changed emit failed: {e}");
                                    }
                                }
                            }
                            Err(e) => log::warn!("sync pass failed: {e}"),
                        }
                        std::thread::sleep(std::time::Duration::from_secs(
                            cxtasks_core::sync::auto::INTERVAL_SECONDS,
                        ));
                    }
                });
            }

            // ── trash purge ────────────────────────────────────────────────
            // The only automatic destroyer of user data in the app, so it runs
            // deliberately slowly: once at launch, then daily. There is nothing
            // to gain from a tighter cadence — a task one hour past its 30-day
            // window is not urgent — and every extra tick is another chance for
            // a bug here to be irreversible.
            //
            // Same "only while the app is open" caveat as the reminder scan; a
            // purge missed while the Mac slept simply happens at next launch,
            // which errs toward keeping data longer than promised rather than
            // shorter. That is the right direction to be wrong in.
            {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let mut interval = tokio::time::interval(
                        std::time::Duration::from_secs(60 * 60 * 24),
                    );
                    loop {
                        interval.tick().await; // fires immediately on the first pass
                        let Some(state) = app_handle.try_state::<AppState>() else {
                            continue;
                        };
                        let purged = {
                            let conn = state.db.safe_lock();
                            let purged = db::tasks::purge_expired(&conn).unwrap_or(0);
                            if purged > 0 {
                                if let Err(error) =
                                    cxtasks_core::attachments::prune_orphaned_files(&conn)
                                {
                                    log::warn!("attachment cleanup after purge failed: {error}");
                                }
                            }
                            purged
                        };
                        if purged > 0 {
                            if let Err(e) = app_handle.emit("tasks-changed", ()) {
                                log::debug!("tasks-changed emit failed: {e}");
                            }
                        }
                    }
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Cmd+W and the red close button hide the window instead of
            // quitting — same posture as cxmail. The app stays running (dock
            // icon, background runner, live bridge all keep going); only an
            // explicit Quit (Cmd+Q) or the tray-less equivalent tears it down.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
                return;
            }
            if let tauri::WindowEvent::Destroyed = event {
                if window.label() == "main" {
                    let _ = fs::remove_file(paths::socket_path());
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building CXTasks")
        .run(|app_handle, event| match event {
            tauri::RunEvent::Reopen { .. } => {
                // Re-show the window when the dock icon is clicked while
                // hidden (no tray icon here, so this is the only way back in).
                if let Some(window) = app_handle.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            // `cxtasks://task/<ref>` — see `links`. Several URLs in one event
            // is last-wins, like the latch it feeds.
            tauri::RunEvent::Opened { urls } => {
                for url in urls {
                    match links::task_ref(url.as_str()) {
                        Some(task_ref) => notify_macos::open_task(&task_ref),
                        None => log::warn!("ignored a link CXTasks can't open: {url}"),
                    }
                }
            }
            _ => {}
        });
}
