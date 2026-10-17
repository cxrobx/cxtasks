//! First-class projects and sections.
//!
//! # Why projects needed a table
//!
//! `tasks.project` is still the column tasks are grouped by, but until v4 that
//! was the *only* record of a project's existence — so a project existed only
//! while some task referenced it. That makes "+ New project" impossible: you'd
//! create one and it would vanish before you could file anything into it. The
//! `projects` table gives a project independent existence; the text column stays
//! as the join key so nothing about existing tasks had to change.
//!
//! # Sections
//!
//! Subdivisions for grouping. A section with `project = Some(name)` belongs to
//! that project; `project = None` groups a *view* instead, so Today can be split
//! into "morning" / "after standup" without inventing a fake project.
//!
//! # Repos (v11)
//!
//! **A project's NAME and a repo's DIRECTORY are not the same thing** and must
//! never be derived from each other. `pocketbuddy` lives at
//! `~/Projects/finance-app`; deriving one from the other invents a project
//! called "finance-app" and splits the work in two. So the mapping is stored,
//! in `project_repos`, many repos to one project — half the portfolio is a
//! web+iOS pair — with a repo owned by at most one project so that
//! [`for_repo_path`] is a function rather than a guess.
//!
//! # Areas and projects (v19)
//!
//! One row type was doing two jobs. `CXVentures` never finishes; a wave of
//! government-contracting certifications filed into it does. Sharing a row type
//! meant sharing a board row, a badge and an activity strip, so the sprint's
//! signal was averaged into the bucket's — and an area with thirty tasks reads
//! as permanently `Active` because *something* in it always moved.
//!
//! [`ProjectKind`] splits them. An **area** is a standing responsibility with no
//! finish line; a **project** is finite, can carry a `target_date`, and can be
//! closed. Nesting is exactly one level — area → project → section — and a
//! section carrying a target date is still the milestone concept, unchanged.
//!
//! `tasks.project` is untouched: the NAME remains the join key for both kinds
//! (invariant 17), which is why nothing about existing tasks had to move.
//! Parenthood is the single exception to name-as-join-key — see
//! [`crate::db::schema::create_tables_v19`] for why it is an id.
//!
//! **The roll-up rule lives in exactly one function**, [`expand_scope`]: an
//! area's counts and the list clicking it opens both cover its nested projects,
//! and they are computed from the same place so they can never disagree.

use crate::error::AppError;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Area or project — the PARA distinction, and the only thing deciding whether
/// this node is allowed to END.
///
/// An **area** is a standing responsibility: CXVentures, Artist Advisory, the
/// music. It has no finish line, so a quiet month is a lull rather than a
/// failure and nobody should ever be asked when it will be done.
///
/// A **project** is finite — a certification wave, a launch, a migration. It can
/// carry a target date, it can be closed, and a quiet month in one is a fact
/// worth saying out loud.
///
/// Defaults to [`ProjectKind::Area`]; `schema::create_tables_v19` has the
/// argument for that direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKind {
    #[default]
    Area,
    Project,
}

impl ProjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectKind::Area => "area",
            ProjectKind::Project => "project",
        }
    }

    /// Read a stored value. **Anything unrecognised reads as `Area`** — an
    /// unknown kind must not make a standing responsibility start demanding a
    /// completion date, which is the direction an older binary writing a value
    /// this one doesn't know would otherwise fail in.
    pub fn from_db(raw: Option<&str>) -> Self {
        match raw {
            Some("project") => ProjectKind::Project,
            _ => ProjectKind::Area,
        }
    }

    /// Read a value a *human* supplied, where a typo must be an error rather
    /// than a silent fallback to `Area`.
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        match raw.trim().to_lowercase().as_str() {
            "area" => Ok(ProjectKind::Area),
            "project" => Ok(ProjectKind::Project),
            other => Err(AppError::Validation(format!(
                "unknown kind '{other}' — expected 'area' or 'project'"
            ))),
        }
    }

    pub fn is_area(self) -> bool {
        matches!(self, ProjectKind::Area)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    /// The sidebar icon — `lucide:<name>` or a PNG `data:` URI; `None` = the
    /// kind's own glyph. Validated on write by `db::appearance`.
    pub icon: Option<String>,
    pub sort_order: i64,
    pub archived: bool,
    pub kind: ProjectKind,
    /// **Name** of the owning area; `None` = top-level. Stored as `parent_id`
    /// and resolved here, so renaming an area carries its children for free.
    pub parent: Option<String>,
    /// Full ISO-8601 UTC with a trailing `Z`, like `sections.target_date`.
    /// Always `None` on an area — an area has no finish line to date.
    pub target_date: Option<String>,
    /// The project is FINISHED. A different claim from `archived` ("hide it"),
    /// the way `sections.completed_at` is different from deleting the section.
    pub closed_at: Option<String>,
    /// Open **top-level** tasks in this project — the sidebar badge.
    ///
    /// `parent_id IS NULL` is not a detail: the badge is a claim about the list
    /// clicking it opens, and that list is [`crate::db::tasks::list`] with
    /// `include_subtasks` false, where subtasks render nested in the parent's
    /// detail pane rather than as rows. Counting them made a project with one
    /// task broken into four steps read "5" and then show a single row — the
    /// badge describing work the board deliberately never displays.
    pub open_count: i64,
    /// Last reported health. `None` = **never reported**, which is a different
    /// claim from `on_track` and renders as a gray dot. Only
    /// [`crate::db::project_updates::post`] ever writes it.
    pub health: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub id: String,
    pub name: String,
    /// None = the section groups a view rather than a project.
    pub project: Option<String>,
    pub sort_order: i64,
    /// **A section carrying a target date IS a milestone.** No separate
    /// milestone concept: a dated phase and a grouping of tasks are the same
    /// thing, and splitting them would mean maintaining two ways to say
    /// "these tasks belong together".
    ///
    /// Full ISO-8601 UTC with a trailing `Z`, like `tasks.due_at` — a bare
    /// `YYYY-MM-DD` renders a day off once JS parses it as local time.
    pub target_date: Option<String>,
    /// Set by hand, deliberately NOT derived from whether its tasks are all
    /// done: a milestone is met when someone says it is. Deriving it would
    /// silently un-complete a milestone the moment a follow-up task is filed
    /// into the phase.
    pub completed_at: Option<String>,
}

/// One repository belonging to a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRepo {
    pub id: String,
    pub project: String,
    pub repo_path: String,
    /// The one used to prefill a task's `repo_path` when its project is set.
    pub is_primary: bool,
    pub sort_order: i64,
}

fn closed() -> String {
    crate::db::tasks::CLOSED_STATUSES
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn normalize(raw: &str) -> String {
    raw.trim().split_whitespace().collect::<Vec<_>>().join(" ")
}

// ── projects ──────────────────────────────────────────────────────────────

/// SQL that is true for a task belonging to `p` **or to any live node nested
/// under it, at any depth** — [`expand_scope`]'s rule, expressed for a
/// correlated subquery.
///
/// Recursive since areas may hold areas (2026-09-27): Products → Artist
/// Advisory → Monthly-Listener Retitle, and Products' badge has to count the
/// grandchild's tasks too, or the badge and the list it opens disagree.
/// `UNION` (not `UNION ALL`) so a hand-edited `parent_id` cycle terminates —
/// the same defence `deps::reaches` and the subtree walks use.
///
/// Archived children are excluded on purpose, and they cut off their subtree:
/// a closed project is archived by [`set_closed`], and letting its leftovers
/// keep inflating the parent's badge would mean closing something made the
/// area look busier.
///
/// `c.name = t.project COLLATE NOCASE` and not the reverse: `projects.name`
/// carries the NOCASE collation, `tasks.project` does not, and SQLite takes the
/// collation from the LEFT operand — written the other way round this silently
/// becomes a case-sensitive join and drops half the roll-up.
const IN_SCOPE_OF_P: &str = "(t.project = p.name COLLATE NOCASE
                             OR EXISTS (WITH RECURSIVE d(id) AS (
                                          SELECT c.id FROM projects c
                                           WHERE c.parent_id = p.id AND c.archived = 0
                                          UNION
                                          SELECT c.id FROM projects c JOIN d ON c.parent_id = d.id
                                           WHERE c.archived = 0)
                                        SELECT 1 FROM d JOIN projects c ON c.id = d.id
                                         WHERE c.name = t.project COLLATE NOCASE))";

pub fn list(conn: &Connection, include_archived: bool) -> Result<Vec<Project>, AppError> {
    let closed = closed();
    let archived_clause = if include_archived { "" } else { "WHERE p.archived = 0" };
    let sql = format!(
        "SELECT p.id, p.name, p.color, p.sort_order, p.archived, p.health,
                p.kind,
                (SELECT a.name FROM projects a WHERE a.id = p.parent_id),
                p.target_date, p.closed_at,
                COALESCE((SELECT COUNT(*) FROM tasks t
                           WHERE {IN_SCOPE_OF_P}
                             AND t.status NOT IN ({closed})
                             AND t.deleted_at IS NULL
                             AND t.archived_at IS NULL
                             AND t.parent_id IS NULL), 0),
                p.icon
           FROM projects p
           {archived_clause}
          ORDER BY p.sort_order ASC, p.name ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Project {
                id: r.get(0)?,
                name: r.get(1)?,
                color: r.get(2)?,
                sort_order: r.get(3)?,
                archived: r.get::<_, i64>(4)? != 0,
                health: r.get(5)?,
                kind: ProjectKind::from_db(r.get::<_, Option<String>>(6)?.as_deref()),
                parent: r.get(7)?,
                target_date: r.get(8)?,
                closed_at: r.get(9)?,
                open_count: r.get(10)?,
                icon: r.get(11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every project name a filter on `name` should match.
///
/// For a **project** — or for a name with no `projects` row at all — that is
/// just the name itself. For an **area** it is the name plus every live project
/// nested under it.
///
/// **This is the one place the roll-up rule lives.** `tasks::list`,
/// `tasks::next_task`, `search::filtered` and [`board`] all route through it,
/// for the same reason [`ActivityState::from_idle_days`] and
/// `tasks::actionable_sql` are single functions: a rule re-derived per caller is
/// a rule that eventually disagrees with itself — and here the disagreement
/// would be a sidebar badge that counts one thing and a list that shows
/// another, which the badge's own contract forbids (see [`Project::open_count`]).
///
/// Returns an empty vector for an empty name, so a caller can treat "no scope"
/// and "nothing matches" the same way.
pub fn expand_scope(conn: &Connection, raw_name: &str) -> Result<Vec<String>, AppError> {
    let name = normalize(raw_name);
    if name.is_empty() {
        return Ok(Vec::new());
    }
    let mut names = vec![name.clone()];
    // Every live descendant, at any depth — areas hold areas now. Ordered
    // shallowest first, then as the sidebar orders siblings. `UNION` and the
    // depth guard both stop a hand-edited cycle: the first on repeated rows,
    // the second on a loop long enough to keep producing new (id, depth) pairs.
    let mut stmt = conn.prepare(
        "WITH RECURSIVE d(id, name, depth, sort_order) AS (
             SELECT id, name, 0, sort_order FROM projects WHERE name = ?1 COLLATE NOCASE
             UNION
             SELECT c.id, c.name, d.depth + 1, c.sort_order
               FROM projects c JOIN d ON c.parent_id = d.id
              WHERE c.archived = 0 AND d.depth < 32
         )
         SELECT name FROM d WHERE depth > 0 ORDER BY depth ASC, sort_order ASC, name ASC",
    )?;
    let kids = stmt
        .query_map(params![name], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for k in kids {
        if !names.iter().any(|n| n.eq_ignore_ascii_case(&k)) {
            names.push(k);
        }
    }
    Ok(names)
}

/// `?1, ?2, …` for a scope, ready to drop into an `IN (…)`.
///
/// Placeholders rather than inlined literals because project names are free
/// text a user typed.
pub fn scope_placeholders(scope: &[String]) -> String {
    (1..=scope.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn create(
    conn: &Connection,
    raw_name: &str,
    kind: ProjectKind,
    parent: Option<&str>,
) -> Result<Project, AppError> {
    let name = normalize(raw_name);
    if name.is_empty() {
        return Err(AppError::Validation("project name must not be empty".into()));
    }
    if get_by_name(conn, &name)?.is_some() {
        return Err(AppError::Validation(format!(
            "a project called '{name}' already exists"
        )));
    }
    let next: i64 = conn
        .query_row("SELECT COALESCE(MAX(sort_order), 0) + 1 FROM projects", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO projects (id, name, sort_order, kind) VALUES (?1, ?2, ?3, ?4)",
        params![id, name, next, kind.as_str()],
    )?;
    // After the insert, not folded into it: `set_parent` is where the depth and
    // kind rules live, and routing the create through it means "+ New project
    // inside CXVentures" can never nest something the reparent menu would
    // refuse.
    if let Some(parent) = parent.map(str::trim).filter(|p| !p.is_empty()) {
        set_parent(conn, &name, Some(parent))?;
    }
    get_by_name(conn, &name)?
        .ok_or_else(|| AppError::NotFound("project vanished after insert".into()))
}

// ── PARA: kind, nesting, and ending ───────────────────────────────────────

/// Names of the live projects nested directly under `id`.
fn children_of(conn: &Connection, id: &str) -> Result<Vec<String>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT name FROM projects WHERE parent_id = ?1 ORDER BY sort_order ASC, name ASC",
    )?;
    let rows = stmt
        .query_map(params![id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every node beneath `id`, at any depth, archived ones included — this is the
/// cycle check, and an archived child still has a `parent_id` that could close
/// a loop. `UNION` so an existing cycle terminates rather than hanging.
fn descendants_of(conn: &Connection, id: &str) -> Result<Vec<String>, AppError> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE d(id) AS (
             SELECT id FROM projects WHERE parent_id = ?1
             UNION
             SELECT c.id FROM projects c JOIN d ON c.parent_id = d.id
         )
         SELECT id FROM d",
    )?;
    let rows = stmt
        .query_map(params![id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn require(conn: &Connection, raw_name: &str) -> Result<Project, AppError> {
    let name = normalize(raw_name);
    get_by_name(conn, &name)?
        .ok_or_else(|| AppError::NotFound(format!("project '{name}' not found")))
}

/// Nest a project under an area, or (with `None`) lift it back to the top.
///
/// Every rule that keeps the tree well-formed is enforced here, and nowhere
/// else, so [`create`] and the reparent menu can't drift apart:
///
/// * anything — a project, or an area with everything under it — may hang off
///   an **area**, at any depth (Products → Artist Advisory → a project). This
///   was one level until 2026-09-27; Chris: "the whole idea is a nested
///   structure";
/// * nothing may hang off a **project** — projects hold SECTIONS, which is the
///   level below them, so a project inside a project would be two words for
///   one thing;
/// * nothing may parent itself or anything beneath it — that is a cycle, and
///   a cycle makes every node in it unreachable from the top of the sidebar.
pub fn set_parent(
    conn: &Connection,
    raw_child: &str,
    raw_parent: Option<&str>,
) -> Result<(), AppError> {
    let child = require(conn, raw_child)?;
    let Some(raw_parent) = raw_parent.map(str::trim).filter(|p| !p.is_empty()) else {
        conn.execute(
            "UPDATE projects SET parent_id = NULL WHERE id = ?1",
            params![child.id],
        )?;
        return Ok(());
    };
    let parent = require(conn, raw_parent)?;
    if parent.id == child.id {
        return Err(AppError::Validation(format!(
            "'{}' cannot be nested inside itself",
            child.name
        )));
    }
    if !parent.kind.is_area() {
        return Err(AppError::Validation(format!(
            "'{}' is a project, so nothing can be nested inside it — projects hold sections, areas hold projects and areas",
            parent.name
        )));
    }
    if descendants_of(conn, &child.id)?.iter().any(|d| d == &parent.id) {
        return Err(AppError::Validation(format!(
            "'{}' is inside '{}', so '{}' cannot go inside it — move '{}' out first",
            parent.name, child.name, child.name, parent.name
        )));
    }
    conn.execute(
        "UPDATE projects SET parent_id = ?1 WHERE id = ?2",
        params![parent.id, child.id],
    )?;
    Ok(())
}

/// Switch a node between area and project.
///
/// Promoting a project to an area **clears its target date and closed stamp**
/// rather than refusing. Both are claims only a finite thing can make, and an
/// area that kept a target date would be asking a question it can never answer.
/// It KEEPS its parent: areas nest now, so a project in Products that becomes
/// an area stays in Products (it used to jump to the top level).
///
/// Demoting an area that holds anything is refused: a project holds sections,
/// not projects or areas.
pub fn set_kind(conn: &Connection, raw_name: &str, kind: ProjectKind) -> Result<(), AppError> {
    let p = require(conn, raw_name)?;
    if p.kind == kind {
        return Ok(());
    }
    match kind {
        ProjectKind::Area => {
            conn.execute(
                "UPDATE projects
                    SET kind = 'area', target_date = NULL, closed_at = NULL
                  WHERE id = ?1",
                params![p.id],
            )?;
        }
        ProjectKind::Project => {
            let kids = children_of(conn, &p.id)?;
            if !kids.is_empty() {
                return Err(AppError::Validation(format!(
                    "'{}' holds {} nested item(s) — move them out before making it a project, which holds sections rather than projects",
                    p.name,
                    kids.len()
                )));
            }
            conn.execute(
                "UPDATE projects SET kind = 'project' WHERE id = ?1",
                params![p.id],
            )?;
        }
    }
    Ok(())
}

/// Set (or clear) a project's finish line. Refused on an area, which has none.
///
/// Full ISO-8601 UTC with a trailing `Z`, like `sections::set_section_target_date`
/// — a bare `YYYY-MM-DD` renders a day off once JS parses it as local time.
pub fn set_target_date(
    conn: &Connection,
    raw_name: &str,
    target_date: Option<&str>,
) -> Result<(), AppError> {
    let p = require(conn, raw_name)?;
    if p.kind.is_area() {
        return Err(AppError::Validation(format!(
            "'{}' is an area — an area has no finish line to date",
            p.name
        )));
    }
    let value = target_date.map(str::trim).filter(|d| !d.is_empty());
    conn.execute(
        "UPDATE projects SET target_date = ?1 WHERE id = ?2",
        params![value, p.id],
    )?;
    Ok(())
}

/// Close a finished project — **and archive it in the same breath.**
///
/// Two stamps because they answer different questions. `closed_at` says the work
/// ended; `archived` says stop showing it. Closing implies both, but reopening
/// only un-archives what closing archived, so a project you had *separately*
/// chosen to hide doesn't come back just because you reopened it. The COALESCE
/// mirrors [`set_section_completed`]: closing twice must not quietly rewrite
/// *when* it finished.
pub fn set_closed(conn: &Connection, raw_name: &str, closed: bool) -> Result<(), AppError> {
    let p = require(conn, raw_name)?;
    if p.kind.is_area() {
        return Err(AppError::Validation(format!(
            "'{}' is an area — areas don't end. Close the projects inside it instead",
            p.name
        )));
    }
    if closed {
        conn.execute(
            "UPDATE projects
                SET closed_at = COALESCE(closed_at, strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                    archived  = 1
              WHERE id = ?1",
            params![p.id],
        )?;
    } else {
        conn.execute(
            "UPDATE projects SET closed_at = NULL, archived = 0 WHERE id = ?1",
            params![p.id],
        )?;
    }
    Ok(())
}

/// Make sure a `projects` row exists for `name`, creating it if not.
///
/// Called from [`crate::db::tasks::create`] and from
/// [`crate::db::project_updates::post`] — the two places a project name can
/// arrive from outside the app, where nobody pressed "+ New project". The
/// `lower(hex(randomblob(16)))` id is the same idiom the v4 backfill used, so a
/// row created here is indistinguishable from one it produced.
///
/// Unlike [`create`] this is silent about an existing project: `OR IGNORE`
/// against the `UNIQUE COLLATE NOCASE` index means filing into "CXVentures"
/// when "cxventures" exists reuses the existing row rather than erroring.
pub fn ensure_exists(conn: &Connection, name: &str) -> Result<(), AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(());
    }
    conn.execute(
        "INSERT OR IGNORE INTO projects (id, name, sort_order)
         VALUES (lower(hex(randomblob(16))), ?1,
                 COALESCE((SELECT MAX(sort_order) + 1 FROM projects), 0))",
        params![name],
    )?;
    Ok(())
}

pub fn get_by_name(conn: &Connection, raw_name: &str) -> Result<Option<Project>, AppError> {
    let name = normalize(raw_name);
    Ok(list(conn, true)?
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(&name)))
}

/// Rename a project and carry every task, section, update and repo with it —
/// **merging into the target when that name already exists.**
///
/// The merge is what makes the v11 cleanup possible: a phantom project named
/// after a directory (`finance-app`) is folded into the real one (`pocketbuddy`)
/// by renaming it, exactly as `tags::rename` already behaves. Refusing instead —
/// which is what this did before v11 — left no way to undo the split except
/// hand-editing the database.
///
/// Two details are load-bearing.
///
/// **Sections are absorbed BEFORE the text joins move.** `sections` is
/// `UNIQUE(project, name)`, so merging two projects that both have a "phase 1"
/// would fail that constraint mid-rename, having already moved the tasks. See
/// [`absorb_sections`] for why the collision moves tasks rather than deleting
/// the row.
///
/// **A merge deletes the source `projects` row; a plain rename updates it.**
/// Renaming the row in a merge would hit the UNIQUE name index, and the target's
/// own id, health and sort_order are the ones that should survive.
pub fn rename(conn: &Connection, from: &str, to: &str) -> Result<Project, AppError> {
    let old = normalize(from);
    let new = normalize(to);
    if new.is_empty() {
        return Err(AppError::Validation("project name must not be empty".into()));
    }
    // Rename operates on the NAME, not on the record — because `tasks.project`
    // is the join key and the `projects` row is secondary, a name can have work
    // filed under it and no record at all. That is not hypothetical: it is
    // exactly the state a phantom project ends up in once someone deletes the
    // sidebar entry (invariant 11b keeps the tasks), and it was the state of the
    // real `finance-app` — one task, no row. Requiring the record here would
    // make the merge unavailable for precisely the mess it exists to clean up,
    // and the symptom is a "project not found" error about a name plainly
    // visible on a task.
    let record = get_by_name(conn, &old)?;
    if record.is_none() && !name_is_referenced(conn, &old)? {
        return Err(AppError::NotFound(format!("project '{old}' not found")));
    }
    let merging = match (&record, get_by_name(conn, &new)?) {
        (Some(src), Some(dst)) => src.id != dst.id,
        // No source record to carry over, so there is nothing to rename or
        // delete — the text joins move and that IS the merge.
        (None, Some(_)) => true,
        _ => false,
    };

    // Deliberately NOT gated on `merging`. Sections can exist under a name that
    // has no `projects` row at all — `upsert_section` creates them for
    // `/breakdown` before any task lands — and `sections` is UNIQUE(project,
    // name), so a collision would fail the constraint below having already moved
    // the tasks. The guard is the *name*, not the record.
    //
    // Skipped when the names differ only in case ("cxventures" → "CXVentures"):
    // `list_sections` matches COLLATE NOCASE, so both sides would be the SAME
    // rows and every section would collide with itself and be deleted.
    if !old.eq_ignore_ascii_case(&new) {
        absorb_sections(conn, &old, &new)?;
    }

    // Every text join keyed by project name has to move in the same breath, or
    // the rename orphans what it was supposed to carry. Four of them now.
    //
    // Still four after v19: a project's PARENT is keyed by row id, not name,
    // precisely so renaming an area doesn't need a fifth line here — the one
    // place in this schema where the name-as-join-key convention is broken on
    // purpose. See `schema::create_tables_v19`.
    conn.execute(
        "UPDATE tasks SET project = ?1 WHERE project = ?2 COLLATE NOCASE",
        params![new, old],
    )?;
    conn.execute(
        "UPDATE sections SET project = ?1 WHERE project = ?2 COLLATE NOCASE",
        params![new, old],
    )?;
    // `project_updates` is keyed by name too (invariant 11a). Miss this line and
    // a rename silently orphans the project's entire status history — the board
    // row goes blank and reads as "never reported on", which is precisely the
    // signal the GSD layer exists to make trustworthy.
    conn.execute(
        "UPDATE project_updates SET project = ?1 WHERE project = ?2 COLLATE NOCASE",
        params![new, old],
    )?;
    // v11's join. Losing it would strand the repo mapping on a name nothing
    // references any more, and the symptom is `file_task` quietly going back to
    // resolving no project at all.
    conn.execute(
        "UPDATE project_repos SET project = ?1 WHERE project = ?2 COLLATE NOCASE",
        params![new, old],
    )?;

    match (&record, merging) {
        (Some(src), true) => {
            // Children move to the survivor BEFORE the row goes. `parent_id` is
            // `ON DELETE SET NULL`, so skipping this doesn't error — it quietly
            // dumps every nested project to the top level, and the symptom is a
            // merge that flattened a hierarchy nobody asked it to touch.
            // `merging` on this arm means both records existed, so the
            // destination is here; the `if let` is a refusal to write a NULL or
            // an empty id if that ever stops being true.
            if let Some(dst) = get_by_name(conn, &new)? {
                conn.execute(
                    "UPDATE projects SET parent_id = ?1 WHERE parent_id = ?2",
                    params![dst.id, src.id],
                )?;
            }
            conn.execute("DELETE FROM projects WHERE id = ?1", params![src.id])?;
        }
        (Some(src), false) => {
            conn.execute("UPDATE projects SET name = ?1 WHERE id = ?2", params![new, src.id])?;
        }
        // Renaming a name that has work but no record. Ensure the destination
        // exists so the result is a real project rather than a second orphan.
        (None, _) => ensure_exists(conn, &new)?,
    }
    // Both sides may have brought a primary repo.
    normalize_primary(conn, &new)?;

    get_by_name(conn, &new)?
        .ok_or_else(|| AppError::NotFound("project vanished after rename".into()))
}

/// Does any row anywhere still file work under this project name?
///
/// `tasks` is the one that matters — the others are carried for completeness so
/// that a name holding only sections or only a status log is still renameable.
fn name_is_referenced(conn: &Connection, name: &str) -> Result<bool, AppError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM tasks          WHERE project = ?1 COLLATE NOCASE)
             OR EXISTS(SELECT 1 FROM sections       WHERE project = ?1 COLLATE NOCASE)
             OR EXISTS(SELECT 1 FROM project_updates WHERE project = ?1 COLLATE NOCASE)
             OR EXISTS(SELECT 1 FROM project_repos   WHERE project = ?1 COLLATE NOCASE)",
        params![name],
        |r| r.get::<_, i64>(0),
    )? != 0)
}

/// Fold `from`'s sections into `into`, resolving same-name collisions.
///
/// The collision is resolved by **moving the tasks into the surviving section
/// and then deleting the duplicate**, never by deleting the section and letting
/// the FK sort it out: `tasks.section_id` is `ON DELETE SET NULL` (invariant
/// 11c), so the lazy version silently ungroups everything that was in the source
/// project's "phase 1" — a merge that loses the grouping it was merging.
fn absorb_sections(conn: &Connection, from: &str, into: &str) -> Result<(), AppError> {
    let source = list_sections(conn, Some(from))?;
    let target = list_sections(conn, Some(into))?;
    for s in source {
        let Some(t) = target.iter().find(|t| t.name.eq_ignore_ascii_case(&s.name)) else {
            continue; // no collision — the UPDATE in `rename` carries it over
        };
        if t.id == s.id {
            continue; // belt and braces against a section absorbing itself
        }
        conn.execute(
            "UPDATE tasks SET section_id = ?1 WHERE section_id = ?2",
            params![t.id, s.id],
        )?;
        conn.execute("DELETE FROM sections WHERE id = ?1", params![s.id])?;
    }
    Ok(())
}

pub fn set_archived(conn: &Connection, raw_name: &str, archived: bool) -> Result<(), AppError> {
    let name = normalize(raw_name);
    conn.execute(
        "UPDATE projects SET archived = ?1 WHERE name = ?2 COLLATE NOCASE",
        params![i64::from(archived), name],
    )?;
    Ok(())
}

/// Delete the project record. Tasks are NOT deleted — they keep their `project`
/// text and simply stop appearing under a sidebar entry. Deleting a grouping
/// must never delete the work inside it.
///
/// Deleting an **area** lifts its nested projects to the top level rather than
/// taking them with it — `parent_id` is `ON DELETE SET NULL` for exactly this,
/// and it is the same argument one level up: removing a container must never
/// remove what was inside it (invariant 11b).
pub fn delete(conn: &Connection, raw_name: &str, clear_from_tasks: bool) -> Result<(), AppError> {
    let name = normalize(raw_name);
    conn.execute("DELETE FROM projects WHERE name = ?1 COLLATE NOCASE", params![name])?;
    conn.execute("DELETE FROM sections WHERE project = ?1 COLLATE NOCASE", params![name])?;
    // The update log goes with the project, unlike its tasks. Tasks are the WORK
    // and must survive (see the doc comment); the log is a record *about* this
    // project record, and leaving it behind means creating a new project with
    // the same name later resurrects a stranger's status history under it.
    conn.execute(
        "DELETE FROM project_updates WHERE project = ?1 COLLATE NOCASE",
        params![name],
    )?;
    // Same reasoning as the update log, and the same asymmetry with tasks: the
    // repo mapping is a record *about* this project record. Leaving it behind
    // would hand a later project that reuses the name a stranger's repos — and
    // silently point `file_task` at them.
    conn.execute(
        "DELETE FROM project_repos WHERE project = ?1 COLLATE NOCASE",
        params![name],
    )?;
    if clear_from_tasks {
        conn.execute(
            "UPDATE tasks SET project = NULL WHERE project = ?1 COLLATE NOCASE",
            params![name],
        )?;
    }
    Ok(())
}

pub fn reorder(conn: &mut Connection, names: &[String]) -> Result<(), AppError> {
    // A savepoint rather than a transaction, so this still works when a caller
    // has already opened one — see the note on `reorder_tasks` in `api.rs`.
    let tx = conn.savepoint()?;
    for (i, name) in names.iter().enumerate() {
        tx.execute(
            "UPDATE projects SET sort_order = ?1 WHERE name = ?2 COLLATE NOCASE",
            params![i as i64, normalize(name)],
        )?;
    }
    tx.commit()?;
    Ok(())
}

// ── repos ─────────────────────────────────────────────────────────────────
//
// The mapping that stops a project name and a directory name being derived from
// each other. See the module docs and `schema::create_tables_v11`.

/// Canonical spelling of a repo path, so two spellings of one directory can't
/// become two rows and split the reverse lookup.
///
/// Expands a leading `~/`, trims whitespace, and drops trailing slashes —
/// deliberately WITHOUT touching the filesystem. `canonicalize` would resolve
/// symlinks and fail outright on a path that doesn't exist yet, and this has to
/// stay usable on a repo that is merely typed, not mounted.
pub fn normalize_repo_path(raw: &str) -> String {
    let trimmed = raw.trim();
    let expanded = match trimmed.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|h| h.join(rest).to_string_lossy().to_string())
            .unwrap_or_else(|| trimmed.to_string()),
        None => trimmed.to_string(),
    };
    let stripped = expanded.trim_end_matches('/');
    if stripped.is_empty() && expanded.starts_with('/') {
        "/".to_string()
    } else {
        stripped.to_string()
    }
}

fn row_to_repo(r: &rusqlite::Row) -> rusqlite::Result<ProjectRepo> {
    Ok(ProjectRepo {
        id: r.get(0)?,
        project: r.get(1)?,
        repo_path: r.get(2)?,
        is_primary: r.get::<_, i64>(3)? != 0,
        sort_order: r.get(4)?,
    })
}

const REPO_COLUMNS: &str = "id, project, repo_path, is_primary, sort_order";

/// Every repo mapping, primaries first within each project.
///
/// The frontend loads this once rather than per project: the detail pane needs
/// *some* project's repos on every selection change, and one small table beats
/// an IPC round-trip per click.
pub fn all_repos(conn: &Connection) -> Result<Vec<ProjectRepo>, AppError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {REPO_COLUMNS} FROM project_repos
          ORDER BY project COLLATE NOCASE ASC, is_primary DESC, sort_order ASC"
    ))?;
    let rows = stmt.query_map([], row_to_repo)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_repos(conn: &Connection, raw_project: &str) -> Result<Vec<ProjectRepo>, AppError> {
    let project = normalize(raw_project);
    let mut stmt = conn.prepare(&format!(
        "SELECT {REPO_COLUMNS} FROM project_repos
          WHERE project = ?1 COLLATE NOCASE
          ORDER BY is_primary DESC, sort_order ASC"
    ))?;
    let rows = stmt
        .query_map(params![project], row_to_repo)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The repo a task in this project gets by default. `None` = unmapped.
///
/// **A nested project with no repos of its own inherits its AREA's primary**
/// (v19). A finite project inside an area is nearly always work in that area's
/// repo — a certification wave inside CXVentures runs in `~/Projects/cxventures`
/// — and without the fallback, nesting a task would silently strip its
/// workspace, which presents as the launch console reading "Needs repo" for work
/// whose directory the app plainly knows.
///
/// **Deliberately one-directional.** [`for_repo_path`] is NOT changed to match:
/// a directory still resolves to exactly one owner (the area that registered
/// it), because that lookup has to be a function — two projects answering for
/// one directory is the ambiguity `project_repos` exists to prevent. Project →
/// directory may inherit; directory → project may not.
pub fn primary_repo(conn: &Connection, raw_project: &str) -> Result<Option<String>, AppError> {
    if let Some(own) = list_repos(conn, raw_project)?.into_iter().next() {
        return Ok(Some(own.repo_path));
    }
    // Walk up the areas: a project in Artist Advisory inside Products inherits
    // Artist Advisory's repo, and failing that Products'. Bounded, so a
    // hand-edited cycle cannot spin here.
    let mut current = get_by_name(conn, raw_project)?.and_then(|p| p.parent);
    for _ in 0..32 {
        let Some(parent) = current else { return Ok(None) };
        if let Some(own) = list_repos(conn, &parent)?.into_iter().next() {
            return Ok(Some(own.repo_path));
        }
        current = get_by_name(conn, &parent)?.and_then(|p| p.parent);
    }
    Ok(None)
}

/// **The reverse lookup: which project owns this directory?**
///
/// This is what replaces "take the basename and call it the project". Matching
/// is by longest path prefix, so a path *inside* a repo
/// (`~/Projects/finance-app/src/lib`) resolves to the same project as the repo
/// root — an agent's cwd is often a subdirectory, and requiring an exact match
/// would silently resolve nothing precisely when it matters.
///
/// Longest-wins also settles nested repos deterministically: with both
/// `~/Projects/foo` and `~/Projects/foo/ios` mapped, a path under `ios` belongs
/// to whoever claimed `ios`, which is the more specific and more useful answer.
pub fn for_repo_path(conn: &Connection, raw_path: &str) -> Result<Option<String>, AppError> {
    let needle = normalize_repo_path(raw_path).to_lowercase();
    if needle.is_empty() {
        return Ok(None);
    }
    let mut best: Option<(usize, String)> = None;
    for repo in all_repos(conn)? {
        let candidate = normalize_repo_path(&repo.repo_path).to_lowercase();
        if candidate.is_empty() {
            continue;
        }
        let hit = needle == candidate || needle.starts_with(&format!("{candidate}/"));
        if hit && best.as_ref().is_none_or(|(len, _)| candidate.len() > *len) {
            best = Some((candidate.len(), repo.project));
        }
    }
    Ok(best.map(|(_, project)| project))
}

/// Map a repo to a project, creating the project record if it's new.
///
/// **A repo already owned by a different project is an error, not a move.** The
/// UNIQUE index would reject it anyway; catching it here is what turns
/// "constraint failed" into a message naming the current owner, which is the
/// difference between a fixable mistake and a mystery. Re-adding a repo the same
/// project already owns is a no-op that can still promote it to primary.
pub fn add_repo(
    conn: &Connection,
    raw_project: &str,
    raw_path: &str,
    make_primary: bool,
) -> Result<ProjectRepo, AppError> {
    let project = normalize(raw_project);
    if project.is_empty() {
        return Err(AppError::Validation("project name must not be empty".into()));
    }
    let path = normalize_repo_path(raw_path);
    if path.is_empty() {
        return Err(AppError::Validation("repo path must not be empty".into()));
    }
    if !std::path::Path::new(&path).is_absolute() {
        return Err(AppError::Validation(format!(
            "repo path must be absolute, got '{path}'"
        )));
    }

    let existing: Option<ProjectRepo> = conn
        .query_row(
            &format!("SELECT {REPO_COLUMNS} FROM project_repos WHERE repo_path = ?1 COLLATE NOCASE"),
            params![path],
            row_to_repo,
        )
        .optional()?;

    let repo = match existing {
        Some(r) if !r.project.eq_ignore_ascii_case(&project) => {
            return Err(AppError::Validation(format!(
                "'{}' already belongs to the project '{}'. Remove it there first — a repo \
                 can only have one owner, which is what lets a filed task resolve its \
                 project from the directory it came from.",
                path, r.project
            )));
        }
        Some(r) => r,
        None => {
            ensure_exists(conn, &project)?;
            let id = uuid::Uuid::new_v4().to_string();
            let next: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM project_repos
                      WHERE project = ?1 COLLATE NOCASE",
                    params![project],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            conn.execute(
                "INSERT INTO project_repos (id, project, repo_path, is_primary, sort_order)
                 VALUES (?1, ?2, ?3, 0, ?4)",
                params![id, project, path, next],
            )?;
            ProjectRepo {
                id,
                project: project.clone(),
                repo_path: path.clone(),
                is_primary: false,
                sort_order: next,
            }
        }
    };

    if make_primary {
        set_primary_repo(conn, &repo.id)?;
    } else {
        // The first repo mapped is the primary by default: a project with repos
        // and no primary prefills nothing, which reads as the mapping not having
        // saved rather than as a setting nobody chose.
        normalize_primary(conn, &project)?;
    }

    list_repos(conn, &project)?
        .into_iter()
        .find(|r| r.id == repo.id)
        .ok_or_else(|| AppError::NotFound("repo vanished after insert".into()))
}

pub fn remove_repo(conn: &Connection, id: &str) -> Result<(), AppError> {
    let project: Option<String> = conn
        .query_row(
            "SELECT project FROM project_repos WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    conn.execute("DELETE FROM project_repos WHERE id = ?1", params![id])?;
    // Removing the primary must hand the role on, not leave the project without
    // one.
    if let Some(p) = project {
        normalize_primary(conn, &p)?;
    }
    Ok(())
}

pub fn set_primary_repo(conn: &Connection, id: &str) -> Result<(), AppError> {
    let Some(project): Option<String> = conn
        .query_row(
            "SELECT project FROM project_repos WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?
    else {
        return Err(AppError::NotFound(format!("repo {id} not found")));
    };
    // One statement, so there is no window in which the project has two
    // primaries or none.
    conn.execute(
        "UPDATE project_repos SET is_primary = (id = ?2) WHERE project = ?1 COLLATE NOCASE",
        params![project, id],
    )?;
    Ok(())
}

/// Force "exactly one primary" for a project, preferring one already marked.
///
/// Called after every mutation that could break the property: a merge brings two
/// primaries together, removing one leaves zero.
fn normalize_primary(conn: &Connection, raw_project: &str) -> Result<(), AppError> {
    let project = normalize(raw_project);
    let winner: Option<String> = conn
        .query_row(
            "SELECT id FROM project_repos
              WHERE project = ?1 COLLATE NOCASE AND is_primary = 1
              ORDER BY rowid ASC LIMIT 1",
            params![project],
            |r| r.get(0),
        )
        .optional()?
        .or(conn
            .query_row(
                "SELECT id FROM project_repos WHERE project = ?1 COLLATE NOCASE
                  ORDER BY sort_order ASC, rowid ASC LIMIT 1",
                params![project],
                |r| r.get(0),
            )
            .optional()?);
    if let Some(id) = winner {
        conn.execute(
            "UPDATE project_repos SET is_primary = (id = ?2) WHERE project = ?1 COLLATE NOCASE",
            params![project, id],
        )?;
    }
    Ok(())
}

// ── sections ──────────────────────────────────────────────────────────────

/// Sections for a project, or the view-level sections when `project` is None.
pub fn list_sections(conn: &Connection, project: Option<&str>) -> Result<Vec<Section>, AppError> {
    let (sql, bind): (&str, Option<String>) = match project.map(normalize).filter(|p| !p.is_empty())
    {
        Some(p) => (
            "SELECT id, name, project, sort_order, target_date, completed_at FROM sections
              WHERE project = ?1 COLLATE NOCASE ORDER BY sort_order ASC, name ASC",
            Some(p),
        ),
        None => (
            "SELECT id, name, project, sort_order, target_date, completed_at FROM sections
              WHERE project IS NULL ORDER BY sort_order ASC, name ASC",
            None,
        ),
    };
    let mut stmt = conn.prepare(sql)?;
    let map = |r: &rusqlite::Row| {
        Ok(Section {
            id: r.get(0)?,
            name: r.get(1)?,
            project: r.get(2)?,
            sort_order: r.get(3)?,
            target_date: r.get(4)?,
            completed_at: r.get(5)?,
        })
    };
    let rows = match bind {
        Some(p) => stmt.query_map(params![p], map)?.collect::<Result<Vec<_>, _>>()?,
        None => stmt.query_map([], map)?.collect::<Result<Vec<_>, _>>()?,
    };
    Ok(rows)
}

pub fn create_section(
    conn: &Connection,
    project: Option<&str>,
    raw_name: &str,
) -> Result<Section, AppError> {
    let name = normalize(raw_name);
    if name.is_empty() {
        return Err(AppError::Validation("section name must not be empty".into()));
    }
    let project = project.map(normalize).filter(|p| !p.is_empty());
    let next: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(sort_order), 0) + 1 FROM sections
              WHERE (project IS ?1) OR (project = ?1 COLLATE NOCASE)",
            params![project],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO sections (id, name, project, sort_order) VALUES (?1, ?2, ?3, ?4)",
        params![id, name, project, next],
    )
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            AppError::Validation(format!("a section called '{name}' already exists here"))
        } else {
            AppError::Database(e)
        }
    })?;
    Ok(Section {
        id,
        name,
        project,
        sort_order: next,
        target_date: None,
        completed_at: None,
    })
}

pub fn rename_section(conn: &Connection, id: &str, raw_name: &str) -> Result<(), AppError> {
    let name = normalize(raw_name);
    if name.is_empty() {
        return Err(AppError::Validation("section name must not be empty".into()));
    }
    let n = conn.execute("UPDATE sections SET name = ?1 WHERE id = ?2", params![name, id])?;
    if n == 0 {
        return Err(AppError::NotFound(format!("section {id} not found")));
    }
    Ok(())
}

/// Delete a section. Its tasks survive and become ungrouped (the FK is
/// ON DELETE SET NULL) — verified by test.
pub fn delete_section(conn: &Connection, id: &str) -> Result<(), AppError> {
    conn.execute("DELETE FROM sections WHERE id = ?1", params![id])?;
    Ok(())
}

/// Save the complete order within one scope. Reject stale or cross-project
/// lists before writing, so a concurrent section edit cannot corrupt the order.
pub fn reorder_sections(
    conn: &mut Connection,
    project: Option<&str>,
    ids: &[String],
) -> Result<(), AppError> {
    // Sync wraps mutations in a transaction; a savepoint also works inside it.
    let tx = conn.savepoint()?;
    let current = list_sections(&tx, project)?;
    let mut remaining: std::collections::HashSet<&str> =
        current.iter().map(|s| s.id.as_str()).collect();
    if ids.len() != current.len() || ids.iter().any(|id| !remaining.remove(id.as_str())) {
        return Err(AppError::Validation(
            "section order must include every section in this view exactly once".into(),
        ));
    }
    for (position, id) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE sections SET sort_order = ?1 WHERE id = ?2",
            params![position as i64, id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn move_task_to_section(
    conn: &Connection,
    task_id: &str,
    section_id: Option<&str>,
) -> Result<(), AppError> {
    conn.execute(
        "UPDATE tasks SET section_id = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![section_id, task_id],
    )?;
    Ok(())
}

// ── milestones ────────────────────────────────────────────────────────────
//
// A milestone is just a section with a `target_date`. There is no separate
// table and no separate concept: the thing you group phase-1 tasks under and the
// thing you put a date on are the same thing, and splitting them would mean two
// ways to say "these belong together" that immediately drift apart.

/// Set or clear a section's target date, turning it into a milestone or back.
pub fn set_section_target_date(
    conn: &Connection,
    id: &str,
    target_date: Option<&str>,
) -> Result<(), AppError> {
    let n = conn.execute(
        "UPDATE sections SET target_date = ?1 WHERE id = ?2",
        params![target_date, id],
    )?;
    if n == 0 {
        return Err(AppError::NotFound(format!("section {id} not found")));
    }
    Ok(())
}

/// Mark a milestone met, or reopen it.
///
/// Millisecond format, matching the other manually-stamped bookkeeping columns.
/// Deliberately manual: deriving "done" from the task counts would un-complete a
/// milestone the moment somebody files a follow-up into the phase.
pub fn set_section_completed(conn: &Connection, id: &str, done: bool) -> Result<(), AppError> {
    if !exists_section(conn, id)? {
        return Err(AppError::NotFound(format!("section {id} not found")));
    }
    if done {
        // COALESCE keeps the original stamp, so completing twice is a no-op
        // rather than quietly rewriting *when* the milestone was met.
        conn.execute(
            "UPDATE sections
                SET completed_at = COALESCE(completed_at, strftime('%Y-%m-%d %H:%M:%f','now'))
              WHERE id = ?1",
            params![id],
        )?;
    } else {
        conn.execute(
            "UPDATE sections SET completed_at = NULL WHERE id = ?1",
            params![id],
        )?;
    }
    Ok(())
}

fn exists_section(conn: &Connection, id: &str) -> Result<bool, AppError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sections WHERE id = ?1)",
        params![id],
        |r| r.get(0),
    )?)
}

/// Get-or-create a section by name within a project, optionally dating it.
///
/// What `/breakdown` calls once per phase, and what makes re-running a breakdown
/// safe: the second run finds the existing phase instead of failing the UNIQUE
/// constraint.
///
/// **A `target_date` is only applied to a section that doesn't already have
/// one.** Re-running a plan must not silently overwrite a date the user moved by
/// hand afterwards — a milestone date that quietly snaps back to whatever an
/// agent last guessed is worse than no date at all. Clearing or changing an
/// existing date is [`set_section_target_date`], which says so explicitly.
pub fn upsert_section(
    conn: &Connection,
    project: Option<&str>,
    raw_name: &str,
    target_date: Option<&str>,
) -> Result<Section, AppError> {
    let name = normalize(raw_name);
    if name.is_empty() {
        return Err(AppError::Validation("section name must not be empty".into()));
    }
    let scope = project.map(normalize).filter(|p| !p.is_empty());

    let existing = list_sections(conn, scope.as_deref())?
        .into_iter()
        .find(|s| s.name.eq_ignore_ascii_case(&name));

    let section = match existing {
        Some(s) => s,
        None => create_section(conn, scope.as_deref(), &name)?,
    };

    if let Some(date) = target_date.map(str::trim).filter(|d| !d.is_empty()) {
        if section.target_date.is_none() {
            set_section_target_date(conn, &section.id, Some(date))?;
            return Ok(Section {
                target_date: Some(date.to_string()),
                ..section
            });
        }
    }
    Ok(section)
}

// ── the GSD board ─────────────────────────────────────────────────────────

/// The next dated, uncompleted section in a project, with its task counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardMilestone {
    pub section_id: String,
    pub name: String,
    pub target_date: Option<String>,
    pub open_count: i64,
    pub total_count: i64,
}

/// One row of the GSD board: is this project on track, and is anything moving?
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardRow {
    pub project: String,
    /// Area or project. The board groups by it, and it changes what a quiet
    /// stretch *means* — see [`ActivityState`].
    pub kind: ProjectKind,
    /// Owning area's name, `None` for a top-level row.
    pub parent: Option<String>,
    /// A project's finish line; always `None` on an area.
    pub target_date: Option<String>,
    /// Set once the project is finished. Closed rows are archived, so they are
    /// off the board by default — this is here for the caller that asks for them.
    pub closed_at: Option<String>,
    pub health: Option<String>,
    pub last_update: Option<crate::db::project_updates::ProjectUpdate>,
    pub next_milestone: Option<BoardMilestone>,
    pub open_count: i64,
    pub blocked_count: i64,
    pub done_this_week: i64,
    /// What [`crate::db::tasks::next_task`] would hand you for this project —
    /// so the board answers "what do I do about it" as well as "how is it".
    pub next_task_title: Option<String>,
    pub next_task_short_id: Option<i64>,
    /// Open and workable now — nothing is holding it back
    /// ([`crate::db::tasks::actionable_sql`]). A future due date does NOT
    /// disqualify a task: a deadline says when work is due, not when it may
    /// begin.
    pub actionable_count: i64,
    /// Open but HELD by a date or a condition — deferred, or on an armed
    /// tripwire ([`crate::db::tasks::planned_sql`]). The name predates the
    /// retirement of the Upcoming view and is kept because the iOS client
    /// decodes this key non-optionally.
    pub planned_count: i64,
    /// Finished background work waiting on a human. The loudest signal on the
    /// board: it is the one state where the board knows something you don't.
    pub in_review_count: i64,
    pub created_this_week: i64,
    /// Newest of task edits, filings, comments and run starts across the
    /// project's live tasks. `None` = nothing has ever happened here.
    pub last_activity: Option<String>,
    /// Whole days since `last_activity`. `None` when there is none.
    pub idle_days: Option<i64>,
    /// [`ActivityState`], resolved server-side — see its docs for why.
    pub activity: ActivityState,
    /// Event counts per local day for the last [`ACTIVITY_WINDOW_DAYS`] days,
    /// **oldest first**, always exactly that many entries so the frontend can
    /// render bars without bounds-checking.
    pub activity_strip: Vec<i64>,
}

/// How alive a project is, derived from the clock and nothing else.
///
/// **The state is resolved in Rust, not in each renderer.** The app and the MCP
/// both print it, and a threshold duplicated in TypeScript is a threshold that
/// eventually disagrees with this one — the same argument
/// [`crate::db::tasks::actionable_sql`] makes for the hold predicate. Callers
/// style it; they never re-derive it.
///
/// This is deliberately NOT `projects.health`. Health is a claim someone has to
/// remember to make, and an unmaintained claim decays into decoration (invariant
/// 14's own warning, arriving). `Idle` is simply true, needs nobody, and clears
/// itself the moment anything moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    /// Something moved within [`ACTIVE_WITHIN_DAYS`].
    Active,
    /// Quiet, but not for long enough to be abandoned.
    Idle,
    /// Untouched for [`DORMANT_AFTER_DAYS`]+.
    Dormant,
    /// Nothing has ever happened in this project.
    Empty,
}

/// Days of history in the activity strip. Two weeks: long enough for a weekly
/// rhythm to be visible, short enough that one busy day doesn't flatten the rest.
pub const ACTIVITY_WINDOW_DAYS: i64 = 14;
/// Touched this recently and the project is moving. Three days spans a weekend,
/// so Friday's work still reads as active on Monday morning.
pub const ACTIVE_WITHIN_DAYS: i64 = 3;
/// Untouched this long and it isn't a lull, it's a project you've left.
pub const DORMANT_AFTER_DAYS: i64 = 30;

impl ActivityState {
    /// The one place the thresholds are applied.
    pub fn from_idle_days(idle_days: Option<i64>) -> Self {
        match idle_days {
            None => ActivityState::Empty,
            Some(d) if d <= ACTIVE_WITHIN_DAYS => ActivityState::Active,
            Some(d) if d < DORMANT_AFTER_DAYS => ActivityState::Idle,
            Some(_) => ActivityState::Dormant,
        }
    }
}

/// Every timestamp that counts as "something happened in this project", as one
/// UNION ALL over the three tables that record activity.
///
/// `tasks.updated_at` covers edits and completions; `created_at` covers filing
/// (a project where you only ever file is genuinely active); comments cover
/// human and agent notes; `task_runs.started_at` covers background work — a run
/// that fires overnight should make the project read as moving in the morning.
///
/// Every branch is filtered to `LIVE` (invariant 13): a trashed task's history
/// must not keep a dead project looking alive.
fn activity_events_sql() -> String {
    let live = crate::db::tasks::LIVE;
    format!(
        "SELECT t.project AS project, datetime(t.updated_at) AS stamp
           FROM tasks t WHERE t.project IS NOT NULL AND {live}
         UNION ALL
         SELECT t.project, datetime(t.created_at)
           FROM tasks t WHERE t.project IS NOT NULL AND {live}
         UNION ALL
         SELECT t.project, datetime(c.created_at)
           FROM task_comments c JOIN tasks t ON t.id = c.task_id
          WHERE t.project IS NOT NULL AND {live}
         UNION ALL
         SELECT t.project, datetime(r.started_at)
           FROM task_runs r JOIN tasks t ON t.id = r.task_id
          WHERE t.project IS NOT NULL AND {live}"
    )
}

/// Every project, with enough state to see what's on track and what's rotting.
///
/// Two things about the implementation are load-bearing:
///
/// * **Every count respects `tasks::LIVE`** (invariant 13). The board is a new
///   surface that hands work out — the `next_task_*` fields especially — so a
///   trashed task leaking into it would be the same class of bug as a trashed
///   task firing a reminder.
/// * **`blocked_task_ids` is computed once**, not once per project. It's a
///   whole-graph query; running it inside the loop turns a board render into
///   N graph walks.
pub fn board(conn: &Connection) -> Result<Vec<BoardRow>, AppError> {
    let closed = closed();
    let projects = list(conn, false)?;

    let blocked = crate::db::deps::blocked_task_ids(conn)?;
    let blocked_clause = if blocked.is_empty() {
        // `IN ()` is not valid SQLite, so an empty set needs a constant-false
        // predicate rather than an empty list.
        "0".to_string()
    } else {
        let quoted: Vec<String> = blocked
            .iter()
            .map(|id| format!("'{}'", id.replace('\'', "''")))
            .collect();
        format!("id IN ({})", quoted.join(", "))
    };

    // Both activity queries run ONCE for every project, not once per project —
    // same N+1 argument as `blocked_task_ids` above. They key on the project
    // name lowercased, because `tasks.project` is a NOCASE text join and two
    // spellings of one project must not become two rows here.
    let last_activity = last_activity_by_project(conn)?;
    let strips = activity_strips(conn)?;

    let mut rows = Vec::with_capacity(projects.len());
    for p in projects {
        // An area's row covers the projects nested inside it, because that is
        // what its badge and its click-through already claim (`expand_scope`).
        // A board where "CXVentures: 3 open" and its sidebar badge disagreed
        // would make both untrustworthy.
        let scope = expand_scope(conn, &p.name)?;
        let placeholders = scope_placeholders(&scope);
        let counts_sql = format!(
            "SELECT
               COALESCE(SUM(status NOT IN ({closed})), 0),
               COALESCE(SUM(status NOT IN ({closed}) AND {blocked_clause}), 0),
               -- Both operands wrapped in datetime() per invariant 1: comparing
               -- these byte-wise would drift the week boundary by a day.
               COALESCE(SUM(status IN ({closed})
                            AND completed_at IS NOT NULL
                            AND datetime(completed_at) >= datetime('now','-7 days')), 0),
               COALESCE(SUM(status NOT IN ({closed}) AND {actionable}), 0),
               COALESCE(SUM(status NOT IN ({closed}) AND {planned}), 0),
               COALESCE(SUM(status = 'in_review'), 0),
               COALESCE(SUM(datetime(created_at) >= datetime('now','-7 days')), 0)
             FROM tasks
            WHERE project COLLATE NOCASE IN ({placeholders}) AND {live}",
            live = crate::db::tasks::LIVE,
            actionable = crate::db::tasks::actionable_sql(),
            planned = crate::db::tasks::planned_sql(),
        );
        let binds: Vec<&dyn rusqlite::ToSql> =
            scope.iter().map(|n| n as &dyn rusqlite::ToSql).collect();
        let (
            open_count,
            blocked_count,
            done_this_week,
            actionable_count,
            planned_count,
            in_review_count,
            created_this_week,
        ): (i64, i64, i64, i64, i64, i64, i64) =
            conn.query_row(&counts_sql, binds.as_slice(), |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?;

        let next_milestone = next_milestone(conn, &p.name, &closed)?;
        let next = crate::db::tasks::next_task(conn, Some(&p.name))?;

        // Activity rolls up the same way: NEWEST stamp across the scope, and
        // per-day counts SUMMED across it. Taking only the area's own tasks
        // would let an area whose every live task sits in a nested project read
        // as `Empty` while that project was worked on this morning.
        let (last_activity, idle_days) = scope
            .iter()
            .filter_map(|n| last_activity.get(&n.to_lowercase()))
            .min_by_key(|(_, days)| *days)
            .map(|(stamp, days)| (Some(stamp.clone()), Some(*days)))
            .unwrap_or((None, None));
        let mut activity_strip = vec![0i64; ACTIVITY_WINDOW_DAYS as usize];
        for n in &scope {
            if let Some(strip) = strips.get(&n.to_lowercase()) {
                for (slot, add) in activity_strip.iter_mut().zip(strip) {
                    *slot += add;
                }
            }
        }

        rows.push(BoardRow {
            last_update: crate::db::project_updates::latest(conn, &p.name)?,
            health: p.health,
            next_milestone,
            open_count,
            blocked_count,
            done_this_week,
            next_task_short_id: next.as_ref().and_then(|t| t.short_id),
            next_task_title: next.map(|t| t.title),
            actionable_count,
            planned_count,
            in_review_count,
            created_this_week,
            last_activity,
            idle_days,
            activity: ActivityState::from_idle_days(idle_days),
            activity_strip,
            kind: p.kind,
            parent: p.parent,
            target_date: p.target_date,
            closed_at: p.closed_at,
            project: p.name,
        });
    }
    Ok(rows)
}

/// Newest activity per project, with whole days elapsed since.
///
/// The day count is computed in SQL against the same `now` that picked the max,
/// so a row can never report a stamp and an age from two different clocks.
/// `julianday` difference truncated toward zero: "changed 20 hours ago" is 0
/// days idle, which is what "today" means to a reader.
fn last_activity_by_project(
    conn: &Connection,
) -> Result<std::collections::HashMap<String, (String, i64)>, AppError> {
    let sql = format!(
        "SELECT lower(project), MAX(stamp) AS last,
                CAST(julianday('now') - julianday(MAX(stamp)) AS INTEGER)
           FROM ({events})
          WHERE stamp IS NOT NULL
          GROUP BY lower(project)",
        events = activity_events_sql()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            (r.get::<_, String>(1)?, r.get::<_, i64>(2)?),
        ))
    })?;
    let mut out = std::collections::HashMap::new();
    for row in rows {
        let (k, v) = row?;
        out.insert(k, v);
    }
    Ok(out)
}

/// Per-day event counts for the last [`ACTIVITY_WINDOW_DAYS`], oldest first.
///
/// **Days are LOCAL days**, so the strip lines up with the days the user
/// actually lived; the stamps are UTC, hence `date(stamp,'localtime')` on the
/// column side and a local `date('now')` on the other (invariant 1's shape).
/// Every project gets a full-length vector — missing days are zeros, not gaps,
/// so a bar chart never has to reason about holes.
fn activity_strips(
    conn: &Connection,
) -> Result<std::collections::HashMap<String, Vec<i64>>, AppError> {
    let sql = format!(
        "SELECT lower(project),
                CAST(julianday(date('now','localtime'))
                     - julianday(date(stamp,'localtime')) AS INTEGER) AS days_ago,
                COUNT(*)
           FROM ({events})
          WHERE stamp IS NOT NULL
            AND date(stamp,'localtime') > date('now','localtime','-{window} days')
          GROUP BY lower(project), days_ago",
        events = activity_events_sql(),
        window = ACTIVITY_WINDOW_DAYS
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    let mut out: std::collections::HashMap<String, Vec<i64>> = std::collections::HashMap::new();
    for row in rows {
        let (project, days_ago, count) = row?;
        let strip = out
            .entry(project)
            .or_insert_with(|| vec![0; ACTIVITY_WINDOW_DAYS as usize]);
        // days_ago 0 is today, which is the LAST bar. A future stamp (a
        // hand-edited row, or a clock that moved) would index out of range, so
        // it is clamped into today's bucket rather than panicking the board.
        let idx = (ACTIVITY_WINDOW_DAYS - 1 - days_ago.clamp(0, ACTIVITY_WINDOW_DAYS - 1)) as usize;
        strip[idx] += count;
    }
    Ok(out)
}

/// The earliest **uncompleted dated** section in a project.
///
/// Undated sections are plain groupings and are skipped — that distinction is
/// the whole of what makes a section a milestone.
fn next_milestone(
    conn: &Connection,
    project: &str,
    closed: &str,
) -> Result<Option<BoardMilestone>, AppError> {
    let sql = format!(
        "SELECT s.id, s.name, s.target_date,
                COALESCE((SELECT COUNT(*) FROM tasks t
                           WHERE t.section_id = s.id AND t.status NOT IN ({closed})
                             AND {live}), 0),
                COALESCE((SELECT COUNT(*) FROM tasks t
                           WHERE t.section_id = s.id AND {live}), 0)
           FROM sections s
          WHERE s.project = ?1 COLLATE NOCASE
            AND s.target_date IS NOT NULL
            AND s.completed_at IS NULL
          ORDER BY datetime(s.target_date) ASC
          LIMIT 1",
        // Unqualified is unambiguous here: `sections` has neither column, so
        // both resolve against `tasks t` in the correlated subquery.
        live = crate::db::tasks::LIVE
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut found = stmt.query_map(params![project], |r| {
        Ok(BoardMilestone {
            section_id: r.get(0)?,
            name: r.get(1)?,
            target_date: r.get(2)?,
            open_count: r.get(3)?,
            total_count: r.get(4)?,
        })
    })?;
    match found.next() {
        Some(m) => Ok(Some(m?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{self, NewTask};

    fn task_in(conn: &Connection, title: &str, project: Option<&str>) -> String {
        tasks::create(
            conn,
            &NewTask {
                title: title.into(),
                project: project.map(str::to_string),
                ..Default::default()
            },
        )
        .unwrap()
        .id
    }

    #[test]
    fn a_created_project_persists_with_no_tasks() {
        // The whole point of the table: this is what makes a "+" button work.
        let conn = test_conn();
        create(&conn, "brand new", ProjectKind::Area, None).unwrap();
        let all = list(&conn, false).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "brand new");
        assert_eq!(all[0].open_count, 0);
    }

    #[test]
    fn duplicate_project_names_are_rejected_case_insensitively() {
        let conn = test_conn();
        create(&conn, "CXVentures", ProjectKind::Area, None).unwrap();
        assert!(create(&conn, "cxventures", ProjectKind::Area, None).is_err());
        assert!(create(&conn, "  cxventures  ", ProjectKind::Area, None).is_err());
    }

    #[test]
    fn open_count_counts_only_open_tasks_in_that_project() {
        let conn = test_conn();
        create(&conn, "cxventures", ProjectKind::Area, None).unwrap();
        task_in(&conn, "a", Some("cxventures"));
        let done = task_in(&conn, "b", Some("cxventures"));
        task_in(&conn, "elsewhere", Some("chrisx"));
        tasks::complete(&conn, &done).unwrap();
        let p = get_by_name(&conn, "cxventures").unwrap().unwrap();
        assert_eq!(p.open_count, 1);
    }

    /// The badge must count the rows clicking it shows, and the project
    /// drill-down is `tasks::list` with `include_subtasks` false. The real DB
    /// had a project reading "5" over a list of one row — one task and its four
    /// steps — which is the failure this pins.
    #[test]
    fn open_count_matches_the_list_the_badge_opens() {
        let conn = test_conn();
        create(&conn, "cxnotes", ProjectKind::Area, None).unwrap();
        let parent = task_in(&conn, "resolve the licence position", Some("cxnotes"));
        for step in ["add the licence copy", "send the email"] {
            tasks::create(
                &conn,
                &NewTask {
                    title: step.into(),
                    project: Some("cxnotes".into()),
                    parent_id: Some(parent.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        }

        let listed = tasks::list(
            &conn,
            &tasks::ListFilters {
                view: Some(tasks::View::All),
                project: Some("cxnotes".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let p = get_by_name(&conn, "cxnotes").unwrap().unwrap();
        assert_eq!(listed.len(), 1, "subtasks render nested, not as rows");
        assert_eq!(p.open_count, listed.len() as i64);
    }

    #[test]
    fn rename_carries_tasks_and_sections_with_it() {
        let conn = test_conn();
        create(&conn, "old name", ProjectKind::Area, None).unwrap();
        let t = task_in(&conn, "a", Some("old name"));
        create_section(&conn, Some("old name"), "phase 1").unwrap();
        crate::db::project_updates::post(
            &conn,
            "old name",
            crate::db::comments::AUTHOR_ME,
            "kicked off",
            Some("on_track"),
        )
        .unwrap();

        rename(&conn, "old name", "new name").unwrap();

        assert_eq!(
            tasks::get(&conn, &t).unwrap().unwrap().project.as_deref(),
            Some("new name"),
            "a rename that orphans its tasks is worse than no rename"
        );
        assert_eq!(list_sections(&conn, Some("new name")).unwrap().len(), 1);
        assert!(list_sections(&conn, Some("old name")).unwrap().is_empty());

        // `project_updates` is the third text join, and the one most likely to
        // be forgotten: losing it makes a renamed project read as never having
        // been reported on, which is exactly the signal the board exists for.
        assert_eq!(
            crate::db::project_updates::for_project(&conn, "new name", 10).unwrap().len(),
            1,
            "the status log must move with the rename"
        );
        assert!(crate::db::project_updates::for_project(&conn, "old name", 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            get_by_name(&conn, "new name").unwrap().unwrap().health.as_deref(),
            Some("on_track")
        );
    }

    /// Deleting a project must not leave its log behind for the *next* project
    /// that happens to reuse the name to inherit.
    #[test]
    fn deleting_a_project_deletes_its_update_log() {
        let conn = test_conn();
        crate::db::project_updates::post(
            &conn,
            "reused",
            crate::db::comments::AUTHOR_ME,
            "old project's private business",
            Some("off_track"),
        )
        .unwrap();
        delete(&conn, "reused", false).unwrap();

        create(&conn, "reused", ProjectKind::Area, None).unwrap();
        assert!(
            crate::db::project_updates::for_project(&conn, "reused", 10).unwrap().is_empty(),
            "a new project must not inherit the deleted one's status history"
        );
        assert!(
            get_by_name(&conn, "reused").unwrap().unwrap().health.is_none(),
            "…and must start as never-reported"
        );
    }

    /// Renaming onto an existing name MERGES, the way `tags::rename` does.
    /// Before v11 this was refused, which left no way to undo a project that had
    /// been split in two by the directory-name-as-project-name bug.
    #[test]
    fn rename_onto_an_existing_name_merges_into_it() {
        let conn = test_conn();
        create(&conn, "pocketbuddy", ProjectKind::Area, None).unwrap();
        let phantom = task_in(&conn, "fix broken podcast audio", Some("finance-app"));
        let real = task_in(&conn, "check SEO", Some("pocketbuddy"));
        add_repo(&conn, "finance-app", "/tmp/cxtasks-test/finance-app", true).unwrap();
        crate::db::project_updates::post(
            &conn,
            "finance-app",
            crate::db::comments::AUTHOR_ME,
            "filed against the directory name by mistake",
            Some("at_risk"),
        )
        .unwrap();

        let merged = rename(&conn, "finance-app", "pocketbuddy").unwrap();
        assert_eq!(merged.name, "pocketbuddy");

        let names: Vec<_> = list(&conn, true).unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["pocketbuddy"], "the phantom record must be gone");

        for t in [&phantom, &real] {
            assert_eq!(
                tasks::get(&conn, t).unwrap().unwrap().project.as_deref(),
                Some("pocketbuddy")
            );
        }
        assert_eq!(merged.open_count, 2);
        // The repo mapping and the written log follow the work.
        assert_eq!(
            primary_repo(&conn, "pocketbuddy").unwrap().as_deref(),
            Some("/tmp/cxtasks-test/finance-app"),
            "a merge that drops the repo mapping silently un-fixes the thing it was for"
        );
        assert_eq!(
            crate::db::project_updates::for_project(&conn, "pocketbuddy", 10).unwrap().len(),
            1
        );
    }

    /// The collision case: both sides have a "phase 1". `sections` is
    /// UNIQUE(project, name), so the merge has to resolve it — and the resolution
    /// must move the tasks, because deleting the section instead would let
    /// `ON DELETE SET NULL` quietly ungroup them (invariant 11c).
    ///
    /// No `create(&conn, "pocketbuddy", ProjectKind::Area, None)` here **on purpose**: the target has
    /// sections but no `projects` row, which is the state `/breakdown` leaves
    /// behind when it calls `upsert_section` before filing anything. Gating the
    /// absorb on the record existing (rather than on the name) skips it here and
    /// leaves the tasks in the section that was supposed to be absorbed.
    #[test]
    fn merging_absorbs_a_colliding_section_without_ungrouping_its_tasks() {
        let conn = test_conn();
        let keep = create_section(&conn, Some("pocketbuddy"), "phase 1").unwrap();
        let doomed = create_section(&conn, Some("finance-app"), "Phase 1").unwrap();
        let solo = create_section(&conn, Some("finance-app"), "ios only").unwrap();

        let a = task_in(&conn, "in the colliding phase", Some("finance-app"));
        let b = task_in(&conn, "in the unique phase", Some("finance-app"));
        move_task_to_section(&conn, &a, Some(&doomed.id)).unwrap();
        move_task_to_section(&conn, &b, Some(&solo.id)).unwrap();

        rename(&conn, "finance-app", "pocketbuddy").unwrap();

        assert_eq!(
            tasks::get(&conn, &a).unwrap().unwrap().section_id.as_deref(),
            Some(keep.id.as_str()),
            "a colliding section's tasks must land in the surviving section, not nowhere"
        );
        assert_eq!(
            tasks::get(&conn, &b).unwrap().unwrap().section_id.as_deref(),
            Some(solo.id.as_str()),
            "a non-colliding section just comes along"
        );
        let sections = list_sections(&conn, Some("pocketbuddy")).unwrap();
        assert_eq!(sections.len(), 2, "the duplicate grouping is gone, not doubled");
        assert!(list_sections(&conn, Some("finance-app")).unwrap().is_empty());
    }

    /// **The real `finance-app` was in exactly this state**: one task filed
    /// under the name, and no `projects` row (the sidebar entry had been
    /// deleted, and invariant 11b keeps the tasks). `tasks.project` is the join
    /// key and the record is secondary, so the merge has to work on the NAME —
    /// gating it on the record makes the cleanup unavailable for precisely the
    /// mess it was built for, and reports "project not found" about a name
    /// sitting in plain view on a task.
    #[test]
    fn a_name_with_tasks_but_no_record_can_still_be_merged_away() {
        let conn = test_conn();
        create(&conn, "pocketbuddy", ProjectKind::Area, None).unwrap();
        let orphan = task_in(&conn, "fix broken podcast audio", Some("finance-app"));
        delete(&conn, "finance-app", false).unwrap(); // drop the record, keep the task
        assert!(get_by_name(&conn, "finance-app").unwrap().is_none());

        rename(&conn, "finance-app", "pocketbuddy").unwrap();

        assert_eq!(
            tasks::get(&conn, &orphan).unwrap().unwrap().project.as_deref(),
            Some("pocketbuddy")
        );
        let names: Vec<_> = list(&conn, true).unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["pocketbuddy"]);

        // A name nothing references at all is still an error, so a typo in the
        // rename box doesn't silently "succeed" having done nothing.
        assert!(rename(&conn, "never-existed", "pocketbuddy").is_err());
    }

    /// Re-casing a project must not eat its sections. `list_sections` matches
    /// COLLATE NOCASE, so for a case-only rename both "sides" of the absorb are
    /// the same rows — and without the guard every section collides with itself
    /// and gets deleted, silently ungrouping the entire project.
    #[test]
    fn recasing_a_project_leaves_its_sections_alone() {
        let conn = test_conn();
        create(&conn, "cxventures", ProjectKind::Area, None).unwrap();
        let s = create_section(&conn, Some("cxventures"), "phase 1").unwrap();
        let t = task_in(&conn, "wire the webhook", Some("cxventures"));
        move_task_to_section(&conn, &t, Some(&s.id)).unwrap();

        rename(&conn, "cxventures", "CXVentures").unwrap();

        let sections = list_sections(&conn, Some("CXVentures")).unwrap();
        assert_eq!(sections.len(), 1, "the section must survive a re-case");
        assert_eq!(sections[0].id, s.id);
        assert_eq!(
            tasks::get(&conn, &t).unwrap().unwrap().section_id.as_deref(),
            Some(s.id.as_str())
        );
        assert_eq!(list(&conn, true).unwrap()[0].name, "CXVentures");
    }

    /// A plain rename (no target) still just renames, and still carries the repos.
    #[test]
    fn a_plain_rename_carries_the_repo_mapping() {
        let conn = test_conn();
        add_repo(&conn, "old name", "/tmp/cxtasks-test/repo-a", false).unwrap();
        rename(&conn, "old name", "new name").unwrap();
        assert!(list_repos(&conn, "old name").unwrap().is_empty());
        assert_eq!(
            primary_repo(&conn, "new name").unwrap().as_deref(),
            Some("/tmp/cxtasks-test/repo-a")
        );
    }

    // ── repos ─────────────────────────────────────────────────────────────

    /// The whole point: a project name and its directory are different strings,
    /// and the directory is what an agent actually knows.
    #[test]
    fn for_repo_path_resolves_a_directory_to_the_project_that_owns_it() {
        let conn = test_conn();
        add_repo(&conn, "pocketbuddy", "/tmp/cxtasks-test/finance-app", true).unwrap();
        add_repo(&conn, "pocketbuddy", "/tmp/cxtasks-test/xcode/pocketbuddy", false).unwrap();

        for path in [
            "/tmp/cxtasks-test/finance-app",
            // A trailing slash and a subdirectory are the same repo: an agent's
            // cwd is very often below the root.
            "/tmp/cxtasks-test/finance-app/",
            "/tmp/cxtasks-test/finance-app/src/lib",
            // The iOS half of the same product resolves to the same project,
            // which is the entire reason repos are many-per-project.
            "/tmp/cxtasks-test/xcode/pocketbuddy",
        ] {
            assert_eq!(
                for_repo_path(&conn, path).unwrap().as_deref(),
                Some("pocketbuddy"),
                "{path} should resolve to pocketbuddy"
            );
        }

        assert_eq!(
            for_repo_path(&conn, "/tmp/cxtasks-test/somewhere-else").unwrap(),
            None,
            "an unmapped directory resolves to nothing — it must NOT fall back to \
             its own basename, which is the bug this table exists to fix"
        );
        // A sibling whose name merely starts with a mapped path is not inside it.
        assert_eq!(
            for_repo_path(&conn, "/tmp/cxtasks-test/finance-app-old").unwrap(),
            None
        );
    }

    #[test]
    fn for_repo_path_prefers_the_most_specific_owner() {
        let conn = test_conn();
        add_repo(&conn, "outer", "/tmp/cxtasks-test/nest", true).unwrap();
        add_repo(&conn, "inner", "/tmp/cxtasks-test/nest/packages/app", true).unwrap();
        assert_eq!(
            for_repo_path(&conn, "/tmp/cxtasks-test/nest/packages/app/src").unwrap().as_deref(),
            Some("inner")
        );
        assert_eq!(
            for_repo_path(&conn, "/tmp/cxtasks-test/nest/docs").unwrap().as_deref(),
            Some("outer")
        );
    }

    /// One owner per repo is what makes the reverse lookup a function rather
    /// than a guess, so a second claim is a named error, not a silent move.
    #[test]
    fn a_repo_belongs_to_exactly_one_project() {
        let conn = test_conn();
        add_repo(&conn, "pocketbuddy", "/tmp/cxtasks-test/finance-app", true).unwrap();
        let err = add_repo(&conn, "chrisx", "/tmp/cxtasks-test/finance-app", true)
            .expect_err("a second project must not be able to claim it");
        assert!(
            err.to_string().contains("pocketbuddy"),
            "the error must name the current owner, got: {err}"
        );
        // …but re-adding it to its own project is a no-op, not an error.
        add_repo(&conn, "pocketbuddy", "/tmp/cxtasks-test/finance-app/", false).unwrap();
        assert_eq!(list_repos(&conn, "pocketbuddy").unwrap().len(), 1);
    }

    /// Exactly one primary, through every mutation. Zero is as broken as two:
    /// with none, setting a task's project prefills nothing and reads as the
    /// mapping never having saved.
    #[test]
    fn a_project_with_repos_always_has_exactly_one_primary() {
        let conn = test_conn();
        let count_primary = |c: &Connection, p: &str| {
            list_repos(c, p).unwrap().iter().filter(|r| r.is_primary).count()
        };

        let first = add_repo(&conn, "p", "/tmp/cxtasks-test/a", false).unwrap();
        assert!(first.is_primary, "the first repo mapped becomes the default");
        let second = add_repo(&conn, "p", "/tmp/cxtasks-test/b", false).unwrap();
        assert_eq!(count_primary(&conn, "p"), 1);
        assert!(!second.is_primary);

        set_primary_repo(&conn, &second.id).unwrap();
        assert_eq!(count_primary(&conn, "p"), 1);
        assert_eq!(primary_repo(&conn, "p").unwrap().as_deref(), Some("/tmp/cxtasks-test/b"));

        // Removing the primary hands the role on rather than leaving none.
        remove_repo(&conn, &second.id).unwrap();
        assert_eq!(count_primary(&conn, "p"), 1);
        assert_eq!(primary_repo(&conn, "p").unwrap().as_deref(), Some("/tmp/cxtasks-test/a"));

        // A merge brings two primaries together and must settle on one.
        add_repo(&conn, "q", "/tmp/cxtasks-test/c", true).unwrap();
        rename(&conn, "q", "p").unwrap();
        assert_eq!(list_repos(&conn, "p").unwrap().len(), 2);
        assert_eq!(count_primary(&conn, "p"), 1);
    }

    /// Invariant 11: `project_repos` is the fourth text join keyed by project
    /// name. Deleting the project takes it (like the update log), because a
    /// later project reusing the name must not inherit a stranger's repos and
    /// silently start resolving tasks into them.
    #[test]
    fn deleting_a_project_deletes_its_repo_mapping() {
        let conn = test_conn();
        add_repo(&conn, "reused", "/tmp/cxtasks-test/repo-x", true).unwrap();
        delete(&conn, "reused", false).unwrap();
        assert!(list_repos(&conn, "reused").unwrap().is_empty());
        assert_eq!(
            for_repo_path(&conn, "/tmp/cxtasks-test/repo-x").unwrap(),
            None,
            "and the reverse lookup must stop resolving to a project that no longer exists"
        );
    }

    #[test]
    fn repo_paths_are_normalized_to_one_spelling() {
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        assert_eq!(normalize_repo_path("  /tmp/a/  "), "/tmp/a");
        assert_eq!(normalize_repo_path("~/Projects/finance-app"), format!("{home}/Projects/finance-app"));
        assert_eq!(normalize_repo_path("/"), "/");
        assert!(add_repo(&test_conn(), "p", "relative/path", false).is_err());
    }

    #[test]
    fn deleting_a_project_never_deletes_its_tasks() {
        let conn = test_conn();
        create(&conn, "doomed", ProjectKind::Area, None).unwrap();
        let t = task_in(&conn, "important work", Some("doomed"));
        delete(&conn, "doomed", false).unwrap();
        let task = tasks::get(&conn, &t).unwrap().expect("task must survive");
        assert_eq!(
            task.project.as_deref(),
            Some("doomed"),
            "tasks keep their project text by default so nothing is silently unfiled"
        );
        assert!(list(&conn, true).unwrap().is_empty());
    }

    #[test]
    fn deleting_a_project_can_optionally_unfile_its_tasks() {
        let conn = test_conn();
        create(&conn, "doomed", ProjectKind::Area, None).unwrap();
        let t = task_in(&conn, "work", Some("doomed"));
        delete(&conn, "doomed", true).unwrap();
        assert_eq!(tasks::get(&conn, &t).unwrap().unwrap().project, None);
    }

    #[test]
    fn archived_projects_are_hidden_by_default_but_retrievable() {
        let conn = test_conn();
        create(&conn, "old", ProjectKind::Area, None).unwrap();
        set_archived(&conn, "old", true).unwrap();
        assert!(list(&conn, false).unwrap().is_empty());
        assert_eq!(list(&conn, true).unwrap().len(), 1);
        set_archived(&conn, "old", false).unwrap();
        assert_eq!(list(&conn, false).unwrap().len(), 1);
    }

    // ── sections ──────────────────────────────────────────────────────────

    #[test]
    fn sections_are_scoped_to_their_project() {
        let conn = test_conn();
        create_section(&conn, Some("cxventures"), "phase 1").unwrap();
        create_section(&conn, Some("chrisx"), "mixing").unwrap();
        assert_eq!(list_sections(&conn, Some("cxventures")).unwrap().len(), 1);
        assert_eq!(list_sections(&conn, Some("chrisx")).unwrap().len(), 1);
    }

    #[test]
    fn a_view_level_section_has_no_project() {
        let conn = test_conn();
        create_section(&conn, None, "morning").unwrap();
        let s = list_sections(&conn, None).unwrap();
        assert_eq!(s.len(), 1);
        assert!(s[0].project.is_none());
        // And it must not leak into a project's list.
        assert!(list_sections(&conn, Some("cxventures")).unwrap().is_empty());
    }

    #[test]
    fn section_reordering_is_persistent_and_preserves_tasks_and_other_scopes() {
        let mut conn = test_conn();
        let a = create_section(&conn, Some("p"), "a").unwrap();
        let b = create_section(&conn, Some("p"), "b").unwrap();
        let other = create_section(&conn, Some("other"), "other").unwrap();
        let loose_a = create_section(&conn, None, "loose a").unwrap();
        let loose_b = create_section(&conn, None, "loose b").unwrap();
        let task = task_in(&conn, "keep filed", Some("p"));
        move_task_to_section(&conn, &task, Some(&a.id)).unwrap();
        set_section_target_date(&conn, &a.id, Some("2026-10-01T12:00:00Z")).unwrap();
        set_section_completed(&conn, &a.id, true).unwrap();

        reorder_sections(&mut conn, Some(" P "), &[b.id.clone(), a.id.clone()]).unwrap();
        let saved = list_sections(&conn, Some("p")).unwrap();
        assert_eq!(saved.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), [b.id.as_str(), a.id.as_str()]);
        assert_eq!(saved.iter().map(|s| s.sort_order).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(saved[1].target_date.as_deref(), Some("2026-10-01T12:00:00Z"));
        assert!(saved[1].completed_at.is_some());
        assert_eq!(tasks::get(&conn, &task).unwrap().unwrap().section_id, Some(a.id));
        assert_eq!(list_sections(&conn, Some("other")).unwrap()[0].sort_order, other.sort_order);

        reorder_sections(&mut conn, None, &[loose_b.id.clone(), loose_a.id.clone()]).unwrap();
        assert_eq!(list_sections(&conn, None).unwrap()[0].id, loose_b.id);
        assert_eq!(list_sections(&conn, Some("p")).unwrap()[0].id, b.id);
        let added = create_section(&conn, Some("p"), "new").unwrap();
        assert_eq!(list_sections(&conn, Some("p")).unwrap().last().unwrap().id, added.id);
    }

    #[test]
    fn invalid_section_orders_leave_the_entire_order_unchanged() {
        let mut conn = test_conn();
        let a = create_section(&conn, Some("p"), "a").unwrap();
        let b = create_section(&conn, Some("p"), "b").unwrap();
        let other = create_section(&conn, None, "other").unwrap();
        for ids in [
            vec![b.id.clone()],
            vec![b.id.clone(), b.id.clone()],
            vec![b.id.clone(), other.id.clone()],
            vec![b.id.clone(), "missing".into()],
            vec![b.id.clone(), a.id.clone(), other.id.clone()],
        ] {
            assert!(reorder_sections(&mut conn, Some("p"), &ids).is_err());
            let saved = list_sections(&conn, Some("p")).unwrap();
            assert_eq!(saved[0].id, a.id);
            assert_eq!(saved[0].sort_order, a.sort_order);
            assert_eq!(saved[1].id, b.id);
            assert_eq!(saved[1].sort_order, b.sort_order);
        }
    }

    #[test]
    fn the_same_section_name_can_exist_in_two_projects() {
        let conn = test_conn();
        create_section(&conn, Some("a"), "phase 1").unwrap();
        create_section(&conn, Some("b"), "phase 1").expect("names are per-project");
        assert!(
            create_section(&conn, Some("a"), "phase 1").is_err(),
            "but must be unique within one project"
        );
    }

    #[test]
    fn deleting_a_section_ungroups_its_tasks_rather_than_deleting_them() {
        let conn = test_conn();
        let s = create_section(&conn, Some("p"), "phase 1").unwrap();
        let t = task_in(&conn, "real work", Some("p"));
        move_task_to_section(&conn, &t, Some(&s.id)).unwrap();
        assert_eq!(
            tasks::get(&conn, &t).unwrap().unwrap().section_id.as_deref(),
            Some(s.id.as_str())
        );

        delete_section(&conn, &s.id).unwrap();
        let task = tasks::get(&conn, &t)
            .unwrap()
            .expect("tidying up a grouping must never destroy the work in it");
        assert_eq!(task.section_id, None, "the task should just be ungrouped");
    }

    #[test]
    fn a_task_can_be_moved_between_and_out_of_sections() {
        let conn = test_conn();
        let a = create_section(&conn, Some("p"), "a").unwrap();
        let b = create_section(&conn, Some("p"), "b").unwrap();
        let t = task_in(&conn, "x", Some("p"));
        move_task_to_section(&conn, &t, Some(&a.id)).unwrap();
        move_task_to_section(&conn, &t, Some(&b.id)).unwrap();
        assert_eq!(
            tasks::get(&conn, &t).unwrap().unwrap().section_id.as_deref(),
            Some(b.id.as_str())
        );
        move_task_to_section(&conn, &t, None).unwrap();
        assert_eq!(tasks::get(&conn, &t).unwrap().unwrap().section_id, None);
    }

    #[test]
    fn blank_names_are_rejected() {
        let conn = test_conn();
        assert!(create(&conn, "   ", ProjectKind::Area, None).is_err());
        assert!(create_section(&conn, Some("p"), " ").is_err());
    }

    // ── milestones & the board ────────────────────────────────────────────

    /// ISO-8601 UTC with a trailing Z, `days` from now — the format
    /// `target_date` stores, matching `tasks.due_at`.
    fn in_days(conn: &Connection, days: i64) -> String {
        conn.query_row(
            &format!("SELECT strftime('%Y-%m-%dT%H:%M:%S.000Z','now','{days:+} days')"),
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn upsert_section_reuses_and_never_overwrites_an_existing_date() {
        let conn = test_conn();
        let first = upsert_section(&conn, Some("p"), "phase 1", Some("2026-08-01T00:00:00.000Z"))
            .unwrap();
        assert_eq!(first.target_date.as_deref(), Some("2026-08-01T00:00:00.000Z"));

        // Re-running a breakdown must find the phase, not fail the UNIQUE
        // constraint and not stomp a date the user moved by hand afterwards.
        set_section_target_date(&conn, &first.id, Some("2026-09-15T00:00:00.000Z")).unwrap();
        let again =
            upsert_section(&conn, Some("p"), "Phase 1", Some("2026-08-01T00:00:00.000Z")).unwrap();
        assert_eq!(again.id, first.id, "must reuse the existing section");
        assert_eq!(
            list_sections(&conn, Some("p")).unwrap()[0].target_date.as_deref(),
            Some("2026-09-15T00:00:00.000Z"),
            "a hand-moved milestone date must survive a re-run of the plan"
        );
        assert_eq!(list_sections(&conn, Some("p")).unwrap().len(), 1);

        // An undated section can still be dated later by an upsert.
        let plain = upsert_section(&conn, Some("p"), "phase 2", None).unwrap();
        assert!(plain.target_date.is_none(), "an undated section is not a milestone");
        let dated = upsert_section(&conn, Some("p"), "phase 2", Some("2026-10-01T00:00:00.000Z"))
            .unwrap();
        assert_eq!(dated.target_date.as_deref(), Some("2026-10-01T00:00:00.000Z"));
    }

    #[test]
    fn next_milestone_is_the_earliest_uncompleted_dated_section() {
        let conn = test_conn();
        create(&conn, "p", ProjectKind::Area, None).unwrap();
        let soon = upsert_section(&conn, Some("p"), "soon", Some(&in_days(&conn, 3))).unwrap();
        upsert_section(&conn, Some("p"), "later", Some(&in_days(&conn, 30))).unwrap();
        // An undated section is a plain grouping and must never be picked.
        upsert_section(&conn, Some("p"), "someday", None).unwrap();

        let row = &board(&conn).unwrap()[0];
        assert_eq!(row.next_milestone.as_ref().unwrap().name, "soon");

        // Completing the nearest one advances to the next.
        set_section_completed(&conn, &soon.id, true).unwrap();
        assert_eq!(
            board(&conn).unwrap()[0].next_milestone.as_ref().unwrap().name,
            "later"
        );

        // …and reopening it brings it back.
        set_section_completed(&conn, &soon.id, false).unwrap();
        assert_eq!(
            board(&conn).unwrap()[0].next_milestone.as_ref().unwrap().name,
            "soon"
        );
    }

    #[test]
    fn board_reports_health_last_update_and_next_milestone() {
        let conn = test_conn();
        create(&conn, "acme", ProjectKind::Area, None).unwrap();
        let phase = upsert_section(&conn, Some("acme"), "phase 1", Some(&in_days(&conn, 7))).unwrap();

        let a = task_in(&conn, "wire the webhook", Some("acme"));
        let b = task_in(&conn, "write the intake form", Some("acme"));
        let done = task_in(&conn, "send the deck", Some("acme"));
        move_task_to_section(&conn, &a, Some(&phase.id)).unwrap();
        move_task_to_section(&conn, &b, Some(&phase.id)).unwrap();
        tasks::complete(&conn, &done).unwrap();

        // `a` waits on `b`, so it counts as blocked and must not be handed out.
        crate::db::deps::add(&conn, &a, &b).unwrap();

        crate::db::project_updates::post(
            &conn,
            "acme",
            crate::db::comments::AUTHOR_ME,
            "deck sent, waiting on Dana",
            Some("at_risk"),
        )
        .unwrap();

        let rows = board(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.project, "acme");
        assert_eq!(r.health.as_deref(), Some("at_risk"));
        assert_eq!(r.last_update.as_ref().unwrap().body, "deck sent, waiting on Dana");
        assert_eq!(r.open_count, 2);
        assert_eq!(r.blocked_count, 1);
        assert_eq!(r.done_this_week, 1);

        let m = r.next_milestone.as_ref().unwrap();
        assert_eq!((m.name.as_str(), m.open_count, m.total_count), ("phase 1", 2, 2));

        // The board's next-task must obey the same blocked rule as `next_task`.
        assert_eq!(
            r.next_task_title.as_deref(),
            Some("write the intake form"),
            "a blocked task must not be offered on the board either"
        );
        assert!(r.next_task_short_id.is_some());
    }

    /// A project nobody has reported on shows a NULL health — the gray dot —
    /// rather than being presumed healthy.
    #[test]
    fn a_never_reported_project_has_no_health_and_no_update() {
        let conn = test_conn();
        create(&conn, "quiet", ProjectKind::Area, None).unwrap();
        let r = &board(&conn).unwrap()[0];
        assert!(r.health.is_none());
        assert!(r.last_update.is_none());
        assert!(r.next_milestone.is_none());
        assert_eq!((r.open_count, r.blocked_count, r.done_this_week), (0, 0, 0));
        assert!(r.next_task_title.is_none());
    }

    /// A project with no tasks at all has never had anything happen in it, and
    /// that is `Empty` — NOT `Dormant`. "You have left this project" and "this
    /// project has never begun" are different sentences, and only the first is
    /// a nudge; rendering a brand-new project as abandoned is the same
    /// accusation-by-default that the health dot's "never reported" made.
    #[test]
    fn a_project_with_no_activity_is_empty_not_dormant() {
        let conn = test_conn();
        create(&conn, "unstarted", ProjectKind::Area, None).unwrap();
        let r = &board(&conn).unwrap()[0];
        assert_eq!(r.activity, ActivityState::Empty);
        assert!(r.last_activity.is_none());
        assert!(r.idle_days.is_none());
        assert_eq!(
            r.activity_strip.len(),
            ACTIVITY_WINDOW_DAYS as usize,
            "the strip is always full length so the renderer never bounds-checks"
        );
        assert!(r.activity_strip.iter().all(|n| *n == 0));
    }

    /// Filing a task is activity, and today's events land in the LAST bar.
    #[test]
    fn filing_a_task_makes_a_project_active_today() {
        let conn = test_conn();
        create(&conn, "moving", ProjectKind::Area, None).unwrap();
        task_in(&conn, "do the thing", Some("moving"));

        let r = &board(&conn).unwrap()[0];
        assert_eq!(r.activity, ActivityState::Active);
        assert_eq!(r.idle_days, Some(0), "just-filed is zero days idle");
        assert!(r.last_activity.is_some());
        assert!(
            *r.activity_strip.last().unwrap() > 0,
            "today is the rightmost bar: {:?}",
            r.activity_strip
        );
        assert!(r.activity_strip[..ACTIVITY_WINDOW_DAYS as usize - 1]
            .iter()
            .all(|n| *n == 0));
    }

    /// The thresholds, at their exact boundaries. `from_idle_days` is the ONE
    /// place they are applied, so this is the whole rule under test.
    #[test]
    fn activity_state_thresholds_are_inclusive_where_they_claim_to_be() {
        use ActivityState::*;
        assert_eq!(ActivityState::from_idle_days(None), Empty);
        assert_eq!(ActivityState::from_idle_days(Some(0)), Active);
        assert_eq!(ActivityState::from_idle_days(Some(ACTIVE_WITHIN_DAYS)), Active);
        assert_eq!(ActivityState::from_idle_days(Some(ACTIVE_WITHIN_DAYS + 1)), Idle);
        assert_eq!(ActivityState::from_idle_days(Some(DORMANT_AFTER_DAYS - 1)), Idle);
        assert_eq!(ActivityState::from_idle_days(Some(DORMANT_AFTER_DAYS)), Dormant);
    }

    /// Old activity reads as idle, and the strip only covers its window.
    ///
    /// The fixture back-dates `updated_at` directly, which is the only way to
    /// age a row without a sleep — and `updated_at` is one of the four event
    /// sources, so this also proves the UNION is reading that column.
    #[test]
    fn an_untouched_project_goes_idle_and_falls_off_the_strip() {
        let conn = test_conn();
        create(&conn, "quiet", ProjectKind::Area, None).unwrap();
        let id = task_in(&conn, "stale work", Some("quiet"));
        conn.execute(
            "UPDATE tasks SET created_at = datetime('now','-20 days'),
                              updated_at = datetime('now','-20 days')
             WHERE id = ?1",
            params![id],
        )
        .unwrap();

        let r = &board(&conn).unwrap()[0];
        assert_eq!(r.activity, ActivityState::Idle);
        assert_eq!(r.idle_days, Some(20));
        assert!(
            r.activity_strip.iter().all(|n| *n == 0),
            "20 days ago is outside a {ACTIVITY_WINDOW_DAYS}-day window: {:?}",
            r.activity_strip
        );
    }

    /// A trashed task must not keep a dead project looking alive — invariant 13
    /// applied to the activity signals, which are four new queries over `tasks`.
    #[test]
    fn a_trashed_tasks_history_does_not_keep_a_project_active() {
        let conn = test_conn();
        create(&conn, "abandoned", ProjectKind::Area, None).unwrap();
        let id = task_in(&conn, "the only task", Some("abandoned"));
        assert_eq!(board(&conn).unwrap()[0].activity, ActivityState::Active);

        tasks::trash(&conn, &id).unwrap();
        let r = &board(&conn).unwrap()[0];
        assert_eq!(r.activity, ActivityState::Empty);
        assert!(r.activity_strip.iter().all(|n| *n == 0));
    }

    /// The board's actionable/planned counts must agree with the views, because
    /// they are the same predicates — a task deferred into the future is
    /// planned, not actionable, on both surfaces. A task merely DUE later is
    /// actionable: a deadline holds nothing back.
    #[test]
    fn board_counts_split_actionable_from_planned_like_the_views_do() {
        let conn = test_conn();
        create(&conn, "split", ProjectKind::Area, None).unwrap();
        task_in(&conn, "do now", Some("split"));

        let later = task_in(&conn, "start next month", Some("split"));
        conn.execute(
            "UPDATE tasks SET defer_until = '2099-01-01T00:00:00.000Z' WHERE id = ?1",
            params![later],
        )
        .unwrap();

        let due_later = task_in(&conn, "due next month", Some("split"));
        conn.execute(
            "UPDATE tasks SET due_at = '2099-02-01T00:00:00.000Z' WHERE id = ?1",
            params![due_later],
        )
        .unwrap();

        let r = &board(&conn).unwrap()[0];
        assert_eq!((r.open_count, r.actionable_count, r.planned_count), (3, 2, 1));

        // The same numbers the sidebar would show, by construction.
        let count = |view| {
            crate::db::tasks::list(
                &conn,
                &crate::db::tasks::ListFilters {
                    view: Some(view),
                    project: Some("split".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .len() as i64
        };
        assert_eq!(count(crate::db::tasks::View::Open), r.actionable_count);

        // `planned` has no view of its own since Upcoming was retired, so the
        // parity check is against the facet that means the same thing: held by
        // a date or a condition, which is `planned_sql` spelled out.
        let planned = crate::db::search::filtered(
            &conn,
            &crate::db::search::FilterSpec {
                project: Some("split".into()),
                waiting: Some(vec!["deferred".into(), "tripwire".into()]),
                exclude_closed: Some(true),
                ..Default::default()
            },
        )
        .unwrap()
        .len() as i64;
        assert_eq!(planned, r.planned_count);
    }

    /// Finished background work waiting on a human is the board's loudest
    /// signal, so it gets its own count rather than hiding inside `open_count`.
    #[test]
    fn in_review_work_is_counted_separately() {
        let conn = test_conn();
        create(&conn, "reviewme", ProjectKind::Area, None).unwrap();
        let id = task_in(&conn, "agent finished this", Some("reviewme"));
        tasks::update(
            &conn,
            &id,
            &tasks::TaskPatch {
                status: Some("in_review".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let r = &board(&conn).unwrap()[0];
        assert_eq!(r.in_review_count, 1);
        assert_eq!(r.created_this_week, 1);
    }

    #[test]
    fn done_this_week_counts_only_the_last_seven_days() {
        let conn = test_conn();
        create(&conn, "p", ProjectKind::Area, None).unwrap();
        let recent = task_in(&conn, "just finished", Some("p"));
        let stale = task_in(&conn, "finished ages ago", Some("p"));
        tasks::complete(&conn, &recent).unwrap();
        tasks::complete(&conn, &stale).unwrap();
        conn.execute(
            "UPDATE tasks SET completed_at = datetime('now','-8 days') WHERE id = ?1",
            params![stale],
        )
        .unwrap();

        assert_eq!(board(&conn).unwrap()[0].done_this_week, 1);

        // A task completed earlier TODAY must count — the same lexicographic
        // trap as invariant 1, where a byte-wise comparison drops it.
        let today = task_in(&conn, "closed this morning", Some("p"));
        tasks::complete(&conn, &today).unwrap();
        assert_eq!(board(&conn).unwrap()[0].done_this_week, 2);
    }

    #[test]
    fn reorder_sets_sort_order_in_the_given_sequence() {
        let mut conn = test_conn();
        create(&conn, "a", ProjectKind::Area, None).unwrap();
        create(&conn, "b", ProjectKind::Area, None).unwrap();
        create(&conn, "c", ProjectKind::Area, None).unwrap();
        reorder(&mut conn, &["c".into(), "a".into(), "b".into()]).unwrap();
        let names: Vec<_> = list(&conn, false).unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["c", "a", "b"]);
    }

    // ── PARA: areas, projects and the roll-up (v19) ───────────────────────

    /// A project created without a kind is an AREA — the direction the v19
    /// backfill took, and the direction that never invents a deadline.
    #[test]
    fn a_node_nobody_classified_is_an_area() {
        let conn = test_conn();
        let p = create(&conn, "cxventures", ProjectKind::default(), None).unwrap();
        assert_eq!(p.kind, ProjectKind::Area);
        assert!(p.parent.is_none());
        assert!(p.target_date.is_none());
        assert!(p.closed_at.is_none());
    }

    /// The roll-up rule, and the two things it has to keep in agreement.
    #[test]
    fn an_areas_scope_covers_its_nested_projects_and_nothing_else() {
        let conn = test_conn();
        create(&conn, "cxventures", ProjectKind::Area, None).unwrap();
        create(&conn, "gov contracting", ProjectKind::Project, Some("cxventures")).unwrap();
        create(&conn, "artist advisory", ProjectKind::Area, None).unwrap();

        let scope = expand_scope(&conn, "cxventures").unwrap();
        assert_eq!(scope, vec!["cxventures", "gov contracting"]);
        // A project scopes to itself: a nested project is not a container.
        assert_eq!(
            expand_scope(&conn, "gov contracting").unwrap(),
            vec!["gov contracting"]
        );
        // A sibling area is never swept in.
        assert_eq!(
            expand_scope(&conn, "artist advisory").unwrap(),
            vec!["artist advisory"]
        );
        // A name with no record at all still scopes to itself — phantom
        // projects have to stay filterable.
        assert_eq!(expand_scope(&conn, "ghost").unwrap(), vec!["ghost"]);
    }

    /// **The badge's contract**: `open_count` is a claim about the list clicking
    /// it opens, so an area's count and `tasks::list` filtered to that area must
    /// be the same number. This is the assertion that catches a roll-up added to
    /// one side and not the other.
    #[test]
    fn an_areas_badge_and_its_list_agree_after_nesting() {
        let conn = test_conn();
        create(&conn, "cxventures", ProjectKind::Area, None).unwrap();
        create(&conn, "gov contracting", ProjectKind::Project, Some("cxventures")).unwrap();
        task_in(&conn, "dmarc", Some("cxventures"));
        task_in(&conn, "file EBO", Some("gov contracting"));
        task_in(&conn, "file SBO", Some("gov contracting"));

        let area = get_by_name(&conn, "cxventures").unwrap().unwrap();
        assert_eq!(area.open_count, 3, "area rolls up its nested project");
        assert_eq!(
            get_by_name(&conn, "gov contracting").unwrap().unwrap().open_count,
            2,
            "the project counts only its own"
        );

        let listed = tasks::list(
            &conn,
            &tasks::ListFilters {
                project: Some("cxventures".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(listed.len() as i64, area.open_count);
    }

    /// Closing a project takes its leftovers out of the parent's badge. Closing
    /// something must never make the area it lived in look busier.
    #[test]
    fn an_archived_child_drops_out_of_the_roll_up() {
        let conn = test_conn();
        create(&conn, "cxventures", ProjectKind::Area, None).unwrap();
        create(&conn, "gov contracting", ProjectKind::Project, Some("cxventures")).unwrap();
        task_in(&conn, "dmarc", Some("cxventures"));
        task_in(&conn, "file EBO", Some("gov contracting"));
        assert_eq!(get_by_name(&conn, "cxventures").unwrap().unwrap().open_count, 2);

        set_closed(&conn, "gov contracting", true).unwrap();
        assert_eq!(get_by_name(&conn, "cxventures").unwrap().unwrap().open_count, 1);
        assert_eq!(expand_scope(&conn, "cxventures").unwrap(), vec!["cxventures"]);
    }

    /// The nesting rules, from the one place that enforces them: areas nest in
    /// areas to any depth, carrying everything under them; nothing nests in a
    /// project; nothing nests in itself or beneath itself.
    #[test]
    fn areas_nest_to_any_depth_but_never_under_a_project_or_in_a_loop() {
        let conn = test_conn();
        create(&conn, "products", ProjectKind::Area, None).unwrap();
        create(&conn, "artist advisory", ProjectKind::Area, None).unwrap();
        create(&conn, "retitle", ProjectKind::Project, Some("artist advisory")).unwrap();
        create(&conn, "wave 0", ProjectKind::Project, None).unwrap();

        // The move this rule change exists for: an area holding a project goes
        // inside another area, and takes its project with it.
        set_parent(&conn, "artist advisory", Some("products")).unwrap();
        let aa = get_by_name(&conn, "artist advisory").unwrap().unwrap();
        assert_eq!(aa.kind, ProjectKind::Area, "it stays an area");
        assert_eq!(aa.parent.as_deref(), Some("products"));
        assert_eq!(
            get_by_name(&conn, "retitle").unwrap().unwrap().parent.as_deref(),
            Some("artist advisory"),
            "its project comes with it"
        );

        // Projects hold sections, not projects.
        assert!(set_parent(&conn, "wave 0", Some("retitle")).is_err());
        // Nothing parents itself …
        assert!(set_parent(&conn, "products", Some("products")).is_err());
        // … or anything beneath itself, at any depth.
        let err = set_parent(&conn, "products", Some("artist advisory")).unwrap_err();
        assert!(err.to_string().contains("move 'artist advisory' out first"), "{err}");
        // An area holding anything can't become a project — projects hold sections.
        assert!(set_kind(&conn, "artist advisory", ProjectKind::Project).is_err());

        // And it all comes back out to the top level.
        set_parent(&conn, "artist advisory", None).unwrap();
        assert!(get_by_name(&conn, "artist advisory").unwrap().unwrap().parent.is_none());
    }

    /// Three levels: the badge and the list agree at EVERY level, which is the
    /// assertion a roll-up made recursive on one side and not the other fails.
    #[test]
    fn the_roll_up_reaches_every_level_and_the_badge_still_matches_the_list() {
        let conn = test_conn();
        create(&conn, "products", ProjectKind::Area, None).unwrap();
        create(&conn, "artist advisory", ProjectKind::Area, Some("products")).unwrap();
        create(&conn, "retitle", ProjectKind::Project, Some("artist advisory")).unwrap();
        task_in(&conn, "pricing page", Some("products"));
        task_in(&conn, "mentor agent", Some("artist advisory"));
        task_in(&conn, "rename column", Some("retitle"));
        task_in(&conn, "ship it", Some("retitle"));

        for (node, expected) in [("products", 4), ("artist advisory", 3), ("retitle", 2)] {
            let badge = get_by_name(&conn, node).unwrap().unwrap().open_count;
            assert_eq!(badge, expected, "{node}'s badge");
            let listed = tasks::list(
                &conn,
                &tasks::ListFilters { project: Some(node.into()), ..Default::default() },
            )
            .unwrap();
            assert_eq!(listed.len() as i64, badge, "{node}: the badge describes the list it opens");
        }
        assert_eq!(expand_scope(&conn, "products").unwrap(), vec!["products", "artist advisory", "retitle"]);

        // Closing the grandchild takes it out of every ancestor's count.
        set_closed(&conn, "retitle", true).unwrap();
        assert_eq!(get_by_name(&conn, "products").unwrap().unwrap().open_count, 2);
    }

    /// A project inherits the nearest ancestor's repo, however deep it sits.
    #[test]
    fn the_repo_is_inherited_from_the_nearest_ancestor_that_has_one() {
        let conn = test_conn();
        create(&conn, "products", ProjectKind::Area, None).unwrap();
        create(&conn, "artist advisory", ProjectKind::Area, Some("products")).unwrap();
        create(&conn, "retitle", ProjectKind::Project, Some("artist advisory")).unwrap();
        add_repo(&conn, "products", "/tmp/products-monorepo", true).unwrap();
        assert_eq!(primary_repo(&conn, "retitle").unwrap().as_deref(), Some("/tmp/products-monorepo"));
        add_repo(&conn, "artist advisory", "/tmp/artisttracker", true).unwrap();
        assert_eq!(primary_repo(&conn, "retitle").unwrap().as_deref(), Some("/tmp/artisttracker"));
    }

    /// Promoting a project to an area drops the claims only a finite thing can
    /// make — and KEEPS its place: areas nest now, so it stays where it was.
    #[test]
    fn promoting_to_an_area_clears_target_and_closure_and_keeps_its_parent() {
        let conn = test_conn();
        create(&conn, "cxventures", ProjectKind::Area, None).unwrap();
        create(&conn, "gov contracting", ProjectKind::Project, Some("cxventures")).unwrap();
        set_target_date(&conn, "gov contracting", Some("2026-12-01T00:00:00.000Z")).unwrap();
        set_closed(&conn, "gov contracting", true).unwrap();

        set_kind(&conn, "gov contracting", ProjectKind::Area).unwrap();
        let p = get_by_name(&conn, "gov contracting").unwrap().unwrap();
        assert_eq!(p.kind, ProjectKind::Area);
        assert_eq!(p.parent.as_deref(), Some("cxventures"));
        assert!(p.target_date.is_none());
        assert!(p.closed_at.is_none());
    }

    /// An area has no finish line, so neither setter will pretend it does.
    #[test]
    fn an_area_can_neither_be_dated_nor_closed() {
        let conn = test_conn();
        create(&conn, "cxventures", ProjectKind::Area, None).unwrap();
        assert!(set_target_date(&conn, "cxventures", Some("2026-12-01T00:00:00.000Z")).is_err());
        assert!(set_closed(&conn, "cxventures", true).is_err());
    }

    /// Closing archives; reopening un-archives and clears the stamp. Closing
    /// twice must not rewrite *when* it finished.
    #[test]
    fn closing_a_project_archives_it_and_keeps_the_first_stamp() {
        let conn = test_conn();
        create(&conn, "gov contracting", ProjectKind::Project, None).unwrap();
        set_closed(&conn, "gov contracting", true).unwrap();
        let first = get_by_name(&conn, "gov contracting").unwrap().unwrap();
        assert!(first.closed_at.is_some());
        assert!(first.archived);
        assert!(
            list(&conn, false).unwrap().is_empty(),
            "a closed project is off the default list"
        );

        set_closed(&conn, "gov contracting", true).unwrap();
        assert_eq!(
            get_by_name(&conn, "gov contracting").unwrap().unwrap().closed_at,
            first.closed_at
        );

        set_closed(&conn, "gov contracting", false).unwrap();
        let back = get_by_name(&conn, "gov contracting").unwrap().unwrap();
        assert!(back.closed_at.is_none());
        assert!(!back.archived);
    }

    /// A merge must carry the hierarchy, not flatten it. `parent_id` is
    /// `ON DELETE SET NULL`, so forgetting to re-point first fails silently.
    #[test]
    fn merging_two_areas_carries_the_nested_projects_over() {
        let conn = test_conn();
        create(&conn, "cxv", ProjectKind::Area, None).unwrap();
        create(&conn, "cxventures", ProjectKind::Area, None).unwrap();
        create(&conn, "gov contracting", ProjectKind::Project, Some("cxv")).unwrap();

        rename(&conn, "cxv", "cxventures").unwrap();
        assert_eq!(
            get_by_name(&conn, "gov contracting")
                .unwrap()
                .unwrap()
                .parent
                .as_deref(),
            Some("cxventures")
        );
        assert_eq!(expand_scope(&conn, "cxventures").unwrap().len(), 2);
    }

    /// Renaming an area carries its children with no extra join, which is the
    /// whole reason parenthood is keyed by id.
    #[test]
    fn renaming_an_area_moves_its_children_with_it() {
        let conn = test_conn();
        create(&conn, "cxventures", ProjectKind::Area, None).unwrap();
        create(&conn, "gov contracting", ProjectKind::Project, Some("cxventures")).unwrap();
        rename(&conn, "cxventures", "CX Ventures").unwrap();
        assert_eq!(
            get_by_name(&conn, "gov contracting")
                .unwrap()
                .unwrap()
                .parent
                .as_deref(),
            Some("CX Ventures")
        );
    }

    /// Deleting a container never deletes what was inside it — one level up
    /// from invariant 11b, and the same argument.
    #[test]
    fn deleting_an_area_lifts_its_projects_instead_of_taking_them() {
        let conn = test_conn();
        create(&conn, "cxventures", ProjectKind::Area, None).unwrap();
        create(&conn, "gov contracting", ProjectKind::Project, Some("cxventures")).unwrap();
        task_in(&conn, "file EBO", Some("gov contracting"));

        delete(&conn, "cxventures", false).unwrap();
        let child = get_by_name(&conn, "gov contracting").unwrap().unwrap();
        assert!(child.parent.is_none(), "orphaned to the top level, not deleted");
        assert_eq!(child.open_count, 1, "and it kept its work");
    }

    /// An area whose every live task sits in a nested project must not read as
    /// `Empty` while that project was worked on this morning.
    #[test]
    fn an_areas_activity_rolls_up_from_its_projects() {
        let conn = test_conn();
        create(&conn, "cxventures", ProjectKind::Area, None).unwrap();
        create(&conn, "gov contracting", ProjectKind::Project, Some("cxventures")).unwrap();
        task_in(&conn, "file EBO", Some("gov contracting"));

        let rows = board(&conn).unwrap();
        let area = rows.iter().find(|r| r.project == "cxventures").unwrap();
        assert_eq!(area.activity, ActivityState::Active);
        assert_eq!(area.open_count, 1);
        assert!(area.last_activity.is_some());
        assert!(
            area.activity_strip.iter().sum::<i64>() > 0,
            "{:?}",
            area.activity_strip
        );
        assert_eq!(area.kind, ProjectKind::Area);
        let child = rows.iter().find(|r| r.project == "gov contracting").unwrap();
        assert_eq!(child.parent.as_deref(), Some("cxventures"));
    }

    /// A project nested in an area inherits that area's repo for PREFILL, while
    /// the directory still resolves to the area that registered it.
    #[test]
    fn a_nested_project_inherits_its_areas_repo_one_way() {
        let conn = test_conn();
        create(&conn, "cxventures", ProjectKind::Area, None).unwrap();
        create(&conn, "gov contracting", ProjectKind::Project, Some("cxventures")).unwrap();
        add_repo(&conn, "cxventures", "/tmp/cxv-repo", true).unwrap();

        assert_eq!(
            primary_repo(&conn, "gov contracting").unwrap().as_deref(),
            Some("/tmp/cxv-repo")
        );
        // …but the reverse lookup still names exactly one owner.
        assert_eq!(
            for_repo_path(&conn, "/tmp/cxv-repo").unwrap().as_deref(),
            Some("cxventures")
        );

        // Its own repo outranks the inherited one.
        add_repo(&conn, "gov contracting", "/tmp/govcon-repo", true).unwrap();
        assert_eq!(
            primary_repo(&conn, "gov contracting").unwrap().as_deref(),
            Some("/tmp/govcon-repo")
        );
    }

    /// `/next cxventures` has to be able to hand back work that lives in a
    /// project nested under it — otherwise nesting a wave of tasks quietly
    /// removes them from what the area hands you.
    #[test]
    fn next_task_for_an_area_reaches_into_its_projects() {
        let conn = test_conn();
        create(&conn, "cxventures", ProjectKind::Area, None).unwrap();
        create(&conn, "gov contracting", ProjectKind::Project, Some("cxventures")).unwrap();
        task_in(&conn, "file EBO", Some("gov contracting"));
        assert_eq!(
            tasks::next_task(&conn, Some("cxventures")).unwrap().unwrap().title,
            "file EBO"
        );
    }
}
