//! Tags — shared labels, many-to-many with tasks.
//!
//! Distinct from `tasks.project`, which is one free-text field per task and
//! answers "where does this live". A tag answers "what kind of thing is this"
//! and crosses projects: `#waiting-on-someone`, `#quick-win`, `#billable` are
//! all orthogonal to which repo the work is in.
//!
//! Names are compared case-insensitively (`COLLATE NOCASE` + a UNIQUE index), so
//! `Billable` and `billable` are the same tag. Without that you end up with
//! three spellings of the same label and filtering silently misses rows.
//!
//! # A tag is emergent; a project is curated
//!
//! This is the asymmetry that governs everything below, and it is already in the
//! schema: `projects` became its own table in v4 *specifically* so a project
//! could exist with no tasks in it, because otherwise a "+ New project" button
//! is impossible. **Tags never got that argument and must not be given it.**
//! [`upsert`] has exactly one caller — [`attach`] — so a tag comes into
//! existence only by being put on a task, and there is deliberately no
//! "+ New tag" anywhere in the app or the MCP surface.
//!
//! So a tag carrying no tasks is not an empty vocabulary entry waiting to be
//! used again, it is **garbage** — and [`gc`] deletes it rather than leaving it
//! to accumulate. Agents file tags ad hoc and schemes get abandoned, so without
//! this the sidebar fills up with labels nobody will use again and the list
//! stops being worth reading. `color` is the corroborating detail: nothing
//! outside this module has ever set it, so a tag holds no state that
//! re-creating it by typing the name again would lose.
//!
//! Two boundaries on that, both load-bearing:
//!
//! * **GC keys on having no `task_tags` rows, NEVER on
//!   [`TagWithCount::open_count`].** They are different numbers — a tag sitting
//!   on three finished tasks counts 0 open while being very much still in use.
//!   Collapsing the two destroys the labels on your archive and silently breaks
//!   filtering over it. Hiding such a tag from the sidebar is a *display*
//!   decision the frontend makes and reverses the moment a task reopens.
//! * **GC never touches a tag that is on tasks**, so it is not a substitute for
//!   [`delete`]. When an agent tags eight tasks `#urgnet`, cleaning that up is a
//!   bulk detach, which is a different operation and stays explicit.

use crate::error::AppError;
use rusqlite::{params, params_from_iter, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagWithCount {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    /// The sidebar icon (`db::appearance`). Only on THIS struct: the per-task
    /// [`Tag`] rides every task listing, and an inline image has no business
    /// there.
    pub icon: Option<String>,
    /// Open tasks carrying this tag — what the sidebar badge shows.
    pub open_count: i64,
}

/// Normalize a tag name: trimmed, no leading `#`, collapsed inner whitespace.
/// `#Quick Win ` and `quick  win` both become `quick win`.
pub fn normalize(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('#')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A PERSON is a tag whose name starts with `@` — `@sam`, `@dana`. A
/// convention on the existing table, not a column and not a contacts book
/// (invariant 29): the question it answers is "which of my open work involves
/// this person", and a tag already answers "which tasks carry this label"
/// with a facet, a chip, a sidebar row and sync, all for free. The `@` is the
/// whole convention; everything else is ordinary tag machinery.
pub const PERSON_PREFIX: char = '@';

pub fn is_person(name: &str) -> bool {
    name.starts_with(PERSON_PREFIX) && name.len() > 1
}

/// The tag a person's name resolves to: `sam`, `@Sam` and `@sam` all
/// become `@sam`. Empty when there is no name.
pub fn person_tag(raw: &str) -> String {
    let inner = normalize(raw.trim().trim_start_matches(PERSON_PREFIX));
    if inner.is_empty() {
        String::new()
    } else {
        format!("{PERSON_PREFIX}{inner}")
    }
}

// ── Vetting a NEW tag ──────────────────────────────────────────────────────
//
// Emergence is the design (module header), and on its own it produced 128 tags
// for 232 tasks, 70 of them used exactly once: every agent session coins its
// own word because nothing shows it the words that already exist. So a tag an
// AGENT is about to create is vetted first. Chris typing a tag in the app is
// not — he is the vocabulary's owner, and the app never calls this.

/// Tags code attaches for their behaviour. Never vetted: the code that puts
/// them on must not be refused by the guard that protects the vocabulary.
pub const SYSTEM_TAGS: &[&str] = &[crate::db::tasks::DECISION_TAG, "agent-filed"];

/// Names that are already a FIELD or a facet. Tagging them duplicates state the
/// task already carries, and the copy drifts: `#tripwire` stayed on tasks whose
/// tripwire had been cleared.
const FIELD_NAMES: &[&str] = &[
    "inbox", "todo", "in-progress", "queued", "done", "blocked", "waiting",
    "deferred", "tripwire", "tripwires", "p0", "p1", "p2", "p3", "urgent",
];

/// Why a new tag was not created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewTagIssue {
    /// The name is a project, a project's repo folder, or a repo a task runs in
    /// — `project` already answers "where does this live".
    ShadowsProject { tag: String, project: String },
    /// The name restates a status, priority, or Waiting facet.
    IsAField { tag: String },
    /// Close to tags that already exist (plural, prefix, a typo away).
    NearDuplicate { tag: String, existing: Vec<String> },
    /// Nothing like it exists. Fine — when it is deliberate.
    Unknown { tag: String },
}

impl NewTagIssue {
    /// Can the caller override this by saying the tag is deliberate? A
    /// near-duplicate or brand-new label can be; restating a project or a
    /// field cannot, because the right home for that information exists.
    pub fn overridable(&self) -> bool {
        matches!(self, Self::NearDuplicate { .. } | Self::Unknown { .. })
    }
}

/// Lowercase letters and digits only — `zen-mcp`, `Zen MCP` and `zenmcp` are
/// one key.
fn key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The key minus a trailing version number and a plural `s`: `release-2` →
/// `release`, `tripwires` → `tripwire`.
fn stem(name: &str) -> String {
    let k = key(name);
    let k = k.trim_end_matches(|c: char| c.is_ascii_digit());
    let k = if k.len() > 3 { k.strip_suffix('s').unwrap_or(k) } else { k };
    k.to_string()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Would someone scanning the sidebar read these two as the same label?
///
/// Deliberately conservative — a false "did you mean" costs an agent one
/// retry, but a rule loose enough to pair `seo` with `pseo` teaches agents to
/// override reflexively, and then the guard protects nothing.
pub fn similar(a: &str, b: &str) -> bool {
    let (sa, sb) = (stem(a), stem(b));
    if sa.is_empty() || sb.is_empty() {
        return false;
    }
    if sa == sb {
        return true;
    }
    let (short, long) = if sa.len() <= sb.len() { (&sa, &sb) } else { (&sb, &sa) };
    // `cert` / `certification`, `draft` / `drafting`, `decision` / `decision-gate`.
    if short.len() >= 4 && long.starts_with(short.as_str()) {
        return true;
    }
    // A typo away. Tighter on short words, where one letter is a different
    // word, and never across a different first letter: `certification` and
    // `verification` are two edits apart and two different things.
    let limit = match short.len() {
        0..=4 => 0,
        5..=7 => 1,
        _ => 2,
    };
    limit > 0 && sa.chars().next() == sb.chars().next() && edit_distance(&sa, &sb) <= limit
}

fn basename(path: &str) -> Option<&str> {
    path.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty())
}

/// Every name that means "where the work lives": project names, the folders
/// of their repos, and the folder of every repo a task runs in (a repo with no
/// project row is still a place, not a kind).
fn place_names(conn: &Connection) -> Result<Vec<String>, AppError> {
    let mut out = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT name FROM projects
         UNION SELECT project FROM tasks WHERE project IS NOT NULL
         UNION SELECT repo_path FROM project_repos
         UNION SELECT repo_path FROM tasks WHERE repo_path IS NOT NULL",
    )?;
    for row in stmt.query_map([], |r| r.get::<_, String>(0))? {
        let s = row?;
        if s.contains('/') {
            if let Some(b) = basename(&s) {
                out.push(b.to_string());
            }
        } else {
            out.push(s);
        }
    }
    Ok(out)
}

/// What is wrong with creating `raw_name`, or `None` when nothing is: the tag
/// already exists, is a person, or is a system tag. Read-only.
pub fn vet_new(conn: &Connection, raw_name: &str) -> Result<Option<NewTagIssue>, AppError> {
    let tag = normalize(raw_name);
    if tag.is_empty() || is_person(&tag) || SYSTEM_TAGS.contains(&tag.as_str()) {
        return Ok(None);
    }
    if get_by_name(conn, &tag)?.is_some() {
        return Ok(None);
    }
    let k = key(&tag);
    if FIELD_NAMES.iter().any(|f| key(f) == k) {
        return Ok(Some(NewTagIssue::IsAField { tag }));
    }
    if let Some(project) = place_names(conn)?.into_iter().find(|p| key(p) == k) {
        return Ok(Some(NewTagIssue::ShadowsProject { tag, project }));
    }
    // Most-used first, so the suggestion an agent takes is the established one.
    let existing: Vec<String> = list_with_counts(conn)?
        .into_iter()
        .filter(|t| !is_person(&t.name) && similar(&t.name, &tag))
        .map(|t| t.name)
        .take(3)
        .collect();
    if existing.is_empty() {
        Ok(Some(NewTagIssue::Unknown { tag }))
    } else {
        Ok(Some(NewTagIssue::NearDuplicate { tag, existing }))
    }
}

/// What the weekly `script:tag-audit` tripwire reports (`cxtasks-runner
/// --tag-audit`). The guard in [`vet_new`] stops an agent coining a duplicate
/// through the MCP; this catches what got past it anyway — an older
/// `cxtasks-mcp` still running from a session that predates the guard, a
/// `new_tags: true` used reflexively, or an app-typed near-miss.
#[derive(Debug, Default)]
pub struct TagAudit {
    /// Pairs of existing tags that [`similar`] reads as one label.
    pub near_duplicates: Vec<(String, String)>,
    /// Tags created in the window that restate a project, repo or field.
    pub misplaced: Vec<(String, String)>,
    /// Tags created in the window that are still on exactly one task.
    pub new_singletons: Vec<String>,
}

/// Five new one-off tags in a week is the drift rate the cleanup of
/// 2026-10-02 found (70 singletons in about ten weeks). Below that, a new
/// label is somebody using the vocabulary, not eroding it.
pub const AUDIT_SINGLETON_LIMIT: usize = 5;

impl TagAudit {
    pub fn tripped(&self) -> bool {
        !self.near_duplicates.is_empty()
            || !self.misplaced.is_empty()
            || self.new_singletons.len() >= AUDIT_SINGLETON_LIMIT
    }

    pub fn evidence(&self) -> String {
        let mut out = Vec::new();
        if !self.near_duplicates.is_empty() {
            out.push(format!(
                "Near-duplicate tags (merge with Rename Tag in the sidebar): {}",
                self.near_duplicates
                    .iter()
                    .map(|(a, b)| format!("#{a} ~ #{b}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !self.misplaced.is_empty() {
            out.push(format!(
                "New tags restating a project or field: {}",
                self.misplaced
                    .iter()
                    .map(|(t, why)| format!("#{t} ({why})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out.push(format!(
            "{} new one-off tag(s) this week (trips at {AUDIT_SINGLETON_LIMIT}){}",
            self.new_singletons.len(),
            if self.new_singletons.is_empty() {
                String::new()
            } else {
                format!(": {}", self.new_singletons.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(" "))
            }
        ));
        out.join("\n")
    }
}

/// Score the vocabulary. Read-only. Near-duplicates are judged over EVERY tag
/// (a pair is a problem whenever it was made); the other two only over tags
/// created in the last `days`, because the ones the cleanup deliberately kept
/// (`#globex` on unfiled work) would otherwise trip it every week forever.
pub fn audit(conn: &Connection, days: i64) -> Result<TagAudit, AppError> {
    let all = list_with_counts(conn)?;
    let mut report = TagAudit::default();
    let labels: Vec<&TagWithCount> = all.iter().filter(|t| !is_person(&t.name)).collect();
    for (i, a) in labels.iter().enumerate() {
        for b in &labels[i + 1..] {
            if similar(&a.name, &b.name) {
                report.near_duplicates.push((a.name.clone(), b.name.clone()));
            }
        }
    }

    let mut stmt = conn.prepare(
        "SELECT g.id, g.name, COUNT(tt.task_id)
           FROM tags g LEFT JOIN task_tags tt ON tt.tag_id = g.id
          WHERE datetime(g.created_at) >= datetime('now', ?1)
          GROUP BY g.id",
    )?;
    let recent: Vec<(String, String, i64)> = stmt
        .query_map(params![format!("-{days} days")], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let places = place_names(conn)?;
    for (_, name, uses) in recent {
        if is_person(&name) || SYSTEM_TAGS.contains(&name.as_str()) {
            continue;
        }
        let k = key(&name);
        if FIELD_NAMES.iter().any(|f| key(f) == k) {
            report.misplaced.push((name, "a field".into()));
        } else if let Some(p) = places.iter().find(|p| key(p) == k) {
            report.misplaced.push((name, format!("project/repo {p}")));
        } else if uses == 1 {
            report.new_singletons.push(name);
        }
    }
    Ok(report)
}

/// Find or create a tag by name. Returns the tag.
pub fn upsert(conn: &Connection, raw_name: &str, color: Option<&str>) -> Result<Tag, AppError> {
    let name = normalize(raw_name);
    if name.is_empty() {
        return Err(AppError::Validation("tag name must not be empty".into()));
    }
    if name.len() == 1 && name.starts_with(PERSON_PREFIX) {
        return Err(AppError::Validation("a person tag needs a name after the @".into()));
    }

    if let Some(existing) = get_by_name(conn, &name)? {
        // Only overwrite the colour when one was explicitly supplied, so
        // tagging a task doesn't wipe a colour set earlier.
        if let Some(c) = color {
            conn.execute(
                "UPDATE tags SET color = ?1 WHERE id = ?2",
                params![c, existing.id],
            )?;
            return Ok(Tag {
                color: Some(c.to_string()),
                ..existing
            });
        }
        return Ok(existing);
    }

    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO tags (id, name, color) VALUES (?1, ?2, ?3)",
        params![id, name, color],
    )?;
    Ok(Tag {
        id,
        name,
        color: color.map(str::to_string),
    })
}

pub fn get_by_name(conn: &Connection, raw_name: &str) -> Result<Option<Tag>, AppError> {
    let name = normalize(raw_name);
    let mut stmt = conn.prepare("SELECT id, name, color FROM tags WHERE name = ?1 COLLATE NOCASE")?;
    let mut rows = stmt.query_map(params![name], |r| {
        Ok(Tag {
            id: r.get(0)?,
            name: r.get(1)?,
            color: r.get(2)?,
        })
    })?;
    match rows.next() {
        Some(t) => Ok(Some(t?)),
        None => Ok(None),
    }
}

/// Every tag with its open-task count, for the sidebar.
pub fn list_with_counts(conn: &Connection) -> Result<Vec<TagWithCount>, AppError> {
    let closed = crate::db::tasks::CLOSED_STATUSES
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT g.id, g.name, g.color,
                COALESCE(SUM(CASE WHEN t.status IS NOT NULL
                                   AND t.status NOT IN ({closed})
                                   AND t.deleted_at IS NULL
                                   AND t.archived_at IS NULL THEN 1 ELSE 0 END), 0),
                g.icon
           FROM tags g
           LEFT JOIN task_tags tt ON tt.tag_id = g.id
           LEFT JOIN tasks t      ON t.id = tt.task_id
          GROUP BY g.id
          ORDER BY 4 DESC, g.name ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(TagWithCount {
                id: r.get(0)?,
                name: r.get(1)?,
                color: r.get(2)?,
                open_count: r.get(3)?,
                icon: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn for_task(conn: &Connection, task_id: &str) -> Result<Vec<Tag>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT g.id, g.name, g.color
           FROM task_tags tt JOIN tags g ON g.id = tt.tag_id
          WHERE tt.task_id = ?1
          ORDER BY g.name ASC",
    )?;
    let rows = stmt
        .query_map(params![task_id], |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
                color: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Attach a tag to a task, creating the tag if it's new. Idempotent.
pub fn attach(conn: &Connection, task_id: &str, raw_name: &str) -> Result<Tag, AppError> {
    if crate::db::tasks::get(conn, task_id)?.is_none() {
        return Err(AppError::NotFound(format!("task {task_id} not found")));
    }
    let tag = upsert(conn, raw_name, None)?;
    conn.execute(
        "INSERT OR IGNORE INTO task_tags (task_id, tag_id) VALUES (?1, ?2)",
        params![task_id, tag.id],
    )?;
    Ok(tag)
}

pub fn detach(conn: &Connection, task_id: &str, raw_name: &str) -> Result<(), AppError> {
    let Some(tag) = get_by_name(conn, raw_name)? else {
        return Ok(());
    };
    conn.execute(
        "DELETE FROM task_tags WHERE task_id = ?1 AND tag_id = ?2",
        params![task_id, tag.id],
    )?;
    // Taking a tag off its last task is the commonest way one becomes garbage.
    gc(conn)?;
    Ok(())
}

/// Replace a task's tags wholesale with `names`.
pub fn set_for_task(conn: &Connection, task_id: &str, names: &[String]) -> Result<(), AppError> {
    conn.execute("DELETE FROM task_tags WHERE task_id = ?1", params![task_id])?;
    for name in names {
        if !normalize(name).is_empty() {
            attach(conn, task_id, name)?;
        }
    }
    // AFTER the re-attach, never before: this clears the task's tags first, so
    // collecting in between would delete a tag that is about to be put straight
    // back — surviving only because `upsert` would recreate it, but churning the
    // row's id for anything holding one.
    gc(conn)?;
    Ok(())
}

/// Task ids carrying ALL of `names` (AND semantics — narrowing, which is what
/// you want when combining filters).
pub fn task_ids_with_all(conn: &Connection, names: &[String]) -> Result<Vec<String>, AppError> {
    let normalized: Vec<String> = names
        .iter()
        .map(|n| normalize(n))
        .filter(|n| !n.is_empty())
        .collect();
    if normalized.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; normalized.len()].join(", ");
    let sql = format!(
        "SELECT tt.task_id
           FROM task_tags tt JOIN tags g ON g.id = tt.tag_id
          WHERE g.name IN ({placeholders}) COLLATE NOCASE
          GROUP BY tt.task_id
         HAVING COUNT(DISTINCT g.id) = {}",
        normalized.len()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(normalized.iter()), |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Does the task carry this tag? The name is normalized like everywhere else.
pub fn has(conn: &Connection, task_id: &str, raw_name: &str) -> Result<bool, AppError> {
    let name = normalize(raw_name);
    if name.is_empty() {
        return Ok(false);
    }
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM task_tags tt JOIN tags g ON g.id = tt.tag_id
          WHERE tt.task_id = ?1 AND g.name = ?2 COLLATE NOCASE",
        rusqlite::params![task_id, name],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Ids of tasks involving ANY of these people — `sam`, `@sam` and `@Sam`
/// all mean the tag `@sam`. An empty list, or a `*`, means "any person at
/// all", which is what the October check asks: of the quarter's closed work,
/// how much had a person on it. OR rather than AND, unlike [`task_ids_with_all`]:
/// "Sam or Dana" is the natural question and "Sam and Dana" is rare.
pub fn task_ids_with_any_person(
    conn: &Connection,
    people: &[String],
) -> Result<Vec<String>, AppError> {
    let names: Vec<String> = people
        .iter()
        .map(|p| p.trim())
        .filter(|p| *p != "*" && *p != "any")
        .map(person_tag)
        .filter(|n| !n.is_empty())
        .collect();
    let sql = if names.is_empty() {
        "SELECT DISTINCT tt.task_id FROM task_tags tt JOIN tags g ON g.id = tt.tag_id
          WHERE g.name LIKE '@_%'"
            .to_string()
    } else {
        let placeholders = vec!["?"; names.len()].join(", ");
        format!(
            "SELECT DISTINCT tt.task_id FROM task_tags tt JOIN tags g ON g.id = tt.tag_id
              WHERE g.name IN ({placeholders}) COLLATE NOCASE"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(names.iter()), |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Drop every tag that is no longer on any task. Returns how many went.
///
/// See the module header for why this is deletion rather than hiding: a tag
/// only ever exists because something attached it, so one with no pairings left
/// is spent. Callers don't opt in — this is wired into the write paths that can
/// orphan a tag ([`detach`], [`set_for_task`], and hard task deletion), so it
/// runs for the app, the MCP and the runner alike without any of them knowing.
///
/// **Hooked into the writes rather than run as a sweep at startup** on purpose.
/// A startup sweep would leave a tag sitting in the sidebar for the rest of the
/// session after you strip it off the last task, which is precisely the
/// staleness this exists to remove — you'd watch a `0` linger until relaunch.
///
/// Safe under the binary skew this DB always has: a leftover older binary
/// simply doesn't collect, so a stale tag survives until the next write that
/// does. That is the old behaviour, not a broken one — there is nothing to
/// backfill and no way for the two to disagree about a tag that still has rows.
pub fn gc(conn: &Connection) -> Result<usize, AppError> {
    let n = conn.execute(
        "DELETE FROM tags
          WHERE NOT EXISTS (SELECT 1 FROM task_tags WHERE tag_id = tags.id)",
        [],
    )?;
    Ok(n)
}

/// How many tasks carry this tag, closed and hidden ones included.
///
/// Deliberately NOT [`TagWithCount::open_count`]: this is what the delete
/// confirm quotes, and it has to describe everything the delete would actually
/// strip. Quoting the open count would promise "this affects 1 task" while
/// silently unlabelling nine finished ones.
pub fn usage_count(conn: &Connection, raw_name: &str) -> Result<i64, AppError> {
    let name = normalize(raw_name);
    Ok(conn.query_row(
        "SELECT COUNT(*)
           FROM task_tags tt JOIN tags g ON g.id = tt.tag_id
          WHERE g.name = ?1 COLLATE NOCASE",
        params![name],
        |r| r.get(0),
    )?)
}

/// Every task's tag names, keyed by task id — one query for the whole board.
///
/// The list renders tags per row, and asking per row is N+1. This is the same
/// shape `deps::blocked_task_ids` already uses for the blocked badge: one
/// derived lookup fetched alongside the tasks, not a field on [`Task`].
///
/// Not filtered to live tasks, so the Trash and the Archive keep their labels.
pub fn by_task(conn: &Connection) -> Result<HashMap<String, Vec<String>>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT tt.task_id, g.name
           FROM task_tags tt JOIN tags g ON g.id = tt.tag_id
          ORDER BY g.name ASC",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for (task_id, name) in rows {
        map.entry(task_id).or_default().push(name);
    }
    Ok(map)
}

/// Delete a tag everywhere. The `task_tags` rows cascade.
///
/// For the case [`gc`] can't reach: a tag that IS still on tasks, which you
/// want gone from all of them at once. Destructive by design — it unlabels
/// every task carrying it — so the caller is expected to have confirmed against
/// [`usage_count`] first.
pub fn delete(conn: &Connection, raw_name: &str) -> Result<(), AppError> {
    let name = normalize(raw_name);
    conn.execute("DELETE FROM tags WHERE name = ?1 COLLATE NOCASE", params![name])?;
    Ok(())
}

pub fn rename(conn: &Connection, from: &str, to: &str) -> Result<Tag, AppError> {
    let old = normalize(from);
    let new = normalize(to);
    if new.is_empty() {
        return Err(AppError::Validation("tag name must not be empty".into()));
    }
    let Some(tag) = get_by_name(conn, &old)? else {
        return Err(AppError::NotFound(format!("tag '{old}' not found")));
    };
    // Renaming onto an existing tag would violate UNIQUE; merge instead, which
    // is what the user means by "rename A to B" when B already exists.
    if let Some(target) = get_by_name(conn, &new)? {
        if target.id != tag.id {
            conn.execute(
                "UPDATE OR IGNORE task_tags SET tag_id = ?1 WHERE tag_id = ?2",
                params![target.id, tag.id],
            )?;
            conn.execute("DELETE FROM tags WHERE id = ?1", params![tag.id])?;
            return Ok(target);
        }
    }
    conn.execute("UPDATE tags SET name = ?1 WHERE id = ?2", params![new, tag.id])?;
    Ok(Tag { name: new, ..tag })
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
    fn normalize_strips_hash_case_and_extra_space() {
        assert_eq!(normalize("  #Quick Win "), "quick win");
        assert_eq!(normalize("quick   win"), "quick win");
        assert_eq!(normalize("#"), "");
        assert_eq!(normalize("   "), "");
    }

    #[test]
    fn tags_are_shared_not_duplicated() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        attach(&conn, &a, "billable").unwrap();
        attach(&conn, &b, "Billable").unwrap(); // different case

        let all = list_with_counts(&conn).unwrap();
        assert_eq!(all.len(), 1, "case variants must be the same tag: {all:?}");
        assert_eq!(all[0].open_count, 2);
    }

    #[test]
    fn attach_is_idempotent() {
        let conn = test_conn();
        let a = task(&conn, "a");
        attach(&conn, &a, "x").unwrap();
        attach(&conn, &a, "x").unwrap();
        assert_eq!(for_task(&conn, &a).unwrap().len(), 1);
    }

    #[test]
    fn detach_removes_only_that_pairing() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        attach(&conn, &a, "shared").unwrap();
        attach(&conn, &b, "shared").unwrap();
        detach(&conn, &a, "shared").unwrap();
        assert!(for_task(&conn, &a).unwrap().is_empty());
        assert_eq!(for_task(&conn, &b).unwrap().len(), 1, "the tag itself survives");
    }

    #[test]
    fn set_for_task_replaces_wholesale() {
        let conn = test_conn();
        let a = task(&conn, "a");
        attach(&conn, &a, "old").unwrap();
        set_for_task(&conn, &a, &["new".into(), "newer".into()]).unwrap();
        let names: Vec<_> = for_task(&conn, &a).unwrap().into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["new", "newer"]);
    }

    #[test]
    fn filtering_by_tags_is_AND_not_OR() {
        let conn = test_conn();
        let both = task(&conn, "both");
        let one = task(&conn, "one");
        attach(&conn, &both, "billable").unwrap();
        attach(&conn, &both, "urgent").unwrap();
        attach(&conn, &one, "billable").unwrap();

        let ids = task_ids_with_all(&conn, &["billable".into(), "urgent".into()]).unwrap();
        assert_eq!(ids, vec![both], "must narrow, not widen");

        let ids = task_ids_with_all(&conn, &["billable".into()]).unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn open_count_ignores_closed_tasks() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        attach(&conn, &a, "x").unwrap();
        attach(&conn, &b, "x").unwrap();
        tasks::complete(&conn, &b).unwrap();
        assert_eq!(list_with_counts(&conn).unwrap()[0].open_count, 1);
    }

    /// The inverse of the old `a_tag_with_no_tasks_still_lists_at_zero`. A tag
    /// exists only because something attached it (see the module header), so
    /// once nothing carries it there is nothing to keep — it is collected, not
    /// listed at zero.
    #[test]
    fn a_tag_with_no_tasks_left_is_collected_not_listed() {
        let conn = test_conn();
        let a = task(&conn, "a");
        attach(&conn, &a, "spent").unwrap();
        detach(&conn, &a, "spent").unwrap();
        assert!(
            list_with_counts(&conn).unwrap().is_empty(),
            "an unreferenced tag must not survive its last task"
        );
    }

    /// The boundary GC must not cross. `open_count` is 0 here, but the tag is
    /// still on a task — collecting it would strip the label off the archive and
    /// silently break filtering over closed work.
    #[test]
    fn a_tag_whose_tasks_are_all_closed_survives_at_zero() {
        let conn = test_conn();
        let a = task(&conn, "a");
        attach(&conn, &a, "shipped").unwrap();
        tasks::complete(&conn, &a).unwrap();
        // Force a collection pass; the tag still has a pairing, so it stays.
        gc(&conn).unwrap();
        let all = list_with_counts(&conn).unwrap();
        assert_eq!(all.len(), 1, "still in use — only its OPEN count is 0: {all:?}");
        assert_eq!(all[0].open_count, 0);
        assert_eq!(usage_count(&conn, "shipped").unwrap(), 1);
    }

    #[test]
    fn deleting_a_task_collects_the_tags_it_was_the_last_holder_of() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        attach(&conn, &a, "solo").unwrap();
        attach(&conn, &a, "shared").unwrap();
        attach(&conn, &b, "shared").unwrap();

        tasks::delete(&conn, &a).unwrap();
        let names: Vec<_> = list_with_counts(&conn)
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["shared"], "only the orphan goes");
    }

    /// A trashed task keeps its pairings, so its tags must survive — restoring
    /// it has to bring the labels back with it.
    #[test]
    fn trashing_a_task_does_not_collect_its_tags() {
        let conn = test_conn();
        let a = task(&conn, "a");
        attach(&conn, &a, "later").unwrap();
        tasks::trash(&conn, &a).unwrap();

        let all = list_with_counts(&conn).unwrap();
        assert_eq!(all.len(), 1, "a soft delete must not destroy the label");
        assert_eq!(all[0].open_count, 0, "but it is not open work either");

        tasks::restore(&conn, &a).unwrap();
        assert_eq!(for_task(&conn, &a).unwrap().len(), 1, "restore brings it back");
    }

    /// Re-tagging clears a task's tags before re-attaching them. Collecting
    /// between the two halves would delete a tag that is about to come back.
    #[test]
    fn re_tagging_a_task_keeps_the_tags_it_still_has() {
        let conn = test_conn();
        let a = task(&conn, "a");
        set_for_task(&conn, &a, &["keep".into(), "drop".into()]).unwrap();
        let kept_id = get_by_name(&conn, "keep").unwrap().unwrap().id;

        set_for_task(&conn, &a, &["keep".into(), "added".into()]).unwrap();
        let names: Vec<_> = list_with_counts(&conn)
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names.len(), 2, "the dropped tag goes, the others stay: {names:?}");
        assert!(names.contains(&"keep".to_string()));
        assert!(names.contains(&"added".to_string()));
        assert_eq!(
            get_by_name(&conn, "keep").unwrap().unwrap().id,
            kept_id,
            "a surviving tag must keep its identity, not be destroyed and recreated"
        );
    }

    /// GC handles the empty case; this is the one it deliberately cannot reach.
    #[test]
    fn delete_removes_a_tag_that_is_still_in_use() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        attach(&conn, &a, "urgnet").unwrap();
        attach(&conn, &b, "urgnet").unwrap();
        assert_eq!(usage_count(&conn, "urgnet").unwrap(), 2);

        delete(&conn, "urgnet").unwrap();
        assert!(list_with_counts(&conn).unwrap().is_empty());
        assert!(for_task(&conn, &a).unwrap().is_empty(), "and it leaves the tasks");
        assert!(for_task(&conn, &b).unwrap().is_empty());
    }

    #[test]
    fn by_task_groups_every_tag_under_its_task() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        attach(&conn, &a, "one").unwrap();
        attach(&conn, &a, "two").unwrap();
        attach(&conn, &b, "two").unwrap();

        let map = by_task(&conn).unwrap();
        assert_eq!(map[&a], vec!["one", "two"]);
        assert_eq!(map[&b], vec!["two"]);
        assert_eq!(map.len(), 2, "a task with no tags has no entry");
    }

    #[test]
    fn rename_onto_an_existing_tag_merges_instead_of_failing() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let b = task(&conn, "b");
        attach(&conn, &a, "wip").unwrap();
        attach(&conn, &b, "in-progress").unwrap();

        rename(&conn, "wip", "in-progress").unwrap();
        let all = list_with_counts(&conn).unwrap();
        assert_eq!(all.len(), 1, "should have merged: {all:?}");
        assert_eq!(all[0].name, "in-progress");
        assert_eq!(all[0].open_count, 2);
    }

    #[test]
    fn empty_names_are_rejected_and_skipped() {
        let conn = test_conn();
        let a = task(&conn, "a");
        assert!(upsert(&conn, "  #  ", None).is_err());
        set_for_task(&conn, &a, &["".into(), "real".into()]).unwrap();
        assert_eq!(for_task(&conn, &a).unwrap().len(), 1);
    }

    /// Every pair here was a real duplicate in the live database on 2026-10-02.
    #[test]
    fn similar_catches_the_duplicates_that_actually_happened() {
        for (a, b) in [
            ("certification", "certs"),
            ("tripwire", "tripwires"),
            ("drafting", "drafts"),
            ("decision", "decision-gate"),
            ("release", "release-2"),
            ("deployment", "deploy"),
            ("schema", "scheme"),
            ("Quick Win", "quick-win"),
        ] {
            assert!(similar(a, b), "{a} ~ {b}");
            assert!(similar(b, a), "{b} ~ {a} (symmetric)");
        }
    }

    /// The other half of the contract: distinct labels must stay distinct, or
    /// agents learn to override every refusal and the guard protects nothing.
    #[test]
    fn similar_leaves_distinct_short_labels_alone() {
        for (a, b) in [
            ("seo", "pseo"),
            ("risk", "disk"),
            ("tax", "taxes-2"),
            ("billing", "billable"),
            ("ledger", "legal"),
            ("cost", "content"),
            ("ios", "ops"),
            ("finance", "money"),
            ("certification", "verification"),
        ] {
            assert!(!similar(a, b), "{a} !~ {b}");
        }
    }

    #[test]
    fn vet_new_passes_existing_people_and_system_tags() {
        let conn = test_conn();
        let a = task(&conn, "a");
        attach(&conn, &a, "billable").unwrap();
        for name in ["billable", "#Billable", "@sam", "decision", "agent-filed"] {
            assert_eq!(vet_new(&conn, name).unwrap(), None, "{name}");
        }
    }

    #[test]
    fn vet_new_names_the_near_duplicate() {
        let conn = test_conn();
        let a = task(&conn, "a");
        attach(&conn, &a, "certification").unwrap();
        assert_eq!(
            vet_new(&conn, "certs").unwrap(),
            Some(NewTagIssue::NearDuplicate {
                tag: "certs".into(),
                existing: vec!["certification".into()],
            })
        );
        assert_eq!(
            vet_new(&conn, "roth").unwrap(),
            Some(NewTagIssue::Unknown { tag: "roth".into() })
        );
    }

    #[test]
    fn vet_new_refuses_a_project_or_repo_folder_and_fields() {
        let conn = test_conn();
        tasks::create(
            &conn,
            &NewTask {
                title: "x".into(),
                project: Some("Artist Advisory".into()),
                repo_path: Some("/Users/c/Projects/zen-mcp".into()),
                ..Default::default()
            },
        )
        .unwrap();
        for (name, project) in [("artist-advisory", "Artist Advisory"), ("ZenMCP", "zen-mcp")] {
            match vet_new(&conn, name).unwrap() {
                Some(NewTagIssue::ShadowsProject { project: p, .. }) => assert_eq!(p, project),
                other => panic!("{name}: {other:?}"),
            }
        }
        let issue = vet_new(&conn, "tripwire").unwrap().unwrap();
        assert_eq!(issue, NewTagIssue::IsAField { tag: "tripwire".into() });
        assert!(!issue.overridable());
    }

    #[test]
    fn audit_trips_on_a_near_duplicate_and_on_a_burst_of_one_offs() {
        let conn = test_conn();
        let a = task(&conn, "a");
        let shared = task(&conn, "shared");
        // On two tasks, so it is vocabulary, not a one-off.
        attach(&conn, &a, "billable").unwrap();
        attach(&conn, &shared, "billable").unwrap();
        attach(&conn, &a, "@sam").unwrap();
        let clean = audit(&conn, 7).unwrap();
        assert!(!clean.tripped(), "{}", clean.evidence());

        for t in ["one", "two", "three", "four"] {
            attach(&conn, &a, t).unwrap();
        }
        assert!(!audit(&conn, 7).unwrap().tripped(), "four is under the limit");
        attach(&conn, &a, "five").unwrap();
        let burst = audit(&conn, 7).unwrap();
        assert!(burst.tripped());
        assert!(burst.evidence().contains("#five"), "{}", burst.evidence());

        let conn = test_conn();
        let b = task(&conn, "b");
        attach(&conn, &b, "invoice").unwrap();
        attach(&conn, &b, "invoices").unwrap();
        let dup = audit(&conn, 7).unwrap();
        assert_eq!(dup.near_duplicates, vec![("invoice".to_string(), "invoices".to_string())]);
    }

    /// Old tags the cleanup deliberately kept must not trip it every week —
    /// only tags made inside the window are judged for being misplaced.
    #[test]
    fn audit_judges_misplaced_tags_only_inside_the_window() {
        let conn = test_conn();
        let a = tasks::create(
            &conn,
            &NewTask { title: "x".into(), project: Some("CXMail".into()), ..Default::default() },
        )
        .unwrap()
        .id;
        attach(&conn, &a, "cxmail").unwrap();
        assert_eq!(audit(&conn, 7).unwrap().misplaced.len(), 1);
        conn.execute("UPDATE tags SET created_at = datetime('now', '-30 days')", []).unwrap();
        assert!(!audit(&conn, 7).unwrap().tripped());
    }
}
