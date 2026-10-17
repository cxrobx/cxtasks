//! Opening a dispatched session in Ghostty.
//!
//! Split out of `dispatch.rs` because it is the only part of that module that
//! is macOS-specific — it shells `/usr/bin/osascript` and hardcodes
//! `/Applications/Ghostty.app`. Everything else in `dispatch.rs` (the seed
//! prompt, the scratch dir, repo resolution) is platform-neutral and has moved
//! to `cxtasks-core`.
//!
//! There is no Linux counterpart and there should not be one. Foreground
//! dispatch means "a human is at this machine watching the window", which is
//! false by construction on a headless host — see invariant 12 and the
//! background path's `repo_path` rules.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::dispatch::shell_quote;

/// Locate Ghostty.app.
pub fn find_ghostty_app() -> Option<PathBuf> {
    let p = PathBuf::from("/Applications/Ghostty.app");
    if p.exists() {
        return Some(p);
    }
    if let Some(home) = dirs::home_dir() {
        let user_p = home.join("Applications/Ghostty.app");
        if user_p.exists() {
            return Some(user_p);
        }
    }
    None
}

/// Ask an ALREADY-RUNNING Ghostty for a new window running `launch_path`.
///
/// The good path for foreground dispatch: a window inside the instance the user
/// already has, rather than a whole second Ghostty in the Dock. Returns
/// `Err(reason)` whenever this route isn't available so the caller can fall
/// back to `open -na` — see `commands::dispatch` for what each branch buys.
///
/// Ghostty 1.3 exposes an AppleScript dictionary (`Ghostty.sdef`, gated on
/// `macos-applescript`, default true). Note it is `new window` and not
/// `new tab`: `new tab` answers `errAEEventNotHandled` in 1.3.1 even with no
/// arguments at all, so there is nothing to pass differently.
pub fn open_in_running_ghostty(repo: &Path, launch_path: &Path) -> Result<(), String> {
    let command = ghostty_command(launch_path);
    let output = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg("on run argv")
        // `is running` is answered by Launch Services rather than by an Apple
        // Event, so this asks the question WITHOUT launching Ghostty as a side
        // effect (verified against a quit app). That distinction is the whole
        // guard: launching Ghostty cold restores its saved windows, which is
        // the multi-window mess the fallback needs `--window-save-state=never`
        // to avoid. A cold start therefore belongs on the fallback, which
        // already handles it — and lands exactly one clean instance that every
        // later dispatch can then reuse through this path.
        .arg("-e")
        .arg(
            r#"if application id "com.mitchellh.ghostty" is not running then error "Ghostty isn't running" number -128"#,
        )
        .arg("-e")
        .arg(r#"tell application id "com.mitchellh.ghostty""#)
        // `wait after command:false` deliberately MATCHES the fallback: the
        // window closes when the session ends, exactly as it does today. Set it
        // true and a dispatch would start behaving differently depending on
        // which branch it took, which is the one thing this must not do.
        .arg("-e")
        .arg(
            "new window with configuration {initial working directory:(item 1 of argv), \
             command:(item 2 of argv), wait after command:false}",
        )
        .arg("-e")
        .arg("end tell")
        .arg("-e")
        .arg("end run")
        // The paths travel as ARGUMENTS, never interpolated into the script
        // source. AppleScript string literals escape nothing the way a shell
        // does, so a repo path holding a quote or a backslash would be a syntax
        // error at best. Both are absolute, so neither can be mistaken for an
        // osascript option.
        //
        // The two are quoted DIFFERENTLY and must stay that way — see
        // `ghostty_command`. The working directory is handed to Ghostty raw
        // because it is applied directly; only the command reaches a shell.
        .arg(repo)
        .arg(&command)
        .output()
        .map_err(|e| format!("could not run osascript: {e}"))?;

    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        format!("osascript exited {}", output.status)
    } else {
        stderr
    })
}

/// The `command` value for a Ghostty surface configuration: the launch script,
/// **shell-quoted**.
///
/// Ghostty does not exec this string — it runs it through a login shell,
/// `login -flp <user> /bin/bash --noprofile --norc -c exec -l <command>`, so
/// **bash word-splits it**. Every dispatch hits this, because the scratch dir
/// lives under `~/Library/Application Support/…` and that space is not optional:
/// unquoted, bash tries to exec `~/Library/Application` and Ghostty reports
/// "failed to launch the requested command".
///
/// Note the asymmetry with `initial working directory`, which is applied
/// directly by Ghostty and must therefore be passed RAW — quoting that one puts
/// literal `'` characters into the path and breaks a `cd` that currently works.
/// One field reaches a shell and the other doesn't; they are not interchangeable
/// however similar they look sitting next to each other in the record.
fn ghostty_command(launch_path: &Path) -> String {
    shell_quote(&launch_path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::shell_quote;
    use std::process::Command;

    /// The property `open_in_running_ghostty` depends on: a path handed to
    /// `osascript` as an ARGUMENT arrives byte-identical, whatever is in it.
    ///
    /// This is the guard against the tempting "simplification" of interpolating
    /// the repo path into the script source with `format!`. AppleScript string
    /// literals have their own escaping — nothing like a shell's, and
    /// [`shell_quote`] is the wrong tool for them — so an interpolated path
    /// holding a quote or a backslash is a *compile* error inside osascript,
    /// which surfaces as this route silently failing over to spawning a second
    /// Ghostty instance. Exercising the real `osascript` is the only assertion
    /// that means anything; the script here is deliberately inert (it echoes an
    /// argument and talks to no application) so the test opens no windows and
    /// needs no automation consent.
    #[test]
    fn osascript_arguments_arrive_verbatim_however_they_are_quoted() {
        for evil in [
            "/tmp/a\"b",
            "/tmp/a\\b",
            "/tmp/a'b",
            "/tmp/a b",
            "/tmp/say \"hi\" & quit",
        ] {
            let out = Command::new("/usr/bin/osascript")
                .arg("-e")
                .arg("on run argv")
                .arg("-e")
                .arg("return item 1 of argv")
                .arg("-e")
                .arg("end run")
                .arg(evil)
                .output()
                .expect("run osascript");
            assert!(out.status.success(), "osascript rejected {evil:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'),
                evil,
                "osascript altered {evil:?} — it is not being passed as an argument"
            );
        }
    }

    /// Ghostty's `command` reaches a shell, so it is word-SPLIT — and the real
    /// launch script always sits under `~/Library/Application Support/…`, so
    /// the space is guaranteed rather than exotic. Unquoted, bash tries to exec
    /// `~/Library/Application` and the dispatch dies in a window that says
    /// "Ghostty failed to launch the requested command". It shipped that way
    /// once, because the probe used to develop this lived in a path with no
    /// spaces — which is why this test uses the REAL scratch location's shape.
    ///
    /// Asserting "one word out, equal to the path in" rather than on the quoted
    /// string's shape, for the same reason as
    /// `shell_quote_is_inert_when_a_real_shell_expands_it` above.
    #[test]
    fn the_ghostty_command_is_one_word_after_the_shell_splits_it() {
        for path in [
            // The shape that actually broke.
            "/Users/x/Library/Application Support/com.cxtasks.app/claude-sessions/abc/launch.sh",
            "/tmp/no-spaces/launch.sh",
            "/tmp/it's got a quote/launch.sh",
        ] {
            let command = ghostty_command(Path::new(path));
            // Exactly how Ghostty runs it: through bash, which splits.
            let out = Command::new("/bin/bash")
                .arg("--noprofile")
                .arg("--norc")
                .arg("-c")
                .arg(format!("printf '%s\\n' {command}"))
                .output()
                .expect("run bash");
            assert!(out.status.success(), "bash rejected {command}");
            let words: Vec<_> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_string)
                .collect();
            assert_eq!(
                words,
                vec![path.to_string()],
                "{path:?} did not survive the shell as a single argument"
            );
        }
    }
}
