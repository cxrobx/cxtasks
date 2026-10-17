//! The host registry — who is out there and what may they do.
//!
//! A row per machine, written by that machine and read by all of them (the
//! table is `Synced`, see [`crate::sync::tables`]). Every host writes only its
//! OWN row, which is what makes whole-row merge safe here with no merge policy
//! at all: two hosts cannot produce two versions of one row, so there is never
//! anything to resolve.
//!
//! # The declared half is never overwritten by a restart
//!
//! [`register_self`] inserts a row if there is none and otherwise touches only
//! the *observed* columns. That asymmetry is the whole design. Policy —
//! `max_tier`, `enabled`, `headroom_pct` — is a decision Chris made in the app;
//! a container that restarts twice a day must not quietly reset it to whatever
//! its environment happens to say. What a restart legitimately refreshes is
//! what it just learned by looking: is Claude installed, is the switch on, when
//! did I last breathe.
//!
//! The one exception is [`adopt_env_policy`], which exists so a *new* host can
//! be declared entirely from its deployment rather than needing a hand edit
//! before it is useful — and it runs only on insert, never on update.
//!
//! # Lists are comma-separated TEXT
//!
//! `executors`, `workspaces`, `executors_ready` and `mirrors` hold at most a
//! handful of short tokens each. A join table would be four more tables to
//! classify for sync, sweep and purge, in exchange for a normalization nobody
//! queries across. A comma string is also readable from the `sqlite3` CLI,
//! which is where this table gets inspected when a host is behaving oddly.

use rusqlite::{params, Connection, Row};

use crate::error::AppError;
use crate::executor::BackgroundExecutor;
use crate::routing::{HostCapability, WorkspaceKind};
use crate::runner::ToolTier;

/// A host that has not reported in this long is not worth waiting for.
///
/// Ten minutes rather than one: a host is *deferred to* while it is alive, so a
/// cutoff that is too tight makes work bounce to a worse host during an
/// ordinary network hiccup, and a cutoff that is too loose only delays it. The
/// asymmetry says be generous.
pub const HOST_STALE_MINUTES: i64 = 10;

fn csv(values: &[String]) -> String {
    values.join(",")
}

fn split(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse a list of executors, dropping anything unrecognized.
///
/// Lenient in the same direction and for the same reason as
/// [`BackgroundExecutor::from_db`]: a host row written by a newer binary that
/// declares `gemini` must not make `list` return an error and blank the whole
/// hosts pane. Dropping the unknown entry leaves that host *less* capable than
/// it claims, which is the safe way to be wrong.
fn executors(raw: &str) -> Vec<BackgroundExecutor> {
    split(raw)
        .iter()
        .filter_map(|s| BackgroundExecutor::parse(s).ok())
        .collect()
}

fn workspaces(raw: &str) -> Vec<WorkspaceKind> {
    split(raw)
        .iter()
        .filter_map(|s| WorkspaceKind::parse(s).ok())
        .collect()
}

const COLUMNS: &str = "id, label, max_tier, executors, workspaces, can_checkpoint, \
     max_concurrent, headroom_pct, priority, enabled, last_seen_at, paused, gate, \
     executors_ready, mirrors";

fn row_to_host(row: &Row) -> rusqlite::Result<HostCapability> {
    Ok(HostCapability {
        id: row.get(0)?,
        label: row.get(1)?,
        // Lenient for the same reason as the executor list: an unknown tier
        // must not blank the pane. It falls to the LEAST capable value, so a
        // host running some future tier this binary cannot read is treated as
        // read-only rather than as unrestricted.
        max_tier: ToolTier::parse(&row.get::<_, String>(2)?).unwrap_or(ToolTier::ReadOnly),
        executors: executors(&row.get::<_, String>(3)?),
        workspaces: workspaces(&row.get::<_, String>(4)?),
        can_checkpoint: row.get::<_, i64>(5)? != 0,
        max_concurrent: row.get(6)?,
        headroom_pct: row.get(7)?,
        priority: row.get(8)?,
        enabled: row.get::<_, i64>(9)? != 0,
        last_seen_at: row.get(10)?,
        paused: row.get::<_, i64>(11)? != 0,
        gate: row.get(12)?,
        executors_ready: executors(&row.get::<_, String>(13)?),
    })
}

/// Every registered host, best first — enabled, then by priority.
pub fn list(conn: &Connection) -> Result<Vec<HostCapability>, AppError> {
    let sql = format!("SELECT {COLUMNS} FROM hosts ORDER BY enabled DESC, priority ASC, label ASC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], row_to_host)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<HostCapability>, AppError> {
    let sql = format!("SELECT {COLUMNS} FROM hosts WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![id], row_to_host)?;
    Ok(match rows.next() {
        Some(r) => Some(r?),
        None => None,
    })
}

/// Which repos this host holds a mirror of.
///
/// # The one field a host does not write about itself
///
/// Everything else on this row is written by the machine it describes, which is
/// what makes whole-row merge free of contention. `mirrors` is written by
/// whichever host PUSHED the mirror — and that is safe for a concrete reason,
/// not by luck: a repo lives on exactly one machine, so there is exactly one
/// candidate pusher for it, and today only the Mac has live trees at all.
///
/// It cannot be the holder's job. A bare repo is named by `repo_key`, which is
/// a hash of the source path — so the NAS can enumerate its mirrors and still
/// have no idea that `web-a1b2c3d4.git` is `~/Projects/artisttracker/web`. The
/// side that knows is the side that made it.
///
/// Separate from [`get`] because [`crate::routing::satisfies`] takes it
/// separately: it changes on a different clock from the capability, and is
/// meaningless for a local host.
pub fn mirrors(conn: &Connection, id: &str) -> Result<Vec<String>, AppError> {
    let raw: Option<String> = conn
        .query_row("SELECT mirrors FROM hosts WHERE id = ?1", params![id], |r| {
            r.get(0)
        })
        .ok();
    Ok(raw.map(|r| split(&r)).unwrap_or_default())
}

/// This machine's row, creating it from `default_cap` if it has never
/// registered.
///
/// **On conflict this updates NOTHING.** See the module docs: a restart may
/// refresh what it observed, never what Chris declared. [`heartbeat`] is the
/// call that writes the observed half, and it is a separate statement so that
/// the distinction is visible at the call site rather than buried in an
/// `ON CONFLICT` clause nobody reads.
pub fn register_self(conn: &Connection, default_cap: &HostCapability) -> Result<(), AppError> {
    conn.execute(
        "INSERT INTO hosts (id, label, max_tier, executors, workspaces, can_checkpoint,
                            max_concurrent, headroom_pct, priority, enabled)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO NOTHING",
        params![
            default_cap.id,
            default_cap.label,
            default_cap.max_tier.as_str(),
            csv(&default_cap
                .executors
                .iter()
                .map(|e| e.as_str().to_string())
                .collect::<Vec<_>>()),
            csv(&default_cap
                .workspaces
                .iter()
                .map(|w| w.as_str().to_string())
                .collect::<Vec<_>>()),
            i64::from(default_cap.can_checkpoint),
            default_cap.max_concurrent,
            default_cap.headroom_pct,
            default_cap.priority,
            i64::from(default_cap.enabled),
        ],
    )?;
    Ok(())
}

/// The observed half: what this host just learned by looking at itself.
///
/// `updated_at` moves, which is what puts the row on the wire (the sweep
/// watermarks this table on `updated_at`). That is also why the caller beats
/// once a minute rather than on its 3-second control loop — see
/// [`crate::sync::sweep::SWEEP`].
///
/// Note `mirrors` is NOT here. A host cannot observe its own mirror set in any
/// useful form — see [`mirrors`] — so writing it from a heartbeat would blank
/// what the pusher recorded, every minute, forever.
pub fn heartbeat(
    conn: &Connection,
    id: &str,
    paused: bool,
    gate: Option<&str>,
    executors_ready: &[BackgroundExecutor],
) -> Result<(), AppError> {
    conn.execute(
        "UPDATE hosts
            SET last_seen_at = datetime('now'),
                paused = ?2,
                gate = ?3,
                executors_ready = ?4,
                updated_at = datetime('now')
          WHERE id = ?1",
        params![
            id,
            i64::from(paused),
            gate,
            csv(&executors_ready
                .iter()
                .map(|e| e.as_str().to_string())
                .collect::<Vec<_>>()),
        ],
    )?;
    Ok(())
}

/// Record that `host_id` now holds a mirror of `repo_path`.
///
/// Called by the pusher after a successful push, and idempotent — the common
/// case is re-pushing a repo that is already mirrored, twenty times an hour.
pub fn record_mirror(conn: &Connection, host_id: &str, repo_path: &str) -> Result<(), AppError> {
    let mut held = mirrors(conn, host_id)?;
    if held.iter().any(|m| m == repo_path) {
        return Ok(());
    }
    held.push(repo_path.to_string());
    conn.execute(
        "UPDATE hosts SET mirrors = ?2, updated_at = datetime('now') WHERE id = ?1",
        params![host_id, csv(&held)],
    )?;
    Ok(())
}

/// Where this host's mirrors are pushed — an ssh destination plus a base path,
/// e.g. `nas-ts:/volume1/cxtasks/mirrors`. `None` means "do not mirror to it",
/// which is what every host says until somebody sets one.
pub fn mirror_remote(conn: &Connection, host_id: &str) -> Result<Option<String>, AppError> {
    let raw: Option<Option<String>> = conn
        .query_row(
            "SELECT mirror_remote FROM hosts WHERE id = ?1",
            params![host_id],
            |r| r.get(0),
        )
        .ok();
    Ok(raw
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty()))
}

pub fn set_mirror_remote(
    conn: &Connection,
    host_id: &str,
    remote: Option<&str>,
) -> Result<(), AppError> {
    conn.execute(
        "UPDATE hosts SET mirror_remote = ?2, updated_at = datetime('now') WHERE id = ?1",
        params![host_id, remote.map(str::trim).filter(|s| !s.is_empty())],
    )?;
    Ok(())
}

/// What a person may change about a host from the app.
///
/// Only the declared half is here on purpose. There is no way to hand-edit
/// `last_seen_at` or `executors_ready`, because they are not opinions — a
/// settable "Claude is installed" checkbox would be a lie waiting to be
/// believed by [`crate::routing::satisfies`].
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct HostPolicy {
    pub label: Option<String>,
    pub max_tier: Option<ToolTier>,
    pub executors: Option<Vec<BackgroundExecutor>>,
    pub workspaces: Option<Vec<WorkspaceKind>>,
    pub can_checkpoint: Option<bool>,
    pub max_concurrent: Option<i64>,
    pub headroom_pct: Option<i64>,
    pub priority: Option<i64>,
    pub enabled: Option<bool>,
}

pub fn set_policy(conn: &Connection, id: &str, policy: &HostPolicy) -> Result<(), AppError> {
    let mut sets: Vec<&str> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(v) = &policy.label {
        sets.push("label = ?");
        binds.push(Box::new(v.clone()));
    }
    if let Some(v) = policy.max_tier {
        sets.push("max_tier = ?");
        binds.push(Box::new(v.as_str()));
    }
    if let Some(v) = &policy.executors {
        sets.push("executors = ?");
        binds.push(Box::new(csv(
            &v.iter().map(|e| e.as_str().to_string()).collect::<Vec<_>>()
        )));
    }
    if let Some(v) = &policy.workspaces {
        sets.push("workspaces = ?");
        binds.push(Box::new(csv(
            &v.iter().map(|w| w.as_str().to_string()).collect::<Vec<_>>()
        )));
    }
    if let Some(v) = policy.can_checkpoint {
        sets.push("can_checkpoint = ?");
        binds.push(Box::new(i64::from(v)));
    }
    if let Some(v) = policy.max_concurrent {
        sets.push("max_concurrent = ?");
        binds.push(Box::new(v));
    }
    if let Some(v) = policy.headroom_pct {
        sets.push("headroom_pct = ?");
        binds.push(Box::new(v));
    }
    if let Some(v) = policy.priority {
        sets.push("priority = ?");
        binds.push(Box::new(v));
    }
    if let Some(v) = policy.enabled {
        sets.push("enabled = ?");
        binds.push(Box::new(i64::from(v)));
    }
    if sets.is_empty() {
        return Ok(());
    }
    sets.push("updated_at = datetime('now')");
    binds.push(Box::new(id.to_string()));

    let sql = format!("UPDATE hosts SET {} WHERE id = ?", sets.join(", "));
    let refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    conn.execute(&sql, refs.as_slice())?;
    Ok(())
}

/// Forget a host.
///
/// Not a soft delete: a host that is gone should stop being offered work
/// immediately, and there is nothing on the row worth keeping — every fact it
/// held is either policy (re-declarable) or observation (re-observed on the
/// next start). Runs it executed keep their `task_runs.host` string, which is
/// what preserves the history this row is not carrying.
pub fn remove(conn: &Connection, id: &str) -> Result<bool, AppError> {
    let n = conn.execute("DELETE FROM hosts WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

/// This host's capability, registering the local default first if it is new.
///
/// The runner and the app both call this on every start, so a machine appears
/// in the registry by virtue of running the software rather than by anybody
/// remembering to add it.
pub fn local(conn: &Connection, default_cap: &HostCapability) -> Result<HostCapability, AppError> {
    register_self(conn, default_cap)?;
    get(conn, &default_cap.id)?.ok_or_else(|| {
        AppError::General(format!(
            "host {} was registered and then could not be read back",
            default_cap.id
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;

    fn cap() -> HostCapability {
        HostCapability::local_default("mac-1".into(), "this Mac".into())
    }

    #[test]
    fn a_host_registers_itself_and_reads_back_what_it_declared() {
        let conn = test_conn();
        let got = local(&conn, &cap()).unwrap();
        assert_eq!(got.id, "mac-1");
        assert_eq!(got.max_tier, ToolTier::Write);
        assert_eq!(got.workspaces, vec![WorkspaceKind::Live]);
        assert!(got.can_checkpoint);
    }

    /// **The restart rule.** A container comes back twice a day; policy set in
    /// the app must survive that. If this ever fails, a NAS someone downgraded
    /// to read-only is silently back to whatever its image declares.
    #[test]
    fn re_registering_never_overwrites_declared_policy() {
        let conn = test_conn();
        local(&conn, &cap()).unwrap();
        set_policy(
            &conn,
            "mac-1",
            &HostPolicy {
                max_tier: Some(ToolTier::ReadOnly),
                enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();

        // ...and now the process restarts, declaring its defaults again.
        local(&conn, &cap()).unwrap();

        let got = get(&conn, "mac-1").unwrap().unwrap();
        assert_eq!(
            got.max_tier,
            ToolTier::ReadOnly,
            "a restart re-asserted a policy the user had changed"
        );
        assert!(!got.enabled, "a restart re-enabled a host the user switched off");
    }

    #[test]
    fn a_heartbeat_writes_only_what_the_host_observed() {
        let conn = test_conn();
        local(&conn, &cap()).unwrap();
        set_policy(
            &conn,
            "mac-1",
            &HostPolicy {
                max_tier: Some(ToolTier::Shell),
                ..Default::default()
            },
        )
        .unwrap();

        heartbeat(&conn, "mac-1", true, Some("Paused"), &[BackgroundExecutor::Claude]).unwrap();
        record_mirror(&conn, "mac-1", "/repo/a").unwrap();

        let got = get(&conn, "mac-1").unwrap().unwrap();
        assert!(got.paused);
        assert_eq!(got.gate.as_deref(), Some("Paused"));
        assert_eq!(got.executors_ready, vec![BackgroundExecutor::Claude]);
        assert!(got.last_seen_at.is_some());
        assert_eq!(
            got.max_tier,
            ToolTier::Shell,
            "the heartbeat trod on the declared half"
        );
        assert_eq!(mirrors(&conn, "mac-1").unwrap(), vec!["/repo/a".to_string()]);
    }

    /// Effective capability is declared ∩ observed. A host that declares Codex
    /// but reports it missing must not be offered Codex work — and the
    /// registry has to be able to REPRESENT that gap, which is the thing a
    /// single `executors` column could not.
    #[test]
    fn declared_and_observed_executors_are_stored_separately() {
        let conn = test_conn();
        local(&conn, &cap()).unwrap();
        heartbeat(&conn, "mac-1", false, None, &[BackgroundExecutor::Claude]).unwrap();

        let got = get(&conn, "mac-1").unwrap().unwrap();
        assert_eq!(got.executors, BackgroundExecutor::ALL.to_vec());
        assert_eq!(got.executors_ready, vec![BackgroundExecutor::Claude]);
        assert_eq!(got.effective_executors(), vec![BackgroundExecutor::Claude]);
    }

    /// A row from a newer binary must not blank the pane — and must land LESS
    /// capable than it claims, never more.
    #[test]
    fn an_unreadable_tier_or_executor_degrades_rather_than_failing_the_read() {
        let conn = test_conn();
        local(&conn, &cap()).unwrap();
        conn.execute(
            "UPDATE hosts SET max_tier = 'omniscient', executors = 'claude,gemini' WHERE id = ?1",
            params!["mac-1"],
        )
        .unwrap();

        let got = get(&conn, "mac-1").unwrap().unwrap();
        assert_eq!(
            got.max_tier,
            ToolTier::ReadOnly,
            "an unknown tier must fall to the least capable value, not the most"
        );
        assert_eq!(got.executors, vec![BackgroundExecutor::Claude]);
    }

    #[test]
    fn removing_a_host_takes_it_out_of_the_list() {
        let conn = test_conn();
        local(&conn, &cap()).unwrap();
        assert_eq!(list(&conn).unwrap().len(), 1);
        assert!(remove(&conn, "mac-1").unwrap());
        assert!(list(&conn).unwrap().is_empty());
        assert!(!remove(&conn, "mac-1").unwrap());
    }

    #[test]
    fn hosts_are_listed_enabled_first_then_by_priority() {
        let conn = test_conn();
        for (id, prio, enabled) in [("a", 50, true), ("b", 10, true), ("c", 1, false)] {
            let mut c = HostCapability::local_default(id.into(), id.into());
            c.priority = prio;
            c.enabled = enabled;
            register_self(&conn, &c).unwrap();
        }
        let ids: Vec<String> = list(&conn).unwrap().into_iter().map(|h| h.id).collect();
        assert_eq!(ids, vec!["b", "a", "c"]);
    }
}
