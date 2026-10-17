// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `--notify` posts one banner and exits, without ever starting Tauri.
    //
    // It has to live on THIS binary rather than on `cxtasks-runner`, and that
    // is the whole point of it: macOS grants notification permission to an APP,
    // and an app is its bundle's main executable. A helper sitting beside it in
    // `Contents/MacOS` resolves the same bundle identifier and is still refused
    // — `authorizationStatus` reads `notDetermined` for it no matter what
    // activation policy it adopts. So processes that cannot post for themselves
    // (the runner, and every `tauri dev` build, which has no bundle at all) run
    // this instead. See `notify_macos::show_notification`.
    //
    // Before `run()` on purpose: this must not open the database, bind the live
    // bridge, or put a second window on screen while the real app is open.
    #[cfg(target_os = "macos")]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.iter().any(|a| a == "--notify") {
            let value = |flag: &str| {
                args.iter()
                    .position(|a| a == flag)
                    .and_then(|i| args.get(i + 1))
                    .map(String::as_str)
                    .unwrap_or("")
            };
            let posted = cxtasks_lib::notify_macos::post_and_wait(
                value("--task-id"),
                value("--title"),
                value("--body"),
            );
            // The exit status is the caller's answer: non-zero is what sends it
            // to the osascript fallback rather than posting nothing at all.
            std::process::exit(if posted { 0 } else { 1 });
        }
    }

    cxtasks_lib::run()
}
