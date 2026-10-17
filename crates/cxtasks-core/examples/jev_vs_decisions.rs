//! Measure OpenAI's Decisions API against TypeSafe/Jev on capture routing.
//!
//! Measurement only: nothing here is called by the app, the runner or the MCP,
//! and the database it reads is a COPY. Point it at a `sqlite3 .backup` of the
//! live file, never the live file itself — each item is evaluated inside a
//! transaction that soft-deletes the task being judged and is then rolled back,
//! so the copy is written to (and left unchanged).
//!
//! # What is compared
//!
//! [`crate::intake::assess`]'s capture judgment, which is the one Jev answers on
//! every quick-add: project (choice over the same described destinations
//! `assess` builds), priority (choice), status (choice), needs_clarification
//! (noul) and one noul per tag. Both models get the SAME state (`{"task": text}`)
//! and the same wording; Jev's `choice`/`noul` map onto Decisions'
//! `choice`/`predicate` one for one, with the same keys as choice values.
//!
//! # Labels
//!
//! The task's settled `project` (or, with none, a bare repo it is filed against,
//! else "none") is the truth. Leave-one-out: the task under test is removed
//! before the destinations are described and before the deterministic route
//! runs, because `describe` lists recent titles and would otherwise hand the
//! model the very task it is judging.
//!
//! ```bash
//! sqlite3 "file:$HOME/Library/Application Support/com.cxtasks.app/cxtasks.db?mode=ro" ".backup /path/snap.db"
//! secret run -k TYPESAFE_API_KEY -k OPENAI_API_KEY -- \
//!   cargo run --release -p cxtasks-core --example jev_vs_decisions -- /path/snap.db /path/out [limit]
//! ```
//!
//! Writes `items.jsonl` (task text included — keep the out dir out of the repo)
//! and `summary.json`, and prints the summary.

use cxtasks_core::db;
use cxtasks_core::intake;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const JEV_ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const JEV_MODEL: &str = "jev-1.13.0";
const DECISIONS_ENDPOINT: &str = "https://api.openai.com/v1/decisions";
const DECISIONS_MODEL: &str = "gpt-6-luna";
/// $/M input tokens. Decisions has no output charge; Jev's output is noted, not priced.
const JEV_PRICE: f64 = 0.042;
const DECISIONS_PRICE: f64 = 0.10;
/// Production thresholds, copied from `jev.rs` / `intake.rs`.
const PROJECT_CONFIDENCE: f64 = 0.55;
const TAG_BAR: f64 = 0.72;
const CLARIFY_STATUS_BAR: f64 = 0.65;
const TOO_VAGUE: f64 = 0.90;
/// Same as `intake::CRITERION_TERMS`.
const CRITERION_TERMS: usize = 8;
/// The day Jev capture shipped (9a2c710). Labels set before it cannot have been
/// seeded by a Jev suggestion.
const JEV_SHIPPED: &str = "2026-09-19";
const WORKERS: usize = 4;

#[derive(Clone, Debug, PartialEq)]
enum Dest {
    Project(String),
    Repo(String),
    None,
}

impl Dest {
    fn same(&self, other: &Dest) -> bool {
        match (self, other) {
            (Dest::Project(a), Dest::Project(b)) | (Dest::Repo(a), Dest::Repo(b)) => {
                a.eq_ignore_ascii_case(b)
            }
            (Dest::None, Dest::None) => true,
            _ => false,
        }
    }
    fn label(&self) -> String {
        match self {
            Dest::Project(p) => format!("project:{p}"),
            Dest::Repo(r) => format!("repo:{r}"),
            Dest::None => "none".into(),
        }
    }
}

struct Prepared {
    id: String,
    created_at: String,
    text: String,
    truth: Dest,
    truth_offered: bool,
    truth_priority: i64,
    truth_tags: Vec<String>,
    dests: Vec<Dest>,
    tags: Vec<String>,
    route: Dest,
    jev_questions: Value,
    decision_questions: Value,
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// `intake::describe`, which is private; same SQL, same term rule.
fn describe(conn: &Connection, dest: &Dest) -> String {
    let (head, col, key) = match dest {
        Dest::Project(p) => (p.clone(), "project", p.clone()),
        Dest::Repo(path) => {
            let label = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path);
            (format!("{label} (a repo with no project)"), "repo_path", path.clone())
        }
        Dest::None => unreachable!(),
    };
    let sql = format!(
        "SELECT title FROM tasks WHERE {col} = ?1 COLLATE NOCASE AND deleted_at IS NULL
          ORDER BY created_at DESC LIMIT 12"
    );
    let titles: Vec<String> = conn
        .prepare(&sql)
        .and_then(|mut s| s.query_map([key], |r| r.get(0))?.collect())
        .unwrap_or_default();
    let mut terms: Vec<String> = Vec::new();
    for word in titles.iter().flat_map(|t| words(t)) {
        if word.chars().count() >= 4 && !terms.contains(&word) {
            terms.push(word);
        }
        if terms.len() >= CRITERION_TERMS {
            break;
        }
    }
    if terms.is_empty() {
        head
    } else {
        format!("{head} — recent work mentions: {}", terms.join(", "))
    }
}

/// `jev::assess_capture`'s questions, verbatim, plus their Decisions spelling.
fn questions(described: &[String], tags: &[String]) -> (Value, Value) {
    let mut project_criteria = Map::new();
    project_criteria.insert("none".into(), json!("No listed project clearly applies"));
    for (i, project) in described.iter().take(40).enumerate() {
        project_criteria.insert(format!("p{i}"), json!(project));
    }
    let project_instr = "Which existing project best fits this task? Choose none when the fit is unclear.";
    let priority_instr = "Choose the task priority. P0 is urgent and important; P3 is ordinary backlog work.";
    let priority_criteria = json!({"p0":"Immediate critical work","p1":"High priority","p2":"Normal planned work","p3":"Low priority or backlog"});
    let status_instr = "Is this only captured for later triage or is it already actionable?";
    let status_criteria = json!({"inbox":"Needs later triage","todo":"Actionable now"});
    let clarify_instr = "The task is too ambiguous to act on without asking a clarifying question.";

    let mut jev = json!({
        "project": {"type":"choice", "instructions":project_instr, "criteria":project_criteria},
        "priority": {"type":"choice", "instructions":priority_instr, "criteria":priority_criteria},
        "status": {"type":"choice", "instructions":status_instr, "criteria":status_criteria},
        "needs_clarification": {"type":"noul", "instructions":clarify_instr}
    });
    let tag_instr = |tag: &str| format!("The tag '{tag}' meaningfully applies to this task.");
    if let Some(qs) = jev.as_object_mut() {
        for (i, tag) in tags.iter().take(20).enumerate() {
            qs.insert(format!("tag_{i}"), json!({"type":"noul", "instructions":tag_instr(tag)}));
        }
    }

    let choices = |m: &Map<String, Value>| -> Value {
        Value::Array(
            m.iter()
                .map(|(k, v)| json!({"value": k, "description": v}))
                .collect(),
        )
    };
    let mut decisions = vec![
        json!({"type":"choice","name":"project","instructions":project_instr,"choices":choices(&project_criteria)}),
        json!({"type":"choice","name":"priority","instructions":priority_instr,"choices":choices(priority_criteria.as_object().unwrap())}),
        json!({"type":"choice","name":"status","instructions":status_instr,"choices":choices(status_criteria.as_object().unwrap())}),
        json!({"type":"predicate","name":"needs_clarification","instructions":clarify_instr}),
    ];
    for (i, tag) in tags.iter().take(20).enumerate() {
        decisions.push(json!({"type":"predicate","name":format!("tag_{i}"),"instructions":tag_instr(tag)}));
    }
    (jev, Value::Array(decisions))
}

fn prepare(conn: &mut Connection, limit: usize) -> anyhow::Result<Vec<Prepared>> {
    let items: Vec<(String, String, Option<String>, Option<String>, Option<String>, i64, String)> = conn
        .prepare(
            "SELECT id, title, body, project, repo_path, priority, created_at FROM tasks
              WHERE deleted_at IS NULL ORDER BY created_at",
        )?
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for (id, title, body, project, repo_path, priority, created_at) in items.into_iter().take(limit) {
        let text = match body.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
            Some(b) => format!("{title}\n{}", b.chars().take(1500).collect::<String>()),
            None => title.clone(),
        };
        let truth_tags: Vec<String> = conn
            .prepare("SELECT g.name FROM task_tags tt JOIN tags g ON g.id = tt.tag_id WHERE tt.task_id = ?1")?
            .query_map([&id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;

        let tx = conn.transaction()?;
        // Leave-one-out: the judged task must not describe its own destination.
        tx.execute("UPDATE tasks SET deleted_at = datetime('now') WHERE id = ?1", [&id])?;
        let projects = db::projects::list(&tx, false)?;
        let routing = intake::route(&tx, &text, &projects)?;
        let repos = intake::bare_repos(&tx)?;
        let dests: Vec<Dest> = projects
            .iter()
            .map(|p| Dest::Project(p.name.clone()))
            .chain(repos.iter().map(|r| Dest::Repo(r.path.clone())))
            .collect();
        let described: Vec<String> = dests.iter().map(|d| describe(&tx, d)).collect();
        let tags: Vec<String> = db::tags::list_with_counts(&tx)
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.name)
            .collect();
        tx.rollback()?;

        let truth = match (project.filter(|p| !p.trim().is_empty()), repo_path) {
            (Some(p), _) => Dest::Project(p),
            (None, Some(r)) => {
                let r = db::projects::normalize_repo_path(&r);
                if repos.iter().any(|b| b.path.eq_ignore_ascii_case(&r)) {
                    Dest::Repo(r)
                } else {
                    Dest::None
                }
            }
            (None, None) => Dest::None,
        };
        let offered = dests.iter().take(40).collect::<Vec<_>>();
        let truth_offered = truth == Dest::None || offered.iter().any(|d| d.same(&truth));
        let route = match (routing.pick, routing.repo) {
            (Some(p), _) => Dest::Project(p),
            (None, Some(r)) => Dest::Repo(r),
            _ => Dest::None,
        };
        let tags: Vec<String> = tags.into_iter().take(20).collect();
        let (jev_questions, decision_questions) = questions(&described, &tags);
        out.push(Prepared {
            id,
            created_at,
            text,
            truth,
            truth_offered,
            truth_priority: priority,
            truth_tags,
            dests,
            tags,
            route,
            jev_questions,
            decision_questions,
        });
    }
    Ok(out)
}

/// One normalised answer set, whichever API gave it.
#[derive(Default, Clone)]
struct Judged {
    ok: bool,
    error: Option<String>,
    ms: f64,
    input_tokens: f64,
    output_tokens: f64,
    project_key: String,
    project_conf: f64,
    priority: i64,
    status: String,
    clarify: f64,
    tags: Vec<f64>,
}

fn post(agent: &ureq::Agent, url: &str, key: &str, body: &Value) -> (Result<Value, String>, f64) {
    let mut last = String::new();
    for attempt in 0..3 {
        let t = Instant::now();
        let r = agent
            .post(url)
            .set("Authorization", &format!("Bearer {key}"))
            .set("Content-Type", "application/json")
            .send_json(body.clone());
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        match r {
            Ok(resp) => match resp.into_json::<Value>() {
                Ok(v) => return (Ok(v), ms),
                Err(e) => last = format!("bad json: {e}"),
            },
            Err(ureq::Error::Status(code, resp)) => {
                last = format!("{code}: {}", resp.into_string().unwrap_or_default().chars().take(300).collect::<String>());
                if code != 429 && code < 500 {
                    break;
                }
            }
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(std::time::Duration::from_millis(1500 * (attempt + 1)));
    }
    (Err(last), 0.0)
}

fn prio(key: &str) -> i64 {
    key.strip_prefix('p').and_then(|n| n.parse().ok()).unwrap_or(3)
}

fn ask_jev(agent: &ureq::Agent, key: &str, p: &Prepared) -> (Judged, Value) {
    let body = json!({"model": JEV_MODEL, "state": {"task": p.text}, "questions": p.jev_questions});
    let (res, ms) = post(agent, JEV_ENDPOINT, key, &body);
    let v = match res {
        Ok(v) => v,
        Err(e) => return (Judged { error: Some(e), ..Default::default() }, Value::Null),
    };
    let a = &v["answers"];
    let choice = |k: &str| (a[k]["choice"].as_str().unwrap_or("").to_string(), a[k]["confidence"].as_f64().unwrap_or(0.0));
    let (project_key, project_conf) = choice("project");
    let j = Judged {
        ok: !project_key.is_empty(),
        error: project_key.is_empty().then(|| "no project answer".into()),
        ms,
        input_tokens: v["usage"]["input_tokens"].as_f64().unwrap_or(0.0),
        output_tokens: v["usage"]["output_tokens"].as_f64().unwrap_or(0.0),
        project_key,
        project_conf,
        priority: prio(&choice("priority").0),
        status: choice("status").0,
        clarify: a["needs_clarification"]["noul"].as_f64().unwrap_or(0.0),
        tags: (0..p.tags.len()).map(|i| a[format!("tag_{i}")]["noul"].as_f64().unwrap_or(0.0)).collect(),
    };
    (j, v["answers"].clone())
}

fn ask_decisions(agent: &ureq::Agent, key: &str, p: &Prepared) -> (Judged, Value) {
    let input = json!({"task": p.text}).to_string();
    let body = json!({"model": DECISIONS_MODEL, "input": input, "questions": p.decision_questions});
    let (res, ms) = post(agent, DECISIONS_ENDPOINT, key, &body);
    let v = match res {
        Ok(v) => v,
        Err(e) => return (Judged { error: Some(e), ..Default::default() }, Value::Null),
    };
    let by_name: Map<String, Value> = v["answers"]
        .as_array()
        .map(|xs| xs.iter().filter_map(|x| Some((x["name"].as_str()?.to_string(), x.clone()))).collect())
        .unwrap_or_default();
    let get = |k: &str| by_name.get(k).cloned().unwrap_or(Value::Null);
    let choice = |k: &str| {
        let x = get(k);
        (x["choice"].as_str().unwrap_or("").to_string(), x["confidence"].as_f64().unwrap_or(0.0))
    };
    let (project_key, project_conf) = choice("project");
    let j = Judged {
        ok: !project_key.is_empty(),
        error: project_key.is_empty().then(|| "no project answer".into()),
        ms,
        input_tokens: v["usage"]["input_tokens"].as_f64().unwrap_or(0.0),
        output_tokens: 0.0,
        project_key,
        project_conf,
        priority: prio(&choice("priority").0),
        status: choice("status").0,
        clarify: get("needs_clarification")["probability"].as_f64().unwrap_or(0.0),
        tags: (0..p.tags.len()).map(|i| get(&format!("tag_{i}"))["probability"].as_f64().unwrap_or(0.0)).collect(),
    };
    (j, v["answers"].clone())
}

fn pick(p: &Prepared, j: &Judged) -> Dest {
    j.project_key
        .strip_prefix('p')
        .and_then(|n| n.parse::<usize>().ok())
        .and_then(|i| p.dests.get(i).cloned())
        .unwrap_or(Dest::None)
}

fn pct(xs: &mut [f64], q: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs[((xs.len() as f64 - 1.0) * q).round() as usize]
}

fn ratio(n: usize, d: usize) -> Value {
    if d == 0 {
        json!(null)
    } else {
        json!(format!("{:.1}% ({n}/{d})", 100.0 * n as f64 / d as f64))
    }
}

/// Everything reported for one model over one slice of items.
fn model_summary(rows: &[(&Prepared, &Judged)], price: f64) -> Value {
    let ok: Vec<_> = rows.iter().filter(|(_, j)| j.ok).collect();
    let labeled: Vec<_> = ok.iter().filter(|(p, _)| p.truth != Dest::None).collect();
    let unlabeled: Vec<_> = ok.iter().filter(|(p, _)| p.truth == Dest::None).collect();
    let raw_hit = |set: &[&&(&Prepared, &Judged)]| set.iter().filter(|(p, j)| pick(p, j).same(&p.truth)).count();
    // At the production bar: a pick below 0.55 is "none" (deferred to code / the user).
    let placed = |j: &Judged, p: &Prepared| pick(p, j) != Dest::None && j.project_conf >= PROJECT_CONFIDENCE;
    let placed_l: Vec<_> = labeled.iter().filter(|(p, j)| placed(j, p)).collect();
    let placed_hit = placed_l.iter().filter(|(p, j)| pick(p, j).same(&p.truth)).count();
    let unl_placed = unlabeled.iter().filter(|(p, j)| placed(j, p)).count();
    // Production pipeline: the deterministic route first, the model only when it declines.
    let pipeline_hit = ok
        .iter()
        .filter(|(p, j)| {
            let final_dest = if p.route != Dest::None {
                p.route.clone()
            } else if placed(j, p) {
                pick(p, j)
            } else {
                Dest::None
            };
            final_dest.same(&p.truth)
        })
        .count();
    let consulted: Vec<_> = labeled.iter().filter(|(p, _)| p.route == Dest::None).collect();
    let consulted_placed: Vec<_> = consulted.iter().filter(|(p, j)| placed(j, p)).collect();
    let consulted_hit = consulted_placed.iter().filter(|(p, j)| pick(p, j).same(&p.truth)).count();

    let prio_exact = ok.iter().filter(|(p, j)| j.priority == p.truth_priority).count();
    let prio_near = ok.iter().filter(|(p, j)| (j.priority - p.truth_priority).abs() <= 1).count();

    let (mut tp, mut fp, mut fn_) = (0, 0, 0);
    for (p, j) in &ok {
        for (i, tag) in p.tags.iter().enumerate() {
            let truth = p.truth_tags.iter().any(|t| t.eq_ignore_ascii_case(tag));
            let said = j.tags.get(i).copied().unwrap_or(0.0) >= TAG_BAR;
            match (said, truth) {
                (true, true) => tp += 1,
                (true, false) => fp += 1,
                (false, true) => fn_ += 1,
                _ => {}
            }
        }
    }

    let mut ms: Vec<f64> = ok.iter().map(|(_, j)| j.ms).collect();
    let tokens: f64 = ok.iter().map(|(_, j)| j.input_tokens).sum();
    let out_tokens: f64 = ok.iter().map(|(_, j)| j.output_tokens).sum();
    let n = ok.len().max(1) as f64;
    json!({
        "answered": ratio(ok.len(), rows.len()),
        "project_raw_accuracy_labeled": ratio(raw_hit(&labeled), labeled.len()),
        "project_at_0.55_coverage_labeled": ratio(placed_l.len(), labeled.len()),
        "project_at_0.55_precision_labeled": ratio(placed_hit, placed_l.len()),
        "project_at_0.55_deferred_labeled": ratio(labeled.len() - placed_l.len(), labeled.len()),
        "unlabeled_raw_says_none": ratio(raw_hit(&unlabeled), unlabeled.len()),
        "unlabeled_placed_anyway_at_0.55": ratio(unl_placed, unlabeled.len()),
        "route_declined_labeled_model_consulted": consulted.len(),
        "consulted_placed": ratio(consulted_placed.len(), consulted.len()),
        "consulted_precision": ratio(consulted_hit, consulted_placed.len()),
        "pipeline_accuracy_all": ratio(pipeline_hit, ok.len()),
        "priority_exact": ratio(prio_exact, ok.len()),
        "priority_within_1": ratio(prio_near, ok.len()),
        "status_todo_and_clear": ratio(ok.iter().filter(|(_, j)| j.status == "todo" && j.clarify < CLARIFY_STATUS_BAR).count(), ok.len()),
        "too_vague_at_0.90": ratio(ok.iter().filter(|(_, j)| j.clarify >= TOO_VAGUE).count(), ok.len()),
        "tags_at_0.72_precision": ratio(tp, tp + fp),
        "tags_at_0.72_recall": ratio(tp, tp + fn_),
        "latency_ms_p50": pct(&mut ms.clone(), 0.5).round(),
        "latency_ms_p95": pct(&mut ms, 0.95).round(),
        "input_tokens_mean": (tokens / n).round(),
        "output_tokens_mean": (out_tokens / n).round(),
        "usd_per_1k_calls_input": format!("{:.3}", tokens / n * 1000.0 * price / 1e6),
    })
}

fn summary(prepared: &[Prepared], results: &[(Judged, Judged)]) -> Value {
    let slice = |keep: &dyn Fn(&Prepared) -> bool| -> Value {
        let idx: Vec<usize> = (0..prepared.len()).filter(|&i| keep(&prepared[i]) && prepared[i].truth_offered).collect();
        let jev: Vec<_> = idx.iter().map(|&i| (&prepared[i], &results[i].0)).collect();
        let dec: Vec<_> = idx.iter().map(|&i| (&prepared[i], &results[i].1)).collect();
        let both: Vec<usize> = idx.iter().copied().filter(|&i| results[i].0.ok && results[i].1.ok).collect();
        let agree = both.iter().filter(|&&i| pick(&prepared[i], &results[i].0).same(&pick(&prepared[i], &results[i].1))).count();
        let labeled_both: Vec<usize> = both.iter().copied().filter(|&i| prepared[i].truth != Dest::None).collect();
        let route_hit = idx.iter().filter(|&&i| prepared[i].route.same(&prepared[i].truth)).count();
        let route_placed: Vec<_> = idx.iter().filter(|&&i| prepared[i].route != Dest::None).collect();
        let route_placed_hit = route_placed.iter().filter(|&&&i| prepared[i].route.same(&prepared[i].truth)).count();
        let (mut only_jev, mut only_dec) = (0, 0);
        for &i in &labeled_both {
            let j = pick(&prepared[i], &results[i].0).same(&prepared[i].truth);
            let d = pick(&prepared[i], &results[i].1).same(&prepared[i].truth);
            if j && !d { only_jev += 1 }
            if d && !j { only_dec += 1 }
        }
        json!({
            "items": idx.len(),
            "labeled": idx.iter().filter(|&&i| prepared[i].truth != Dest::None).count(),
            "deterministic_route_accuracy_all": ratio(route_hit, idx.len()),
            "deterministic_route_placed": ratio(route_placed.len(), idx.len()),
            "deterministic_route_precision": ratio(route_placed_hit, route_placed.len()),
            "project_argmax_agreement": ratio(agree, both.len()),
            "labeled_right_only_jev": only_jev,
            "labeled_right_only_decisions": only_dec,
            "jev": model_summary(&jev, JEV_PRICE),
            "decisions": model_summary(&dec, DECISIONS_PRICE),
        })
    };
    json!({
        "all": slice(&|_| true),
        "before_jev_shipped": slice(&|p| p.created_at.as_str() < JEV_SHIPPED),
        "after_jev_shipped": slice(&|p| p.created_at.as_str() >= JEV_SHIPPED),
        "excluded_truth_not_offered": prepared.iter().filter(|p| !p.truth_offered).count(),
    })
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let db_path = args.get(1).expect("usage: jev_vs_decisions <snapshot.db> <out_dir> [limit]");
    let out_dir = std::path::PathBuf::from(args.get(2).expect("out_dir"));
    let limit = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    anyhow::ensure!(
        !db_path.contains("com.cxtasks.app"),
        "refusing to open the live database; pass a .backup copy"
    );
    let jev_key = std::env::var("TYPESAFE_API_KEY")?;
    let openai_key = std::env::var("OPENAI_API_KEY")?;
    std::fs::create_dir_all(&out_dir)?;

    let mut conn = Connection::open(db_path)?;
    let prepared = Arc::new(prepare(&mut conn, limit)?);
    eprintln!("prepared {} items", prepared.len());

    let results: Arc<Mutex<Vec<Option<(Judged, Judged, Value, Value)>>>> =
        Arc::new(Mutex::new(vec![None; prepared.len()]));
    let next = Arc::new(Mutex::new(0usize));
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(60)).build();
    let handles: Vec<_> = (0..WORKERS)
        .map(|_| {
            let (prepared, results, next, agent) = (prepared.clone(), results.clone(), next.clone(), agent.clone());
            let (jev_key, openai_key) = (jev_key.clone(), openai_key.clone());
            std::thread::spawn(move || loop {
                let i = {
                    let mut n = next.lock().unwrap();
                    let i = *n;
                    *n += 1;
                    i
                };
                let Some(p) = prepared.get(i) else { break };
                // Alternate who goes first so neither sees a systematically warmer connection.
                let (j, d) = if i % 2 == 0 {
                    let j = ask_jev(&agent, &jev_key, p);
                    (j, ask_decisions(&agent, &openai_key, p))
                } else {
                    let d = ask_decisions(&agent, &openai_key, p);
                    (ask_jev(&agent, &jev_key, p), d)
                };
                if i % 20 == 0 {
                    eprintln!("{i}/{}", prepared.len());
                }
                results.lock().unwrap()[i] = Some((j.0, d.0, j.1, d.1));
            })
        })
        .collect();
    for h in handles {
        h.join().expect("worker panicked");
    }

    let results = Arc::try_unwrap(results).ok().unwrap().into_inner()?;
    let mut lines = String::new();
    let mut pairs = Vec::new();
    for (p, r) in prepared.iter().zip(results) {
        let (j, d, jraw, draw) = r.expect("every item answered or errored");
        lines.push_str(
            &json!({
                "id": p.id, "created_at": p.created_at, "text": p.text,
                "truth": p.truth.label(), "truth_offered": p.truth_offered,
                "truth_priority": p.truth_priority, "truth_tags": p.truth_tags,
                "route": p.route.label(),
                "jev": {"pick": pick(p, &j).label(), "conf": j.project_conf, "ms": j.ms, "error": j.error, "answers": jraw},
                "decisions": {"pick": pick(p, &d).label(), "conf": d.project_conf, "ms": d.ms, "error": d.error, "answers": draw},
            })
            .to_string(),
        );
        lines.push('\n');
        pairs.push((j, d));
    }
    std::fs::write(out_dir.join("items.jsonl"), lines)?;
    let s = summary(&prepared, &pairs);
    std::fs::write(out_dir.join("summary.json"), serde_json::to_string_pretty(&s)?)?;
    println!("{}", serde_json::to_string_pretty(&s)?);
    Ok(())
}
