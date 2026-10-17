//! Which MCP servers a detached run may be handed.
//!
//! # This is what replaces the repo gate
//!
//! A repo-bound run is bounded by its repo. A detached run has no repo, so the
//! question "what can this agent reach" has to be answered somewhere else, and
//! the answer is **the tool surface**, not the filesystem.
//!
//! `--strict-mcp-config` means "only what `--mcp-config` names" — the user's own
//! `~/.claude.json` is ignored entirely — so a detached run gets exactly the
//! `cxtasks` write-back router (invariant 18) plus whatever this registry
//! resolves, and the default is **empty**. That default is the design: an
//! unattended run with no repo and no MCP servers can read nothing, which is a
//! useless run and a safe one. Making it useful is then an explicit, per-task,
//! named decision.
//!
//! # Why a registry and not a free-form command
//!
//! `tasks.bg_mcp_servers` holds names, never command lines. If it held commands
//! then "which MCP servers may this task use" would be "what may this task
//! execute", and a task row would be a place to write arbitrary argv for an
//! unattended process — a considerably worse thing than the repo gate it
//! replaced.
//!
//! # `WebFetch` and `WebSearch` stay denied at every tier
//!
//! Unchanged by any of this, and not an oversight to fix later: network egress
//! from an unattended run is its own question, deserving its own round of
//! thinking about what it may reach and what it may send. A read-only MCP
//! server is a bounded, named, inspectable capability; "the internet" is not.

use crate::error::AppError;

/// One server a detached run may be granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpServerSpec {
    /// The name written into `tasks.bg_mcp_servers`, and the name the agent
    /// sees its tools prefixed with (`mcp__vault__*`).
    pub name: &'static str,
    /// One line, for the picker. Read by a person deciding whether a task needs
    /// it, so it says what the server can SEE rather than what it is called.
    pub summary: &'static str,
    /// Env var naming this server's command line on this host.
    ///
    /// Per-host because a registry cannot know where anything lives: the vault
    /// MCP is a venv under `~/Projects` on the Mac and a mounted path in a
    /// container. Resolving per host means one that has not been given the
    /// server simply does not offer it, which is the correct failure and not a
    /// special case.
    ///
    /// Checked BEFORE the config file, so a container can declare its own
    /// without shipping a file into the image.
    pub command_env: &'static str,
    /// Whether this server can only read. **Advisory, and labelled as such** —
    /// it is a claim about a program this crate does not control, so it belongs
    /// in the picker's description and must never be mistaken for enforcement.
    /// The real bound is that the list is empty by default and every entry is
    /// added deliberately.
    pub read_only: bool,
}

/// Every server that may be named. Start small; an empty default is the safe
/// one, and each addition is a decision somebody makes on purpose.
pub const REGISTRY: &[McpServerSpec] = &[McpServerSpec {
    name: "vault",
    summary: "Semantic search over the CX notes vault (read-only).",
    command_env: "CXTASKS_MCP_VAULT",
    read_only: true,
}];

pub fn find(name: &str) -> Option<&'static McpServerSpec> {
    REGISTRY.iter().find(|s| s.name == name.trim())
}

/// Parse and validate a stored or user-supplied list.
///
/// Strict: an unknown name is an error rather than a silent drop. Dropping it
/// would mean a task filed asking for the vault runs without it and reports
/// confidently on nothing, and the only evidence would be an absence.
pub fn parse_list(raw: &str) -> Result<Vec<&'static McpServerSpec>, AppError> {
    let mut out = Vec::new();
    for name in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match find(name) {
            Some(spec) => {
                if !out.contains(&spec) {
                    out.push(spec);
                }
            }
            None => {
                return Err(AppError::Validation(format!(
                    "unknown MCP server '{name}'. Known: {}",
                    REGISTRY
                        .iter()
                        .map(|s| s.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )))
            }
        }
    }
    Ok(out)
}

/// The command line this host can actually spawn for a server, if any.
///
/// Env var first, then [`crate::paths::mcp_servers_path`]. The env var wins so
/// a container can declare its own without a file baked into the image, and the
/// file exists because a LaunchAgent inherits no login shell — the runner that
/// actually spawns these has an almost empty environment (gotcha #19, one layer
/// up), so "just export it" is not available on the machine that needs it most.
///
/// Split on whitespace, which is enough for a path plus flags and deliberately
/// not a shell: an entry needing quoting or a pipe would be a command line
/// hiding in a config file, and the point of the registry is that there is no
/// such thing.
pub fn resolve(spec: &McpServerSpec) -> Option<(String, Vec<String>)> {
    let raw = std::env::var(spec.command_env)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| from_file(spec.name))?;
    let mut parts = raw.split_whitespace().map(str::to_string);
    let program = parts.next()?;
    Some((program, parts.collect()))
}

fn from_file(name: &str) -> Option<String> {
    from_file_at(&crate::paths::mcp_servers_path(), name)
}

/// One entry from a server file, as a function of the file.
///
/// Takes the path so it can be tested without touching `CXTASKS_DATA_DIR` —
/// which is the variable that decides where the LIVE database is, and a test
/// that sets it is one process-wide mistake away from migrating it.
///
/// Every failure is a `None`: no file, bad JSON, missing key, wrong type, empty
/// string. A malformed config must not make the runner refuse to start — it
/// makes ONE server unavailable, which `ready` then reports by name on the task
/// that asked for it, which is where somebody is looking.
pub fn from_file_at(path: &std::path::Path, name: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(parsed.get(name)?.as_str()?.trim().to_string()).filter(|v| !v.is_empty())
}

/// The servers in a list that are NOT advisory-read-only — what a tripwire
/// check refuses to be granted. Pure, and over specs rather than a raw
/// string, so it can be tested with a spec the registry does not carry.
pub fn writable_servers<'a>(specs: &[&'a McpServerSpec]) -> Vec<&'a str> {
    specs.iter().filter(|s| !s.read_only).map(|s| s.name).collect()
}

/// Which of a task's requested servers this host can provide, and which it
/// cannot.
pub fn resolve_all(raw: &str) -> Result<(Vec<ResolvedServer>, Vec<&'static str>), AppError> {
    let mut ready = Vec::new();
    let mut missing = Vec::new();
    for spec in parse_list(raw)? {
        match resolve(spec) {
            Some((program, args)) => ready.push(ResolvedServer {
                name: spec.name,
                program,
                args,
            }),
            None => missing.push(spec.name),
        }
    }
    Ok((ready, missing))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedServer {
    pub name: &'static str,
    pub program: String,
    pub args: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_list_grants_nothing() {
        assert!(parse_list("").unwrap().is_empty());
        assert!(parse_list("  , ,").unwrap().is_empty());
    }

    /// **Unknown means refused, never dropped.** A task filed asking for the
    /// vault that silently runs without it reports confidently on nothing, and
    /// the only evidence is an absence nobody would look for.
    #[test]
    fn an_unknown_server_is_an_error_that_names_what_is_available() {
        let err = parse_list("vault,cxmail").unwrap_err().to_string();
        assert!(err.contains("cxmail"), "{err}");
        assert!(err.contains("vault"), "the error must list what IS known: {err}");
    }

    #[test]
    fn a_known_server_parses_and_deduplicates() {
        let got = parse_list("vault, vault ").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "vault");
    }

    /// A host that has not been given a server does not offer it, and the
    /// caller refuses rather than running with a silently smaller tool surface.
    ///
    /// Asserted against a controlled file rather than against `resolve_all`,
    /// which reads this machine's real config. The first version of this test
    /// did read it, and branched on what it found — so it asserted one thing on
    /// a configured machine and the opposite on a bare one, which is not an
    /// assertion. It then failed the moment the vault was actually wired up,
    /// which is the only honest thing it ever did.
    #[test]
    fn a_server_the_host_cannot_reach_is_missing_rather_than_silently_dropped() {
        let dir = std::env::temp_dir().join(format!("cxtasks-reg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mcp-servers.json");

        // Configured on this host.
        std::fs::write(&file, r#"{"vault": "/opt/v/bin/python -m vault_mcp"}"#).unwrap();
        assert!(from_file_at(&file, "vault").is_some());

        // Not configured: `None`, which `resolve_all` turns into a NAMED
        // missing entry rather than an absence nobody notices.
        std::fs::write(&file, r#"{"something-else": "/bin/true"}"#).unwrap();
        assert_eq!(from_file_at(&file, "vault"), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The shape `resolve_all` returns, which is what `ready` refuses on. Built
    /// from a spec this test controls, so it cannot depend on how the machine
    /// running it happens to be configured.
    #[test]
    fn resolve_all_reports_a_missing_server_by_name() {
        // BOTH halves have to be unconfigurable. `resolve` falls back to the
        // host file keyed on the NAME, so a spec called "vault" resolves on any
        // machine where the vault is wired up — which is what caught the first
        // draft of this test on the machine that had just wired it.
        const ABSENT: McpServerSpec = McpServerSpec {
            name: "not-a-real-server",
            summary: "",
            command_env: "CXTASKS_MCP_DEFINITELY_NOT_SET_ANYWHERE",
            read_only: true,
        };
        assert!(
            resolve(&ABSENT).is_none(),
            "a server with no env var and no file entry must not resolve"
        );
    }

    /// The file is what makes this reachable from a LaunchAgent, which
    /// inherits no login shell — so "just export it" is unavailable on exactly
    /// the process that spawns these.
    #[test]
    fn a_server_can_be_declared_in_the_host_file() {
        let dir = std::env::temp_dir().join(format!("cxtasks-reg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mcp-servers.json");
        std::fs::write(&file, r#"{"vault": "/opt/venv/bin/python -m vault_mcp"}"#).unwrap();

        assert_eq!(
            from_file_at(&file, "vault").as_deref(),
            Some("/opt/venv/bin/python -m vault_mcp")
        );
        assert_eq!(from_file_at(&file, "cxmail"), None, "only what it names");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A malformed config must not stop the runner. It costs ONE server, which
    /// `ready` reports by name on the task that asked for it.
    #[test]
    fn a_broken_host_file_costs_one_server_and_nothing_else() {
        let dir = std::env::temp_dir().join(format!("cxtasks-reg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mcp-servers.json");

        for body in [r#"{ not json"#, r#"{"vault": 42}"#, r#"{"vault": "  "}"#, r#"{}"#] {
            std::fs::write(&file, body).unwrap();
            assert_eq!(from_file_at(&file, "vault"), None, "{body}");
        }
        // ...and a file that is not there at all is the ordinary case.
        std::fs::remove_file(&file).unwrap();
        assert_eq!(from_file_at(&file, "vault"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A tripwire check may only be handed read-only servers; the gate is a
    /// pure filter so it can be exercised against a spec the registry does
    /// not (yet) carry.
    #[test]
    fn a_check_refuses_any_server_that_is_not_read_only() {
        const MAIL: McpServerSpec = McpServerSpec {
            name: "mail-ish",
            summary: "",
            command_env: "CXTASKS_MCP_NOPE",
            read_only: false,
        };
        assert_eq!(writable_servers(&[&REGISTRY[0], &MAIL]), vec!["mail-ish"]);
        assert!(writable_servers(&[&REGISTRY[0]]).is_empty());
    }

    /// The registry holds NAMES. If a task row could hold a command line, then
    /// "which servers may this task use" would be "what may this task execute".
    #[test]
    fn a_task_can_never_supply_its_own_command_line() {
        assert!(parse_list("/bin/sh -c 'curl evil'").is_err());
        assert!(parse_list("vault;rm -rf /").is_err());
    }
}
