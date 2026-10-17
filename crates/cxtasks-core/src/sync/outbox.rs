//! What this host has done and not yet told the peer about.
//!
//! # Why intents rather than rows
//!
//! Pushing the VERB and its arguments means the peer applies the change through
//! `api::dispatch` — the same code path, with the same compositions. Completing
//! a recurring task materializes the next instance; changing a project prefills
//! `repo_path`. A row-level push would carry the completed task and quietly
//! lose the instance the peer never created.
//!
//! It also means there is no merge logic anywhere, which is the property worth
//! protecting: field-level merge manufactures rows `tasks::update` would refuse
//! to write, and nothing catches them because they were not written by
//! `update`.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;

/// One recorded verb, waiting to be pushed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Intent {
    pub local_seq: i64,
    pub cmd: String,
    pub args: Value,
    /// Which machine originated it. Not used for routing — the peer applies it
    /// either way — but it is what makes a replayed change attributable when
    /// two hosts disagree about a row.
    pub host: String,
    pub created_at: String,
}

/// What a peer did with a pushed batch.
///
/// Both lists, always. A count would make the sender choose between dropping
/// intents it is unsure of and re-sending ones that landed, and the first loses
/// work. Defined here rather than in the server so there is one definition of
/// the wire shape rather than one at each end.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PushResult {
    pub accepted: Vec<i64>,
    pub failed: Vec<PushFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushFailure {
    pub local_seq: i64,
    pub err: String,
}

/// Record a verb. **Must run inside the caller's transaction.**
///
/// Taking `&Connection` rather than opening its own is the point: the row must
/// commit with the change it describes, and a function that could open its own
/// transaction would make it possible to call this correctly-looking and
/// non-atomically.
pub fn append(conn: &Connection, cmd: &str, args: &Value, host: &str) -> Result<i64, AppError> {
    conn.execute(
        "INSERT INTO sync_outbox (cmd, args_json, host) VALUES (?1, ?2, ?3)",
        params![cmd, encode(args)?, host],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Mapped rather than `?`: `AppError` has no `From<serde_json::Error>`, and
/// adding one to a shared enum for a single call site is a wider change than
/// this needs.
fn encode(args: &Value) -> Result<String, AppError> {
    serde_json::to_string(args)
        .map_err(|e| AppError::General(format!("could not encode intent arguments: {e}")))
}

/// Everything waiting, oldest first.
///
/// `local_seq ASC` is the replay order and the only correct one: two edits to a
/// task must arrive in the order they happened, or the older one wins.
pub fn pending(conn: &Connection, limit: u32) -> Result<Vec<Intent>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT local_seq, cmd, args_json, host, created_at
           FROM sync_outbox ORDER BY local_seq ASC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |r| {
            let raw: String = r.get(2)?;
            Ok(Intent {
                local_seq: r.get(0)?,
                cmd: r.get(1)?,
                args: serde_json::from_str(&raw).unwrap_or(Value::Null),
                host: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn count(conn: &Connection) -> Result<i64, AppError> {
    Ok(conn.query_row("SELECT COUNT(*) FROM sync_outbox", [], |r| r.get(0))?)
}

/// Drop the intents the peer has accepted.
///
/// By EXPLICIT id list rather than `local_seq <= high`. A push can partially
/// succeed, and a range delete would silently discard the one that failed along
/// with the ones that landed — losing a change with nothing to show for it. The
/// caller passes exactly what was accepted.
pub fn clear(conn: &Connection, accepted: &[i64]) -> Result<usize, AppError> {
    let mut removed = 0;
    for seq in accepted {
        removed += conn.execute("DELETE FROM sync_outbox WHERE local_seq = ?1", params![seq])?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use serde_json::json;

    #[test]
    fn intents_come_back_in_the_order_they_were_recorded() {
        let conn = test_conn();
        for i in 0..5 {
            append(&conn, "update_task", &json!({ "n": i }), "mac").unwrap();
        }
        let got = pending(&conn, 100).unwrap();
        assert_eq!(got.len(), 5);
        assert_eq!(
            got.iter().map(|i| i.args["n"].as_i64().unwrap()).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4],
            "replay order is the order things happened"
        );
        assert!(got.windows(2).all(|w| w[0].local_seq < w[1].local_seq));
    }

    #[test]
    fn arguments_survive_the_round_trip_intact() {
        let conn = test_conn();
        let args = json!({ "id": "abc", "patch": { "title": "keep me", "due_at": null } });
        append(&conn, "update_task", &args, "mac").unwrap();
        let got = pending(&conn, 10).unwrap();
        assert_eq!(got[0].args, args, "an explicit null must survive as null");
        assert_eq!(got[0].cmd, "update_task");
        assert_eq!(got[0].host, "mac");
    }

    /// **A partial push must not lose the failure.**
    ///
    /// Clearing by range would delete the intent that failed along with the
    /// ones that landed, and the change it carried would be gone with nothing
    /// on screen. Clearing by explicit id leaves it queued for the next drain.
    #[test]
    fn clearing_removes_only_what_was_accepted() {
        let conn = test_conn();
        let a = append(&conn, "update_task", &json!({ "n": 1 }), "mac").unwrap();
        let _b = append(&conn, "update_task", &json!({ "n": 2 }), "mac").unwrap();
        let c = append(&conn, "update_task", &json!({ "n": 3 }), "mac").unwrap();

        // The middle one was refused; the other two landed.
        assert_eq!(clear(&conn, &[a, c]).unwrap(), 2);

        let left = pending(&conn, 10).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].args["n"], 2, "the failure stays queued, not discarded");
    }
}
