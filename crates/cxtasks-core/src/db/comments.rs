//! Task comments — the running log, like Meta Tasks.
//!
//! Separate from `tasks.body` on purpose. `body` is the task's *definition* —
//! what needs doing, edited in place. Comments are an append-only *history* —
//! what happened, who said it, when. Folding the second into the first is how
//! you end up with a body that nobody dares edit because it also holds the
//! record.
//!
//! `author` is free text so a dispatched agent can sign its own entries and be
//! visually distinguishable from Chris's.

use crate::error::AppError;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// Author used when the app itself writes a comment on the user's behalf.
pub const AUTHOR_ME: &str = "me";
/// Author used by Claude sessions — foreground dispatch, a Claude background
/// run, and any MCP client that is not recognisably another agent.
pub const AUTHOR_CLAUDE: &str = "claude";
/// Author used by Codex — a Codex background run, or Codex as the MCP client.
///
/// An agent signs as the agent that ran, never as a blanket "claude": a log
/// that credits Claude for Codex's work misattributes exactly the thing the
/// two-executor review loop exists to tell apart. Both agent authors equal
/// [`crate::executor::BackgroundExecutor::as_str`], so `executor.as_str()` is
/// the author for a run's own entries.
pub const AUTHOR_CODEX: &str = "codex";
/// Every author an AGENT writes under — what the prose audit reads.
pub const AGENT_AUTHORS: &[&str] = &[AUTHOR_CLAUDE, AUTHOR_CODEX];
/// Author of the entries a tripwire check writes — the evidence on a trip, the
/// note on a failure. Its own name so a condition coming true reads as an
/// event in the log, not as something an agent said.
pub const AUTHOR_TRIPWIRE: &str = "tripwire";
/// Author for structured post-run flags produced by the optional Jev reviewer.
pub const AUTHOR_JEV: &str = "jev";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub task_id: String,
    pub author: String,
    pub body: String,
    pub created_at: String,
}

pub fn add(
    conn: &Connection,
    task_id: &str,
    author: &str,
    body: &str,
) -> Result<Comment, AppError> {
    insert(conn, task_id, author, body, false)
}

/// Create a comment whose image is the content. The ordinary `add` path keeps
/// rejecting blanks (including MCP callers); only the attachment command may
/// opt into an empty caption, and it does so only after validating at least one
/// image.
pub fn add_with_attachment(
    conn: &Connection,
    task_id: &str,
    author: &str,
    body: &str,
) -> Result<Comment, AppError> {
    insert(conn, task_id, author, body, true)
}

fn insert(
    conn: &Connection,
    task_id: &str,
    author: &str,
    body: &str,
    allow_empty: bool,
) -> Result<Comment, AppError> {
    let body = body.trim();
    if body.is_empty() && !allow_empty {
        return Err(AppError::Validation("comment body must not be empty".into()));
    }
    if crate::db::tasks::get(conn, task_id)?.is_none() {
        return Err(AppError::NotFound(format!("task {task_id} not found")));
    }
    let author = match author.trim() {
        "" => AUTHOR_ME,
        a => a,
    };

    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO task_comments (id, task_id, author, body) VALUES (?1, ?2, ?3, ?4)",
        params![id, task_id, author, body],
    )?;
    get(conn, &id)?.ok_or_else(|| AppError::NotFound("comment vanished after insert".into()))
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Comment>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, author, body, created_at FROM task_comments WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], row_to_comment)?;
    match rows.next() {
        Some(c) => Ok(Some(c?)),
        None => Ok(None),
    }
}

fn row_to_comment(r: &rusqlite::Row) -> rusqlite::Result<Comment> {
    Ok(Comment {
        id: r.get(0)?,
        task_id: r.get(1)?,
        author: r.get(2)?,
        body: r.get(3)?,
        created_at: r.get(4)?,
    })
}

/// Oldest first — a log reads top-down.
pub fn for_task(conn: &Connection, task_id: &str) -> Result<Vec<Comment>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, author, body, created_at
           FROM task_comments WHERE task_id = ?1
          -- rowid is the definitive tiebreaker: it's insertion order and
          -- monotonic, whereas `id` is a random UUID and `created_at` can tie
          -- even at millisecond precision under a fast loop.
          ORDER BY datetime(created_at) ASC, rowid ASC",
    )?;
    let rows = stmt
        .query_map(params![task_id], row_to_comment)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn count_for_task(conn: &Connection, task_id: &str) -> Result<i64, AppError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task_comments WHERE task_id = ?1",
        params![task_id],
        |r| r.get(0),
    )?;
    Ok(n)
}

pub fn edit(conn: &Connection, id: &str, body: &str) -> Result<Comment, AppError> {
    let body = body.trim();
    if body.is_empty() {
        return Err(AppError::Validation("comment body must not be empty".into()));
    }
    let n = conn.execute(
        // `edited_at` is what lets the edit sync — see schema v28. Same
        // millisecond format as `created_at`, so the sweep can take the MAX
        // of the two as one string.
        "UPDATE task_comments
            SET body = ?1, edited_at = strftime('%Y-%m-%d %H:%M:%f', 'now')
          WHERE id = ?2",
        params![body, id],
    )?;
    if n == 0 {
        return Err(AppError::NotFound(format!("comment {id} not found")));
    }
    get(conn, id)?.ok_or_else(|| AppError::NotFound(format!("comment {id} not found")))
}

pub fn delete(conn: &Connection, id: &str) -> Result<(), AppError> {
    let n = conn.execute("DELETE FROM task_comments WHERE id = ?1", params![id])?;
    if n == 0 {
        return Err(AppError::NotFound(format!("comment {id} not found")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{self, NewTask};

    /// The runner signs a run's comment with `executor.as_str()`, so every
    /// executor must map onto a known agent author — a third executor added
    /// without one would sign under a name the prose audit never reads.
    #[test]
    fn every_executor_signs_as_a_known_agent_author() {
        use crate::executor::BackgroundExecutor;
        assert_eq!(BackgroundExecutor::Claude.as_str(), AUTHOR_CLAUDE);
        assert_eq!(BackgroundExecutor::Codex.as_str(), AUTHOR_CODEX);
        for e in BackgroundExecutor::ALL {
            assert!(AGENT_AUTHORS.contains(&e.as_str()), "{e:?}");
        }
    }

    fn task(conn: &Connection) -> String {
        tasks::create(
            conn,
            &NewTask {
                title: "t".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .id
    }

    /// Regression: with second-resolution timestamps and a UUID tiebreaker,
    /// comments added in the same second came back in random order. Adding
    /// several in a tight loop is what exposes it — a test that sleeps between
    /// inserts would pass against the broken ordering.
    #[test]
    fn add_and_read_in_chronological_order() {
        let conn = test_conn();
        let t = task(&conn);
        for i in 0..8 {
            add(&conn, &t, AUTHOR_ME, &format!("entry {i}")).unwrap();
        }
        let bodies: Vec<_> = for_task(&conn, &t)
            .unwrap()
            .into_iter()
            .map(|c| c.body)
            .collect();
        assert_eq!(
            bodies,
            (0..8).map(|i| format!("entry {i}")).collect::<Vec<_>>(),
            "comments must stay in insertion order even when added within one second"
        );

        let t2 = task(&conn);
        add(&conn, &t2, AUTHOR_ME, "first").unwrap();
        add(&conn, &t2, AUTHOR_CLAUDE, "second").unwrap();
        let log = for_task(&conn, &t2).unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].body, "first");
        assert_eq!(log[1].body, "second");
        assert_eq!(log[1].author, "claude");
    }

    #[test]
    fn body_is_trimmed_and_blank_is_rejected() {
        let conn = test_conn();
        let t = task(&conn);
        let c = add(&conn, &t, AUTHOR_ME, "  padded  ").unwrap();
        assert_eq!(c.body, "padded");
        assert!(add(&conn, &t, AUTHOR_ME, "   ").is_err());
        assert!(add(&conn, &t, AUTHOR_ME, "").is_err());
    }

    #[test]
    fn only_attachment_path_may_create_an_empty_caption() {
        let conn = test_conn();
        let t = task(&conn);
        let comment = add_with_attachment(&conn, &t, AUTHOR_ME, "   ").unwrap();
        assert_eq!(comment.body, "");
        assert!(add(&conn, &t, AUTHOR_ME, "   ").is_err());
    }

    #[test]
    fn blank_author_falls_back_to_me() {
        let conn = test_conn();
        let t = task(&conn);
        assert_eq!(add(&conn, &t, "  ", "x").unwrap().author, AUTHOR_ME);
    }

    #[test]
    fn commenting_on_a_missing_task_is_rejected() {
        let conn = test_conn();
        assert!(add(&conn, "nope", AUTHOR_ME, "x").is_err());
    }

    #[test]
    fn comments_cascade_when_the_task_is_deleted() {
        let conn = test_conn();
        let t = task(&conn);
        add(&conn, &t, AUTHOR_ME, "x").unwrap();
        tasks::delete(&conn, &t).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM task_comments", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn edit_and_delete() {
        let conn = test_conn();
        let t = task(&conn);
        let c = add(&conn, &t, AUTHOR_ME, "typo").unwrap();
        let fixed = edit(&conn, &c.id, "fixed").unwrap();
        assert_eq!(fixed.body, "fixed");
        assert!(edit(&conn, &c.id, "  ").is_err(), "blank edit must be rejected");

        delete(&conn, &c.id).unwrap();
        assert_eq!(count_for_task(&conn, &t).unwrap(), 0);
        assert!(delete(&conn, &c.id).is_err(), "double delete should error");
    }

    #[test]
    fn count_is_scoped_to_one_task() {
        let conn = test_conn();
        let a = task(&conn);
        let b = task(&conn);
        add(&conn, &a, AUTHOR_ME, "1").unwrap();
        add(&conn, &a, AUTHOR_ME, "2").unwrap();
        add(&conn, &b, AUTHOR_ME, "3").unwrap();
        assert_eq!(count_for_task(&conn, &a).unwrap(), 2);
        assert_eq!(count_for_task(&conn, &b).unwrap(), 1);
    }
}
