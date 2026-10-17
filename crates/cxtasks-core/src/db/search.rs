//! FTS5 search over tasks.
//!
//! Ported wholesale from cxmail's `db/search.rs` discipline (gotcha #26):
//!   * `tasks_fts` is written **only** by the schema triggers and [`rebuild`].
//!   * User keywords **must** go through [`build_match_expr`]. Raw interpolation
//!     into `MATCH` turns a query like `budget (` or `AND` into an FTS syntax
//!     error, which surfaces as a silent "No results".
//!   * `verify_or_rebuild` repairs count drift in either direction and never
//!     panics — a broken index must not brick app launch.

use crate::db::tasks::{Task, View, NOT_DELETED};
use crate::error::AppError;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// Quote every token so FTS5 operators and punctuation in user input are
/// treated as literal text. Internal `"` is doubled per FTS5 string rules.
/// Tokens are ANDed, and the trailing token gets a `*` so search-as-you-type
/// matches prefixes.
pub fn build_match_expr(query: &str) -> Option<String> {
    // Drop tokens with nothing indexable in them. A quoted punctuation-only
    // token like `"("` is syntactically valid but matches zero documents, so
    // typing `budget (` would silently return nothing instead of the budget
    // task. Dropping them means the caller falls back to LIKE only when the
    // *entire* query is punctuation.
    let tokens: Vec<&str> = query
        .split_whitespace()
        .filter(|t| t.chars().any(|c| c.is_alphanumeric()))
        .collect();
    if tokens.is_empty() {
        return None;
    }
    let last = tokens.len() - 1;
    let parts: Vec<String> = tokens
        .iter()
        .enumerate()
        .map(|(i, tok)| {
            let escaped = tok.replace('"', "\"\"");
            if i == last && escaped.chars().all(|c| c.is_alphanumeric() || c == '_') {
                // Prefix-match the final token, but only when it has no
                // characters that would need quoting around the `*`.
                format!("\"{escaped}\"*")
            } else {
                format!("\"{escaped}\"")
            }
        })
        .collect();
    Some(parts.join(" AND "))
}

fn has_fts(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='tasks_fts')",
        [],
        |r| r.get(0),
    )
    .unwrap_or(false)
}

/// Full-text search. Falls back to a LIKE scan when the FTS table is missing
/// (a stale DB from an older binary) so search degrades instead of erroring.
pub fn search(conn: &Connection, query: &str, limit: u32) -> Result<Vec<Task>, AppError> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.clamp(1, 200);
    let ids = text_match_ids(conn, query, limit)?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(t) = crate::db::tasks::get(conn, &id)? {
            out.push(t);
        }
    }
    Ok(out)
}

/// Ids matching a free-text query, best first.
///
/// The shared text half of [`search`] and [`filtered`]: FTS-ranked when the
/// index exists and the expression is usable, LIKE otherwise. Returning IDS is
/// what lets `filtered` fold text into a facet query as one `id IN (…)` clause
/// — the same shape `tags` already takes in `db::tasks::list` — instead of
/// re-implementing the facet WHERE on this side.
fn text_match_ids(conn: &Connection, query: &str, limit: u32) -> Result<Vec<String>, AppError> {
    // A T-number is not text. `short_id` is not in the FTS index (it is a
    // number, and the index holds prose), so typing `T11` used to reach only
    // the tasks whose NOTES happened to mention T117 — never T11 itself. The
    // reference lane runs first and its hits lead the result: an exact number
    // at the top, then the prefix band (`T11` → T110…T119, search-as-you-type
    // on the way to T117), then whatever the text lanes add.
    let mut ids = short_id_ids(conn, query, limit)?;
    let text = text_only_ids(conn, query, limit)?;
    for id in text {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.truncate(limit as usize);
    Ok(ids)
}

/// Tasks whose T-number the query names, exact first, then by number.
///
/// The query must be a lone `T<digits>` (case-insensitive) — `T11 embed` is a
/// text search that happens to start with a T-number, and adding a T-number
/// band to it would mean the id-set fold in `filtered` returns tasks that match
/// neither word. Empty when the query is anything else.
fn short_id_ids(conn: &Connection, query: &str, limit: u32) -> Result<Vec<String>, AppError> {
    let Some(n) = crate::db::tasks::parse_short_ref(query) else {
        return Ok(Vec::new());
    };
    // Trashed out, archived in — the same scope as the text lanes.
    let sql = format!(
        "SELECT id FROM tasks
         WHERE {NOT_DELETED}
           AND short_id IS NOT NULL
           AND CAST(short_id AS TEXT) LIKE ?1
         ORDER BY CASE WHEN short_id = ?2 THEN 0 ELSE 1 END, short_id ASC
         LIMIT {limit}"
    );
    Ok(conn
        .prepare(&sql)?
        .query_map(params![format!("{n}%"), n], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

/// The text half of [`text_match_ids`]: FTS when it can be, LIKE otherwise.
fn text_only_ids(conn: &Connection, query: &str, limit: u32) -> Result<Vec<String>, AppError> {
    if has_fts(conn) {
        if let Some(expr) = build_match_expr(query) {
            // Trashed tasks are excluded here rather than by evicting them from
            // the index: they stay indexed so a restore needs no rebuild, and so
            // `verify_or_rebuild`'s row-count check keeps matching. Archived
            // tasks DO match — an archive you can't search is a black hole.
            let sql = format!(
                "SELECT t.id FROM tasks_fts f
                 JOIN tasks t ON t.rowid = f.rowid
                 WHERE tasks_fts MATCH ?1 AND t.{NOT_DELETED}
                 ORDER BY bm25(tasks_fts, 4.0, 1.0, 2.0)
                 LIMIT {limit}"
            );
            let ids: Result<Vec<String>, _> = conn
                .prepare(&sql)?
                .query_map([&expr], |r| r.get::<_, String>(0))?
                .collect();
            match ids {
                Ok(ids) => return Ok(ids),
                Err(e) => {
                    // A malformed MATCH shouldn't lose the user's search.
                    log::warn!("tasks_fts MATCH failed ({e}); falling back to LIKE");
                }
            }
        }
    }
    like_ids(conn, query, limit)
}

fn like_ids(conn: &Connection, query: &str, limit: u32) -> Result<Vec<String>, AppError> {
    let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
    let sql = format!(
        "SELECT id FROM tasks
         WHERE {NOT_DELETED}
           AND (   title LIKE ?1 ESCAPE '\\'
                OR COALESCE(body,'') LIKE ?1 ESCAPE '\\'
                OR COALESCE(project,'') LIKE ?1 ESCAPE '\\')
         ORDER BY datetime(updated_at) DESC
         LIMIT {limit}"
    );
    Ok(conn
        .prepare(&sql)?
        .query_map([&pattern], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

/// The filter bar's query: facets ANDed together, values within a facet ORed
/// (except `tags`, which narrows), free text folded in on top.
///
/// **Scope is `NOT_DELETED`, exactly like text search** — trashed tasks out,
/// archived tasks in, and no view. The filter bar replaces the sidebar as the
/// scope while it's active (the sidebar dims to say so); making it compose with
/// the current view instead would let a `status is done` chip return nothing
/// inside Today with no explanation on screen.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FilterSpec {
    /// Free text, matched like [`search`]. Optional — chips alone are a query.
    pub query: Option<String>,
    /// Task is in ANY of these projects.
    pub projects: Option<Vec<String>>,
    /// Task holds ANY of these statuses. Validated like `ListFilters.status`.
    pub statuses: Option<Vec<String>>,
    /// Task holds ANY of these priorities (0–3, P0 highest — invariant 9).
    pub priorities: Option<Vec<i64>>,
    /// Task carries ALL of these tags — narrowing, matching the sidebar filter
    /// and `db::tasks::list`. The one facet that ANDs its values, because
    /// that's what the existing tag semantics everywhere else already mean.
    pub tags: Option<Vec<String>>,
    /// Local calendar days, `YYYY-MM-DD`. `before` = due earlier than that day;
    /// `after` = due later than it. The named day itself is in neither — the
    /// two compose into "due outside …", and making either bound inclusive
    /// would double-count the boundary day.
    pub due_before: Option<String>,
    pub due_after: Option<String>,
    pub created_before: Option<String>,
    pub created_after: Option<String>,
    /// Tasks involving ANY of these people — `@name` tags, with or without
    /// the `@`. `["*"]` means "any person at all". OR within the facet,
    /// unlike `tags`: see `db::tags::task_ids_with_any_person`.
    pub people: Option<Vec<String>>,
    /// Leave finished work out. **Defaults to TRUE**, and the default is the
    /// interesting part.
    ///
    /// Chips replace the view rather than narrowing it, so before this the only
    /// status constraint on a filtered query was none at all — and "show me
    /// everything in CXVentures" answered with the year's completed work on
    /// top. Every VIEW that hands out work already excludes closed
    /// (`View::All`, which a project drill-down uses, is
    /// `status NOT IN ('done')`), so a filter that included it was the one
    /// surface in either app where finished tasks turned up unasked.
    ///
    /// It yields to an explicit status facet — see [`filtered`].
    pub exclude_closed: Option<bool>,

    // ── the view half (invariant 21) ───────────────────────────────────────
    /// The sidebar view this query is scoped to, or `None` for the search
    /// scope (every live task, archived included, no view).
    ///
    /// **Chips REFINE a view rather than replacing it**, which is what made
    /// this the one query builder: `db::tasks::list` is now an adapter that
    /// fills this field, so a facet cannot mean one thing in the sidebar and
    /// another in the filter strip.
    pub view: Option<View>,
    /// The NODE the sidebar has selected — an area or a project — expanded
    /// through `projects::expand_scope`.
    ///
    /// Deliberately separate from `projects`, which is the OR-facet a chip
    /// builds. Folding the two would turn "in area Y" + a `project is X` chip
    /// into "Y **or** X", widening a scope the user was narrowing.
    pub project: Option<String>,
    /// Hide tasks with an open blocker (deps only, unlike the `blocked`
    /// waiting kind, which also covers a hand-set `blocked` status).
    pub hide_blocked: Option<bool>,
    /// Include subtasks as top-level rows. Only consulted when `view` is set —
    /// the view-less search scope has always returned them.
    pub include_subtasks: Option<bool>,
    /// How to order the result. `None` = each view's own order.
    pub sort: Option<Sort>,

    // ── negatives ──────────────────────────────────────────────────────────
    // Every one of these is NULL-safe, and that is not decoration: `NOT IN`
    // over a nullable column is NULL for a NULL row, `NOT (… OR NULL)` is
    // NULL, and NULL matches nothing — so a naive negation silently drops
    // every unfiled task from "project is not X". Same trap `waiting_sql`'s
    // COALESCE was written for.
    pub exclude_statuses: Option<Vec<String>>,
    pub exclude_priorities: Option<Vec<i64>>,
    pub exclude_projects: Option<Vec<String>>,
    /// Tasks carrying NONE of these tags. ANY-negation, not ALL: "not tagged
    /// A, B" said out loud means neither, and ALL would return a task tagged
    /// only A.
    pub exclude_tags: Option<Vec<String>>,
    pub exclude_people: Option<Vec<String>>,

    /// Tasks held back in ANY of these ways — `deferred`, `tripwire`,
    /// `blocked`, or `*` for the whole [`db::tasks::waiting_sql`] predicate.
    ///
    /// Replaces the older `waiting: bool`: the Waiting view groups by WHY, so
    /// "hide the deferred ones but keep what is blocked" was unsayable while
    /// this was one flag. The MCP still speaks the bool and maps it to `["*"]`.
    pub waiting: Option<Vec<String>>,
    /// The complement — tasks held back in NONE of these ways.
    pub exclude_waiting: Option<Vec<String>>,

    pub limit: Option<u32>,
}

/// Which column the list is ordered by, and which way.
///
/// `Default` is each view's own order (board order; Waiting oldest-first;
/// Trash/Archive by when they were set aside), so it is a real choice rather
/// than the absence of one — and it is the only sort under which manual
/// drag-reorder means anything, since `sort_order` is only a tiebreaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    #[default]
    Default,
    Due,
    Priority,
    Created,
    Updated,
    Title,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct Sort {
    #[serde(default)]
    pub key: SortKey,
    #[serde(default)]
    pub dir: SortDir,
}

impl Sort {
    fn dir_sql(self) -> &'static str {
        match self.dir {
            SortDir::Asc => "ASC",
            SortDir::Desc => "DESC",
        }
    }

    /// The ORDER BY keys for this sort, or `None` for the view's own order.
    ///
    /// Undated rows sink LAST whichever way `Due` runs: an undated task has no
    /// place at the head of a list ordered by date, and flipping the direction
    /// would put every one of them there.
    fn keys(self) -> Option<String> {
        let d = self.dir_sql();
        Some(match self.key {
            SortKey::Default => return None,
            SortKey::Due => {
                format!("CASE WHEN due_at IS NULL THEN 1 ELSE 0 END ASC, datetime(due_at) {d}")
            }
            SortKey::Priority => format!("priority {d}"),
            SortKey::Created => format!("datetime(created_at) {d}"),
            SortKey::Updated => format!("datetime(updated_at) {d}"),
            SortKey::Title => format!("title COLLATE NOCASE {d}"),
        })
    }
}

/// The ways a task can be held back, as a filter vocabulary.
///
/// One name per row badge, so "Waiting is not Tripwire" takes off exactly the
/// rows wearing the tripwire badge. `*` is the whole predicate — what the MCP's
/// `waiting: true` means, and what the Waiting view is.
fn waiting_kind_sql(kind: &str) -> Result<String, AppError> {
    Ok(match kind.trim().to_ascii_lowercase().as_str() {
        "deferred" | "date" => crate::db::tasks::DEFERRED_SQL.to_string(),
        "tripwire" | "condition" => format!("({})", crate::db::tasks::TRIPWIRE_ARMED),
        "blocked" => crate::db::tasks::blocked_sql(),
        "*" | "any" => crate::db::tasks::waiting_sql(),
        other => {
            return Err(AppError::Validation(format!(
                "unknown waiting kind '{other}' — expected deferred, tripwire, blocked or *"
            )))
        }
    })
}

/// `(k1 OR k2 OR …)` for a set of waiting kinds.
fn waiting_group_sql(kinds: &[String]) -> Result<String, AppError> {
    let parts: Result<Vec<String>, AppError> = kinds.iter().map(|k| waiting_kind_sql(k)).collect();
    Ok(format!("({})", parts?.join(" OR ")))
}

impl FilterSpec {
    fn has_facets(&self) -> bool {
        fn some<T>(v: &Option<Vec<T>>) -> bool {
            v.as_ref().is_some_and(|x| !x.is_empty())
        }
        some(&self.projects)
            || some(&self.statuses)
            || some(&self.priorities)
            || some(&self.tags)
            || some(&self.people)
            || some(&self.waiting)
            || some(&self.exclude_statuses)
            || some(&self.exclude_priorities)
            || some(&self.exclude_projects)
            || some(&self.exclude_tags)
            || some(&self.exclude_people)
            || some(&self.exclude_waiting)
            || self.project.as_deref().is_some_and(|p| !p.trim().is_empty())
            || self.due_before.is_some()
            || self.due_after.is_some()
            || self.created_before.is_some()
            || self.created_after.is_some()
    }
}

/// Refuse a date bound SQLite can't parse.
///
/// Asked of SQLite itself rather than re-implemented: `datetime(x)` is NULL for
/// anything unparseable, and a NULL bound makes every comparison NULL — the
/// filter would silently match nothing, which reads as "no tasks due before
/// then" rather than as the input error it is. Same class of silence
/// `build_match_expr` exists to prevent on the FTS side.
fn validate_date(conn: &Connection, label: &str, value: &str) -> Result<(), AppError> {
    let ok: bool = conn.query_row("SELECT datetime(?1) IS NOT NULL", [value], |r| r.get(0))?;
    if ok {
        Ok(())
    } else {
        Err(AppError::Validation(format!(
            "unparseable {label} date '{value}' — expected YYYY-MM-DD"
        )))
    }
}

/// Run a [`FilterSpec`].
///
/// A wholly empty spec returns nothing rather than everything: the UI only
/// calls this once at least one chip exists, so an empty spec is a caller bug,
/// and "every task you have ever archived" is the wrong way to surface one.
pub fn filtered(conn: &Connection, spec: &FilterSpec) -> Result<Vec<Task>, AppError> {
    let limit = spec.limit.unwrap_or(500).min(2000);
    let Some((where_parts, binds)) = where_clause(conn, spec, limit)? else {
        return Ok(Vec::new());
    };

    // ── ORDER BY, in four bands ────────────────────────────────────────────
    //
    // 1. SECTIONS, so a hand-curated board keeps its shape. Only under the
    //    default sort — an explicit "by due date" that still grouped by
    //    section would be answering a question nobody asked — and never in a
    //    hidden view, which orders by when things were set aside.
    // 2. The SINK: deferred and armed rows fall to the bottom of whatever band
    //    they are in. Under every sort, because "not yet" is a property of the
    //    task rather than of the ordering. Not in Waiting (every row there is
    //    held back, and the view already groups by WHY) and not in
    //    Trash/Archive.
    // 3. The SORT keys — the view's own order, or the user's choice.
    // 4. Tiebreakers, ending in `rowid` so the order is total and a redraw
    //    cannot reshuffle two otherwise-equal rows.
    let sort = spec.sort.unwrap_or_default();
    let hidden_view = spec.view.is_some_and(|v| v.is_hidden_view());
    let mut order: Vec<String> = Vec::new();
    if !hidden_view && sort.key == SortKey::Default {
        order.push(crate::db::tasks::SECTION_ORDER.to_string());
    }
    if !matches!(
        spec.view,
        Some(View::Waiting) | Some(View::Trash) | Some(View::Archive)
    ) {
        order.push(format!(
            "CASE WHEN {} OR {} THEN 1 ELSE 0 END ASC",
            crate::db::tasks::DEFERRED_SQL,
            crate::db::tasks::TRIPWIRE_ARMED
        ));
    }
    order.push(
        sort.keys()
            .unwrap_or_else(|| crate::db::tasks::view_default_order(spec.view).to_string()),
    );
    order.push("sort_order ASC, datetime(created_at) ASC, rowid ASC".into());

    // One query, selecting the columns — not a list of ids re-fetched one
    // `tasks::get` at a time, which is what this did while it was only the
    // filter bar's path and is N+1 now that it is every list in the app.
    let columns = crate::db::tasks::COLUMNS;
    let sql = format!(
        "SELECT {columns} FROM tasks WHERE {} ORDER BY {} LIMIT {limit}",
        where_parts.join(" AND "),
        order.join(", ")
    );
    Ok(conn
        .prepare(&sql)?
        .query_map(
            rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
            crate::db::tasks::row_to_task,
        )?
        .collect::<Result<Vec<_>, _>>()?)
}

/// How many rows [`filtered`] would return for `spec`, ignoring its limit.
///
/// The sidebar's node badges. A badge is a claim about the list clicking it
/// opens, and that list is `filtered` with the node's own saved chips — so the
/// count has to be the SAME WHERE clause, not `projects::list`'s "open and
/// top-level", which is what let an area read "8" over a list that said
/// "Nothing here" once its saved filters hid the held rows. One clause,
/// [`where_clause`], serves both, so they cannot drift.
pub fn count(conn: &Connection, spec: &FilterSpec) -> Result<i64, AppError> {
    let limit = spec.limit.unwrap_or(500).min(2000);
    let Some((where_parts, binds)) = where_clause(conn, spec, limit)? else {
        return Ok(0);
    };
    let sql = format!("SELECT COUNT(*) FROM tasks WHERE {}", where_parts.join(" AND "));
    Ok(conn.query_row(
        &sql,
        rusqlite::params_from_iter(binds.iter().map(|b| b.as_ref())),
        |r| r.get(0),
    )?)
}

/// [`count`] for several specs in one call — the sidebar asks for every node
/// at once rather than one round-trip per row.
pub fn count_many(conn: &Connection, specs: &[FilterSpec]) -> Result<Vec<i64>, AppError> {
    specs.iter().map(|s| count(conn, s)).collect()
}

/// The WHERE clause a [`FilterSpec`] compiles to, with its binds — or `None`
/// when the spec can match nothing (an empty spec, a tag nobody carries, text
/// that hits no row), so callers return their empty value without a query.
///
/// `limit` only bounds the text-match id set; the row limit is the caller's.
fn where_clause(
    conn: &Connection,
    spec: &FilterSpec,
    limit: u32,
) -> Result<Option<(Vec<String>, Vec<Box<dyn rusqlite::ToSql>>)>, AppError> {
    let query = spec.query.as_deref().map(str::trim).unwrap_or("");
    // A VIEW is itself a query, so the empty-spec guard only applies to the
    // view-less search scope — where an empty spec really is a caller bug.
    if spec.view.is_none() && query.is_empty() && !spec.has_facets() {
        return Ok(None);
    }

    let mut where_parts: Vec<String> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    // Scope. A view supplies its own (`LIVE`, or the Trash/Archive rules); the
    // view-less search scope is `NOT_DELETED` — trashed out, archived in.
    match spec.view {
        Some(view) => where_parts.extend(crate::db::tasks::view_where(view)),
        None => where_parts.push(NOT_DELETED.into()),
    }

    // The sidebar's selected node, through `expand_scope` (v19) so an AREA
    // brings the projects nested under it. Separate from the `projects` facet
    // so a project chip inside an area narrows instead of widening.
    if let Some(node) = spec.project.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        let scope = crate::db::projects::expand_scope(conn, node)?;
        let placeholders = crate::db::projects::scope_placeholders(&scope);
        where_parts.push(format!("project COLLATE NOCASE IN ({placeholders})"));
        for name in scope {
            binds.push(Box::new(name));
        }
    }

    if let Some(projects) = spec.projects.as_ref().filter(|p| !p.is_empty()) {
        // Each facet value expands through `projects::expand_scope` (v19), so a
        // project chip naming an AREA matches the projects nested under it. The
        // filter bar's chips are built from the same sidebar rows whose badges
        // already roll up; a chip that didn't would return fewer tasks than the
        // number printed next to the name it was made from.
        let mut scoped: Vec<String> = Vec::new();
        for p in projects {
            for name in crate::db::projects::expand_scope(conn, p)? {
                if !scoped.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                    scoped.push(name);
                }
            }
        }
        if scoped.is_empty() {
            return Ok(None);
        }
        let placeholders = vec!["?"; scoped.len()].join(", ");
        where_parts.push(format!("project COLLATE NOCASE IN ({placeholders})"));
        for p in scoped {
            binds.push(Box::new(p));
        }
    }
    if let Some(statuses) = spec.statuses.as_ref().filter(|s| !s.is_empty()) {
        for s in statuses {
            if !crate::db::tasks::is_valid_status(s) {
                return Err(AppError::Validation(format!("unknown status '{s}'")));
            }
        }
        let placeholders = vec!["?"; statuses.len()].join(", ");
        where_parts.push(format!("status IN ({placeholders})"));
        for s in statuses {
            binds.push(Box::new(s.clone()));
        }
    }
    // Closed work is out unless it was ASKED for.
    //
    // "Asked for" means a status facet, not the flag: someone who picks
    // `status is done` has said exactly what they want, and applying the
    // exclusion on top would return nothing for a chip the UI offers — a filter
    // that answers its own question with silence. So an explicit status wins,
    // and the flag exists for a caller that wants closed work back WITHOUT
    // naming a status.
    //
    // The DEFAULT flips on `view`: a view-scoped caller (the app, through
    // `tasks::list`) states its own closure rule as `exclude_statuses`, which
    // the filter strip renders as a removable chip — so a second implicit rule
    // here would make that chip impossible to take off. A view-less caller
    // (MCP `search_tasks`) keeps the old default-true.
    let named_status = spec.statuses.as_ref().is_some_and(|s| !s.is_empty());
    if spec.exclude_closed.unwrap_or(spec.view.is_none()) && !named_status {
        let closed = crate::db::tasks::CLOSED_STATUSES
            .iter()
            .map(|s| format!("'{s}'"))
            .collect::<Vec<_>>()
            .join(", ");
        where_parts.push(format!("status NOT IN ({closed})"));
    }
    if let Some(priorities) = spec.priorities.as_ref().filter(|p| !p.is_empty()) {
        for p in priorities {
            if !(0..=3).contains(p) {
                return Err(AppError::Validation(format!("priority {p} is not 0–3")));
            }
        }
        let placeholders = vec!["?"; priorities.len()].join(", ");
        where_parts.push(format!("priority IN ({placeholders})"));
        for p in priorities {
            binds.push(Box::new(*p));
        }
    }
    if let Some(tags) = spec.tags.as_ref().filter(|t| !t.is_empty()) {
        let ids = crate::db::tags::task_ids_with_all(conn, tags)?;
        if ids.is_empty() {
            return Ok(None);
        }
        let placeholders = vec!["?"; ids.len()].join(", ");
        where_parts.push(format!("id IN ({placeholders})"));
        for id in ids {
            binds.push(Box::new(id));
        }
    }
    // People take the tags facet's shape (an id set) but OR within the facet.
    if let Some(people) = spec.people.as_ref().filter(|p| !p.is_empty()) {
        let ids = crate::db::tags::task_ids_with_any_person(conn, people)?;
        if ids.is_empty() {
            return Ok(None);
        }
        let placeholders = vec!["?"; ids.len()].join(", ");
        where_parts.push(format!("id IN ({placeholders})"));
        for id in ids {
            binds.push(Box::new(id));
        }
    }
    if let Some(kinds) = spec.waiting.as_ref().filter(|k| !k.is_empty()) {
        where_parts.push(waiting_group_sql(kinds)?);
    }
    if let Some(kinds) = spec.exclude_waiting.as_ref().filter(|k| !k.is_empty()) {
        where_parts.push(format!("NOT {}", waiting_group_sql(kinds)?));
    }

    // ── negatives ──────────────────────────────────────────────────────────
    if let Some(statuses) = spec.exclude_statuses.as_ref().filter(|s| !s.is_empty()) {
        for st in statuses {
            if !crate::db::tasks::is_valid_status(st) {
                return Err(AppError::Validation(format!("unknown status '{st}'")));
            }
        }
        let placeholders = vec!["?"; statuses.len()].join(", ");
        // `status` is NOT NULL, so a bare NOT IN is safe here — and only here.
        where_parts.push(format!("status NOT IN ({placeholders})"));
        for st in statuses {
            binds.push(Box::new(st.clone()));
        }
    }
    if let Some(priorities) = spec.exclude_priorities.as_ref().filter(|p| !p.is_empty()) {
        for pr in priorities {
            if !(0..=3).contains(pr) {
                return Err(AppError::Validation(format!("priority {pr} is not 0–3")));
            }
        }
        let placeholders = vec!["?"; priorities.len()].join(", ");
        where_parts.push(format!("priority NOT IN ({placeholders})"));
        for pr in priorities {
            binds.push(Box::new(*pr));
        }
    }
    if let Some(projects) = spec.exclude_projects.as_ref().filter(|p| !p.is_empty()) {
        let mut scoped: Vec<String> = Vec::new();
        for pr in projects {
            for name in crate::db::projects::expand_scope(conn, pr)? {
                if !scoped.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                    scoped.push(name);
                }
            }
        }
        if !scoped.is_empty() {
            let placeholders = vec!["?"; scoped.len()].join(", ");
            // COALESCE, because `project` is NULLABLE: a bare NOT IN is NULL
            // for an unfiled task, and NULL matches nothing — so "not in Acme"
            // would silently hide every task with no project at all.
            where_parts.push(format!(
                "COALESCE(project,'') COLLATE NOCASE NOT IN ({placeholders})"
            ));
            for pr in scoped {
                binds.push(Box::new(pr));
            }
        }
    }
    // Tags and people negate as a correlated NOT EXISTS rather than
    // `id NOT IN (<id list>)`: the subquery is never NULL, and it doesn't
    // materialize a list that could be empty for one reason and huge for
    // another.
    if let Some(tags) = spec.exclude_tags.as_ref().filter(|t| !t.is_empty()) {
        let names: Vec<String> = tags.iter().map(|t| crate::db::tags::normalize(t)).collect();
        let placeholders = vec!["?"; names.len()].join(", ");
        where_parts.push(format!(
            "NOT EXISTS (SELECT 1 FROM task_tags cxx_tt JOIN tags cxx_g ON cxx_g.id = cxx_tt.tag_id
                          WHERE cxx_tt.task_id = tasks.id
                            AND cxx_g.name COLLATE NOCASE IN ({placeholders}))"
        ));
        for n in names {
            binds.push(Box::new(n));
        }
    }
    if let Some(people) = spec.exclude_people.as_ref().filter(|p| !p.is_empty()) {
        if people.iter().any(|p| p.trim() == "*") {
            where_parts.push(
                "NOT EXISTS (SELECT 1 FROM task_tags cxp_tt JOIN tags cxp_g ON cxp_g.id = cxp_tt.tag_id
                              WHERE cxp_tt.task_id = tasks.id AND cxp_g.name LIKE '@_%')"
                    .into(),
            );
        } else {
            let names: Vec<String> = people
                .iter()
                .map(|p| crate::db::tags::person_tag(p))
                .collect();
            let placeholders = vec!["?"; names.len()].join(", ");
            where_parts.push(format!(
                "NOT EXISTS (SELECT 1 FROM task_tags cxp_tt JOIN tags cxp_g ON cxp_g.id = cxp_tt.tag_id
                              WHERE cxp_tt.task_id = tasks.id
                                AND cxp_g.name COLLATE NOCASE IN ({placeholders}))"
            ));
            for n in names {
                binds.push(Box::new(n));
            }
        }
    }

    // Subtasks render nested in the detail pane rather than as top-level rows
    // — a VIEW rule. The view-less search scope has always returned them, and
    // in Trash/Archive `batch_roots` does this job instead: a subtask trashed
    // on its own is its own restorable entry.
    if let Some(view) = spec.view {
        if !spec.include_subtasks.unwrap_or(false) && !view.is_hidden_view() {
            if view == View::Today {
                // A decision filed as a SUBTASK — how older agent
                // follow-ups were filed — is still a question for Chris, and nesting
                // it under its parent is exactly how three P1 decisions sat
                // unseen for weeks. Today alone lets them through.
                where_parts.push(format!(
                    "(parent_id IS NULL OR {})",
                    crate::db::tasks::is_decision_sql()
                ));
            } else {
                where_parts.push("parent_id IS NULL".into());
            }
        }
    }

    if spec.hide_blocked.unwrap_or(false) {
        let blocked = crate::db::deps::blocked_task_ids(conn)?;
        if !blocked.is_empty() {
            let placeholders = vec!["?"; blocked.len()].join(", ");
            where_parts.push(format!("id NOT IN ({placeholders})"));
            for id in blocked {
                binds.push(Box::new(id));
            }
        }
    }

    // Date bounds compare LOCAL days. `due_at` is UTC ISO and `created_at` is
    // SQLite's UTC bookkeeping format; both operands of every comparison are
    // wrapped in `datetime()` per invariant 1, and the column side additionally
    // shifts to 'localtime' so "before Aug 3" means the user's Aug 3, not
    // UTC's. The bound side is a bare `YYYY-MM-DD` — midnight at that day's
    // start — so `before` is `< day` and `after` is `>= day+1`.
    if let Some(d) = spec.due_before.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        validate_date(conn, "due-before", d)?;
        where_parts.push("(due_at IS NOT NULL AND datetime(due_at,'localtime') < datetime(?))".into());
        binds.push(Box::new(d.to_string()));
    }
    if let Some(d) = spec.due_after.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        validate_date(conn, "due-after", d)?;
        where_parts.push(
            "(due_at IS NOT NULL AND datetime(due_at,'localtime') >= datetime(?, '+1 day'))".into(),
        );
        binds.push(Box::new(d.to_string()));
    }
    if let Some(d) = spec.created_before.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        validate_date(conn, "created-before", d)?;
        where_parts.push("datetime(created_at,'localtime') < datetime(?)".into());
        binds.push(Box::new(d.to_string()));
    }
    if let Some(d) = spec.created_after.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        validate_date(conn, "created-after", d)?;
        where_parts.push("datetime(created_at,'localtime') >= datetime(?, '+1 day')".into());
        binds.push(Box::new(d.to_string()));
    }

    // Text folds in as an id set, exactly the shape the tags facet takes. The
    // ranked order is discarded on purpose: with chips active this is a
    // list-maker, not a relevance search, and the deterministic board order
    // below keeps the same task in the same place as chips are added.
    if !query.is_empty() {
        let ids = text_match_ids(conn, query, limit)?;
        if ids.is_empty() {
            return Ok(None);
        }
        let placeholders = vec!["?"; ids.len()].join(", ");
        where_parts.push(format!("id IN ({placeholders})"));
        for id in ids {
            binds.push(Box::new(id));
        }
    }

    Ok(Some((where_parts, binds)))
}

/// Clear and re-populate the FTS index from `tasks`. The **only** sanctioned
/// direct writer of `tasks_fts`.
pub fn rebuild(conn: &Connection) -> Result<(), AppError> {
    conn.execute("DELETE FROM tasks_fts", [])?;
    conn.execute(
        "INSERT INTO tasks_fts(rowid, title, body, project)
         SELECT rowid, title, COALESCE(body,''), COALESCE(project,'') FROM tasks",
        [],
    )?;
    Ok(())
}

/// Startup integrity net: repair drift in either direction. Never panics.
pub fn verify_or_rebuild(conn: &Connection) {
    if !has_fts(conn) {
        return;
    }
    let tasks: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap_or(0);
    let indexed: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks_fts", [], |r| r.get(0))
        .unwrap_or(-1);
    if tasks != indexed {
        log::warn!("tasks_fts drift: {tasks} tasks vs {indexed} indexed — rebuilding");
        if let Err(e) = rebuild(conn) {
            log::warn!("tasks_fts rebuild failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{create, NewTask};

    fn seed(conn: &Connection, title: &str, body: Option<&str>, project: Option<&str>) {
        create(
            conn,
            &NewTask {
                title: title.into(),
                body: body.map(str::to_string),
                project: project.map(str::to_string),
                ..Default::default()
            },
        )
        .unwrap();
    }

    /// The asymmetry is deliberate and easy to get backwards: archiving is
    /// "keep this, off my board", so search is the main way it comes back —
    /// an unsearchable archive is a black hole. Trash is the opposite: a
    /// deleted task turning up in results reads as "the delete didn't work".
    #[test]
    fn search_finds_archived_tasks_but_never_trashed_ones() {
        let conn = test_conn();
        seed(&conn, "webhook retry backoff", None, None);
        let ids: Vec<String> = search(&conn, "webhook", 10)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids.len(), 1);
        let id = ids[0].clone();

        crate::db::tasks::archive(&conn, &id).unwrap();
        assert_eq!(
            search(&conn, "webhook", 10).unwrap().len(),
            1,
            "archived work must stay findable by keyword"
        );

        crate::db::tasks::trash(&conn, &id).unwrap();
        assert!(
            search(&conn, "webhook", 10).unwrap().is_empty(),
            "trashed work must not surface in search"
        );

        crate::db::tasks::restore(&conn, &id).unwrap();
        assert_eq!(
            search(&conn, "webhook", 10).unwrap().len(),
            1,
            "restore must not require an FTS rebuild"
        );
    }

    /// Trashed rows stay in the index on purpose (that's what makes restore
    /// free), so the startup drift check must not read that as corruption and
    /// rebuild on every launch.
    #[test]
    fn trashed_tasks_stay_indexed_so_the_drift_check_stays_quiet() {
        let conn = test_conn();
        seed(&conn, "indexed", None, None);
        let id = search(&conn, "indexed", 10).unwrap()[0].id.clone();
        crate::db::tasks::trash(&conn, &id).unwrap();

        let tasks: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap();
        let indexed: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tasks, indexed, "trashing must not desync the FTS index");
    }

    /// `short_id` is a number and the FTS index holds prose, so a T-number
    /// typed into the search box has to take its own lane — otherwise `T11`
    /// finds only the tasks whose notes mention T117 and never T11 itself.
    #[test]
    fn search_finds_a_task_by_its_t_number() {
        let conn = test_conn();
        // Numbers are claimed in creation order, so this pins the counter.
        crate::db::meta::set(&conn, crate::db::meta::KEY_NEXT_SHORT_ID, "11").unwrap();
        seed(&conn, "the eleventh", None, None); // T11
        seed(&conn, "the twelfth", None, None); // T12
        seed(&conn, "mentions T117 in its notes", Some("remainder of T117"), None); // T13

        let titles = |q: &str| -> Vec<String> {
            search(&conn, q, 10).unwrap().into_iter().map(|t| t.title).collect()
        };
        assert_eq!(
            titles("T11"),
            vec!["the eleventh", "mentions T117 in its notes"],
            "exact T-number leads; the text hit on 'T117' follows"
        );
        assert_eq!(titles("t12"), vec!["the twelfth"], "case-insensitive");
        // `T1` is a prefix of every number here: exact match would be T1 (none),
        // then the band ascends — search-as-you-type on the way to a number.
        assert_eq!(titles("T1"), vec!["the eleventh", "the twelfth", "mentions T117 in its notes"]);
        assert!(titles("T99").is_empty());
        // A T-number followed by words is a text search, not a reference.
        assert_eq!(titles("T11 twelfth"), Vec::<String>::new());

        // The filter bar's path folds the same id set.
        let spec = FilterSpec { query: Some("T12".into()), ..Default::default() };
        let hits = filtered(&conn, &spec).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "the twelfth");
    }

    #[test]
    fn match_expr_quotes_every_token() {
        assert_eq!(build_match_expr("webhook retry").unwrap(), "\"webhook\" AND \"retry\"*");
        assert_eq!(build_match_expr("   ").is_none(), true);
    }

    #[test]
    fn match_expr_neutralizes_fts_operators() {
        // `AND` is quoted into literal text; the bare `(` is dropped as
        // unindexable rather than quoted into a zero-match term.
        let expr = build_match_expr("budget ( AND \"x\"").unwrap();
        assert!(expr.contains("\"budget\""), "got {expr}");
        assert!(expr.contains("\"AND\""), "got {expr}");
        assert!(!expr.contains("\"(\""), "bare paren should be dropped: {expr}");
        assert!(expr.contains("\"\"\"x\"\"\""), "got {expr}");
    }

    #[test]
    fn match_expr_is_none_for_punctuation_only_queries() {
        assert!(build_match_expr("!!! ??").is_none());
    }

    #[test]
    fn search_finds_by_title_body_and_project() {
        let conn = test_conn();
        seed(&conn, "fix the webhook retry", Some("backoff on 429"), Some("cxventures"));
        seed(&conn, "master the bridge", None, Some("chrisx"));

        assert_eq!(search(&conn, "webhook", 10).unwrap().len(), 1);
        assert_eq!(search(&conn, "backoff", 10).unwrap().len(), 1);
        assert_eq!(search(&conn, "chrisx", 10).unwrap().len(), 1);
        assert_eq!(search(&conn, "nothingmatches", 10).unwrap().len(), 0);
    }

    #[test]
    fn search_survives_operator_soup_instead_of_erroring() {
        let conn = test_conn();
        seed(&conn, "budget review", None, None);
        // Would be an FTS syntax error if interpolated raw.
        let hits = search(&conn, "budget (", 10).unwrap();
        assert_eq!(hits.len(), 1, "unbalanced paren must not lose the search");
        assert!(search(&conn, "AND OR NOT", 10).is_ok());
    }

    #[test]
    fn empty_query_returns_nothing_not_everything() {
        let conn = test_conn();
        seed(&conn, "a", None, None);
        seed(&conn, "b", None, None);
        assert!(search(&conn, "   ", 10).unwrap().is_empty());
    }

    #[test]
    fn verify_or_rebuild_repairs_drift() {
        let conn = test_conn();
        seed(&conn, "orphaned index row", None, None);
        // Simulate drift by clearing the index behind the triggers' back.
        conn.execute("DELETE FROM tasks_fts", []).unwrap();
        assert!(search(&conn, "orphaned", 10).unwrap().is_empty());

        verify_or_rebuild(&conn);
        assert_eq!(search(&conn, "orphaned", 10).unwrap().len(), 1);
    }

    // ---- the filter bar ----

    fn seed_full(
        conn: &Connection,
        title: &str,
        project: Option<&str>,
        status: Option<&str>,
        priority: Option<i64>,
        due_at: Option<&str>,
    ) -> String {
        let t = create(
            conn,
            &NewTask {
                title: title.into(),
                project: project.map(str::to_string),
                status: status.map(str::to_string),
                priority,
                due_at: due_at.map(str::to_string),
                ..Default::default()
            },
        )
        .unwrap();
        t.id
    }

    // ── the one-builder fold (invariant 21) ────────────────────────────────

    /// **The parity test for the fold.** `db::tasks::list` is now an adapter
    /// over `filtered`, so for every view the two must return the same rows in
    /// the same order. Without this, the adapter can quietly drop a view's
    /// predicate and every other test still passes — the failure would be a
    /// view showing work it used to hide.
    #[test]
    fn list_is_the_filtered_adapter_for_every_view() {
        use crate::db::tasks::{list, ListFilters, View};
        let conn = test_conn();
        for t in ["alpha", "beta", "gamma", "delta"] {
            seed(&conn, t, None, Some("proj"));
        }
        let rows = list(&conn, &ListFilters { view: Some(View::All), ..Default::default() }).unwrap();
        crate::db::tasks::update(
            &conn,
            &rows[0].id,
            &crate::db::tasks::TaskPatch { status: Some("done".into()), ..Default::default() },
        )
        .unwrap();
        conn.execute(
            "UPDATE tasks SET defer_until = '2099-01-01T00:00:00.000Z' WHERE id = ?1",
            [&rows[1].id],
        )
        .unwrap();
        conn.execute(
            "UPDATE tasks SET tripwire = 'script:x', tripwire_state = 'armed' WHERE id = ?1",
            [&rows[2].id],
        )
        .unwrap();
        crate::db::tasks::trash(&conn, &rows[3].id).unwrap();

        for view in [
            View::Inbox, View::Today, View::Waiting, View::Open,
            View::All, View::InReview, View::Done, View::Trash, View::Archive,
        ] {
            let via_list = list(&conn, &ListFilters { view: Some(view), ..Default::default() }).unwrap();
            let spec = FilterSpec {
                view: Some(view),
                exclude_statuses: (!crate::db::tasks::view_owns_status(view))
                    .then(|| vec!["done".to_string()]),
                exclude_closed: Some(false),
                ..Default::default()
            };
            let via_filtered = filtered(&conn, &spec).unwrap();
            assert_eq!(
                via_list.iter().map(|t| &t.id).collect::<Vec<_>>(),
                via_filtered.iter().map(|t| &t.id).collect::<Vec<_>>(),
                "{view:?} disagrees between the adapter and the builder"
            );
        }
    }

    /// A view is a query on its own — the empty-spec guard is for the
    /// view-LESS search scope, where an empty spec really is a caller bug.
    #[test]
    fn a_view_alone_is_a_query_but_an_empty_spec_is_still_nothing() {
        use crate::db::tasks::View;
        let conn = test_conn();
        seed(&conn, "something", None, None);
        assert_eq!(
            filtered(&conn, &FilterSpec { view: Some(View::All), ..Default::default() })
                .unwrap()
                .len(),
            1
        );
        assert!(filtered(&conn, &FilterSpec::default()).unwrap().is_empty());
    }

    /// Closure is the CALLER's to state once a view is set, or the "not Done"
    /// chip could never be taken off: the search-side default would silently
    /// re-apply what the user just removed.
    #[test]
    fn a_view_query_owns_its_own_closure_rule() {
        use crate::db::tasks::View;
        let conn = test_conn();
        seed(&conn, "open one", None, None);
        seed(&conn, "closed one", None, None);
        let all = filtered(&conn, &FilterSpec { view: Some(View::All), ..Default::default() }).unwrap();
        let done = all.iter().find(|t| t.title == "closed one").unwrap();
        crate::db::tasks::update(
            &conn,
            &done.id,
            &crate::db::tasks::TaskPatch { status: Some("done".into()), ..Default::default() },
        )
        .unwrap();

        // No exclusion asked for, view set → finished work is present.
        let bare = FilterSpec { view: Some(View::All), exclude_closed: Some(false), ..Default::default() };
        assert_eq!(titles(&filtered(&conn, &bare).unwrap()).len(), 2);
        // The chip the app sends by default.
        let chipped = FilterSpec {
            exclude_statuses: Some(vec!["done".into()]),
            ..bare.clone()
        };
        assert_eq!(titles(&filtered(&conn, &chipped).unwrap()), vec!["open one"]);
        // A VIEW-LESS caller (MCP search_tasks) keeps the old default-true.
        let mcp = FilterSpec { query: Some("one".into()), ..Default::default() };
        assert_eq!(titles(&filtered(&conn, &mcp).unwrap()), vec!["open one"]);
    }

    /// **A node's badge counts the list clicking it opens.** The real app had
    /// pocketbuddy reading "8" in the sidebar over a list that said "Nothing
    /// here": the badge was `projects::list`'s "open and top-level", while the
    /// list applied the area's saved chips (`Status is not Done`, `Waiting is
    /// not Tripwire, Deferred, Blocked`). `count` takes the SAME spec, so the
    /// two numbers are one query.
    #[test]
    fn count_is_the_length_of_the_filtered_list_under_the_nodes_own_chips() {
        use crate::db::tasks::View;
        let conn = test_conn();
        crate::db::projects::create(&conn, "pocketbuddy", crate::db::projects::ProjectKind::Area, None)
            .unwrap();
        seed(&conn, "ready", None, Some("pocketbuddy"));
        seed(&conn, "deferred", None, Some("pocketbuddy"));
        seed(&conn, "armed", None, Some("pocketbuddy"));
        seed(&conn, "finished", None, Some("pocketbuddy"));
        seed(&conn, "elsewhere", None, Some("cxmail"));
        let all = filtered(
            &conn,
            &FilterSpec { view: Some(View::All), exclude_closed: Some(false), ..Default::default() },
        )
        .unwrap();
        let id = |title: &str| all.iter().find(|t| t.title == title).unwrap().id.clone();
        conn.execute(
            "UPDATE tasks SET defer_until = '2099-01-01T00:00:00.000Z' WHERE id = ?1",
            [id("deferred")],
        )
        .unwrap();
        conn.execute(
            "UPDATE tasks SET tripwire = 'script:x', tripwire_state = 'armed' WHERE id = ?1",
            [id("armed")],
        )
        .unwrap();
        crate::db::tasks::complete(&conn, &id("finished")).unwrap();
        // A subtask of the ready one: nested in the detail pane, never a row.
        crate::db::tasks::create(
            &conn,
            &crate::db::tasks::NewTask {
                title: "a step".into(),
                project: Some("pocketbuddy".into()),
                parent_id: Some(id("ready")),
                ..Default::default()
            },
        )
        .unwrap();

        // What the sidebar sends for the node: its view, the node, its chips.
        let spec = FilterSpec {
            view: Some(View::All),
            project: Some("pocketbuddy".into()),
            exclude_closed: Some(false),
            exclude_statuses: Some(vec!["done".into()]),
            exclude_waiting: Some(vec!["tripwire".into(), "deferred".into(), "blocked".into()]),
            ..Default::default()
        };
        let rows = filtered(&conn, &spec).unwrap();
        assert_eq!(titles(&rows), vec!["ready"]);
        assert_eq!(count(&conn, &spec).unwrap(), rows.len() as i64);
        // The old badge would have said 3 (open, top-level). It must not.
        assert_eq!(count(&conn, &spec).unwrap(), 1);

        // Without the waiting chip the held rows come back, and so does the count.
        let baseline = FilterSpec { exclude_waiting: None, ..spec.clone() };
        assert_eq!(count(&conn, &baseline).unwrap(), filtered(&conn, &baseline).unwrap().len() as i64);
        assert_eq!(count(&conn, &baseline).unwrap(), 3);

        // Several nodes in one call, in order; a node matching nothing is 0.
        let none = FilterSpec { project: Some("nowhere".into()), ..spec.clone() };
        assert_eq!(count_many(&conn, &[spec.clone(), baseline, none]).unwrap(), vec![1, 3, 0]);

        // An empty spec matches nothing rather than everything, like `filtered`.
        assert_eq!(count(&conn, &FilterSpec::default()).unwrap(), 0);
    }

    /// **Three-valued logic is the trap in every negative facet.** `NOT IN`
    /// over a nullable column is NULL for a NULL row, and NULL matches
    /// nothing — so a naive negation drops exactly the rows that have nothing
    /// to exclude. Each assertion here fails against the obvious wrong
    /// implementation and passes against the COALESCE/NOT EXISTS one.
    #[test]
    fn every_negative_facet_is_null_safe() {
        use crate::db::tasks::View;
        let conn = test_conn();
        // No project, no tags, no tripwire, no person: the row a naive
        // negation loses.
        seed(&conn, "bare", None, None);
        seed(&conn, "filed", None, Some("acme"));
        let all = filtered(
            &conn,
            &FilterSpec { view: Some(View::All), exclude_closed: Some(false), ..Default::default() },
        )
        .unwrap();
        let filed = all.iter().find(|t| t.title == "filed").unwrap();
        crate::db::tags::attach(&conn, &filed.id, "billable").unwrap();
        crate::db::tags::attach(&conn, &filed.id, "@dana").unwrap();
        conn.execute(
            "UPDATE tasks SET tripwire = 'script:x', tripwire_state = 'armed' WHERE id = ?1",
            [&filed.id],
        )
        .unwrap();

        let base = FilterSpec { view: Some(View::All), exclude_closed: Some(false), ..Default::default() };
        let cases: Vec<(&str, FilterSpec)> = vec![
            ("project", FilterSpec { exclude_projects: Some(vec!["acme".into()]), ..base.clone() }),
            ("tag", FilterSpec { exclude_tags: Some(vec!["billable".into()]), ..base.clone() }),
            ("person", FilterSpec { exclude_people: Some(vec!["@dana".into()]), ..base.clone() }),
            ("person *", FilterSpec { exclude_people: Some(vec!["*".into()]), ..base.clone() }),
            ("tripwire", FilterSpec { exclude_waiting: Some(vec!["tripwire".into()]), ..base.clone() }),
            ("waiting *", FilterSpec { exclude_waiting: Some(vec!["*".into()]), ..base.clone() }),
        ];
        for (label, spec) in cases {
            assert_eq!(
                titles(&filtered(&conn, &spec).unwrap()),
                vec!["bare"],
                "exclude_{label} lost the row that has no {label} at all"
            );
        }
    }

    /// "Not tagged A, B" means neither, so exclusion is ANY — ALL-negation
    /// would return a task tagged only A, which nobody reads that phrase as.
    #[test]
    fn excluding_tags_means_none_of_them_not_all_of_them() {
        use crate::db::tasks::View;
        let conn = test_conn();
        seed(&conn, "just a", None, None);
        seed(&conn, "neither", None, None);
        let base = FilterSpec { view: Some(View::All), exclude_closed: Some(false), ..Default::default() };
        let rows = filtered(&conn, &base).unwrap();
        let a = rows.iter().find(|t| t.title == "just a").unwrap();
        crate::db::tags::attach(&conn, &a.id, "a").unwrap();

        let spec = FilterSpec {
            exclude_tags: Some(vec!["a".into(), "b".into()]),
            ..base.clone()
        };
        assert_eq!(titles(&filtered(&conn, &spec).unwrap()), vec!["neither"]);
    }

    /// An undated task never leads a list ordered by date — in EITHER
    /// direction. Flipping the direction naively would put every undated row
    /// at the head of "due, descending".
    #[test]
    fn sorting_by_due_keeps_undated_rows_last_both_ways() {
        use crate::db::tasks::View;
        let conn = test_conn();
        seed(&conn, "undated", None, None);
        seed(&conn, "early", None, None);
        seed(&conn, "late", None, None);
        let base = FilterSpec { view: Some(View::All), exclude_closed: Some(false), ..Default::default() };
        let rows = filtered(&conn, &base).unwrap();
        for (title, due) in [("early", "2030-01-01T00:00:00.000Z"), ("late", "2031-01-01T00:00:00.000Z")] {
            let t = rows.iter().find(|r| r.title == title).unwrap();
            conn.execute("UPDATE tasks SET due_at = ?2 WHERE id = ?1", rusqlite::params![t.id, due])
                .unwrap();
        }
        let by_due = |dir| FilterSpec {
            sort: Some(Sort { key: SortKey::Due, dir }),
            ..base.clone()
        };
        assert_eq!(
            titles(&filtered(&conn, &by_due(SortDir::Asc)).unwrap()),
            vec!["early", "late", "undated"]
        );
        assert_eq!(
            titles(&filtered(&conn, &by_due(SortDir::Desc)).unwrap()),
            vec!["late", "early", "undated"],
            "reversing the direction must not float undated rows to the top"
        );
    }

    /// The sink sits UNDER the section keys and OVER the sort keys: a held
    /// row falls to the bottom of its own section, not out of it, and it stays
    /// down whichever column you sort by.
    #[test]
    fn the_sink_sits_under_the_sections_and_over_the_sort() {
        use crate::db::tasks::{View, PRIORITY_HIGHEST};
        let conn = test_conn();
        let section = crate::db::projects::create_section(&conn, Some("proj"), "phase 1").unwrap();
        for t in ["held", "ready"] {
            seed(&conn, t, None, Some("proj"));
        }
        seed(&conn, "loose", None, Some("proj"));
        let base = FilterSpec { view: Some(View::All), exclude_closed: Some(false), ..Default::default() };
        let rows = filtered(&conn, &base).unwrap();
        for title in ["held", "ready"] {
            let t = rows.iter().find(|r| r.title == title).unwrap();
            conn.execute(
                "UPDATE tasks SET section_id = ?2 WHERE id = ?1",
                rusqlite::params![t.id, section.id],
            )
            .unwrap();
        }
        let held = rows.iter().find(|r| r.title == "held").unwrap();
        // Deferred AND top priority — so priority alone would put it first.
        conn.execute(
            "UPDATE tasks SET defer_until = '2099-01-01T00:00:00.000Z', priority = ?2 WHERE id = ?1",
            rusqlite::params![held.id, PRIORITY_HIGHEST],
        )
        .unwrap();

        assert_eq!(
            titles(&filtered(&conn, &base).unwrap()),
            vec!["ready", "held", "loose"],
            "the held row sinks WITHIN its section, never out of it"
        );
        let by_priority = FilterSpec {
            sort: Some(Sort { key: SortKey::Priority, dir: SortDir::Asc }),
            ..base.clone()
        };
        assert_eq!(
            titles(&filtered(&conn, &by_priority).unwrap()).last(),
            Some(&"held"),
            "a P0 that is deferred still sorts below every open row"
        );
    }

    /// Trash and Archive route through the same builder and must keep their
    /// own scope, their own batch-root rule and their own newest-first order.
    #[test]
    fn hidden_views_keep_their_scope_and_order_through_the_builder() {
        use crate::db::tasks::{list, ListFilters, View};
        let conn = test_conn();
        seed(&conn, "first", None, None);
        seed(&conn, "second", None, None);
        let rows = list(&conn, &ListFilters { view: Some(View::All), ..Default::default() }).unwrap();
        for r in &rows {
            crate::db::tasks::trash(&conn, &r.id).unwrap();
        }
        let trashed = filtered(
            &conn,
            &FilterSpec { view: Some(View::Trash), exclude_closed: Some(false), ..Default::default() },
        )
        .unwrap();
        assert_eq!(trashed.len(), 2, "the Trash sees past LIVE");
        assert!(
            filtered(
                &conn,
                &FilterSpec { view: Some(View::All), exclude_closed: Some(false), ..Default::default() }
            )
            .unwrap()
            .is_empty(),
            "and every other view still does not"
        );
    }

    fn titles(tasks: &[Task]) -> Vec<&str> {
        tasks.iter().map(|t| t.title.as_str()).collect()
    }

    /// Finished work stays out unless it was asked for.
    ///
    /// Chips replace the view rather than narrowing it, so a filtered query has
    /// no view supplying the usual `status NOT IN ('done')` — which made the
    /// filter bar the one surface in either app that answered "show me
    /// CXVentures" with the year's completed tasks on top.
    #[test]
    fn a_filter_leaves_finished_work_out_by_default() {
        let conn = test_conn();
        seed(&conn, "open one", None, Some("Acme"));
        seed(&conn, "closed one", None, Some("Acme"));
        let all = filtered(
            &conn,
            &FilterSpec { projects: Some(vec!["Acme".into()]), ..Default::default() },
        )
        .unwrap();
        assert_eq!(all.len(), 2, "both are open to start with");

        let done = all.iter().find(|t| t.title == "closed one").unwrap().id.clone();
        crate::db::tasks::complete(&conn, &done).unwrap();

        let after = filtered(
            &conn,
            &FilterSpec { projects: Some(vec!["Acme".into()]), ..Default::default() },
        )
        .unwrap();
        assert_eq!(
            after.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(),
            vec!["open one"],
            "the completed task must not come back"
        );
    }

    /// The trap this rule has to avoid.
    ///
    /// `status is done` is a chip the UI offers. If the default exclusion also
    /// applied there the filter would answer its own question with silence, and
    /// the user would conclude the app had lost their finished work.
    #[test]
    fn naming_done_explicitly_beats_the_default_exclusion() {
        let conn = test_conn();
        seed(&conn, "closed one", None, Some("Acme"));
        let id = filtered(
            &conn,
            &FilterSpec { projects: Some(vec!["Acme".into()]), ..Default::default() },
        )
        .unwrap()[0]
            .id
            .clone();
        crate::db::tasks::complete(&conn, &id).unwrap();

        let found = filtered(
            &conn,
            &FilterSpec {
                statuses: Some(vec!["done".into()]),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(found.len(), 1, "asking for done must return done");
    }

    /// The flag is the other way back in, for a caller that wants closed work
    /// without narrowing to it.
    #[test]
    fn exclude_closed_false_brings_finished_work_back() {
        let conn = test_conn();
        seed(&conn, "closed one", None, Some("Acme"));
        let id = filtered(
            &conn,
            &FilterSpec { projects: Some(vec!["Acme".into()]), ..Default::default() },
        )
        .unwrap()[0]
            .id
            .clone();
        crate::db::tasks::complete(&conn, &id).unwrap();

        let spec = FilterSpec {
            projects: Some(vec!["Acme".into()]),
            exclude_closed: Some(false),
            ..Default::default()
        };
        assert_eq!(filtered(&conn, &spec).unwrap().len(), 1);
    }

    #[test]
    fn an_empty_spec_returns_nothing_not_everything() {
        let conn = test_conn();
        seed(&conn, "a", None, None);
        assert!(filtered(&conn, &FilterSpec::default()).unwrap().is_empty());
        // Whitespace text is still empty — same rule as `search`.
        let spec = FilterSpec { query: Some("   ".into()), ..Default::default() };
        assert!(filtered(&conn, &spec).unwrap().is_empty());
    }

    /// Values within a facet OR; separate facets AND. The Meta-tool convention
    /// the chips implement, pinned so neither half quietly becomes the other.
    #[test]
    fn values_within_a_facet_or_while_facets_and() {
        let conn = test_conn();
        seed_full(&conn, "acme deck", Some("acme"), Some("todo"), None, None);
        // `in_progress`, not `done`. The status here is incidental — this test
        // is about OR/AND — and a closed fixture now changes the row COUNT,
        // because a filter leaves finished work out by default. Keeping `done`
        // would make this test quietly also about that rule.
        seed_full(&conn, "acme deploy", Some("acme"), Some("in_progress"), None, None);
        seed_full(&conn, "initech crm", Some("initech"), Some("todo"), None, None);
        seed_full(&conn, "globex audit", Some("globex"), Some("todo"), None, None);

        // Two projects OR together.
        let spec = FilterSpec {
            projects: Some(vec!["acme".into(), "initech".into()]),
            ..Default::default()
        };
        assert_eq!(filtered(&conn, &spec).unwrap().len(), 3);

        // …and a status ANDs against them.
        let spec = FilterSpec {
            projects: Some(vec!["acme".into(), "initech".into()]),
            statuses: Some(vec!["todo".into()]),
            ..Default::default()
        };
        let hits = filtered(&conn, &spec).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(titles(&hits).contains(&"acme deck"));
        assert!(titles(&hits).contains(&"initech crm"));
    }

    /// `status is done` is the chip that must work from anywhere — the whole
    /// reason the filter scope ignores the sidebar view.
    #[test]
    fn a_done_chip_finds_closed_work() {
        let conn = test_conn();
        let id = seed_full(&conn, "shipped thing", None, Some("todo"), None, None);
        crate::recurrence::complete_with_recurrence(&conn, &id, None).unwrap();

        let spec = FilterSpec { statuses: Some(vec!["done".into()]), ..Default::default() };
        assert_eq!(titles(&filtered(&conn, &spec).unwrap()), vec!["shipped thing"]);
    }

    #[test]
    fn unknown_status_and_bad_priority_are_loud_errors() {
        let conn = test_conn();
        seed(&conn, "a", None, None);
        let spec = FilterSpec { statuses: Some(vec!["cancelled".into()]), ..Default::default() };
        assert!(filtered(&conn, &spec).is_err(), "v12 removed 'cancelled'; refuse it");
        let spec = FilterSpec { priorities: Some(vec![7]), ..Default::default() };
        assert!(filtered(&conn, &spec).is_err());
    }

    /// An unparseable date must be an error, not an empty result. `datetime()`
    /// of garbage is NULL, NULL comparisons match nothing, and "no tasks due
    /// before then" is exactly what a typo must not read as.
    #[test]
    fn an_unparseable_date_is_an_error_not_an_empty_result() {
        let conn = test_conn();
        seed(&conn, "a", None, None);
        let spec = FilterSpec { due_before: Some("next tuesday".into()), ..Default::default() };
        assert!(filtered(&conn, &spec).is_err());
    }

    /// Boundary semantics: `before d` and `after d` both EXCLUDE day `d`
    /// itself. Fixtures straddle today by whole days so the local/UTC gap
    /// (invariant 1's territory) can't fake a pass in either direction.
    #[test]
    fn due_bounds_exclude_the_named_day() {
        let conn = test_conn();
        let day = |offset: &str| -> String {
            conn.query_row(
                &format!("SELECT date('now','localtime','{offset}')"),
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        // Stored as UTC instants well inside the local day (noon local), so
        // the localtime round-trip in the query can't shift them across a
        // boundary.
        let noon_utc = |offset: &str| -> String {
            conn.query_row(
                &format!(
                    "SELECT strftime('%Y-%m-%dT%H:%M:%S', datetime(date('now','localtime','{offset}') || ' 12:00:00', 'utc')) || '.000Z'"
                ),
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        seed_full(&conn, "yesterday", None, None, None, Some(&noon_utc("-1 day")));
        seed_full(&conn, "today", None, None, None, Some(&noon_utc("+0 day")));
        seed_full(&conn, "tomorrow", None, None, None, Some(&noon_utc("+1 day")));
        seed_full(&conn, "undated", None, None, None, None);

        let today = day("+0 day");
        let before = FilterSpec { due_before: Some(today.clone()), ..Default::default() };
        assert_eq!(
            titles(&filtered(&conn, &before).unwrap()),
            vec!["yesterday"],
            "'before today' is strictly earlier days — today itself excluded, undated excluded"
        );
        let after = FilterSpec { due_after: Some(today), ..Default::default() };
        assert_eq!(
            titles(&filtered(&conn, &after).unwrap()),
            vec!["tomorrow"],
            "'after today' is strictly later days"
        );
    }

    /// `created after <today>` must not match a task created this instant —
    /// the bound excludes the named day, and every fixture here was created
    /// today. The inverse bound holds everything.
    #[test]
    fn created_bounds_use_the_same_day_semantics() {
        let conn = test_conn();
        seed(&conn, "made just now", None, None);
        let today: String = conn
            .query_row("SELECT date('now','localtime')", [], |r| r.get(0))
            .unwrap();
        let tomorrow: String = conn
            .query_row("SELECT date('now','localtime','+1 day')", [], |r| r.get(0))
            .unwrap();

        let after = FilterSpec { created_after: Some(today), ..Default::default() };
        assert!(filtered(&conn, &after).unwrap().is_empty());
        let before = FilterSpec { created_before: Some(tomorrow), ..Default::default() };
        assert_eq!(filtered(&conn, &before).unwrap().len(), 1);
    }

    /// Text + facet compose: the query narrows to FTS matches, the facet
    /// narrows those. And the scope stays search's, not list's: archived
    /// matches, trashed never.
    #[test]
    fn text_composes_with_facets_over_the_search_scope() {
        let conn = test_conn();
        seed_full(&conn, "webhook retry", Some("acme"), None, None, None);
        seed_full(&conn, "webhook docs", Some("initech"), None, None, None);
        let archived = seed_full(&conn, "webhook archive", Some("acme"), None, None, None);
        crate::db::tasks::archive(&conn, &archived).unwrap();
        let trashed = seed_full(&conn, "webhook trash", Some("acme"), None, None, None);
        crate::db::tasks::trash(&conn, &trashed).unwrap();

        let spec = FilterSpec {
            query: Some("webhook".into()),
            projects: Some(vec!["acme".into()]),
            ..Default::default()
        };
        let hits = filtered(&conn, &spec).unwrap();
        let got = titles(&hits);
        assert!(got.contains(&"webhook retry"));
        assert!(got.contains(&"webhook archive"), "archived tasks are in scope, like search");
        assert!(!got.contains(&"webhook docs"), "wrong project");
        assert!(!got.contains(&"webhook trash"), "trashed tasks never surface");
    }

    /// Tags stay ALL — the one facet that narrows rather than ORs, because
    /// that is what the sidebar filter and `list_tasks` already mean by a
    /// multi-tag filter. Changing it here would make one gesture mean two
    /// things in two places.
    #[test]
    fn tag_values_narrow_instead_of_or() {
        let conn = test_conn();
        let both = seed_full(&conn, "both", None, None, None, None);
        let one = seed_full(&conn, "one", None, None, None, None);
        crate::db::tags::attach(&conn, &both, "billable").unwrap();
        crate::db::tags::attach(&conn, &both, "quick-win").unwrap();
        crate::db::tags::attach(&conn, &one, "billable").unwrap();

        let spec = FilterSpec {
            tags: Some(vec!["billable".into(), "quick-win".into()]),
            ..Default::default()
        };
        assert_eq!(titles(&filtered(&conn, &spec).unwrap()), vec!["both"]);
    }

    #[test]
    fn priority_facet_uses_the_inverted_scale() {
        let conn = test_conn();
        seed_full(&conn, "urgent", None, None, Some(0), None);
        seed_full(&conn, "whenever", None, None, Some(3), None);

        // P0 means highest (invariant 9) — a filter for 0 must find "urgent".
        let spec = FilterSpec { priorities: Some(vec![0]), ..Default::default() };
        assert_eq!(titles(&filtered(&conn, &spec).unwrap()), vec!["urgent"]);
    }
}
