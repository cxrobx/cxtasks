//! Task dependencies — "blocked by" / "blocking".
//!
//! # How this differs from subtasks
//!
//! `tasks.parent_id` is **containment**: a subtask is *part of* its parent, one
//! parent only, and it lives inside that parent's scope. `task_deps` is
//! **ordering**: task A can't start until task B is done, A and B are peers, the
//! relationship is many-to-many, and B can live in a completely different
//! project. Containment cannot express "the Acme deploy waits on the cxventures
//! webhook fix" — that's two projects and neither owns the other.
//!
//! # Why cycles have to be rejected at write time
//!
//! A dependency graph with a cycle has no valid ordering, so every task in the
//! cycle is permanently blocked and `next_task` silently skips all of them. The
//! failure mode is "the app stopped suggesting work" with nothing visibly wrong.
//! SQL can only `CHECK` self-reference; anything longer needs a graph walk, so
//! [`add`] does one before inserting.

use crate::error::AppError;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// A dependency edge with enough of the blocker's state to render it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepEdge {
    pub task_id: String,
    pub title: String,
    pub status: String,
    pub priority: i64,
    pub project: Option<String>,
    /// True when this blocker is still open, i.e. actually blocking.
    pub is_open: bool,
}

fn closed_list() -> String {
    crate::db::tasks::CLOSED_STATUSES
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// SQL for "this blocker is still actually blocking": open **and** still on the
/// board. Trashing or archiving a blocker has to release whatever it was
/// holding, exactly like closing it does — otherwise deleting one task strands
/// every task downstream of it, permanently unactionable and invisible in
/// `next_task`, with nothing on screen to explain why (invariant 10).
const OPEN_BLOCKER: &str = "t.deleted_at IS NULL AND t.archived_at IS NULL";

/// Row-side twin of [`OPEN_BLOCKER`], for rendering an edge that is kept but no
/// longer enforced.
fn edge_is_open(status: &str, deleted_at: &Option<String>, archived_at: &Option<String>) -> bool {
    !crate::db::tasks::CLOSED_STATUSES.contains(&status)
        && deleted_at.is_none()
        && archived_at.is_none()
}

const EDGE_COLUMNS: &str = "t.id, t.title, t.status, t.priority, t.project, \
     t.deleted_at, t.archived_at";

fn row_to_edge(r: &rusqlite::Row) -> rusqlite::Result<DepEdge> {
    let status: String = r.get(2)?;
    let deleted_at: Option<String> = r.get(5)?;
    let archived_at: Option<String> = r.get(6)?;
    Ok(DepEdge {
        is_open: edge_is_open(&status, &deleted_at, &archived_at),
        task_id: r.get(0)?,
        title: r.get(1)?,
        status,
        priority: r.get(3)?,
        project: r.get(4)?,
    })
}

/// Everything `task_id` is waiting on.
///
/// Trashed blockers are still *listed* — the edge outliving its blocker is the
/// point, so the record of what blocked what stays readable — but they come back
/// with `is_open: false` and no longer hold anything up.
pub fn blocked_by(conn: &Connection, task_id: &str) -> Result<Vec<DepEdge>, AppError> {
    let closed = closed_list();
    let sql = format!(
        "SELECT {EDGE_COLUMNS}
           FROM task_deps d
           JOIN tasks t ON t.id = d.blocked_by_id
          WHERE d.task_id = ?1
          ORDER BY (t.status IN ({closed}) OR t.deleted_at IS NOT NULL
                    OR t.archived_at IS NOT NULL) ASC,
                   t.priority ASC, t.title ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![task_id], row_to_edge)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Everything waiting on `task_id` — the reverse view, so finishing a task can
/// tell you what it just unblocked.
pub fn blocking(conn: &Connection, task_id: &str) -> Result<Vec<DepEdge>, AppError> {
    let closed = closed_list();
    let sql = format!(
        "SELECT {EDGE_COLUMNS}
           FROM task_deps d
           JOIN tasks t ON t.id = d.task_id
          WHERE d.blocked_by_id = ?1
          ORDER BY (t.status IN ({closed}) OR t.deleted_at IS NOT NULL
                    OR t.archived_at IS NOT NULL) ASC,
                   t.priority ASC, t.title ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![task_id], row_to_edge)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Does `task_id` have at least one blocker that is still open?
pub fn is_blocked(conn: &Connection, task_id: &str) -> Result<bool, AppError> {
    let closed = closed_list();
    let sql = format!(
        "SELECT EXISTS(
            SELECT 1 FROM task_deps d
              JOIN tasks t ON t.id = d.blocked_by_id
             WHERE d.task_id = ?1 AND t.status NOT IN ({closed}) AND {OPEN_BLOCKER}
         )"
    );
    let blocked: bool = conn.query_row(&sql, params![task_id], |r| r.get(0))?;
    Ok(blocked)
}

/// Walk the graph from `start` following "blocked by" edges. Returns true if
/// `target` is reachable — i.e. adding `target → start` would close a cycle.
fn reaches(conn: &Connection, start: &str, target: &str) -> Result<bool, AppError> {
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![start.to_string()];
    while let Some(node) = stack.pop() {
        if node == target {
            return Ok(true);
        }
        if !seen.insert(node.clone()) {
            continue; // already explored — also stops us looping on a pre-existing cycle
        }
        let mut stmt = conn.prepare("SELECT blocked_by_id FROM task_deps WHERE task_id = ?1")?;
        let next: Vec<String> = stmt
            .query_map(params![node], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        stack.extend(next);
    }
    Ok(false)
}

/// Record that `task_id` is blocked by `blocked_by_id`.
///
/// Rejects self-blocking, unknown ids, and any edge that would create a cycle.
/// Idempotent: adding the same edge twice is a no-op rather than an error, so a
/// double-click or a retried MCP call doesn't fail.
pub fn add(conn: &Connection, task_id: &str, blocked_by_id: &str) -> Result<(), AppError> {
    if task_id == blocked_by_id {
        return Err(AppError::Validation(
            "a task can't block itself".into(),
        ));
    }
    for id in [task_id, blocked_by_id] {
        if crate::db::tasks::get(conn, id)?.is_none() {
            return Err(AppError::NotFound(format!("task {id} not found")));
        }
    }

    // Adding `task_id -> blocked_by_id` closes a cycle iff task_id is already
    // reachable from blocked_by_id.
    if reaches(conn, blocked_by_id, task_id)? {
        return Err(AppError::Validation(
            "that would create a circular dependency — the two tasks would block \
             each other and neither could ever become actionable"
                .into(),
        ));
    }

    conn.execute(
        "INSERT OR IGNORE INTO task_deps (task_id, blocked_by_id) VALUES (?1, ?2)",
        params![task_id, blocked_by_id],
    )?;
    Ok(())
}

pub fn remove(conn: &Connection, task_id: &str, blocked_by_id: &str) -> Result<(), AppError> {
    conn.execute(
        "DELETE FROM task_deps WHERE task_id = ?1 AND blocked_by_id = ?2",
        params![task_id, blocked_by_id],
    )?;
    Ok(())
}

/// Ids of every task with at least one open blocker. Used to exclude them from
/// `next_task` and from the background queue in one query rather than N.
pub fn blocked_task_ids(conn: &Connection) -> Result<Vec<String>, AppError> {
    let closed = closed_list();
    let sql = format!(
        "SELECT DISTINCT d.task_id
           FROM task_deps d
           JOIN tasks t ON t.id = d.blocked_by_id
          WHERE t.status NOT IN ({closed}) AND {OPEN_BLOCKER}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{self, NewTask};

    fn task(conn: &Connection, title: &str) -> String {
        tasks::create(
            conn,
            &NewTask {
                title: title.into(),
                ..Default::default()
            },
        )
        .unwrap()
        .id
    }

    #[test]
    fn add_and_read_both_directions() {
        let conn = test_conn();
        let a = task(&conn, "deploy");
        let b = task(&conn, "fix the webhook");
        add(&conn, &a, &b).unwrap();

        let blockers = blocked_by(&conn, &a).unwrap();
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0].title, "fix the webhook");
        assert!(blockers[0].is_open);

        let blocked = blocking(&conn, &b).unwrap();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].title, "deploy");
    }

    #[test]
    fn a_task_cannot_block_itself() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let err = add(&conn, &a, &a).unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    #[test]
    fn direct_cycles_are_rejected() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        add(&conn, &a, &b).unwrap();
        let err = add(&conn, &b, &a).unwrap_err();
        assert!(
            format!("{err}").contains("circular"),
            "expected a cycle error, got: {err}"
        );
    }

    /// The case a naive self-reference CHECK misses entirely.
    #[test]
    fn transitive_cycles_are_rejected() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        let c = task(&conn, "c");
        add(&conn, &a, &b).unwrap(); // a waits on b
        add(&conn, &b, &c).unwrap(); // b waits on c
        let err = add(&conn, &c, &a).unwrap_err(); // c waits on a → cycle
        assert!(format!("{err}").contains("circular"), "{err}");
    }

    #[test]
    fn a_diamond_is_not_a_cycle() {
        // a→b, a→c, b→d, c→d is a DAG; rejecting it would be a false positive.
        let conn = test_conn();
        let (a, b, c, d) = (
            task(&conn, "a"),
            task(&conn, "b"),
            task(&conn, "c"),
            task(&conn, "d"),
        );
        add(&conn, &a, &b).unwrap();
        add(&conn, &a, &c).unwrap();
        add(&conn, &b, &d).unwrap();
        add(&conn, &c, &d).unwrap();
        assert_eq!(blocked_by(&conn, &a).unwrap().len(), 2);
    }

    #[test]
    fn adding_the_same_edge_twice_is_a_no_op() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        add(&conn, &a, &b).unwrap();
        add(&conn, &a, &b).expect("re-adding must not error");
        assert_eq!(blocked_by(&conn, &a).unwrap().len(), 1);
    }

    #[test]
    fn unknown_task_ids_are_rejected() {
        let conn = test_conn();
        let a = task(&conn, "a");
        assert!(add(&conn, &a, "nope").is_err());
        assert!(add(&conn, "nope", &a).is_err());
    }

    #[test]
    fn is_blocked_clears_when_the_blocker_closes() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        add(&conn, &a, &b).unwrap();
        assert!(is_blocked(&conn, &a).unwrap());
        assert_eq!(blocked_task_ids(&conn).unwrap(), vec![a.clone()]);

        tasks::complete(&conn, &b).unwrap();
        assert!(
            !is_blocked(&conn, &a).unwrap(),
            "completing the blocker must unblock"
        );
        assert!(blocked_task_ids(&conn).unwrap().is_empty());
        // The edge itself survives, so the history of what blocked what remains.
        assert_eq!(blocked_by(&conn, &a).unwrap().len(), 1);
        assert!(!blocked_by(&conn, &a).unwrap()[0].is_open);
    }

    #[test]
    fn multiple_blockers_need_all_of_them_closed() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        let c = task(&conn, "c");
        add(&conn, &a, &b).unwrap();
        add(&conn, &a, &c).unwrap();
        tasks::complete(&conn, &b).unwrap();
        assert!(is_blocked(&conn, &a).unwrap(), "one blocker still open");
        tasks::complete(&conn, &c).unwrap();
        assert!(!is_blocked(&conn, &a).unwrap());
    }

    #[test]
    fn deleting_a_task_removes_its_edges() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        add(&conn, &a, &b).unwrap();
        tasks::delete(&conn, &b).unwrap();
        assert!(blocked_by(&conn, &a).unwrap().is_empty());
        assert!(!is_blocked(&conn, &a).unwrap());
    }

    /// Invariant 10, extended to soft delete. Deleting a blocker used to be
    /// impossible to get wrong because the row — and its edges — went with it.
    /// Now the row survives, so a blocker that is merely *hidden* would go on
    /// blocking forever: `next_task` would silently skip the dependent, the
    /// runner would never pick it up, and the only visible symptom is work
    /// quietly disappearing from the board.
    #[test]
    fn trashing_a_blocker_releases_what_it_was_blocking() {
        let conn = test_conn();
        let a = task(&conn, "deploy");
        let b = task(&conn, "the blocker");
        add(&conn, &a, &b).unwrap();
        assert!(is_blocked(&conn, &a).unwrap());

        tasks::trash(&conn, &b).unwrap();
        assert!(
            !is_blocked(&conn, &a).unwrap(),
            "a trashed blocker must stop blocking"
        );
        assert!(blocked_task_ids(&conn).unwrap().is_empty());

        // The edge itself survives, rendered as no-longer-open — same treatment
        // a completed blocker gets, so the history stays readable.
        let edges = blocked_by(&conn, &a).unwrap();
        assert_eq!(edges.len(), 1);
        assert!(!edges[0].is_open);

        // …and restoring the blocker re-blocks, rather than leaving a dead edge.
        tasks::restore(&conn, &b).unwrap();
        assert!(
            is_blocked(&conn, &a).unwrap(),
            "restoring a blocker must restore the block it was enforcing"
        );
    }

    #[test]
    fn archiving_a_blocker_also_releases_it() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        add(&conn, &a, &b).unwrap();
        tasks::archive(&conn, &b).unwrap();
        assert!(!is_blocked(&conn, &a).unwrap());
        assert!(blocked_task_ids(&conn).unwrap().is_empty());
    }

    #[test]
    fn remove_drops_only_the_named_edge() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        let c = task(&conn, "c");
        add(&conn, &a, &b).unwrap();
        add(&conn, &a, &c).unwrap();
        remove(&conn, &a, &b).unwrap();
        let left = blocked_by(&conn, &a).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].task_id, c);
    }

    /// A cycle that predates the guard (hand-edited DB) must not hang the walk.
    #[test]
    fn the_cycle_walk_terminates_on_a_preexisting_cycle() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        // Insert the back-edge directly, bypassing `add`.
        conn.execute(
            "INSERT INTO task_deps (task_id, blocked_by_id) VALUES (?1, ?2), (?2, ?1)",
            params![a, b],
        )
        .unwrap();
        let c = task(&conn, "c");
        // Must return rather than spin forever.
        assert!(reaches(&conn, &a, &c).is_ok());
        assert!(!reaches(&conn, &a, &c).unwrap());
    }
}
