//! Free text in, a routed task out.
//!
//! One sentence typed on a phone — "rerun the seo report for globex" — has to
//! become a task in the right project, against the right repo, optionally with
//! an agent already running against it. This module is the composition that
//! does it, and [`route`] is the part that earns its keep.
//!
//! # Why routing is SQL and not a model
//!
//! The obvious design is to hand the sentence and the project list to a judge.
//! It was measured against the real database first, and it loses on the only
//! cases that matter, for a reason already written down in a different repo:
//!
//! > "A vocabulary gap. No reranker can fix it: a judge never sees a row the
//! > retriever dropped. Only more evidence on the row can."
//! > — `vault-mcp/docs/launcher.md`
//!
//! A judge that is shown project NAMES cannot know that "globex" means
//! `CXVentures`, because Globex is a client whose name appears in no project. That
//! knowledge exists in exactly one place — the tasks already filed — which is
//! what [`history_candidates`] reads. Measured on the live database, the
//! history vote put `CXVentures` at 23.1 against a 6.7 runner-up for that
//! sentence.
//!
//! So the order is: what the caller said, then the project's own name, then
//! what history says. [`crate::jev`] is consulted for the fuzzy judgments it IS
//! good at — priority, whether the brief is actionable, whether it is too vague
//! to hand an unattended agent — and it gets the last word on the project only
//! when nothing deterministic cleared the bar.
//!
//! # Why the thresholds are what they are
//!
//! [`MIN_SHARE`], [`MIN_SUPPORT`] and [`MIN_RATIO`] were fitted to four real
//! sentences against the live database, which is a small sample and is stated
//! here rather than hidden. All three are deliberately **scale-free**, because
//! the first cut used an absolute bm25 floor and it only worked on a database
//! the size of the one it was measured on. Their job is not to be right every
//! time; it is to
//! **decline** rather than guess, because the client renders the pick as a chip
//! the user can change in one tap. A wrong confident answer costs a task filed
//! against the wrong repo. A declined answer costs one tap.
//!
//! The case that set the floor is "buy milk", which has no project and whose
//! best history vote was 9.5 from 2 supporting tasks — over [`MIN_RATIO`], so a
//! ratio test alone would have wrongly filed a grocery errand under a software
//! project.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db;
use crate::db::projects::Project;
use crate::db::tasks::{NewTask, Task, NOT_DELETED};
use crate::error::AppError;
use crate::executor::BackgroundExecutor;
use crate::runner::ToolTier;

/// How many FTS hits get a vote. Beyond this the tail is noise: every weakly
/// matching task in the database would otherwise add a little weight to its own
/// project, which favours whichever project simply has the most tasks.
const VOTE_WINDOW: usize = 12;

/// The winning project must hold this share of all the weight in the window.
///
/// # Why a share and not an absolute score
///
/// bm25 is corpus-relative: its IDF term depends on how many documents contain
/// each word, so the SAME sentence scores differently on a database with 900
/// tasks than on one with 11. This bar started life as an absolute floor of
/// 15.0, fitted against the live database, and the unit tests caught what that
/// costs — "rerun seo report for globex" scores 23.1 live and 9.64 against the
/// test fixture, so the floor would have declined to route anything at all on
/// a fresh install and quietly started working once enough tasks accumulated.
/// A share is scale-free and means the same thing on both.
const MIN_SHARE: f64 = 0.5;

/// ...supported by at least this many distinct tasks.
///
/// This is the bar "buy milk" actually fails (2 tasks), and it is the most
/// honest of the three: one freak match should never route anything. Also
/// scale-free — a count of documents is a count of documents.
const MIN_SUPPORT: usize = 3;

/// ...and lead the runner-up by this factor.
const MIN_RATIO: f64 = 2.0;

/// A name word shorter than this never matches.
///
/// Two-letter fragments of a project name ("x" in "Chris X") appear in ordinary
/// prose constantly and would route on nothing.
const MIN_NAME_WORD: usize = 3;

/// Where a title stops and the body begins.
const TITLE_CHARS: usize = 72;

/// How many of a project's own words enrich its Jev criterion.
///
/// The criteria map takes free text, not a bare name, so a project can be
/// described to the judge by the vocabulary its tasks actually use. This is the
/// "more evidence on the row" the launcher note prescribes, applied at the one
/// place a judge can still act on it.
const CRITERION_TERMS: usize = 8;

/// What decided the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// The caller named it. Never second-guessed.
    Explicit,
    /// A word of the project's own name appears in the text.
    Name,
    /// Tasks already filed under it match the text.
    History,
    /// Nothing deterministic cleared the bar and Jev chose.
    Jev,
    /// The request continues a task already in the thread, so it is filed
    /// where that task is. Nothing was routed.
    Thread,
}

/// What kind of place a candidate is.
///
/// # Why a repo can be a destination on its own
///
/// A task carries a `project` and a `repo_path`, and they are independent: 46 of
/// the 217 live tasks had a repo and no project at all. Routing that only knew
/// projects could not see any of them, so "lock mobile zoom on telly" — telly
/// being a repo — matched nothing, the history vote landed on four unrelated
/// projects, and the screen had no chip that was right. A repo no project owns
/// is a place work can be filed and run; it just has no grouping above it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestKind {
    #[default]
    Project,
    /// A working directory no project owns.
    Repo,
}

/// One place that could be the answer, and how strongly.
///
/// `score` is only comparable within a single `basis` — a name match's score is
/// matched characters, a history match's is summed bm25 — which is why [`route`]
/// resolves the bases in order rather than merging their numbers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    /// What to call it: a project's name, or for a bare repo its folder name.
    /// The field keeps its old name because the installed phone build decodes it.
    pub project: String,
    pub score: f64,
    /// Distinct tasks behind a history vote. Always 0 for a name match.
    pub support: usize,
    pub basis: Basis,
    #[serde(default)]
    pub kind: DestKind,
    /// Set when `kind` is [`DestKind::Repo`]: the directory itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
}

impl Candidate {
    fn of(dest: &Dest, score: f64, support: usize, basis: Basis) -> Self {
        match dest {
            Dest::Project(name) => Candidate {
                project: name.clone(),
                score,
                support,
                basis,
                kind: DestKind::Project,
                repo_path: None,
            },
            Dest::Repo { label, path } => Candidate {
                project: label.clone(),
                score,
                support,
                basis,
                kind: DestKind::Repo,
                repo_path: Some(path.clone()),
            },
        }
    }

    fn dest(&self) -> Dest {
        match (&self.kind, &self.repo_path) {
            (DestKind::Repo, Some(path)) => {
                Dest::Repo { label: self.project.clone(), path: path.clone() }
            }
            _ => Dest::Project(self.project.clone()),
        }
    }
}

/// A place work can go. The internal spelling of [`Candidate`], with equality
/// so a history vote can tally by destination.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Dest {
    Project(String),
    Repo { label: String, path: String },
}

/// The routing decision, including the runners-up.
///
/// `pick` is `None` when nothing cleared the bar. That is a normal outcome and
/// not an error: the client shows `candidates` and the user taps one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routing {
    /// The project chosen. `None` when nothing was chosen OR when the choice is
    /// a bare repo, which is [`Routing::repo`].
    pub pick: Option<String>,
    /// The directory chosen, when the destination is a repo no project owns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub basis: Option<Basis>,
    pub candidates: Vec<Candidate>,
}

impl Routing {
    /// Is there anywhere for the work to go?
    pub fn placed(&self) -> bool {
        self.pick.is_some() || self.repo.is_some()
    }
}

/// Lowercase alphanumeric words, punctuation dropped.
///
/// `split_whitespace` alone leaves "globex," and "globex" as different words, which
/// is the difference between routing a sentence and not.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// A directory work is filed against that no project owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BareRepo {
    /// The folder's own name, which is what a person calls it.
    pub label: String,
    pub path: String,
}

/// Is this a directory an unattended run could be bounded to?
///
/// Rejects the filesystem root and anyone's home directory. Invariant 12: `$HOME`
/// is not a workspace, and a task whose only repo is `/Users/chris` has made no
/// decision about where it belongs. The check is structural rather than "is it
/// MY home" because the server that answers a phone runs on the NAS and reads
/// paths written on the Mac.
fn is_workspace(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    match parts.as_slice() {
        [] | [_] => false,
        [root, _] if matches!(*root, "Users" | "home") => false,
        _ => true,
    }
}

fn folder_label(path: &str) -> String {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_string()
}

/// Every repo the tasks use that no project owns, most-used first.
///
/// Read from the tasks themselves rather than a registry, because there is no
/// registry: a repo becomes known by a task naming it. A repo a project owns is
/// left out on purpose — it is reached through that project, and listing it a
/// second time would offer two chips for one place.
pub fn bare_repos(conn: &Connection) -> Result<Vec<BareRepo>, AppError> {
    let sql = format!(
        "SELECT repo_path, COUNT(*) AS n FROM tasks
          WHERE {NOT_DELETED} AND repo_path IS NOT NULL AND TRIM(repo_path) <> ''
          GROUP BY repo_path COLLATE NOCASE
          ORDER BY n DESC, repo_path ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let raw = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out: Vec<BareRepo> = Vec::new();
    for path in raw {
        let path = db::projects::normalize_repo_path(&path);
        if !is_workspace(&path)
            || out.iter().any(|r| r.path.eq_ignore_ascii_case(&path))
            || db::projects::for_repo_path(conn, &path)?.is_some()
        {
            continue;
        }
        out.push(BareRepo { label: folder_label(&path), path });
    }
    Ok(out)
}

/// How well a repo's folder name is present in the text. 0 = not at all.
///
/// The WHOLE name has to appear — as one word (`telly`, `cxtasks`) or with every
/// hyphenated part present (`globex-course-studio` needs globex, course AND studio).
/// A project name matches on any one of its words, and that is tolerable for a
/// name somebody chose; a folder name is chosen by the toolchain and made of
/// generic parts, and "studio" alone must not file a music request under a
/// video-course repo.
fn repo_name_score(label: &str, hay: &[String]) -> usize {
    let parts = words(label);
    let compact: String = parts.concat();
    let len = compact.chars().count();
    if len < MIN_NAME_WORD {
        return 0;
    }
    if hay.contains(&compact) || (!parts.is_empty() && parts.iter().all(|p| hay.contains(p))) {
        len
    } else {
        0
    }
}

/// Places whose own name appears in the text, scored by matched characters.
///
/// Character count rather than word count so a longer, more specific name wins:
/// "review the pocketbuddy paywall" matches both `pocketbuddy` (11) and
/// `Apollo Outreach Review` (6, on "review"), and the specific one should win.
fn name_candidates(projects: &[Project], repos: &[BareRepo], text: &str) -> Vec<Candidate> {
    let hay = words(text);
    let mut out: Vec<Candidate> = projects
        .iter()
        .filter_map(|p| {
            let score: usize = words(&p.name)
                .iter()
                .filter(|w| w.chars().count() >= MIN_NAME_WORD && hay.contains(w))
                .map(|w| w.chars().count())
                .sum();
            (score > 0).then(|| {
                Candidate::of(&Dest::Project(p.name.clone()), score as f64, 0, Basis::Name)
            })
        })
        .collect();
    out.extend(repos.iter().filter_map(|r| {
        let score = repo_name_score(&r.label, &hay);
        (score > 0).then(|| {
            Candidate::of(
                &Dest::Repo { label: r.label.clone(), path: r.path.clone() },
                score as f64,
                0,
                Basis::Name,
            )
        })
    }));
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

/// An FTS5 expression that ORs every usable token.
///
/// Deliberately not [`crate::db::search::build_match_expr`], which ANDs: search
/// wants every word present, routing wants the rare word to dominate. bm25's
/// IDF then does the work a stopword list would otherwise have to — "for" and
/// "the" match half the database and score near nothing, "globex" matches four
/// rows and scores high — so no stopword list exists here on purpose.
///
/// Quoting follows the same rule as search (gotcha #26): every token is quoted
/// so FTS5 operators in user input are literal text, and an interior `"` is
/// doubled. Raw interpolation turns "budget (" into a syntax error that
/// surfaces as a silent no-match.
fn or_match_expr(text: &str) -> Option<String> {
    let parts: Vec<String> = text
        .split_whitespace()
        .filter(|t| t.chars().any(char::is_alphanumeric))
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" OR "))
}

fn has_fts(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='tasks_fts')",
        [],
        |r| r.get(0),
    )
    .unwrap_or(false)
}

/// Places that already own tasks matching this text, by summed bm25.
///
/// The sum is computed in Rust rather than SQL because `bm25()` is an
/// fts5 auxiliary function and SQLite refuses it inside an aggregate
/// ("unable to use function bm25 in the requested context").
///
/// A task votes for where it LIVES: its project, or failing that the project
/// that owns its repo, or failing that the repo itself. Tasks with neither are
/// excluded. They are numerous — four of the eight tasks mentioning "globex" on
/// the live database have no project — and counting them would let the absence
/// of a decision vote on the decision. A task with a repo and no project HAS
/// decided where it goes, which is why it votes now: leaving it out made every
/// repo-only task invisible to routing.
fn history_candidates(conn: &Connection, text: &str) -> Result<Vec<Candidate>, AppError> {
    if !has_fts(conn) {
        return Ok(Vec::new());
    }
    let Some(expr) = or_match_expr(text) else {
        return Ok(Vec::new());
    };
    // Over-fetched, because the two windows below are filled independently and a
    // row whose only repo is somebody's home directory is dropped.
    let sql = format!(
        "SELECT t.project, t.repo_path, -bm25(tasks_fts, 4.0, 1.0, 2.0) AS w
           FROM tasks_fts f
           JOIN tasks t ON t.rowid = f.rowid
          WHERE tasks_fts MATCH ?1
            AND ((t.project IS NOT NULL AND t.project <> '')
                 OR (t.repo_path IS NOT NULL AND TRIM(t.repo_path) <> ''))
            AND t.{NOT_DELETED}
          ORDER BY bm25(tasks_fts, 4.0, 1.0, 2.0)
          LIMIT {}",
        VOTE_WINDOW * 5
    );
    // A malformed expression must degrade to "no opinion", never to a failed
    // capture: the task still has to be filed.
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("intake: history vote unavailable: {e}");
            return Ok(Vec::new());
        }
    };
    let rows = match stmt.query_map([&expr], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, f64>(2)?,
        ))
    }) {
        Ok(rows) => rows.collect::<Result<Vec<_>, _>>()?,
        Err(e) => {
            log::warn!("intake: history vote failed for {expr:?}: {e}");
            return Ok(Vec::new());
        }
    };

    // TWO windows, filled independently, and that is what keeps the flagship
    // case working. One shared window let tasks that live in a bare repo take
    // slots from the project's own tasks and dilute its share: "rerun the seo
    // report for globex" fell from 23.1 to 18.1 for CXVentures once the
    // `globex-course-studio` folder's tasks started voting, dropped under
    // [`MIN_SHARE`], and the sentence the whole router was built for started
    // asking. Projects vote exactly as they did before; a bare repo has its own
    // window and is weighed against them in [`route_with`].
    let mut tally: Vec<(Dest, f64, usize)> = Vec::new();
    let (mut project_votes, mut repo_votes) = (0, 0);
    for (project, repo, weight) in rows {
        let project = project.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        let dest = match (project, repo.as_deref().map(str::trim).filter(|r| !r.is_empty())) {
            (Some(p), _) => Dest::Project(p),
            (None, Some(raw)) => {
                let path = db::projects::normalize_repo_path(raw);
                match db::projects::for_repo_path(conn, &path)? {
                    Some(owner) => Dest::Project(owner),
                    None if is_workspace(&path) => Dest::Repo { label: folder_label(&path), path },
                    None => continue,
                }
            }
            (None, None) => continue,
        };
        let votes = if matches!(dest, Dest::Project(_)) { &mut project_votes } else { &mut repo_votes };
        if *votes == VOTE_WINDOW {
            continue;
        }
        *votes += 1;
        match tally.iter_mut().find(|(d, _, _)| *d == dest) {
            Some((_, score, support)) => {
                *score += weight;
                *support += 1;
            }
            None => tally.push((dest, weight, 1)),
        }
    }
    let mut out: Vec<Candidate> = tally
        .iter()
        .map(|(d, score, support)| Candidate::of(d, *score, *support, Basis::History))
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(out)
}

/// Decide the destination, or decline.
///
/// Name beats history unconditionally. That ordering is not a preference, it is
/// the fix for a measured failure: "acme demo follow up" gives the history vote
/// to `CXVentures` (22.0) while the correct answer, `Acme / Intake`, finishes last
/// at 4.45 — because four years of Acme work is discussed in CXVentures tasks.
/// The project's own name is the stronger evidence whenever it is present.
pub fn route(conn: &Connection, text: &str, projects: &[Project]) -> Result<Routing, AppError> {
    route_with(conn, text, projects, true)
}

/// Names only — what [`route`] would say without the history vote.
///
/// This is how a typed ANSWER is read ("Telly", "telly is the project"). An
/// answer names the place; the request it answers was already routed by
/// history and got nowhere, and letting the same evidence vote a second time
/// would put the answer back where the question came from.
pub fn route_names(conn: &Connection, text: &str, projects: &[Project]) -> Result<Routing, AppError> {
    route_with(conn, text, projects, false)
}

fn route_with(
    conn: &Connection,
    text: &str,
    projects: &[Project],
    use_history: bool,
) -> Result<Routing, AppError> {
    let repos = bare_repos(conn)?;
    let names = name_candidates(projects, &repos, text);
    let history = if use_history { history_candidates(conn, text)? } else { Vec::new() };

    // A tie on name match is not a pick. Two places whose names both appear
    // is exactly the case where the user meant one of them and only they know
    // which.
    let name_pick = match names.split_first() {
        Some((top, rest)) if rest.first().is_none_or(|next| next.score < top.score) => {
            Some(top.clone())
        }
        _ => None,
    };

    // Does the top of a tally clear the bar, judged within that tally?
    let clears = |votes: &[&Candidate]| -> Option<Candidate> {
        let top = votes.first()?;
        let runner_up = votes.get(1).map_or(0.0, |c| c.score);
        let total: f64 = votes.iter().map(|c| c.score).sum();
        // `total` can be zero when every hit scores 0.0, which bm25 does return.
        // Guarding it keeps the share from being NaN, which compares false
        // against everything and would fail in the safe direction by accident
        // rather than on purpose.
        (total > 0.0
            && top.score / total >= MIN_SHARE
            && top.support >= MIN_SUPPORT
            && top.score >= runner_up * MIN_RATIO)
            .then(|| (*top).clone())
    };
    let project_votes: Vec<&Candidate> =
        history.iter().filter(|c| c.kind == DestKind::Project).collect();
    let repo_votes: Vec<&Candidate> =
        history.iter().filter(|c| c.kind == DestKind::Repo).collect();
    // A project wins on the terms it always did. A bare repo is only picked when
    // no project clears the bar AND the repo outweighs the strongest project —
    // a folder with three matching tasks must not beat a project that matched
    // nearly as well, because that is the mistake of filing under the folder
    // what belongs to the project that owns the topic.
    let history_pick = clears(&project_votes).or_else(|| {
        let best_project = project_votes.first().map_or(0.0, |c| c.score);
        clears(&repo_votes).filter(|c| c.score >= best_project * MIN_RATIO)
    });

    let (chosen, basis) = match (name_pick, history_pick) {
        (Some(c), _) => (Some(c), Some(Basis::Name)),
        (None, Some(c)) => (Some(c), Some(Basis::History)),
        (None, None) => (None, None),
    };

    let mut candidates = names;
    candidates.extend(history);
    Ok(match chosen {
        Some(c) => match c.dest() {
            Dest::Project(name) => Routing { pick: Some(name), repo: None, basis, candidates },
            Dest::Repo { path, .. } => Routing { pick: None, repo: Some(path), basis, candidates },
        },
        None => Routing { pick: None, repo: None, basis, candidates },
    })
}

/// Describe each place to Jev in the vocabulary its own tasks use.
///
/// The criteria map takes free text, so `CXVentures` can be presented as
/// "CXVentures — recent: globex, seo, dmarc, apollo" instead of a bare name. This
/// is the only lever that addresses the vocabulary gap for a judge, and it is
/// cheap: one indexed query per place against titles already in the database.
fn describe(conn: &Connection, dest: &Dest) -> String {
    let (head, titles): (String, Vec<String>) = match dest {
        Dest::Project(project) => {
            let sql = format!(
                "SELECT title FROM tasks
                  WHERE project = ?1 COLLATE NOCASE AND {NOT_DELETED}
                  ORDER BY created_at DESC LIMIT 12"
            );
            (project.clone(), recent_titles(conn, &sql, project))
        }
        Dest::Repo { label, path } => {
            let sql = format!(
                "SELECT title FROM tasks
                  WHERE repo_path = ?1 COLLATE NOCASE AND {NOT_DELETED}
                  ORDER BY created_at DESC LIMIT 12"
            );
            // Said out loud: a judge shown a bare folder name has no way to
            // know it is a place work goes and not a word in the request.
            (format!("{label} (a repo with no project)"), recent_titles(conn, &sql, path))
        }
    };

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

fn recent_titles(conn: &Connection, sql: &str, key: &str) -> Vec<String> {
    conn.prepare(sql)
        .and_then(|mut s| {
            s.query_map([key], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default()
}

/// What the caller asked for.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct IntakeRequest {
    pub text: String,
    /// Client-minted, so a phone replaying a timed-out capture does not create
    /// a second task. Passed straight to [`NewTask::id`], which documents why.
    #[serde(default)]
    pub id: Option<String>,
    /// Set by a user tapping the project chip. Outranks every inference.
    /// A name no project has yet CREATES the project — that is how the phone
    /// starts one, with no separate step to forget.
    #[serde(default)]
    pub project: Option<String>,
    /// A directory to file it against, chosen by hand. With `project`, the
    /// project is also given this repo, so the next request that names the
    /// project finds it. Alone, the work is filed against the bare repo.
    #[serde(default)]
    pub repo_path: Option<String>,
    /// Continue this task instead of filing a new one: `text` is added to it as
    /// the next thing you said, and it is worked again from where it stopped.
    #[serde(default)]
    pub follow_up_of: Option<String>,
    /// Queue a background run once it is filed.
    #[serde(default)]
    pub arm: bool,
    /// Defaults to the only tier that provably cannot mutate a repo.
    #[serde(default)]
    pub tier: Option<ToolTier>,
    #[serde(default)]
    pub executor: Option<BackgroundExecutor>,
    /// Carried over from a prior [`assess`], which is where Jev's opinion is
    /// formed. Absent is normal — a one-shot capture never asks.
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub status: Option<String>,
}

/// What [`assess`] learned, before anything is written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assessed {
    pub routing: Routing,
    /// `None` whenever capture judgments are off, or neither Decisions nor Jev
    /// answered.
    pub assessment: Option<crate::jev::CaptureAssessment>,
    /// The capture engine's read that the brief is too vague to hand an
    /// unattended agent, against that engine's own bar.
    /// The client decides what to do about it; nothing here refuses to arm.
    pub too_vague: bool,
}

/// Route and judge, writing nothing.
///
/// # Why this is a separate verb from [`intake`]
///
/// [`crate::api::dispatch_synced`] runs every WRITE verb inside
/// `BEGIN IMMEDIATE`, which takes SQLite's write lock for the whole verb. Jev's
/// HTTP call has a 12-second timeout, and four processes share this database —
/// so calling it from the write path would let a slow third-party API block the
/// app, the runner and the MCP from writing for twelve seconds. Here it is
/// read-only, `dispatch_synced` returns before opening any transaction, and the
/// lock is never held across the network.
///
/// The client calls this to populate the project chip and to decide whether to
/// arm, then calls [`intake`] with what it chose. A caller that wants one round
/// trip skips it and gets the deterministic route, which is the same answer
/// minus Jev's tiebreak.
pub fn assess(conn: &Connection, text: &str) -> Result<Assessed, AppError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::Validation("intake needs some text".into()));
    }
    let projects = db::projects::list(conn, false)?;
    let mut routing = route(conn, text, &projects)?;

    // Jev is offered projects first, then the repos no project owns — the same
    // places the deterministic route knows. It used to see projects only, which
    // is why a request about a bare repo could never be answered by it either.
    // Its answer maps back to the real place by position: `assess_capture`
    // returns whichever string it was handed, and the strings are enriched.
    let dests: Vec<Dest> = projects
        .iter()
        .map(|p| Dest::Project(p.name.clone()))
        .chain(bare_repos(conn)?.into_iter().map(|r| Dest::Repo { label: r.label, path: r.path }))
        .collect();
    let described: Vec<String> = dests.iter().map(|d| describe(conn, d)).collect();
    let tags: Vec<String> = db::tags::list_with_counts(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|t| t.name)
        .collect();
    let assessment = match crate::jev::assess_capture(
        conn,
        &crate::jev::CaptureRequest {
            text: text.to_string(),
            projects: described.clone(),
            tags,
        },
    ) {
        Ok(a) => a,
        Err(e) => {
            log::warn!("intake: Jev unavailable, using the deterministic route: {e}");
            None
        }
    };
    let mut jev_pick: Option<Dest> = None;
    let assessment = assessment.map(|mut a| {
        jev_pick = a
            .project
            .take()
            .and_then(|d| described.iter().position(|x| *x == d))
            .map(|i| dests[i].clone());
        // `CaptureAssessment.project` means a PROJECT to every other caller, the
        // Mac's quick-capture included. A bare repo is not one, so it travels
        // only in `routing`.
        a.project = match &jev_pick {
            Some(Dest::Project(name)) => Some(name.clone()),
            _ => None,
        };
        a
    });

    // Jev only gets the place when nothing deterministic cleared the bar.
    if !routing.placed() {
        if let Some(dest) = jev_pick {
            let score = assessment.as_ref().map_or(0.0, |a| a.confidence);
            routing.candidates.push(Candidate::of(&dest, score, 0, Basis::Jev));
            match dest {
                Dest::Project(name) => routing.pick = Some(name),
                Dest::Repo { path, .. } => routing.repo = Some(path),
            }
            routing.basis = Some(Basis::Jev);
        }
    }

    // Read against the answering engine's own bar (`jev::CaptureBars`): the
    // engines are not calibrated alike, so there is no one constant for it.
    let too_vague = assessment.as_ref().is_some_and(|a| a.too_vague);
    Ok(Assessed { routing, assessment, too_vague })
}

/// What happened.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntakeResult {
    pub task: Task,
    pub routing: Routing,
    /// Present when a run was actually queued.
    pub queued: Option<crate::api::QueueResult>,
    /// Why an armed request was filed without being queued. `None` means it
    /// either ran or was never armed.
    pub held: Option<String>,
}

/// First line, clipped at a word boundary.
fn title_of(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() <= TITLE_CHARS {
        return line.to_string();
    }
    let clipped: String = line.chars().take(TITLE_CHARS).collect();
    match clipped.rsplit_once(char::is_whitespace) {
        Some((head, _)) if head.chars().count() >= TITLE_CHARS / 2 => format!("{head}…"),
        _ => format!("{clipped}…"),
    }
}

/// Queue the task if it was armed, or say why it was not.
///
/// A refusal to queue is data here, not a failed request: by this point the
/// task is filed and the sentence is safe, and the caller is better served by
/// "filed, but it cannot run yet because …" than by an error that loses the
/// task reference. Only a `Dispatch` refusal is folded this way — those are
/// the guards in `queue_for_background`, which all run before it writes
/// anything. A database error still fails the request.
fn arm(
    conn: &mut Connection,
    task: Task,
    armed: bool,
    routing: &Routing,
) -> Result<(Task, Option<crate::api::QueueResult>, Option<String>), AppError> {
    // An armed request with nowhere to run is filed and SAID SO, rather than
    // left at `queued` with no explanation — gotcha #18's failure one layer up.
    if armed && !routing.placed() && task.repo_path.is_none() {
        return Ok((task, None, Some("no project — pick one and it will run".to_string())));
    }
    if !armed {
        return Ok((task, None, None));
    }
    match crate::dispatch::queue_for_background(|f| f(conn), &task.id) {
        Ok(outcome) => {
            let queued = crate::api::QueueResult {
                executor: outcome.task.bg_executor,
                task: outcome.task,
                runner_installed: outcome.runner_installed,
                paused: outcome.paused,
            };
            Ok((queued.task.clone(), Some(queued), None))
        }
        Err(AppError::Dispatch(why)) => Ok((task, None, Some(why))),
        Err(other) => Err(other),
    }
}

fn blank(v: &Option<String>) -> bool {
    v.as_deref().is_none_or(|s| s.trim().is_empty())
}

/// Has this task no project and no repo — nowhere to go?
fn is_unplaced(task: &Task) -> bool {
    blank(&task.project) && blank(&task.repo_path)
}

/// Where a task actually is, as a routing. Never a claim about where it should
/// have gone: a replay and a follow-up both report the truth on the row.
fn routing_of(task: &Task, basis: Option<Basis>) -> Routing {
    Routing {
        pick: task.project.clone().filter(|p| !p.trim().is_empty()),
        repo: task
            .project
            .as_deref()
            .is_none_or(|p| p.trim().is_empty())
            .then(|| task.repo_path.clone())
            .flatten(),
        basis,
        candidates: Vec::new(),
    }
}

/// Is a run queued or in flight for this task?
fn underway(task: &Task) -> bool {
    matches!(task.status.as_str(), "queued" | "in_progress")
}

/// A replayed request for a task that already has a place.
///
/// Changes nothing: not the place (the tap that got here may name a different
/// one, and honouring it would need a decision nobody asked for), and not the
/// status of a run in flight — queueing a task that is `in_progress` puts it
/// back at `queued`, where a second claim wins against the run already going.
/// It reports what is true of the row, so the screen never shows a project the
/// task is not in.
fn replay(conn: &mut Connection, task: Task, armed: bool) -> Result<IntakeResult, AppError> {
    let routing = routing_of(&task, None);
    if armed && underway(&task) {
        let queued = crate::api::QueueResult {
            executor: task.bg_executor,
            task: task.clone(),
            runner_installed: crate::platform::drain_is_installed(),
            paused: crate::paths::background_paused(),
        };
        return Ok(IntakeResult { task, routing, queued: Some(queued), held: None });
    }
    let (task, queued, held) = arm(conn, task, armed, &routing)?;
    Ok(IntakeResult { task, routing, queued, held })
}

/// A held request answered late: give an unplaced task its place.
///
/// # Why this exists
///
/// `db::tasks::create` is idempotent on a client id — a replay returns the
/// existing row and changes nothing. That is right for a retried capture and
/// wrong for a chip tap, which re-files the SAME id with a project attached:
/// the tap silently did nothing, the task stayed unplaced, and queueing it
/// failed with "add a repo path first". Every "Which project?" chip has been
/// dead since the Ask tab shipped.
///
/// Only a task with neither project nor repo is placed. One that already has a
/// place is left alone, so a replay of a finished request stays a replay.
fn place(
    conn: &Connection,
    task: Task,
    routing: &Routing,
    workspace: Option<&str>,
) -> Result<Task, AppError> {
    if !is_unplaced(&task) || !routing.placed() {
        return Ok(task);
    }
    if let Some(project) = &routing.pick {
        db::projects::ensure_exists(conn, project)?;
    }
    db::tasks::update(
        conn,
        &task.id,
        &db::tasks::TaskPatch {
            project: routing.pick.clone().map(Some),
            repo_path: workspace.map(|w| Some(w.to_string())),
            ..Default::default()
        },
    )
}

/// Free text to a filed, routed, optionally running task.
///
/// # Nothing here touches the network
///
/// Routing is name matching plus one indexed FTS query, so this holds the write
/// lock for microseconds. Jev's judgment comes from [`assess`], which the client
/// calls first and passes back through `project` / `priority` / `status`. A
/// caller that skips it still gets the deterministic route — which is what makes
/// a one-shot capture from a Shortcut work, and why Jev being off, expired or
/// unreachable never costs anybody a captured sentence.
pub fn intake(conn: &mut Connection, req: &IntakeRequest) -> Result<IntakeResult, AppError> {
    let text = req.text.trim();
    if text.is_empty() {
        return Err(AppError::Validation("intake needs some text".into()));
    }
    if let Some(of) = req.follow_up_of.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return follow_up(conn, req, text, of);
    }

    let existing = match req.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => db::tasks::get(conn, id)?,
        None => None,
    };
    // Before anything is written: a replay of a placed task must not create the
    // project or claim the repo a tap named for a task it then declines to move.
    if let Some(task) = existing.as_ref().filter(|t| !is_unplaced(t)) {
        return replay(conn, task.clone(), req.arm);
    }

    let project = req.project.as_deref().map(str::trim).filter(|p| !p.is_empty());
    let repo = req
        .repo_path
        .as_deref()
        .map(db::projects::normalize_repo_path)
        .filter(|r| !r.is_empty());
    if let Some(r) = &repo {
        if !std::path::Path::new(r).is_absolute() {
            return Err(AppError::Validation(format!("repo path must be absolute, got '{r}'")));
        }
    }

    let routing = if project.is_some() || repo.is_some() {
        Routing {
            pick: project.map(str::to_string),
            // With a project the repo is the project's; alone it IS the place.
            repo: if project.is_none() { repo.clone() } else { None },
            basis: Some(Basis::Explicit),
            candidates: Vec::new(),
        }
    } else {
        let projects = db::projects::list(conn, false)?;
        route(conn, text, &projects)?
    };

    // A project made around a repo owns it from then on, so the next sentence
    // that names the project lands there without being told. Refused, with the
    // current owner named, when another project already has it — before any row
    // is written.
    if let (Some(p), Some(r)) = (project, &repo) {
        db::projects::add_repo(conn, p, r, false)?;
    }
    let workspace = repo.or_else(|| routing.repo.clone());

    let task = match existing {
        Some(task) => place(conn, task, &routing, workspace.as_deref())?,
        None => {
            let tier = req.tier.unwrap_or(ToolTier::ReadOnly);
            db::tasks::create(
                conn,
                &NewTask {
                    id: req.id.clone(),
                    title: title_of(text),
                    // The full sentence is both what a human reads and what the
                    // agent is briefed with; the title is a clipped restatement.
                    body: (text.chars().count() > TITLE_CHARS).then(|| text.to_string()),
                    prompt: Some(text.to_string()),
                    project: routing.pick.clone(),
                    repo_path: workspace,
                    status: req.status.clone(),
                    priority: req.priority,
                    bg_allowed: Some(req.arm),
                    bg_executor: req.executor,
                    bg_writes_allowed: Some(matches!(tier, ToolTier::Write)),
                    bg_shell_allowed: Some(matches!(tier, ToolTier::Shell | ToolTier::Write)),
                    ..Default::default()
                },
            )?
        }
    };

    let (task, queued, held) = arm(conn, task, req.arm, &routing)?;
    Ok(IntakeResult { task, routing, queued, held })
}

/// The next thing said in a thread: comment on the task, then work it again.
///
/// The comment IS the instruction. `dispatch::history_block` replays comments
/// and the last run's handoff, and a continuation prompt says pick up where the
/// previous run stopped — so the second run starts from the first one's answer
/// instead of from nothing, which is what a reply to an agent means.
///
/// A task that is queued or running is only commented on. Queueing it again
/// would let a second claim win against a run already in flight; the message
/// says so, and the comment is there for the next run.
fn follow_up(
    conn: &mut Connection,
    req: &IntakeRequest,
    text: &str,
    of: &str,
) -> Result<IntakeResult, AppError> {
    let task = db::tasks::get(conn, of)?
        .ok_or_else(|| AppError::NotFound(format!("task {of} not found")))?;
    let routing = routing_of(&task, Some(Basis::Thread));

    db::comments::add(conn, &task.id, db::comments::AUTHOR_ME, text)?;

    // The visible tier speaks for THIS send. Left alone, a reply would run
    // with whatever the first request was filed at, however the control now
    // reads.
    let task = match req.tier {
        Some(tier) => db::tasks::update(
            conn,
            &task.id,
            &db::tasks::TaskPatch {
                bg_writes_allowed: Some(matches!(tier, ToolTier::Write)),
                bg_shell_allowed: Some(matches!(tier, ToolTier::Shell | ToolTier::Write)),
                ..Default::default()
            },
        )?,
        None => task,
    };

    if underway(&task) {
        return Ok(IntakeResult {
            task,
            routing,
            queued: None,
            held: Some(
                "it is still working — your message is on the task and the next run will read it"
                    .into(),
            ),
        });
    }
    let (task, queued, held) = arm(conn, task, req.arm, &routing)?;
    Ok(IntakeResult { task, routing, queued, held })
}

/// Read a typed ANSWER to "which project?": names only, writing nothing.
pub fn resolve(conn: &Connection, text: &str) -> Result<Routing, AppError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::Validation("resolve needs some text".into()));
    }
    let projects = db::projects::list(conn, false)?;
    route_names(conn, text, &projects)
}

/// Every place a request can go, for the picker and the new-project form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Places {
    pub projects: Vec<String>,
    /// Repos no project owns. Only these can be given to a NEW project: a repo
    /// has one owner, and the rest already belong to a project listed above.
    pub repos: Vec<BareRepo>,
}

pub fn places(conn: &Connection) -> Result<Places, AppError> {
    Ok(Places {
        projects: db::projects::list(conn, false)?.into_iter().map(|p| p.name).collect(),
        repos: bare_repos(conn)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;

    /// A fixture shaped like the real database, because the thresholds were
    /// fitted against it.
    ///
    /// The four sentences below are the ones measured on the live DB on
    /// 2026-09-19; the titles here are abbreviated versions of real tasks, kept
    /// only dense enough to reproduce each outcome. What must survive is the
    /// SHAPE: `Globex` is a client with no project of its own whose work lives in
    /// `CXVentures`, and `Acme / Intake` is a project whose subject is also
    /// discussed in CXVentures tasks.
    fn seeded() -> Connection {
        let conn = test_conn();
        let file = |title: &str, project: &str| {
            db::tasks::create(
                &conn,
                &NewTask {
                    title: title.into(),
                    project: Some(project.into()),
                    ..Default::default()
                },
            )
            .unwrap();
        };
        file("Ask Dana (Globex) for a testimonial and permission to name them", "CXVentures");
        file("Run the Globex SEO audit baseline and send Dana the findings", "CXVentures");
        file("Run the pSEO wave gate — decide whether Axis B survives", "CXVentures");
        file("Tighten cxventures.io DMARC to p=quarantine", "CXVentures");
        file("Globex oversight: review the Northwind agency SEO deliverables", "CXVentures");
        file("Acme demo: rehearse the intake chat responder walkthrough", "Acme / Intake");
        file("Act on PocketBuddy GSC and Trends findings for Coast", "pocketbuddy");
        file("Request indexing on the 9 never-crawled PocketBuddy pages", "pocketbuddy");
        file("Fix the PocketBuddy paywall copy before the SEO push", "pocketbuddy");
        file("Rebuild artist_vs_target_tier so target-tier gaps show", "Artist Advisory");
        file("Normalize bare text nodes in rich email bodies", "CXMail");
        conn
    }

    fn pick(conn: &Connection, text: &str) -> Option<String> {
        let projects = db::projects::list(conn, false).unwrap();
        route(conn, text, &projects).unwrap().pick
    }

    /// The case the whole module exists for.
    ///
    /// "globex" appears in no project NAME, so a judge shown only names cannot get
    /// this right — it is the vocabulary gap from `vault-mcp/docs/launcher.md`.
    /// History has the answer because the tasks are already filed.
    #[test]
    fn a_client_named_in_no_project_routes_by_history() {
        let conn = seeded();
        assert_eq!(pick(&conn, "rerun seo report for globex").as_deref(), Some("CXVentures"));
    }

    /// The measured failure that made name matching outrank the history vote.
    ///
    /// On the live database this sentence gave `CXVentures` 22.0 and left the
    /// correct answer, `Acme / Intake`, last at 4.45 — because years of Acme work is
    /// discussed in CXVentures tasks. The project's own name is the stronger
    /// evidence whenever it is present.
    #[test]
    fn a_project_named_outright_beats_whatever_history_says() {
        let conn = seeded();
        assert_eq!(pick(&conn, "acme demo follow up").as_deref(), Some("Acme / Intake"));
    }

    /// The measured failure that put an absolute floor under the vote.
    ///
    /// "buy milk" led its runner-up by 3.0x on the live database — clear of
    /// [`MIN_RATIO`] — on the strength of two weak matches. A ratio test alone
    /// would have filed a grocery errand under a software project. Declining is
    /// the correct answer and costs the user one tap.
    #[test]
    fn an_errand_belonging_to_no_project_is_declined_not_guessed() {
        let conn = seeded();
        assert_eq!(pick(&conn, "buy milk"), None);
    }

    /// A specific name beats a generic word of another project's name.
    #[test]
    fn the_longer_name_match_wins() {
        let conn = seeded();
        db::projects::create(
            &conn,
            "Apollo Outreach Review",
            crate::db::projects::ProjectKind::Project,
            None,
        )
        .unwrap();
        // Matches `pocketbuddy` (11 chars) and `Apollo Outreach Review` (6, on
        // "review"). The one that names the actual subject has to win.
        assert_eq!(
            pick(&conn, "review the pocketbuddy paywall").as_deref(),
            Some("pocketbuddy")
        );
    }

    /// Two names present is exactly when the user knows and the code does not.
    #[test]
    fn a_tie_between_two_named_projects_declines() {
        let conn = test_conn();
        for name in ["alpha", "gamma"] {
            db::projects::create(&conn, name, crate::db::projects::ProjectKind::Project, None)
                .unwrap();
        }
        assert_eq!(pick(&conn, "move alpha over to gamma"), None);
    }

    /// FTS5 operators in a captured sentence are literal text, never syntax.
    ///
    /// Invariant 2's rule, restated for the OR expression this module builds:
    /// raw interpolation turns an ordinary sentence into a query error, and the
    /// symptom is a silent no-match rather than anything that looks wrong.
    #[test]
    fn punctuation_in_the_sentence_cannot_break_the_query() {
        let conn = seeded();
        for hostile in [
            "fix the pocketbuddy paywall (urgent)",
            "pocketbuddy AND paywall",
            "pocketbuddy \"paywall\"",
            "* OR pocketbuddy",
            "((((",
        ] {
            // Must not error. The first four should still find the project.
            let got = pick(&conn, hostile);
            if hostile != "((((" {
                assert_eq!(got.as_deref(), Some("pocketbuddy"), "for {hostile:?}");
            }
        }
    }

    /// An explicit project is never second-guessed, even against a sentence
    /// that routes elsewhere on its own.
    #[test]
    fn an_explicit_project_outranks_every_inference() {
        let mut conn = seeded();
        let out = intake(
            &mut conn,
            &IntakeRequest {
                text: "rerun seo report for globex".into(),
                id: None,
                project: Some("CXMail".into()),
                arm: false,
                tier: None,
                executor: None,
                priority: None,
                status: None,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out.task.project.as_deref(), Some("CXMail"));
        assert_eq!(out.routing.basis, Some(Basis::Explicit));
    }

    /// An armed request with no project says why it did not run.
    ///
    /// The alternative is gotcha #18 one layer up: a task sitting at `queued`
    /// with nothing on screen explaining that nothing will ever claim it.
    #[test]
    fn an_armed_request_with_no_project_is_held_and_says_so() {
        let mut conn = seeded();
        let out = intake(
            &mut conn,
            &IntakeRequest {
                text: "buy milk".into(),
                id: None,
                project: None,
                arm: true,
                tier: None,
                executor: None,
                priority: None,
                status: None,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.queued.is_none());
        assert!(out.held.is_some(), "an armed request that did not run must say why");
    }

    /// The default tier is the only one that provably cannot mutate a repo.
    ///
    /// This is what keeps the `Scope::Board` reversal narrow: the phone may
    /// start an agent, but not a writing one unless it says so.
    #[test]
    fn the_default_tier_is_read_only() {
        let mut conn = seeded();
        let out = intake(
            &mut conn,
            &IntakeRequest {
                text: "look at the pocketbuddy paywall".into(),
                id: None,
                project: None,
                arm: false,
                tier: None,
                executor: None,
                priority: None,
                status: None,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!out.task.bg_writes_allowed);
        assert!(!out.task.bg_shell_allowed);
        assert_eq!(ToolTier::for_task(&out.task), ToolTier::ReadOnly);
    }

    /// A long capture keeps its whole text as the brief, and clips only the
    /// title.
    #[test]
    fn a_long_capture_keeps_its_full_text_as_the_prompt() {
        let mut conn = seeded();
        let long = "rerun the seo report for globex and then compare it against \
                    the baseline audit from last quarter, flagging anything \
                    that moved more than ten places";
        let out = intake(
            &mut conn,
            &IntakeRequest {
                text: long.into(),
                id: None,
                project: None,
                arm: false,
                tier: None,
                executor: None,
                priority: None,
                status: None,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.task.title.chars().count() <= TITLE_CHARS + 1, "title is clipped");
        assert_eq!(out.task.prompt.as_deref(), Some(long), "the brief is not clipped");
        assert!(out.task.body.is_some(), "the full text is readable on the task");
    }

    /// A phone replaying a timed-out capture must not create a second task.
    #[test]
    fn a_replayed_capture_is_idempotent_on_its_client_id() {
        let mut conn = seeded();
        let req = IntakeRequest {
            text: "fix the pocketbuddy paywall".into(),
            id: Some("client-minted-1".into()),
            project: None,
            arm: false,
            tier: None,
            executor: None,
            priority: None,
            status: None,
            ..Default::default()
        };
        let first = intake(&mut conn, &req).unwrap();
        let second = intake(&mut conn, &req).unwrap();
        assert_eq!(first.task.id, second.task.id);
    }

    // ── repos as destinations, and everything that hangs off that ─────────

    const TELLY: &str = "/Users/chris/Projects/telly";

    /// `seeded()` plus the shape that broke: repos no project owns.
    ///
    /// `telly` is the phone sentence that started this — three tasks, all with a
    /// repo and none with a project. `globex-course-studio` is a folder whose name
    /// is made of generic words. The home-directory task is what `T222` really
    /// is: a repo of `$HOME`, which decided nothing.
    fn seeded_with_repos() -> Connection {
        let conn = seeded();
        let repo_task = |title: &str, repo: &str| {
            db::tasks::create(
                &conn,
                &NewTask { title: title.into(), repo_path: Some(repo.into()), ..Default::default() },
            )
            .unwrap();
        };
        repo_task("Build Connect Discord in Telly, a one-click Discord link", TELLY);
        repo_task("Telly: plexbot alert when a new episode lands", TELLY);
        repo_task("Telly timeline shows premiere dates to plexbot users", TELLY);
        repo_task("Add Library text and PDFs to the course studio", "/Users/chris/Projects/globex-course-studio");
        repo_task("disk-watch: weekly disk-creep tripwire", "/Users/chris");
        conn
    }

    fn route_of(conn: &Connection, text: &str) -> Routing {
        let projects = db::projects::list(conn, false).unwrap();
        route(conn, text, &projects).unwrap()
    }

    /// The sentence from the phone, verbatim in spirit.
    ///
    /// Before: no candidate was right — the history vote went to four unrelated
    /// projects because `telly` was not one. Now the repo is a destination.
    #[test]
    fn a_repo_no_project_owns_is_routed_by_its_name() {
        let conn = seeded_with_repos();
        let r = route_of(&conn, "I need to make it where mobile zoom is locked on telly");
        assert_eq!(r.pick, None, "a bare repo is not a project");
        assert_eq!(r.repo.as_deref(), Some(TELLY));
        assert_eq!(r.basis, Some(Basis::Name));
    }

    /// **The regression this design exists to prevent.** On the live database
    /// "rerun the seo report for globex" fell from CXVentures 23.1 to 18.1 once
    /// tasks living in a bare repo voted in the same window as the project's
    /// own, dropped below the share bar, and the router's flagship sentence
    /// began asking. A folder that talks about the same client must not be able
    /// to do that.
    #[test]
    fn a_bare_repos_tasks_do_not_dilute_a_projects_history_vote() {
        let conn = seeded_with_repos();
        for title in [
            "Globex learning hub: SEO report for the course pages",
            "Globex course studio: rerun the report on lesson uploads",
            "Send Dana the Globex course studio report",
            "Globex video SEO report for the academy videos",
        ] {
            db::tasks::create(
                &conn,
                &NewTask {
                    title: title.into(),
                    repo_path: Some("/Users/chris/Projects/globex-course-studio".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let r = route_of(&conn, "rerun seo report for globex");
        assert_eq!(r.pick.as_deref(), Some("CXVentures"), "{r:?}");
        assert_eq!(r.repo, None);
        // The folder is still offered — one tap — it just does not get the pick.
        assert!(r.candidates.iter().any(|c| c.kind == DestKind::Repo), "{:?}", r.candidates);
    }

    /// ...and a folder that DOES clearly outweigh every project still wins.
    #[test]
    fn a_repo_that_clearly_outweighs_every_project_still_wins() {
        let conn = seeded_with_repos();
        let r = route_of(&conn, "connect discord for plexbot alerts");
        assert_eq!(r.repo.as_deref(), Some(TELLY));
    }

    #[test]
    fn a_repo_routes_by_history_when_the_name_is_absent() {
        let conn = seeded_with_repos();
        let r = route_of(&conn, "connect discord for plexbot alerts");
        assert_eq!(r.repo.as_deref(), Some(TELLY));
        assert_eq!(r.basis, Some(Basis::History));
    }

    /// The folder name is the WHOLE name, not any word of it.
    #[test]
    fn one_generic_word_of_a_folder_name_does_not_route() {
        let conn = seeded_with_repos();
        assert!(!route_of(&conn, "studio session for the new beat").placed());
        assert!(!route_of(&conn, "plan the course outline").placed());
        // ...but the whole name does, hyphens and all.
        let r = route_of(&conn, "fix globex course studio uploads");
        assert_eq!(r.repo.as_deref(), Some("/Users/chris/Projects/globex-course-studio"));
    }

    /// `$HOME` is not a place work goes. Invariant 12, in the router.
    #[test]
    fn a_home_directory_is_never_a_destination() {
        let conn = seeded_with_repos();
        let repos = bare_repos(&conn).unwrap();
        assert!(repos.iter().all(|r| is_workspace(&r.path)), "{repos:?}");
        assert!(!repos.iter().any(|r| r.path == "/Users/chris"));
        for home in ["/", "/Users/chris", "/home/nas", "/tmp"] {
            assert!(!is_workspace(home), "{home}");
        }
    }

    /// A repo a project owns is reached through the project, once.
    #[test]
    fn a_repo_a_project_owns_is_not_offered_twice() {
        let conn = seeded_with_repos();
        db::projects::add_repo(&conn, "TellyCo", TELLY, true).unwrap();
        assert!(!bare_repos(&conn).unwrap().iter().any(|r| r.path == TELLY));
    }

    /// A task with a repo and no project votes for whoever owns the repo.
    #[test]
    fn a_task_with_only_a_repo_votes_for_the_project_that_owns_it() {
        let conn = seeded_with_repos();
        db::projects::add_repo(&conn, "TellyCo", TELLY, true).unwrap();
        let r = route_of(&conn, "connect discord for plexbot alerts");
        assert_eq!(r.pick.as_deref(), Some("TellyCo"));
        assert_eq!(r.repo, None);
    }

    fn req(text: &str) -> IntakeRequest {
        IntakeRequest { text: text.into(), ..Default::default() }
    }

    /// Filing against a bare repo gives the task that repo, and no project.
    #[test]
    fn a_task_filed_to_a_bare_repo_gets_the_repo_and_no_project() {
        let mut conn = seeded_with_repos();
        let out = intake(&mut conn, &req("lock mobile zoom on telly")).unwrap();
        assert_eq!(out.task.repo_path.as_deref(), Some(TELLY));
        assert_eq!(out.task.project, None);
        assert!(out.routing.placed());
        assert!(out.held.is_none());
    }

    /// The dead chip: re-filing a held task's id with a project must PLACE it.
    ///
    /// `create` is idempotent on the id, so before this the tap returned the old
    /// unplaced row untouched and the queue step then failed for want of a repo.
    #[test]
    fn a_chip_tap_places_the_task_it_answers() {
        let mut conn = seeded_with_repos();
        db::projects::add_repo(&conn, "pocketbuddy", "/Users/chris/Projects/finance-app", true).unwrap();
        let first = intake(
            &mut conn,
            &IntakeRequest { id: Some("turn-1".into()), arm: true, ..req("buy milk") },
        )
        .unwrap();
        assert!(first.held.is_some(), "nowhere to go yet");
        assert_eq!(first.task.project, None);

        let second = intake(
            &mut conn,
            &IntakeRequest {
                id: Some("turn-1".into()),
                project: Some("pocketbuddy".into()),
                ..req("buy milk")
            },
        )
        .unwrap();
        assert_eq!(second.task.id, first.task.id, "the same task, not a stray second one");
        assert_eq!(second.task.project.as_deref(), Some("pocketbuddy"));
        assert_eq!(
            second.task.repo_path.as_deref(),
            Some("/Users/chris/Projects/finance-app"),
            "and it inherited the project's repo, which is what lets it run"
        );
    }

    /// The same, answered with a bare repo instead of a project.
    #[test]
    fn a_chip_tap_can_answer_with_a_bare_repo() {
        let mut conn = seeded_with_repos();
        intake(&mut conn, &IntakeRequest { id: Some("t".into()), ..req("buy milk") }).unwrap();
        let out = intake(
            &mut conn,
            &IntakeRequest { id: Some("t".into()), repo_path: Some(TELLY.into()), ..req("buy milk") },
        )
        .unwrap();
        assert_eq!(out.task.repo_path.as_deref(), Some(TELLY));
        assert_eq!(out.task.project, None);
    }

    /// A replay never moves a task that already has a place.
    #[test]
    fn a_replay_does_not_move_a_placed_task() {
        let mut conn = seeded_with_repos();
        let first = intake(
            &mut conn,
            &IntakeRequest { id: Some("x".into()), project: Some("CXMail".into()), ..req("fix it") },
        )
        .unwrap();
        let again = intake(
            &mut conn,
            &IntakeRequest { id: Some("x".into()), project: Some("Ghost".into()), ..req("fix it") },
        )
        .unwrap();
        assert_eq!(again.task.id, first.task.id);
        assert_eq!(again.task.project.as_deref(), Some("CXMail"));
        assert_eq!(again.routing.pick.as_deref(), Some("CXMail"), "reports the row, not the tap");
        assert!(
            db::projects::get_by_name(&conn, "Ghost").unwrap().is_none(),
            "and a tap the replay declined must not have created a project as a side effect"
        );
    }

    /// Replaying an armed request for a run in flight must not re-queue it.
    #[test]
    fn a_replay_of_a_running_task_leaves_it_running() {
        let mut conn = seeded_with_repos();
        let t = db::tasks::create(
            &conn,
            &NewTask {
                id: Some("run-1".into()),
                title: "look at telly".into(),
                repo_path: Some(TELLY.into()),
                status: Some("in_progress".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let out = intake(
            &mut conn,
            &IntakeRequest { id: Some(t.id.clone()), arm: true, ..req("look at telly") },
        )
        .unwrap();
        assert_eq!(out.task.status, "in_progress", "not put back at queued for a second claim");
        assert!(out.queued.is_some(), "the screen follows it as running");
        assert!(out.held.is_none());
    }

    /// The phone can start a project — and it is FOUND by the next sentence.
    #[test]
    fn a_new_project_made_with_a_repo_owns_it_and_is_routed_to_by_name() {
        let mut conn = seeded_with_repos();
        let out = intake(
            &mut conn,
            &IntakeRequest {
                project: Some("Telly".into()),
                repo_path: Some(TELLY.into()),
                ..req("lock mobile zoom")
            },
        )
        .unwrap();
        assert_eq!(out.task.project.as_deref(), Some("Telly"));
        assert_eq!(out.task.repo_path.as_deref(), Some(TELLY));
        let repos = db::projects::list_repos(&conn, "Telly").unwrap();
        assert_eq!(repos.len(), 1);
        assert!(repos[0].is_primary, "the first repo a project gets is its primary");

        // Now a sentence that only names it lands there, through the project.
        let next = intake(&mut conn, &req("telly needs a dark mode")).unwrap();
        assert_eq!(next.task.project.as_deref(), Some("Telly"));
        assert_eq!(next.task.repo_path.as_deref(), Some(TELLY));
        assert_eq!(next.routing.basis, Some(Basis::Name));
    }

    /// A repo has one owner, and refusing must not leave a task behind.
    #[test]
    fn a_repo_another_project_owns_is_refused_and_files_nothing() {
        let mut conn = seeded_with_repos();
        db::projects::add_repo(&conn, "TellyCo", TELLY, true).unwrap();
        let before: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        let err = intake(
            &mut conn,
            &IntakeRequest {
                project: Some("Other".into()),
                repo_path: Some(TELLY.into()),
                ..req("lock mobile zoom")
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("TellyCo"), "names the owner: {err}");
        let after: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn a_relative_repo_path_is_refused() {
        let mut conn = seeded_with_repos();
        let err = intake(
            &mut conn,
            &IntakeRequest { repo_path: Some("telly".into()), ..req("lock mobile zoom") },
        )
        .unwrap_err();
        assert!(err.to_string().contains("absolute"), "{err}");
    }

    // ── answering a question, and continuing a thread ─────────────────────

    /// An ANSWER is read by name, so "telly is the project" finds telly.
    #[test]
    fn a_typed_answer_is_read_by_name() {
        let conn = seeded_with_repos();
        for said in ["Telly", "telly is the project", "it's the telly repo"] {
            let r = resolve(&conn, said).unwrap();
            assert_eq!(r.repo.as_deref(), Some(TELLY), "for {said:?}");
        }
        let r = resolve(&conn, "the pocketbuddy one").unwrap();
        assert_eq!(r.pick.as_deref(), Some("pocketbuddy"));
    }

    /// An answer that names nothing known says so — that is the create-a-project
    /// case, so it must be an empty result rather than an error or a guess.
    #[test]
    fn an_answer_naming_nothing_known_resolves_to_nothing() {
        let conn = seeded_with_repos();
        let r = resolve(&conn, "zebra").unwrap();
        assert!(!r.placed());
        assert!(r.candidates.is_empty());
    }

    /// The answer must not be routed by the history that already failed.
    #[test]
    fn an_answer_does_not_get_a_second_history_vote() {
        let conn = seeded_with_repos();
        // "globex" routes to CXVentures by history for a request; as an ANSWER it
        // names no place, so it must not.
        assert!(!resolve(&conn, "globex").unwrap().placed());
    }

    #[test]
    fn places_lists_projects_and_only_unowned_repos() {
        let conn = seeded_with_repos();
        db::projects::add_repo(&conn, "TellyCo", TELLY, true).unwrap();
        let p = places(&conn).unwrap();
        assert!(p.projects.iter().any(|n| n == "TellyCo"));
        assert!(!p.repos.iter().any(|r| r.path == TELLY), "owned repos come through their project");
        assert!(p.repos.iter().any(|r| r.label == "globex-course-studio"));
    }

    fn task_with_status(conn: &Connection, status: &str) -> Task {
        db::tasks::create(
            conn,
            &NewTask {
                title: "look at telly".into(),
                repo_path: Some(TELLY.into()),
                status: Some(status.into()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    /// A reply is a comment from Chris on the SAME task, not a new one.
    #[test]
    fn a_follow_up_is_a_comment_on_the_same_task() {
        let mut conn = seeded_with_repos();
        let t = task_with_status(&conn, "in_review");
        let before: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        let out = intake(
            &mut conn,
            &IntakeRequest { follow_up_of: Some(t.id.clone()), ..req("also check the zoom on iPad") },
        )
        .unwrap();
        let after: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap();
        assert_eq!(before, after, "no new task");
        assert_eq!(out.task.id, t.id);
        assert_eq!(out.routing.basis, Some(Basis::Thread));
        let comments = db::comments::for_task(&conn, &t.id).unwrap();
        assert_eq!(comments.last().unwrap().author, db::comments::AUTHOR_ME);
        assert_eq!(comments.last().unwrap().body, "also check the zoom on iPad");
    }

    /// Queueing a task that is already running would let a second claim win.
    #[test]
    fn a_follow_up_on_a_running_task_comments_and_does_not_requeue() {
        let mut conn = seeded_with_repos();
        for status in ["queued", "in_progress"] {
            let t = task_with_status(&conn, status);
            let out = intake(
                &mut conn,
                &IntakeRequest {
                    follow_up_of: Some(t.id.clone()),
                    arm: true,
                    ..req("and the dark theme")
                },
            )
            .unwrap();
            assert_eq!(out.task.status, status, "status untouched");
            assert!(out.queued.is_none());
            assert!(out.held.as_deref().unwrap().contains("still working"));
            assert_eq!(db::comments::for_task(&conn, &t.id).unwrap().len(), 1, "but it was heard");
        }
    }

    /// The tier control speaks for the reply, not for the first request.
    #[test]
    fn a_follow_ups_tier_replaces_the_one_the_task_was_filed_at() {
        let mut conn = seeded_with_repos();
        let t = task_with_status(&conn, "in_review");
        let out = intake(
            &mut conn,
            &IntakeRequest {
                follow_up_of: Some(t.id.clone()),
                tier: Some(ToolTier::Write),
                ..req("go ahead and fix it")
            },
        )
        .unwrap();
        assert!(out.task.bg_writes_allowed);
        let out = intake(
            &mut conn,
            &IntakeRequest {
                follow_up_of: Some(t.id.clone()),
                tier: Some(ToolTier::ReadOnly),
                ..req("actually just look")
            },
        )
        .unwrap();
        assert!(!out.task.bg_writes_allowed && !out.task.bg_shell_allowed);
    }

    #[test]
    fn a_follow_up_on_a_missing_task_is_not_found() {
        let mut conn = seeded_with_repos();
        let err = intake(
            &mut conn,
            &IntakeRequest { follow_up_of: Some("nope".into()), ..req("hello") },
        )
        .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)), "{err}");
    }
}
