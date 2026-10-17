//! Metadata for image attachments.
//!
//! The file-copy boundary lives in the Tauri command layer, where the app-data
//! path is available. This module owns the relational invariant: an attachment
//! belongs to a task, and when it targets a comment that comment must belong to
//! the same task.

use crate::error::AppError;
use rusqlite::{params, Connection};

#[derive(Debug, Clone)]
pub struct AttachmentRecord {
    pub id: String,
    pub task_id: String,
    pub comment_id: Option<String>,
    pub name: String,
    pub mime_type: String,
    pub stored_name: String,
    pub created_at: String,
    /// Lowercase hex SHA-256 of the bytes, or NULL for a row written before
    /// v22. See the v22 migration for why this is never written twice.
    pub sha256: Option<String>,
}

fn row_to_attachment(r: &rusqlite::Row) -> rusqlite::Result<AttachmentRecord> {
    Ok(AttachmentRecord {
        id: r.get(0)?,
        task_id: r.get(1)?,
        comment_id: r.get(2)?,
        name: r.get(3)?,
        mime_type: r.get(4)?,
        stored_name: r.get(5)?,
        created_at: r.get(6)?,
        sha256: r.get(7)?,
    })
}

pub fn add(
    conn: &Connection,
    task_id: &str,
    comment_id: Option<&str>,
    name: &str,
    mime_type: &str,
    stored_name: &str,
    sha256: Option<&str>,
) -> Result<AttachmentRecord, AppError> {
    if crate::db::tasks::get(conn, task_id)?.is_none() {
        return Err(AppError::NotFound(format!("task {task_id} not found")));
    }
    if let Some(comment_id) = comment_id {
        let comment = crate::db::comments::get(conn, comment_id)?
            .ok_or_else(|| AppError::NotFound(format!("comment {comment_id} not found")))?;
        if comment.task_id != task_id {
            return Err(AppError::Validation(
                "attachment comment belongs to a different task".into(),
            ));
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO task_attachments
             (id, task_id, comment_id, name, mime_type, stored_name, sha256)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, task_id, comment_id, name, mime_type, stored_name, sha256],
    )?;
    get(conn, &id)?.ok_or_else(|| {
        AppError::NotFound("attachment vanished after insert".into())
    })
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<AttachmentRecord>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, comment_id, name, mime_type, stored_name, created_at, sha256
           FROM task_attachments WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map(params![id], row_to_attachment)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

pub fn for_task(conn: &Connection, task_id: &str) -> Result<Vec<AttachmentRecord>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, comment_id, name, mime_type, stored_name, created_at, sha256
           FROM task_attachments
          WHERE task_id = ?1
          ORDER BY datetime(created_at) ASC, rowid ASC",
    )?;
    let rows = stmt
        .query_map(params![task_id], row_to_attachment)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn for_comment(
    conn: &Connection,
    comment_id: &str,
) -> Result<Vec<AttachmentRecord>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, comment_id, name, mime_type, stored_name, created_at, sha256
           FROM task_attachments
          WHERE comment_id = ?1
          ORDER BY datetime(created_at) ASC, rowid ASC",
    )?;
    let rows = stmt
        .query_map(params![comment_id], row_to_attachment)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Attachments removed when a task is permanently destroyed, including those
/// on descendants deleted by the task foreign-key cascade.
pub fn for_task_tree(
    conn: &Connection,
    root_task_id: &str,
) -> Result<Vec<AttachmentRecord>, AppError> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE subtree(id) AS (
             SELECT id FROM tasks WHERE id = ?1
             UNION ALL
             SELECT child.id FROM tasks child
             JOIN subtree parent ON child.parent_id = parent.id
         )
         SELECT id, task_id, comment_id, name, mime_type, stored_name, created_at, sha256
           FROM task_attachments
          WHERE task_id IN (SELECT id FROM subtree)",
    )?;
    let rows = stmt
        .query_map(params![root_task_id], row_to_attachment)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn for_trashed_tasks(conn: &Connection) -> Result<Vec<AttachmentRecord>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.task_id, a.comment_id, a.name, a.mime_type,
                a.stored_name, a.created_at, a.sha256
           FROM task_attachments a
           JOIN tasks t ON t.id = a.task_id
          WHERE t.deleted_at IS NOT NULL",
    )?;
    let rows = stmt
        .query_map([], row_to_attachment)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every attachment row, in no particular order. The sync layer's inventory.
pub fn all(conn: &Connection) -> Result<Vec<AttachmentRecord>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, comment_id, name, mime_type, stored_name, created_at, sha256
           FROM task_attachments",
    )?;
    let rows = stmt
        .query_map([], row_to_attachment)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The row that owns a managed filename, if any.
///
/// `stored_name` is UNIQUE (v10), which is what makes this a lookup rather than
/// a scan — and what lets an upload route answer "there is no row for that" as
/// a fact rather than a guess.
pub fn by_stored_name(
    conn: &Connection,
    stored_name: &str,
) -> Result<Option<AttachmentRecord>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, comment_id, name, mime_type, stored_name, created_at, sha256
           FROM task_attachments WHERE stored_name = ?1",
    )?;
    let mut rows = stmt.query_map(params![stored_name], row_to_attachment)?;
    match rows.next() {
        Some(row) => Ok(Some(row?)),
        None => Ok(None),
    }
}

pub fn stored_names(conn: &Connection) -> Result<std::collections::HashSet<String>, AppError> {
    let mut stmt = conn.prepare("SELECT stored_name FROM task_attachments")?;
    let names = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(names)
}

pub fn delete(conn: &Connection, id: &str) -> Result<AttachmentRecord, AppError> {
    let attachment = get(conn, id)?
        .ok_or_else(|| AppError::NotFound(format!("attachment {id} not found")))?;
    conn.execute("DELETE FROM task_attachments WHERE id = ?1", params![id])?;
    Ok(attachment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{comments, schema::test_conn, tasks};

    #[test]
    fn note_and_comment_attachments_are_scoped_and_cascade() {
        let conn = test_conn();
        let task = tasks::create(
            &conn,
            &tasks::NewTask {
                title: "task".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let comment = comments::add(&conn, &task.id, comments::AUTHOR_ME, "look").unwrap();

        add(&conn, &task.id, None, "brief.png", "image/png", "a.png", None).unwrap();
        add(
            &conn,
            &task.id,
            Some(&comment.id),
            "result.jpg",
            "image/jpeg",
            "b.jpg",
            Some("beef"),
        )
        .unwrap();
        assert_eq!(for_task(&conn, &task.id).unwrap().len(), 2);

        tasks::delete(&conn, &task.id).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_attachments", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn comment_must_belong_to_the_same_task() {
        let conn = test_conn();
        let make = |title: &str| {
            tasks::create(
                &conn,
                &tasks::NewTask {
                    title: title.into(),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let a = make("a");
        let b = make("b");
        let comment = comments::add(&conn, &b.id, comments::AUTHOR_ME, "b").unwrap();
        assert!(add(
            &conn,
            &a.id,
            Some(&comment.id),
            "x.png",
            "image/png",
            "x.png",
            None
        )
        .is_err());
    }
}
