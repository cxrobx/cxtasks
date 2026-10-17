//! Project status updates — the written record behind every health change.
//!
//! # Why there is no health setter
//!
//! `projects.health` is written in exactly one place: [`post`], which inserts an
//! update row and moves the dot in the same call. There is deliberately no
//! `set_health(project, health)` anywhere in the codebase, and adding one is the
//! thing to reconsider rather than the obvious convenience.
//!
//! The reason is what the GSD system is *for*. A health dot that can be flipped
//! without saying why decays into decoration within a month: everything is green
//! because nobody wrote down what "green" was standing on, and the board stops
//! being an accountability mechanism and becomes a set of lights someone
//! remembered to click. Forcing the two writes to travel together means the
//! answer to "why is this at risk" is always one click away, and the answer to
//! "when did we last actually look at this" is a timestamp rather than a guess.
//!
//! This is the same split as [`crate::db::comments`] versus `tasks.body`:
//! `projects.health` is the current claim, `project_updates` is the history that
//! produced it.
//!
//! # Keyed by name
//!
//! Rows are keyed by project **name**, matching `sections.project`, which makes
//! two cascades in [`crate::db::projects`] mandatory rather than optional —
//! `rename` carries these rows with it, and `delete` removes them. Without the
//! second, deleting project "X" and later creating a new "X" resurrects the old
//! project's log under the new one.

use crate::error::AppError;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// The health values a project may hold. `NULL` — never reported — is not in
/// this list on purpose: it's the absence of a claim, not one of the claims.
/// Mirrors `ProjectHealth` in `src/types/task.ts`.
pub const HEALTHS: [&str; 4] = ["on_track", "at_risk", "off_track", "paused"];

pub fn is_valid_health(h: &str) -> bool {
    HEALTHS.contains(&h)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectUpdate {
    pub id: String,
    pub project: String,
    pub author: String,
    pub body: String,
    /// The health this update set. `None` = it narrated without moving the dot.
    pub health: Option<String>,
    pub created_at: String,
}

const COLUMNS: &str = "id, project, author, body, health, created_at";

fn row_to_update(r: &rusqlite::Row) -> rusqlite::Result<ProjectUpdate> {
    Ok(ProjectUpdate {
        id: r.get(0)?,
        project: r.get(1)?,
        author: r.get(2)?,
        body: r.get(3)?,
        health: r.get(4)?,
        created_at: r.get(5)?,
    })
}

/// Post an update, optionally moving the project's health.
///
/// `health = None` records a note without changing the status — useful, and the
/// reason `projects.health` isn't simply derived from the newest row: "we're
/// still blocked on legal" shouldn't have to restate a status to be filed.
pub fn post(
    conn: &Connection,
    raw_project: &str,
    author: &str,
    body: &str,
    health: Option<&str>,
) -> Result<ProjectUpdate, AppError> {
    let project = crate::db::projects::normalize(raw_project);
    if project.is_empty() {
        return Err(AppError::Validation("project name must not be empty".into()));
    }
    let body = body.trim();
    if body.is_empty() {
        // An update with no words is the exact thing this table exists to stop.
        return Err(AppError::Validation(
            "an update needs a body — a health change with no explanation is what \
             this is here to prevent"
                .into(),
        ));
    }
    if let Some(h) = health {
        if !is_valid_health(h) {
            return Err(AppError::Validation(format!(
                "unknown health '{h}' (expected one of {})",
                HEALTHS.join(", ")
            )));
        }
    }
    let author = match author.trim() {
        "" => crate::db::comments::AUTHOR_ME,
        a => a,
    };

    // Posting an update about a project nobody has filed a task into yet must
    // work — that's how a project gets started from a session. Same reasoning as
    // `tasks::create`: without this the update log exists but the project has no
    // row on the board it's supposed to be reporting to.
    crate::db::projects::ensure_exists(conn, &project)?;

    if let Some(h) = health {
        conn.execute(
            "UPDATE projects SET health = ?1 WHERE name = ?2 COLLATE NOCASE",
            params![h, project],
        )?;
    }

    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO project_updates (id, project, author, body, health)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, project, author, body, health],
    )?;
    get(conn, &id)?.ok_or_else(|| AppError::NotFound("update vanished after insert".into()))
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<ProjectUpdate>, AppError> {
    let sql = format!("SELECT {COLUMNS} FROM project_updates WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query_map(params![id], row_to_update)?;
    match rows.next() {
        Some(u) => Ok(Some(u?)),
        None => Ok(None),
    }
}

/// A project's log, **newest first** — the opposite of `comments::for_task`.
///
/// A task's comment log reads top-down as a story you're catching up on; a
/// status log is read to answer "what's the latest", so the latest goes first.
///
/// `rowid` is the tiebreaker for the same reason as gotcha #12: millisecond
/// stamps still tie under a tight loop, and `id` is a random UUID, so without it
/// several updates posted in one second come back in arbitrary order.
pub fn for_project(
    conn: &Connection,
    raw_project: &str,
    limit: u32,
) -> Result<Vec<ProjectUpdate>, AppError> {
    let project = crate::db::projects::normalize(raw_project);
    let sql = format!(
        "SELECT {COLUMNS} FROM project_updates
          WHERE project = ?1 COLLATE NOCASE
          ORDER BY datetime(created_at) DESC, rowid DESC
          LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![project, limit.min(500)], row_to_update)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The most recent update for a project, or `None` if it has never been reported
/// on. That `None` is what the board renders as a gray dot and a stale warning.
pub fn latest(conn: &Connection, raw_project: &str) -> Result<Option<ProjectUpdate>, AppError> {
    Ok(for_project(conn, raw_project, 1)?.into_iter().next())
}

pub fn delete(conn: &Connection, id: &str) -> Result<(), AppError> {
    let n = conn.execute("DELETE FROM project_updates WHERE id = ?1", params![id])?;
    if n == 0 {
        return Err(AppError::NotFound(format!("update {id} not found")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::comments::{AUTHOR_CLAUDE, AUTHOR_ME};
    use crate::db::schema::test_conn;

    /// Same shape as `comments::add_and_read_in_chronological_order`, and for the
    /// same reason: a test that sleeps between inserts passes against broken
    /// ordering. Eight in a tight loop is what actually exercises the `rowid`
    /// tiebreaker.
    #[test]
    fn the_log_reads_newest_first_even_within_one_millisecond() {
        let conn = test_conn();
        for i in 0..8 {
            post(&conn, "cxventures", AUTHOR_ME, &format!("update {i}"), None).unwrap();
        }
        let bodies: Vec<_> = for_project(&conn, "cxventures", 50)
            .unwrap()
            .into_iter()
            .map(|u| u.body)
            .collect();
        assert_eq!(
            bodies,
            (0..8).rev().map(|i| format!("update {i}")).collect::<Vec<_>>(),
            "a status log is read for the latest, so newest must come first — and \
             the order must be stable when several land in one millisecond"
        );
        assert_eq!(latest(&conn, "cxventures").unwrap().unwrap().body, "update 7");
    }

    #[test]
    fn posting_an_update_sets_the_projects_health() {
        let conn = test_conn();
        crate::db::projects::create(&conn, "acme", crate::db::projects::ProjectKind::Area, None).unwrap();
        assert!(
            crate::db::projects::get_by_name(&conn, "acme").unwrap().unwrap().health.is_none(),
            "a new project must start as NEVER REPORTED, not as on_track"
        );

        post(&conn, "acme", AUTHOR_ME, "deck sent, waiting on Dana", Some("at_risk")).unwrap();
        assert_eq!(
            crate::db::projects::get_by_name(&conn, "acme").unwrap().unwrap().health.as_deref(),
            Some("at_risk")
        );

        // A narration-only update leaves the dot where it was.
        post(&conn, "acme", AUTHOR_ME, "still waiting", None).unwrap();
        assert_eq!(
            crate::db::projects::get_by_name(&conn, "acme").unwrap().unwrap().health.as_deref(),
            Some("at_risk"),
            "an update with no health must not clear the health"
        );
        assert!(latest(&conn, "acme").unwrap().unwrap().health.is_none());
    }

    #[test]
    fn a_blank_body_is_rejected() {
        let conn = test_conn();
        assert!(post(&conn, "p", AUTHOR_ME, "   ", Some("on_track")).is_err());
        assert!(post(&conn, "p", AUTHOR_ME, "", None).is_err());
        assert_eq!(post(&conn, "p", AUTHOR_ME, "  padded  ", None).unwrap().body, "padded");
    }

    #[test]
    fn a_blank_author_falls_back_to_me_and_claude_signs_its_own() {
        let conn = test_conn();
        assert_eq!(post(&conn, "p", "  ", "x", None).unwrap().author, AUTHOR_ME);
        assert_eq!(post(&conn, "p", AUTHOR_CLAUDE, "y", None).unwrap().author, "claude");
    }

    #[test]
    fn an_invalid_health_is_rejected_before_anything_is_written() {
        let conn = test_conn();
        assert!(post(&conn, "p", AUTHOR_ME, "body", Some("green")).is_err());
        assert!(
            for_project(&conn, "p", 10).unwrap().is_empty(),
            "a rejected health must not leave an update row behind"
        );
    }

    #[test]
    fn an_empty_project_name_is_rejected() {
        let conn = test_conn();
        assert!(post(&conn, "   ", AUTHOR_ME, "body", None).is_err());
    }

    /// Reporting on a project nobody has filed a task into yet is how a project
    /// gets started from a session — it must create the row it reports to.
    #[test]
    fn posting_about_an_unknown_project_gives_it_a_projects_row() {
        let conn = test_conn();
        assert!(crate::db::projects::list(&conn, true).unwrap().is_empty());
        post(&conn, "brand new", AUTHOR_ME, "kicked off", Some("on_track")).unwrap();
        let all = crate::db::projects::list(&conn, true).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "brand new");
        assert_eq!(all[0].health.as_deref(), Some("on_track"));
    }

    #[test]
    fn the_log_is_scoped_to_one_project_case_insensitively() {
        let conn = test_conn();
        post(&conn, "a", AUTHOR_ME, "for a", None).unwrap();
        post(&conn, "b", AUTHOR_ME, "for b", None).unwrap();
        assert_eq!(for_project(&conn, "a", 10).unwrap().len(), 1);
        assert_eq!(
            for_project(&conn, "A", 10).unwrap().len(),
            1,
            "project names match case-insensitively everywhere else; here too"
        );
        assert!(for_project(&conn, "c", 10).unwrap().is_empty());
    }

    #[test]
    fn delete_removes_one_entry() {
        let conn = test_conn();
        let u = post(&conn, "p", AUTHOR_ME, "oops", None).unwrap();
        delete(&conn, &u.id).unwrap();
        assert!(for_project(&conn, "p", 10).unwrap().is_empty());
        assert!(delete(&conn, &u.id).is_err(), "double delete should error");
    }
}
