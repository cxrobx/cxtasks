//! The Obsidian vault's colours, taken from Onyx.
//!
//! Ported from cxmail's `commands/vault_look.rs`, which is the reference —
//! only the HTTP client differs (ureq, which cxtasks already builds, in place
//! of reqwest).
//!
//! CXTasks never reads the vault itself. Onyx's Obsidian plugin measures the
//! styles Obsidian has actually applied (so a community theme's CSS is never
//! parsed anywhere), and the Onyx server turns those measurements into a small
//! palette served at `GET /api/vault-look`. Meeting Copilot and cxmail wear the
//! same palette the same way; this is another consumer, not another derivation.
//!
//! This module is the trust boundary, so it does the whole of the validation:
//!
//! * **Only a loopback address is fetched.** The address is a user setting that
//!   arrives over IPC, and an app that fetches whatever URL its
//!   webview names is a request forger with a mailbox attached.
//! * **Nothing Onyx sends reaches the page as text.** `/api/vault-look` returns a
//!   built stylesheet, not a token map, so the `:root.vault-look{…}` block is read
//!   back out — and every value must be exactly three integers 0–255, parsed into
//!   `[u8; 3]`. A vault theme can change CXTasks' colours; it can never smuggle in
//!   a rule, a `url()` or a font, because the type that crosses IPC cannot hold one.
//!
//! Losing the palette is not an error: Onyx stopped, Obsidian never synced, or
//! Onyx's own "Match vault appearance" switch off all come back as `None`, and
//! the frontend keeps wearing the last palette it had (persisted with its other
//! settings) or falls back to the built-in theme.

use std::time::Duration;

use serde::Serialize;

use crate::error::AppError;

/// Short on purpose: this is polled on window focus and once a minute, and a
/// stopped or hung Onyx must cost nothing a person could notice.
const FETCH_TIMEOUT: Duration = Duration::from_millis(1500);

/// The block Onyx wears the palette on (`vault_look.stylesheet`).
const BLOCK_OPEN: &str = ":root.vault-look{";

/// A validated vault palette. Every colour is Onyx's token of the same name.
///
/// `secondary`/`muted`/`faint` are Onyx's text shades, mixed for an OPAQUE
/// ground; the frontend re-mixes the light ones for the glass (`vaultLook.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultPalette {
    /// `"light"` or `"dark"` — already resolved by Onyx when the vault follows
    /// the system appearance.
    pub mode: String,
    /// Onyx's hash of the palette; changes whenever any colour does.
    pub revision: String,
    pub bg_primary: [u8; 3],
    pub bg_sidebar: [u8; 3],
    pub bg_surface: [u8; 3],
    pub bg_elevated: [u8; 3],
    pub bg_input: [u8; 3],
    pub ink: [u8; 3],
    pub secondary: [u8; 3],
    pub muted: [u8; 3],
    pub faint: [u8; 3],
    pub accent: [u8; 3],
    pub accent_hover: [u8; 3],
}

/// Ask Onyx for the vault palette.
///
/// `Err` only for an address that is not allowed; every network outcome —
/// refused, timed out, not JSON, no palette — is `Ok(None)`.
#[tauri::command]
pub async fn fetch_vault_look(url: String) -> Result<Option<VaultPalette>, AppError> {
    let endpoint = vault_look_endpoint(&url)?;
    // ureq is blocking; a stopped Onyx costs at most FETCH_TIMEOUT, which must
    // not be spent on a Tokio worker.
    tauri::async_runtime::spawn_blocking(move || fetch_blocking(endpoint))
        .await
        .map_err(|e| AppError::General(format!("vault look: fetch task failed: {e}")))
}

/// Every network outcome — refused, timed out, a non-2xx, not JSON — is `None`.
fn fetch_blocking(endpoint: url::Url) -> Option<VaultPalette> {
    let agent = ureq::AgentBuilder::new()
        .timeout(FETCH_TIMEOUT)
        .timeout_connect(FETCH_TIMEOUT)
        // Onyx only ever answers on loopback, and the check above holds only for
        // the address we were given — a redirect would walk straight past it.
        .redirects(0)
        .build();
    let response = match agent.get(endpoint.as_str()).set("accept", "application/json").call() {
        // With redirects off, a 3xx arrives here as Ok — only 2xx is an answer.
        Ok(r) if (200..300).contains(&r.status()) => r,
        Ok(r) => {
            log::debug!("vault look: Onyx answered {}", r.status());
            return None;
        }
        Err(e) => {
            // The common case when Onyx is not running. Debug, not warn: this is
            // polled, and a stopped Onyx is a normal state, not a fault.
            log::debug!("vault look: Onyx unreachable: {e}");
            return None;
        }
    };
    let body: serde_json::Value = match response.into_string().map(|s| serde_json::from_str(&s)) {
        Ok(Ok(v)) => v,
        _ => {
            log::debug!("vault look: response was not JSON");
            return None;
        }
    };
    parse_vault_look(&body)
}

/// The `/api/vault-look` URL for a user-supplied Onyx address, or a refusal.
///
/// Loopback only, http(s) only, no credentials. `localhost` is accepted by name
/// because it is what a person types; it resolves to loopback on every Mac.
fn vault_look_endpoint(address: &str) -> Result<url::Url, AppError> {
    let refuse = |why: &str| {
        AppError::General(format!(
            "Onyx address {address:?} refused: {why}. Use a local address such as http://127.0.0.1:8899."
        ))
    };
    let parsed = url::Url::parse(address.trim()).map_err(|_| refuse("not a URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(refuse("only http and https"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(refuse("no credentials in the address"));
    }
    let loopback = match parsed.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    if !loopback {
        return Err(refuse("it must be this Mac (127.0.0.1, ::1 or localhost)"));
    }
    parsed.join("/api/vault-look").map_err(|_| refuse("not a URL"))
}

/// Read a validated palette out of an `/api/vault-look` payload.
///
/// Any missing or malformed colour means no palette at all — the same answer as
/// Onyx being down — rather than a partial one that would mix the vault's
/// colours with CXTasks'.
fn parse_vault_look(body: &serde_json::Value) -> Option<VaultPalette> {
    if !body.get("ok")?.as_bool()? || !body.get("available")?.as_bool()? {
        return None;
    }
    let mode = body.get("mode")?.as_str()?;
    if mode != "light" && mode != "dark" {
        return None;
    }
    let css = body.get("css")?.as_str()?;
    let start = css.find(BLOCK_OPEN)? + BLOCK_OPEN.len();
    let block = &css[start..start + css[start..].find('}')?];

    let token = |name: &str| -> Option<[u8; 3]> {
        let wanted = format!("--{name}");
        block.split(';').find_map(|decl| {
            let (key, value) = decl.split_once(':')?;
            if key.trim() == wanted {
                parse_triplet(value)
            } else {
                None
            }
        })
    };

    let revision = body
        .get("revision")
        .and_then(|v| v.as_str())
        .filter(|r| r.len() <= 64 && r.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or("")
        .to_string();

    Some(VaultPalette {
        mode: mode.to_string(),
        revision,
        bg_primary: token("bg-primary")?,
        bg_sidebar: token("bg-sidebar")?,
        bg_surface: token("bg-surface")?,
        bg_elevated: token("bg-elevated")?,
        bg_input: token("bg-input")?,
        ink: token("ink")?,
        secondary: token("secondary")?,
        muted: token("muted")?,
        faint: token("faint")?,
        accent: token("accent")?,
        accent_hover: token("accent-hover")?,
    })
}

/// `"253 246 227"` → `[253, 246, 227]`. Exactly three integers, each 0–255,
/// separated by single spaces — the only shape Onyx emits (`_triplet`).
fn parse_triplet(value: &str) -> Option<[u8; 3]> {
    let mut parts = value.trim().split(' ');
    let mut out = [0u8; 3];
    for slot in &mut out {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The shape Onyx served on 2026-09-25 (AnuPpuccin, light), trimmed of the
    /// sidebar rule and the font list.
    fn payload(css: &str) -> serde_json::Value {
        json!({
            "ok": true, "enabled": true, "available": true, "mode": "light",
            "base": [253, 246, 227], "revision": "469ad111e7f682141d2c", "css": css,
        })
    }

    const CSS: &str = ":root.vault-look{--bg-primary:253 246 227;--bg-sidebar:253 246 227;\
        --bg-surface:241 234 210;--bg-elevated:253 246 227;--bg-input:244 237 214;--ink:0 43 54;\
        --secondary:68 98 101;--muted:121 140 137;--faint:164 175 166;--line:rgb(0 43 54/.14);\
        --accent:203 75 22;--accent-hover:152 67 30;--ui-font:\"JetBrains Mono\", Inter;color-scheme:light}\n\
        :root.vault-look body.obsidian-tree aside{--bg-primary:0 0 0}";

    #[test]
    fn reads_the_palette_onyx_actually_serves() {
        let p = parse_vault_look(&payload(CSS)).expect("palette");
        assert_eq!(p.mode, "light");
        assert_eq!(p.revision, "469ad111e7f682141d2c");
        assert_eq!(p.bg_primary, [253, 246, 227]);
        assert_eq!(p.ink, [0, 43, 54]);
        assert_eq!(p.muted, [121, 140, 137]);
        assert_eq!(p.accent_hover, [152, 67, 30]);
    }

    #[test]
    fn reads_only_the_first_block() {
        // The sidebar rule re-declares the tokens; only the root block counts.
        let p = parse_vault_look(&payload(CSS)).unwrap();
        assert_ne!(p.bg_primary, [0, 0, 0]);
    }

    #[test]
    fn a_missing_or_malformed_colour_means_no_palette() {
        for broken in [
            CSS.replace("--ink:0 43 54;", ""),
            CSS.replace("--ink:0 43 54", "--ink:0 43 256"),
            CSS.replace("--ink:0 43 54", "--ink:0 43"),
            CSS.replace("--ink:0 43 54", "--ink:0 43 54 1"),
            CSS.replace("--ink:0 43 54", "--ink:rgb(0 43 54)"),
            CSS.replace("--ink:0 43 54", "--ink:0  43 54"),
            CSS.replace("--ink:0 43 54", "--ink:-1 43 54"),
        ] {
            assert_eq!(parse_vault_look(&payload(&broken)), None, "{broken}");
        }
    }

    #[test]
    fn an_injection_attempt_is_not_a_colour() {
        // A value that tries to close the declaration and open a rule. It never
        // parses as a triplet, so the whole palette is refused.
        let hostile = CSS.replace(
            "--bg-primary:253 246 227",
            "--bg-primary:253 246 227}body{background:url(https://evil.example/x)",
        );
        assert_eq!(parse_vault_look(&payload(&hostile)), None);
    }

    #[test]
    fn follows_onyxs_own_switch_and_modes() {
        let mut off = payload(CSS);
        off["available"] = json!(false);
        assert_eq!(parse_vault_look(&off), None);

        // Onyx's "Match vault appearance" off: available, but nothing worn.
        let mut unworn = payload("");
        unworn["mode"] = serde_json::Value::Null;
        assert_eq!(parse_vault_look(&unworn), None);

        let mut odd = payload(CSS);
        odd["mode"] = json!("sepia");
        assert_eq!(parse_vault_look(&odd), None);
    }

    #[test]
    fn a_bad_revision_is_dropped_not_fatal() {
        let mut body = payload(CSS);
        body["revision"] = json!("<script>");
        assert_eq!(parse_vault_look(&body).unwrap().revision, "");
    }

    #[test]
    fn only_loopback_addresses_are_fetched() {
        for ok in [
            "http://127.0.0.1:8899",
            "http://localhost:8899/",
            "http://[::1]:8899",
            "https://127.0.0.1:9000",
        ] {
            let url = vault_look_endpoint(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
            assert_eq!(url.path(), "/api/vault-look", "{ok}");
        }
        for bad in [
            "http://192.0.2.10:8899",
            "http://example.com",
            "http://127.0.0.1.example.com",
            "http://user:pw@127.0.0.1:8899",
            "file:///etc/passwd",
            "ftp://127.0.0.1",
            "127.0.0.1:8899",
            "",
        ] {
            assert!(vault_look_endpoint(bad).is_err(), "{bad} should be refused");
        }
    }

    /// Against the Onyx on this Mac — `cargo test -p cxtasks vault_look -- --ignored`.
    /// Ignored because it needs Onyx running; it is the only test that drives
    /// the real HTTP path and Onyx's real payload together.
    #[tokio::test]
    #[ignore]
    async fn live_onyx_serves_a_palette() {
        let palette = fetch_vault_look("http://127.0.0.1:8899".into())
            .await
            .expect("loopback address is allowed")
            .expect("Onyx running with a synced vault");
        assert!(palette.mode == "light" || palette.mode == "dark");
        assert!(!palette.revision.is_empty());
        println!("{palette:?}");
    }

    #[tokio::test]
    async fn an_address_with_nothing_listening_is_none_not_an_error() {
        // Port 9 (discard) is never served on a Mac; refused within the timeout.
        let got = fetch_vault_look("http://127.0.0.1:9".into()).await;
        assert!(matches!(got, Ok(None)), "{got:?}");
    }

    #[test]
    fn the_endpoint_replaces_any_path_given() {
        // A pasted Onyx page URL still asks the API, not that page.
        let url = vault_look_endpoint("http://127.0.0.1:8899/reader?x=1").unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:8899/api/vault-look");
    }
}
