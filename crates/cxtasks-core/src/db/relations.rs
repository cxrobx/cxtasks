//! Durable task-to-task context: supersedes and relates_to.
//!
//! Dependencies answer ordering. These edges answer history: which later task
//! replaced this one, and which peer records are useful context. The table is
//! intentionally small; pure plan churn should still be deleted.

use crate::error::AppError;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// A task replaced by a later one is history, not actionable work. Kept as a
/// shared SQL fragment so next_task and both runner claim paths cannot drift.
pub const NOT_SUPERSEDED: &str = "NOT EXISTS(
    SELECT 1 FROM task_relations cxt_rel
     WHERE cxt_rel.to_task_id = tasks.id AND cxt_rel.kind = 'supersedes'
)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Supersedes,
    RelatesTo,
}

impl RelationKind {
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "supersedes" | "supersede" => Ok(Self::Supersedes),
            "relates_to" | "related" | "relates" => Ok(Self::RelatesTo),
            other => Err(AppError::Validation(format!(
                "unknown relation '{other}' (expected supersedes or relates_to)"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supersedes => "supersedes",
            Self::RelatesTo => "relates_to",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationEdge {
    pub task_id: String,
    pub short_id: Option<i64>,
    pub title: String,
    pub status: String,
    pub priority: i64,
    pub project: Option<String>,
    pub created_at: String,
}

const EDGE_COLUMNS: &str =
    "t.id, t.short_id, t.title, t.status, t.priority, t.project, r.created_at";

fn row_to_edge(row: &rusqlite::Row) -> rusqlite::Result<RelationEdge> {
    Ok(RelationEdge {
        task_id: row.get(0)?,
        short_id: row.get(1)?,
        title: row.get(2)?,
        status: row.get(3)?,
        priority: row.get(4)?,
        project: row.get(5)?,
        created_at: row.get(6)?,
    })
}

fn require_task(conn: &Connection, id: &str) -> Result<(), AppError> {
    if crate::db::tasks::get(conn, id)?.is_none() {
        return Err(AppError::NotFound(format!("task {id} not found")));
    }
    Ok(())
}

/// Did this task accumulate an actual conclusion worth preserving?
///
/// A body or seed prompt is only the plan. A run or a comment proves somebody
/// touched the work. Refusing a supersedes edge on pure churn keeps the history
/// graph from becoming a second trash can.
fn has_conclusion(conn: &Connection, id: &str) -> Result<bool, AppError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM task_runs WHERE task_id = ?1)
             OR EXISTS(SELECT 1 FROM task_comments WHERE task_id = ?1)",
        [id],
        |r| r.get(0),
    )?)
}

fn supersedes_reaches(conn: &Connection, start: &str, target: &str) -> Result<bool, AppError> {
    let mut seen = HashSet::new();
    let mut stack = vec![start.to_string()];
    while let Some(id) = stack.pop() {
        if id == target {
            return Ok(true);
        }
        if !seen.insert(id.clone()) {
            continue;
        }
        let mut stmt = conn.prepare(
            "SELECT to_task_id FROM task_relations
              WHERE from_task_id = ?1 AND kind = 'supersedes'",
        )?;
        stack.extend(
            stmt.query_map([id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(false)
}

/// Add a relation. Idempotent.
///
/// For supersedes, from_task_id is the replacement and to_task_id is the
/// older task it makes non-actionable. relates_to is symmetric and stored in
/// canonical UUID order so the same edge has one representation.
pub fn add(
    conn: &Connection,
    from_task_id: &str,
    to_task_id: &str,
    kind: RelationKind,
) -> Result<(), AppError> {
    if from_task_id == to_task_id {
        return Err(AppError::Validation(
            "a task cannot be related to itself".into(),
        ));
    }
    require_task(conn, from_task_id)?;
    require_task(conn, to_task_id)?;

    let (from, to) = match kind {
        RelationKind::RelatesTo if from_task_id > to_task_id => (to_task_id, from_task_id),
        _ => (from_task_id, to_task_id),
    };
    if kind == RelationKind::Supersedes {
        if !has_conclusion(conn, to)? {
            return Err(AppError::Validation(
                "the older task has no run or comment to preserve — delete pure plan churn \
                 instead of adding a supersedes edge"
                    .into(),
            ));
        }
        if supersedes_reaches(conn, to, from)? {
            return Err(AppError::Validation(
                "that would create a supersedes cycle — neither task could honestly be the \
                 later replacement"
                    .into(),
            ));
        }
    }
    conn.execute(
        "INSERT OR IGNORE INTO task_relations (from_task_id, to_task_id, kind)
         VALUES (?1, ?2, ?3)",
        params![from, to, kind.as_str()],
    )?;
    Ok(())
}

pub fn remove(
    conn: &Connection,
    from_task_id: &str,
    to_task_id: &str,
    kind: RelationKind,
) -> Result<(), AppError> {
    let (from, to) = match kind {
        RelationKind::RelatesTo if from_task_id > to_task_id => (to_task_id, from_task_id),
        _ => (from_task_id, to_task_id),
    };
    conn.execute(
        "DELETE FROM task_relations
          WHERE from_task_id = ?1 AND to_task_id = ?2 AND kind = ?3",
        params![from, to, kind.as_str()],
    )?;
    Ok(())
}

/// Older tasks this task replaces.
pub fn supersedes(conn: &Connection, task_id: &str) -> Result<Vec<RelationEdge>, AppError> {
    let sql = format!(
        "SELECT {EDGE_COLUMNS}
           FROM task_relations r JOIN tasks t ON t.id = r.to_task_id
          WHERE r.from_task_id = ?1 AND r.kind = 'supersedes'
          ORDER BY datetime(r.created_at) DESC, r.rowid DESC"
    );
    Ok(conn
        .prepare(&sql)?
        .query_map([task_id], row_to_edge)?
        .collect::<Result<Vec<_>, _>>()?)
}

/// Later tasks that replace this task.
pub fn superseded_by(conn: &Connection, task_id: &str) -> Result<Vec<RelationEdge>, AppError> {
    let sql = format!(
        "SELECT {EDGE_COLUMNS}
           FROM task_relations r JOIN tasks t ON t.id = r.from_task_id
          WHERE r.to_task_id = ?1 AND r.kind = 'supersedes'
          ORDER BY datetime(r.created_at) DESC, r.rowid DESC"
    );
    Ok(conn
        .prepare(&sql)?
        .query_map([task_id], row_to_edge)?
        .collect::<Result<Vec<_>, _>>()?)
}

pub fn related(conn: &Connection, task_id: &str) -> Result<Vec<RelationEdge>, AppError> {
    let sql = format!(
        "SELECT {EDGE_COLUMNS}
           FROM task_relations r
           JOIN tasks t ON t.id = CASE
               WHEN r.from_task_id = ?1 THEN r.to_task_id ELSE r.from_task_id END
          WHERE r.kind = 'relates_to'
            AND (r.from_task_id = ?1 OR r.to_task_id = ?1)
          ORDER BY datetime(r.created_at) DESC, r.rowid DESC"
    );
    Ok(conn
        .prepare(&sql)?
        .query_map([task_id], row_to_edge)?
        .collect::<Result<Vec<_>, _>>()?)
}

pub fn is_superseded(conn: &Connection, task_id: &str) -> Result<bool, AppError> {
    Ok(conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM task_relations
             WHERE to_task_id = ?1 AND kind = 'supersedes'
         )",
        [task_id],
        |r| r.get(0),
    )?)
}

pub fn superseded_task_ids(conn: &Connection) -> Result<Vec<String>, AppError> {
    Ok(conn
        .prepare(
            "SELECT DISTINCT to_task_id FROM task_relations WHERE kind = 'supersedes'",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::{comments, tasks};

    fn task(conn: &Connection, title: &str) -> tasks::Task {
        tasks::create(
            conn,
            &tasks::NewTask {
                title: title.into(),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn supersedes_is_directional_and_requires_a_conclusion() {
        let conn = test_conn();
        let old = task(&conn, "old");
        let new = task(&conn, "new");
        assert!(add(&conn, &new.id, &old.id, RelationKind::Supersedes).is_err());
        comments::add(&conn, &old.id, comments::AUTHOR_ME, "ruled out: API cannot do it")
            .unwrap();
        add(&conn, &new.id, &old.id, RelationKind::Supersedes).unwrap();
        assert_eq!(supersedes(&conn, &new.id).unwrap()[0].task_id, old.id);
        assert_eq!(superseded_by(&conn, &old.id).unwrap()[0].task_id, new.id);
        assert!(is_superseded(&conn, &old.id).unwrap());
        assert!(!is_superseded(&conn, &new.id).unwrap());
    }

    #[test]
    fn relates_to_is_symmetric_and_idempotent() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        add(&conn, &a.id, &b.id, RelationKind::RelatesTo).unwrap();
        add(&conn, &b.id, &a.id, RelationKind::RelatesTo).unwrap();
        assert_eq!(related(&conn, &a.id).unwrap().len(), 1);
        assert_eq!(related(&conn, &b.id).unwrap().len(), 1);
        remove(&conn, &b.id, &a.id, RelationKind::RelatesTo).unwrap();
        assert!(related(&conn, &a.id).unwrap().is_empty());
    }

    #[test]
    fn supersedes_cycles_are_refused() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        comments::add(&conn, &a.id, comments::AUTHOR_ME, "a conclusion").unwrap();
        comments::add(&conn, &b.id, comments::AUTHOR_ME, "b conclusion").unwrap();
        add(&conn, &a.id, &b.id, RelationKind::Supersedes).unwrap();
        assert!(add(&conn, &b.id, &a.id, RelationKind::Supersedes).is_err());
    }
}
