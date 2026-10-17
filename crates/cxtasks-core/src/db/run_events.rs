//! Normalized execution traces for background runs.
//!
//! These rows are deliberately small and safe to render. Provider-native JSON,
//! reasoning, tool output and arbitrary inputs stay out of the application DB:
//! they are both much larger and much more likely to contain sensitive data.

use crate::error::AppError;
use rusqlite::{params, Connection, Row};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEvent {
    pub id: i64,
    pub run_id: String,
    pub seq: i64,
    pub occurred_at: String,
    /// `tool` today; kept explicit so future runner notices do not need a
    /// destructive schema rewrite.
    pub kind: String,
    /// Claude's `tool_use_id` or Codex's item id. Optional for provider events
    /// that genuinely have no stable identity.
    pub call_id: Option<String>,
    pub tool_name: Option<String>,
    /// `started | succeeded | failed`.
    pub phase: String,
    /// Bounded, normalized display text. Never raw provider JSON.
    pub summary: String,
    /// Reserved for normalized, allow-listed metadata. Raw inputs do not go
    /// here; v1 intentionally leaves it empty.
    pub detail_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRunEvent {
    pub kind: String,
    pub call_id: Option<String>,
    pub tool_name: Option<String>,
    pub phase: String,
    pub summary: String,
    pub detail_json: Option<String>,
}

const COLUMNS: &str =
    "id, run_id, seq, occurred_at, kind, call_id, tool_name, phase, summary, detail_json";

fn row_to_event(row: &Row) -> rusqlite::Result<RunEvent> {
    Ok(RunEvent {
        id: row.get(0)?,
        run_id: row.get(1)?,
        seq: row.get(2)?,
        occurred_at: row.get(3)?,
        kind: row.get(4)?,
        call_id: row.get(5)?,
        tool_name: row.get(6)?,
        phase: row.get(7)?,
        summary: row.get(8)?,
        detail_json: row.get(9)?,
    })
}

/// Append one event, returning `None` when the same provider call/phase was
/// already recorded. One writer owns a run in normal operation; the SQL still
/// calculates the next sequence inside the insert so recovery and tests cannot
/// accidentally reuse one.
pub fn append(
    conn: &Connection,
    run_id: &str,
    event: &NewRunEvent,
) -> Result<Option<RunEvent>, AppError> {
    let changed = conn.execute(
        "INSERT OR IGNORE INTO run_events
             (run_id, seq, kind, call_id, tool_name, phase, summary, detail_json)
         VALUES (
             ?1,
             (SELECT COALESCE(MAX(seq), 0) + 1 FROM run_events WHERE run_id = ?1),
             ?2, ?3, ?4, ?5, ?6, ?7
         )",
        params![
            run_id,
            event.kind,
            event.call_id,
            event.tool_name,
            event.phase,
            event.summary,
            event.detail_json,
        ],
    )?;
    if changed == 0 {
        return Ok(None);
    }

    let id = conn.last_insert_rowid();
    let sql = format!("SELECT {COLUMNS} FROM run_events WHERE id = ?1");
    Ok(Some(conn.query_row(&sql, params![id], row_to_event)?))
}

/// Events after the caller's cursor, oldest first. `after_seq = 0` is the full
/// timeline and the same endpoint supports catch-up after a dropped bridge
/// hint without trusting content from the hint itself.
pub fn list_after(
    conn: &Connection,
    run_id: &str,
    after_seq: i64,
) -> Result<Vec<RunEvent>, AppError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM run_events
          WHERE run_id = ?1 AND seq > ?2
          ORDER BY seq ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![run_id, after_seq.max(0)], row_to_event)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::runs;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{self, NewTask};
    use crate::executor::BackgroundExecutor;
    use crate::intent::BackgroundIntent;

    fn seed_run(conn: &Connection) -> runs::TaskRun {
        let task = tasks::create(
            conn,
            &NewTask {
                title: "trace me".into(),
                ..Default::default()
            },
        )
        .unwrap();
        runs::start(
            conn,
            &task.id,
            "background",
            None,
            BackgroundExecutor::Claude,
            BackgroundIntent::Work,
            None,
        )
        .unwrap()
    }

    fn event(call_id: &str, phase: &str) -> NewRunEvent {
        NewRunEvent {
            kind: "tool".into(),
            call_id: Some(call_id.into()),
            tool_name: Some("Read".into()),
            phase: phase.into(),
            summary: "Read src/main.rs".into(),
            detail_json: None,
        }
    }

    #[test]
    fn appends_in_order_and_reads_after_a_cursor() {
        let conn = test_conn();
        let run = seed_run(&conn);
        let first = append(&conn, &run.id, &event("call-1", "started"))
            .unwrap()
            .unwrap();
        let second = append(&conn, &run.id, &event("call-1", "succeeded"))
            .unwrap()
            .unwrap();
        assert_eq!((first.seq, second.seq), (1, 2));
        assert_eq!(list_after(&conn, &run.id, 1).unwrap(), vec![second]);
    }

    #[test]
    fn duplicate_provider_updates_do_not_duplicate_the_trace() {
        let conn = test_conn();
        let run = seed_run(&conn);
        assert!(append(&conn, &run.id, &event("call-1", "started"))
            .unwrap()
            .is_some());
        assert!(append(&conn, &run.id, &event("call-1", "started"))
            .unwrap()
            .is_none());
        assert_eq!(list_after(&conn, &run.id, 0).unwrap().len(), 1);
    }

    #[test]
    fn destroying_the_task_cascades_through_runs_to_events() {
        let conn = test_conn();
        let run = seed_run(&conn);
        append(&conn, &run.id, &event("call-1", "started")).unwrap();
        tasks::delete(&conn, &run.task_id).unwrap();
        assert!(list_after(&conn, &run.id, 0).unwrap().is_empty());
    }
}
