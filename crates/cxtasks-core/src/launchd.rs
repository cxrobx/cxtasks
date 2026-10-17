//! LaunchAgent install/uninstall for `cxtasks-runner`.
//!
//! Ported from cxmail's `launchd.rs`. The important inherited detail is the
//! exit-code-36 branch: `launchctl bootstrap` returns 36 (EALREADY) when the
//! job is already loaded, which is success, not failure — treating it as an
//! error makes re-installs look broken.

use crate::error::AppError;
use std::path::PathBuf;
use std::process::Command;

const LABEL: &str = "com.cxtasks.app.runner";

fn plist_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// In production the binary sits next to the app binary inside the bundle; in
/// dev it's a sibling in target/debug.
fn runner_binary_path() -> PathBuf {
    crate::paths::sibling_binary("cxtasks-runner")
}

pub fn is_installed() -> bool {
    plist_path().exists()
}

pub fn install_launch_agent() -> Result<(), AppError> {
    let runner = runner_binary_path();
    if !runner.exists() {
        return Err(AppError::General(format!(
            "cxtasks-runner not found at {} — build it with \
             `cargo build --release --bin cxtasks-runner`",
            runner.display()
        )));
    }
    let plist = plist_path();
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let log_dir = crate::paths::log_dir();
    std::fs::create_dir_all(&log_dir)?;
    let out_path = log_dir.join("runner.log");
    let err_path = log_dir.join("runner.err.log");

    let plist_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>

    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>

    <key>RunAtLoad</key>
    <true/>

    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>

    <key>ThrottleInterval</key>
    <integer>30</integer>

    <key>StandardOutPath</key>
    <string>{}</string>

    <key>StandardErrorPath</key>
    <string>{}</string>

    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
</dict>
</plist>
"#,
        runner.display(),
        out_path.display(),
        err_path.display(),
    );

    std::fs::write(&plist, plist_content)?;
    log::info!("LaunchAgent plist written to {plist:?}");

    let uid = unsafe { libc::getuid() };
    let status = Command::new("launchctl")
        .args(["bootstrap", &format!("gui/{uid}"), &plist.to_string_lossy()])
        .status();

    match status {
        Ok(s) if s.success() => {
            log::info!("LaunchAgent bootstrapped successfully");
            Ok(())
        }
        Ok(s) => {
            // 36 = EALREADY (already loaded) — that's success.
            let code = s.code().unwrap_or(-1);
            if code == 36 {
                log::info!("LaunchAgent already loaded");
            } else {
                log::warn!("launchctl bootstrap exited with code {code}");
            }
            // The plist is written either way; it loads at next login.
            Ok(())
        }
        Err(e) => {
            log::warn!("failed to run launchctl: {e}");
            Ok(())
        }
    }
}

pub fn uninstall_launch_agent() -> Result<(), AppError> {
    let uid = unsafe { libc::getuid() };
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("gui/{uid}/{LABEL}")])
        .status();

    let plist = plist_path();
    if plist.exists() {
        std::fs::remove_file(&plist)?;
        log::info!("LaunchAgent plist removed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_path_is_in_the_user_launchagents_dir() {
        let p = plist_path();
        assert!(p.ends_with("Library/LaunchAgents/com.cxtasks.app.runner.plist"), "{p:?}");
    }

    #[test]
    fn label_does_not_collide_with_cxmail() {
        assert_ne!(LABEL, "com.cxmail.app.helper");
        assert!(LABEL.starts_with("com.cxtasks."));
    }
}
