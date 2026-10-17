//! Everything the server needs to know, read once at startup.
//!
//! Config-driven rather than compiled-in, because "canonical" is a ROLE and not
//! a machine: the same binary has to run on a Linux box today and a Mac mini
//! later without a rebuild.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

pub struct Config {
    pub bind: String,
    pub token_full: String,
    pub token_board: Option<String>,
    pub tz: String,
    /// Declared here rather than discovered, so a container can be the canonical
    /// node by configuration instead of by somebody editing `app_meta` with a
    /// throwaway sqlite container. Absent means local.
    pub canonical: bool,
}

/// Where to listen. Defaults to loopback ON PURPOSE.
///
/// A default of `0.0.0.0` would put an endpoint that can start write-enabled
/// agents across 25 repos on every interface the moment someone forgets to set
/// this. Binding wide has to be a decision somebody typed.
const DEFAULT_BIND: &str = "127.0.0.1:8787";

impl Config {
    pub fn from_env() -> Result<Self> {
        // TZ is REQUIRED and deliberately not defaulted. `db::tasks` computes
        // "today" against the local day boundary, so an unset TZ does not fail
        // — it silently changes which tasks are due, and the symptom is a board
        // that disagrees with the Mac by a few hours at the edges. Refusing to
        // start is the only honest option.
        let tz = std::env::var("TZ").ok().filter(|v| !v.is_empty()).context(
            "TZ must be set explicitly — it decides what \"today\" means, and \
             guessing it silently changes which tasks are due",
        )?;

        let bind = std::env::var("CXTASKS_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());

        let token_file = std::env::var("CXTASKS_TOKEN_FILE")
            .context("CXTASKS_TOKEN_FILE must point at a file holding the bearer token")?;
        let token_full = read_token(&PathBuf::from(&token_file))?;

        let token_board = match std::env::var("CXTASKS_BOARD_TOKEN_FILE") {
            Ok(p) if !p.is_empty() => Some(read_token(&PathBuf::from(p))?),
            _ => None,
        };

        // `CXTASKS_ROLE=canonical`, anything else (or absent) is local. A typo
        // is refused rather than silently read as local: "canonicl" would
        // otherwise produce a node that serves nothing and says nothing about
        // why, which is the worst way to spend an afternoon.
        let canonical = match std::env::var("CXTASKS_ROLE").ok().as_deref() {
            None | Some("") | Some("local") => false,
            Some("canonical") => true,
            Some(other) => bail!(
                "CXTASKS_ROLE is '{other}' — it must be 'canonical' or 'local' (or unset)"
            ),
        };

        Ok(Self { bind, token_full, token_board, tz, canonical })
    }
}

/// Read a token, refusing anything that would make it not a secret.
///
/// The token goes in a FILE and never in `app_meta`: a secret in the database
/// ends up in every `.backup`, in every debugging `SELECT *`, and — once sync
/// exists — replicated to every peer that pulls a snapshot.
fn read_token(path: &PathBuf) -> Result<String> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("could not read token file {}", path.display()))?;
    let token = raw.trim().to_string();
    if token.len() < 32 {
        bail!(
            "the token in {} is {} characters — too short to be worth having. \
             This endpoint can start write-enabled agents across every repo on \
             this machine; generate one with `openssl rand -hex 32`.",
            path.display(),
            token.len()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            bail!(
                "token file {} is mode {:o} — readable by somebody other than you. \
                 `chmod 600` it.",
                path.display(),
                mode
            );
        }
    }
    Ok(token)
}

impl Config {
    /// The timezone this process is running under, for the health handshake.
    pub fn config_tz(&self) -> &str {
        &self.tz
    }
}
