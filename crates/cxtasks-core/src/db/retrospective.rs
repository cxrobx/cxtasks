//! Read-only episodic recall over closed and superseded work.
//!
//! This module does not ask a model to invent a summary. It returns bounded,
//! cited evidence from run handoffs and comments, grouped into the three
//! questions a retrospective needs to answer. The calling agent may synthesize
//! prose, but every claim has a task and stored source behind it.

use crate::error::AppError;
use rusqlite::{params_from_iter, types::Value, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetrospectiveSpec {
    pub project: Option<String>,
    /// Inclusive UTC calendar date, YYYY-MM-DD.
    pub from: Option<String>,
    /// Inclusive UTC calendar date, YYYY-MM-DD.
    pub to: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrospectiveEvidence {
    pub task_id: String,
    pub task_ref: Option<String>,
    pub title: String,
    pub project: Option<String>,
    /// completed or superseded.
    pub closure: String,
    pub closed_at: String,
    pub source: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecurringTheme {
    pub term: String,
    pub task_count: usize,
    pub task_refs: Vec<String>,
}

/// Who a slice of closed work was with. Derived from person tags (`@name`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonCount {
    pub name: String,
    pub task_count: usize,
}

/// The October check as a count rather than a feeling: of the closed work in
/// scope, how much had a person on it (an introduction, a client, a
/// follow-up) and how much had nobody — which, in a portfolio where nearly
/// every task carries a repo, means code. The Focus Map's builder-reflex tell
/// is "is the quarter's output code, or introductions, cases and channels?";
/// this is the number that answers it, and `people` is the who.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputMix {
    pub with_people: usize,
    pub without_people: usize,
    pub people: Vec<PersonCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Retrospective {
    pub tasks_scanned: usize,
    pub source_items_scanned: usize,
    pub learned: Vec<RetrospectiveEvidence>,
    pub recurring: Vec<RecurringTheme>,
    pub ruled_out: Vec<RetrospectiveEvidence>,
    pub mix: OutputMix,
}

#[derive(Debug)]
struct ClosedTask {
    id: String,
    short_id: Option<i64>,
    title: String,
    project: Option<String>,
    closure: String,
    closed_at: String,
}

#[derive(Debug)]
struct SourceItem {
    source: String,
    text: String,
}

fn validate_date(conn: &Connection, label: &str, raw: &str) -> Result<(), AppError> {
    let valid: bool = conn.query_row(
        "SELECT date(?1) IS NOT NULL AND length(trim(?1)) = 10",
        [raw],
        |r| r.get(0),
    )?;
    if valid {
        Ok(())
    } else {
        Err(AppError::Validation(format!(
            "unparseable retrospective {label} date '{raw}' — expected YYYY-MM-DD"
        )))
    }
}

fn closed_tasks(conn: &Connection, spec: &RetrospectiveSpec) -> Result<Vec<ClosedTask>, AppError> {
    if let Some(raw) = spec.from.as_deref() {
        validate_date(conn, "from", raw)?;
    }
    if let Some(raw) = spec.to.as_deref() {
        validate_date(conn, "to", raw)?;
    }
    if let (Some(from), Some(to)) = (spec.from.as_deref(), spec.to.as_deref()) {
        let reversed: bool =
            conn.query_row("SELECT date(?1) > date(?2)", [from, to], |r| r.get(0))?;
        if reversed {
            return Err(AppError::Validation(
                "retrospective 'from' must not be after 'to'".into(),
            ));
        }
    }

    let closure_expr = "COALESCE(
        (SELECT MIN(r.created_at) FROM task_relations r
          WHERE r.to_task_id = t.id AND r.kind = 'supersedes'),
        t.completed_at
    )";
    let mut where_parts = vec![
        "t.deleted_at IS NULL".to_string(),
        "(t.status = 'done' OR EXISTS(
            SELECT 1 FROM task_relations r
             WHERE r.to_task_id = t.id AND r.kind = 'supersedes'
        ))"
        .to_string(),
    ];
    let mut binds: Vec<Value> = Vec::new();
    if let Some(project) = spec.project.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let names = crate::db::projects::expand_scope(conn, project)?;
        let marks = vec!["?"; names.len()].join(",");
        where_parts.push(format!("t.project COLLATE NOCASE IN ({marks})"));
        binds.extend(names.into_iter().map(Value::Text));
    }
    if let Some(from) = spec.from.as_deref() {
        where_parts.push(format!("date({closure_expr}) >= date(?)"));
        binds.push(Value::Text(from.to_string()));
    }
    if let Some(to) = spec.to.as_deref() {
        where_parts.push(format!("date({closure_expr}) <= date(?)"));
        binds.push(Value::Text(to.to_string()));
    }
    let limit = spec.limit.unwrap_or(30).clamp(1, 100);
    let sql = format!(
        "SELECT t.id, t.short_id, t.title, t.project,
                CASE WHEN EXISTS(
                    SELECT 1 FROM task_relations r
                     WHERE r.to_task_id = t.id AND r.kind = 'supersedes'
                ) THEN 'superseded' ELSE 'completed' END,
                {closure_expr}
           FROM tasks t
          WHERE {}
          ORDER BY datetime({closure_expr}) DESC, t.rowid DESC
          LIMIT {limit}",
        where_parts.join(" AND ")
    );
    Ok(conn
        .prepare(&sql)?
        .query_map(params_from_iter(binds), |r| {
            Ok(ClosedTask {
                id: r.get(0)?,
                short_id: r.get(1)?,
                title: r.get(2)?,
                project: r.get(3)?,
                closure: r.get(4)?,
                closed_at: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn sources(conn: &Connection, task_id: &str) -> Result<Vec<SourceItem>, AppError> {
    let mut out = Vec::new();
    let mut runs = conn.prepare(
        "SELECT id, result_text FROM task_runs
          WHERE task_id = ?1 AND result_text IS NOT NULL AND trim(result_text) <> ''
          ORDER BY datetime(started_at) DESC, rowid DESC",
    )?;
    for row in runs.query_map([task_id], |r| {
        Ok(SourceItem {
            source: format!("run:{}", r.get::<_, String>(0)?),
            text: r.get(1)?,
        })
    })? {
        out.push(row?);
    }
    let mut comments = conn.prepare(
        "SELECT id, body FROM task_comments
          WHERE task_id = ?1 AND trim(body) <> ''
            AND instr(body, '_Full handoff on the run._') = 0
          ORDER BY datetime(created_at) DESC, rowid DESC",
    )?;
    for row in comments.query_map([task_id], |r| {
        Ok(SourceItem {
            source: format!("comment:{}", r.get::<_, String>(0)?),
            text: r.get(1)?,
        })
    })? {
        out.push(row?);
    }
    Ok(out)
}

fn clean_line(raw: &str) -> Option<String> {
    let line = raw
        .trim()
        .trim_start_matches(|c: char| matches!(c, '#' | '-' | '*' | '>' | ' '));
    if line.len() < 12 {
        return None;
    }
    let mut text: String = line.chars().take(600).collect();
    if line.chars().count() > 600 {
        text.push('…');
    }
    Some(text)
}

fn lines(raw: &str) -> Vec<String> {
    raw.lines().filter_map(clean_line).collect()
}

fn contains_signal(text: &str, signals: &[&str]) -> bool {
    let lower = text.to_ascii_lowercase();
    signals.iter().any(|s| lower.contains(s))
}

fn evidence(task: &ClosedTask, source: &SourceItem, text: String) -> RetrospectiveEvidence {
    RetrospectiveEvidence {
        task_id: task.id.clone(),
        task_ref: task.short_id.map(|n| format!("T{n}")),
        title: task.title.clone(),
        project: task.project.clone(),
        closure: task.closure.clone(),
        closed_at: task.closed_at.clone(),
        source: source.source.clone(),
        text,
    }
}

const RULED_OUT: &[&str] = &[
    "ruled out",
    "didn't work",
    "did not work",
    "doesn't work",
    "does not work",
    "won't work",
    "cannot work",
    "not viable",
    "rejected",
    "dead end",
    "do not use",
    "don't use",
    "retired",
    "instead of",
];

const LEARNED: &[&str] = &[
    "learned",
    "root cause",
    "because",
    "confirmed",
    "measured",
    "found that",
    "the fix",
    "lesson",
    "decision",
    "invariant",
    "gotcha",
    "turned out",
];

const STOP: &[&str] = &[
    "about", "after", "again", "against", "also", "been", "before", "being", "between",
    "both", "could", "does", "done", "each", "from", "have", "into", "just", "more",
    "most", "only", "other", "result", "should", "task", "than", "that", "their", "them",
    "then", "there", "these", "they", "this", "through", "using", "very", "what", "when",
    "where", "which", "while", "with", "work", "would", "your",
];

fn terms(raw: &str) -> HashSet<String> {
    raw.split(|c: char| !c.is_alphanumeric() && c != '_')
        .map(str::to_ascii_lowercase)
        .filter(|w| w.len() >= 5 && !STOP.contains(&w.as_str()) && !w.chars().all(|c| c.is_numeric()))
        .collect()
}

/// Read a retrospective. This function performs no writes.
pub fn read(conn: &Connection, spec: &RetrospectiveSpec) -> Result<Retrospective, AppError> {
    let tasks = closed_tasks(conn, spec)?;
    let mut learned = Vec::new();
    let mut ruled_out = Vec::new();
    let mut term_tasks: HashMap<String, HashSet<String>> = HashMap::new();
    let mut refs: HashMap<String, String> = HashMap::new();
    let mut source_items_scanned = 0;
    let mut mix = OutputMix::default();
    let mut person_counts: HashMap<String, usize> = HashMap::new();

    for task in &tasks {
        let items = sources(conn, &task.id)?;
        source_items_scanned += items.len();
        let people: Vec<String> = crate::db::tags::for_task(conn, &task.id)?
            .into_iter()
            .map(|t| t.name)
            .filter(|n| crate::db::tags::is_person(n))
            .collect();
        if people.is_empty() {
            mix.without_people += 1;
        } else {
            mix.with_people += 1;
            for p in people {
                *person_counts.entry(p).or_insert(0) += 1;
            }
        }
        if let Some(short) = task.short_id {
            refs.insert(task.id.clone(), format!("T{short}"));
        }
        let mut learned_for_task = false;
        for item in &items {
            for line in lines(&item.text) {
                for term in terms(&line) {
                    term_tasks.entry(term).or_default().insert(task.id.clone());
                }
                if contains_signal(&line, RULED_OUT) && ruled_out.len() < 40 {
                    ruled_out.push(evidence(task, item, line.clone()));
                }
                if !learned_for_task && contains_signal(&line, LEARNED) && learned.len() < 40 {
                    learned.push(evidence(task, item, line));
                    learned_for_task = true;
                }
            }
        }
        // Every task with stored history contributes at least one grounded
        // learning, even when the author did not use one of the signal phrases.
        if !learned_for_task {
            if let Some(item) = items.first() {
                if let Some(line) = lines(&item.text).into_iter().next() {
                    learned.push(evidence(task, item, line));
                }
            }
        }
    }

    let mut recurring: Vec<RecurringTheme> = term_tasks
        .into_iter()
        .filter_map(|(term, ids)| {
            (ids.len() >= 2).then(|| {
                let mut task_refs: Vec<String> = ids
                    .iter()
                    .map(|id| refs.get(id).cloned().unwrap_or_else(|| id.clone()))
                    .collect();
                task_refs.sort();
                RecurringTheme {
                    term,
                    task_count: ids.len(),
                    task_refs,
                }
            })
        })
        .collect();
    recurring.sort_by(|a, b| {
        b.task_count
            .cmp(&a.task_count)
            .then_with(|| a.term.cmp(&b.term))
    });
    recurring.truncate(12);

    mix.people = person_counts
        .into_iter()
        .map(|(name, task_count)| PersonCount { name, task_count })
        .collect();
    mix.people.sort_by(|a, b| {
        b.task_count
            .cmp(&a.task_count)
            .then_with(|| a.name.cmp(&b.name))
    });
    mix.people.truncate(12);

    Ok(Retrospective {
        tasks_scanned: tasks.len(),
        source_items_scanned,
        learned,
        recurring,
        ruled_out,
        mix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{comments, relations, runs, schema::test_conn, tasks};
    use crate::executor::BackgroundExecutor;
    use crate::intent::BackgroundIntent;

    fn task(conn: &Connection, title: &str, project: &str) -> tasks::Task {
        tasks::create(
            conn,
            &tasks::NewTask {
                title: title.into(),
                project: Some(project.into()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn retrospective_cites_learning_and_ruled_out_evidence_without_writing() {
        let conn = test_conn();
        let a = task(&conn, "first sync", "CXTasks");
        let run = runs::start(
            &conn,
            &a.id,
            "foreground",
            None,
            BackgroundExecutor::Claude,
            BackgroundIntent::Work,
            None,
        )
        .unwrap();
        runs::finish(
            &conn,
            &run.id,
            &runs::RunOutcome {
                status: runs::FOREGROUND_STATUS.into(),
                result_text: Some(
                    "Root cause: byte-wise timestamp comparison shifted the day.\n\
                     We ruled out increasing the polling frequency; it cannot fix ordering."
                        .into(),
                ),
                ..Default::default()
            },
        )
        .unwrap();
        comments::add(
            &conn,
            &a.id,
            "claude",
            "Result · foreground session\n\nRoot cause: duplicate summary\n\n_Full handoff on the run._",
        )
        .unwrap();
        tasks::complete(&conn, &a.id).unwrap();

        let before = conn.total_changes();
        let got = read(
            &conn,
            &RetrospectiveSpec {
                project: Some("cxtasks".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(conn.total_changes(), before, "retrospective must be read-only");
        assert_eq!(got.tasks_scanned, 1);
        assert_eq!(
            got.source_items_scanned, 1,
            "a derived handoff comment must not duplicate its full run result"
        );
        assert!(got.learned.iter().any(|e| e.text.contains("Root cause")));
        assert!(got.ruled_out.iter().any(|e| e.text.contains("ruled out")));
        assert!(got.learned[0].task_ref.as_deref().unwrap().starts_with('T'));
    }

    #[test]
    fn a_superseded_open_task_is_distinct_from_a_completed_one() {
        let conn = test_conn();
        let old = task(&conn, "old plan", "P");
        let new = task(&conn, "new plan", "P");
        comments::add(&conn, &old.id, comments::AUTHOR_ME, "Ruled out: vendor API is absent")
            .unwrap();
        relations::add(
            &conn,
            &new.id,
            &old.id,
            relations::RelationKind::Supersedes,
        )
        .unwrap();
        let got = read(
            &conn,
            &RetrospectiveSpec {
                project: Some("P".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(got.tasks_scanned, 1);
        assert_eq!(got.ruled_out[0].closure, "superseded");
    }

    #[test]
    fn recurring_terms_require_more_than_one_task() {
        let conn = test_conn();
        for title in ["first webhook", "second webhook"] {
            let t = task(&conn, title, "P");
            comments::add(
                &conn,
                &t.id,
                comments::AUTHOR_ME,
                "Confirmed webhook retries need idempotency",
            )
            .unwrap();
            tasks::complete(&conn, &t.id).unwrap();
        }
        let got = read(
            &conn,
            &RetrospectiveSpec {
                project: Some("P".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(got.recurring.iter().any(|r| r.term == "webhook" && r.task_count == 2));
    }
}
