//! Optional TypeSafe/Jev judgments for task capture, readiness, review and recall.
//!
//! Jev is deliberately kept on the fuzzy side of the boundary. It may suggest
//! a project or judge whether a brief is clear; it never computes dates,
//! changes permissions, resolves dependencies, or decides whether a host may
//! run a task. Those remain ordinary Rust and SQL.

use crate::db::{self, tasks::Task};
use crate::error::AppError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Write;

const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const DEFAULT_MODEL: &str = "jev-1.13.0";
const DECISIONS_ENDPOINT: &str = "https://api.openai.com/v1/decisions";
const DECISIONS_MODEL: &str = "gpt-6-luna";

const KEY_ENABLED: &str = "jev_enabled";
const KEY_MODEL: &str = "jev_model";
const KEY_CAPTURE: &str = "jev_capture";
const KEY_READINESS: &str = "jev_readiness";
const KEY_REVIEW: &str = "jev_review";
const KEY_RECALL: &str = "jev_recall";
const KEY_CAPTURE_ENGINE: &str = "capture_engine";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub enabled: bool,
    pub model: String,
    pub capture: bool,
    pub readiness: bool,
    pub review: bool,
    pub recall: bool,
    /// Who answers capture. Absent from an older client reads as the default.
    #[serde(default)]
    pub capture_engine: CaptureEngine,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            model: DEFAULT_MODEL.into(),
            capture: true,
            readiness: true,
            review: true,
            recall: true,
            capture_engine: CaptureEngine::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub settings: Settings,
    pub configured: bool,
    pub credential_source: Option<String>,
    /// The OpenAI key capture's Decisions engine uses.
    #[serde(default)]
    pub decisions_configured: bool,
    #[serde(default)]
    pub decisions_credential_source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveSettings {
    #[serde(flatten)]
    pub settings: Settings,
    /// Absent keeps the credential; blank removes the local credential file.
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureRequest {
    pub text: String,
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureAssessment {
    pub project: Option<String>,
    pub priority: i64,
    pub status: String,
    pub tags: Vec<String>,
    pub needs_clarification: f64,
    pub confidence: f64,
    /// `needs_clarification` read against the answering engine's own bar —
    /// the two engines are not calibrated alike, so callers must not compare
    /// the raw number to a constant of their own.
    #[serde(default)]
    pub too_vague: bool,
    /// Which engine answered. A Decisions failure that fell back says `jev`.
    #[serde(default)]
    pub engine: Option<CaptureEngine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessAssessment {
    pub label: String,
    pub clear_outcome: f64,
    pub enough_context: f64,
    pub unresolved_decision: f64,
    pub bounded: f64,
    pub should_block: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReview {
    pub addresses_request: f64,
    pub verification_present: f64,
    pub unresolved_questions: f64,
    pub needs_attention: bool,
    pub note: String,
}

fn bool_meta(conn: &Connection, key: &str, default: bool) -> bool {
    db::meta::get(conn, key)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<bool>().ok())
        .unwrap_or(default)
}

pub fn settings(conn: &Connection) -> Settings {
    let defaults = Settings::default();
    Settings {
        enabled: bool_meta(conn, KEY_ENABLED, defaults.enabled),
        model: db::meta::get(conn, KEY_MODEL)
            .ok()
            .flatten()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(defaults.model),
        capture: bool_meta(conn, KEY_CAPTURE, defaults.capture),
        readiness: bool_meta(conn, KEY_READINESS, defaults.readiness),
        review: bool_meta(conn, KEY_REVIEW, defaults.review),
        recall: bool_meta(conn, KEY_RECALL, defaults.recall),
        capture_engine: db::meta::get(conn, KEY_CAPTURE_ENGINE)
            .ok()
            .flatten()
            .and_then(|v| CaptureEngine::parse(&v))
            .unwrap_or(defaults.capture_engine),
    }
}

/// The secrets-kit Keychain service every `cx` tool stores under.
const KEYCHAIN_SERVICE: &str = "cx-secret";
const KEYCHAIN_ACCOUNT: &str = "TYPESAFE_API_KEY";
const OPENAI_KEYCHAIN_ACCOUNT: &str = "OPENAI_API_KEY";

/// Read the key straight out of the login Keychain.
///
/// # Why this exists, when a local file already worked
///
/// Because the file has to be CREATED, and creating it means moving the secret
/// through a shell — which is the one thing the house rules forbid, and which
/// an agent is mechanically blocked from doing. So the file could only ever be
/// written by hand, which made "turn Jev on" a manual step forever.
///
/// zen-mcp solved the same problem the same way (`server/src/secrets.ts`,
/// `resolveSecret`): the CONSUMING PROCESS reads the Keychain itself, so the
/// value never touches a shell, a file, or a transcript. This is that, in Rust.
///
/// macOS only, and that matters structurally: `cxtasks-core` builds for Linux
/// because the runner and the canonical server run in a container, where
/// `security` does not exist and there is no login Keychain to read. Those
/// hosts keep using the env var or the file.
#[cfg(target_os = "macos")]
fn from_keychain(account: &str) -> Option<String> {
    let out = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            account,
            "-w",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        // Not an error worth surfacing: "no such secret" is the ordinary state
        // on a machine where Jev was never set up.
        return None;
    }
    let key = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!key.is_empty()).then_some(key)
}

#[cfg(not(target_os = "macos"))]
fn from_keychain(_account: &str) -> Option<String> {
    None
}

/// Where the key comes from, in order of how deliberate each source is.
///
/// Env first (a wrapper like `secret run` was used on purpose for this
/// process), then the local file (somebody typed it into Settings), then the
/// Keychain (the house default, shared with every other `cx` tool). The
/// explicit sources outrank the ambient one so a deliberately different key
/// cannot be silently overridden by the shared one.
fn credential() -> Option<(String, &'static str)> {
    if let Ok(key) = std::env::var("TYPESAFE_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Some((key, "environment"));
        }
    }
    if let Some(key) = std::fs::read_to_string(crate::paths::jev_api_key_path())
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        return Some((key, "local file"));
    }
    from_keychain(KEYCHAIN_ACCOUNT).map(|v| (v, "login Keychain"))
}

/// The OpenAI key, read the same way as TypeSafe's minus the local file:
/// nothing in Settings writes one, so there is nothing deliberate to honour.
fn decisions_credential() -> Option<(String, &'static str)> {
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Some((key, "environment"));
        }
    }
    from_keychain(OPENAI_KEYCHAIN_ACCOUNT).map(|v| (v, "login Keychain"))
}

pub fn status(conn: &Connection) -> Status {
    let source = credential().map(|(_, source)| source.to_string());
    let decisions = decisions_credential().map(|(_, source)| source.to_string());
    Status {
        settings: settings(conn),
        configured: source.is_some(),
        credential_source: source,
        decisions_configured: decisions.is_some(),
        decisions_credential_source: decisions,
    }
}

pub fn save_settings(conn: &Connection, value: &SaveSettings) -> Result<Status, AppError> {
    let model = value.settings.model.trim();
    if model.is_empty() {
        return Err(AppError::Validation("Jev model must not be empty".into()));
    }
    for (key, enabled) in [
        (KEY_ENABLED, value.settings.enabled),
        (KEY_CAPTURE, value.settings.capture),
        (KEY_READINESS, value.settings.readiness),
        (KEY_REVIEW, value.settings.review),
        (KEY_RECALL, value.settings.recall),
    ] {
        db::meta::set(conn, key, if enabled { "true" } else { "false" })?;
    }
    db::meta::set(conn, KEY_MODEL, model)?;
    db::meta::set(conn, KEY_CAPTURE_ENGINE, value.settings.capture_engine.as_str())?;
    if let Some(key) = value.api_key.as_deref() {
        save_credential(key)?;
    }
    Ok(status(conn))
}

fn save_credential(raw: &str) -> Result<(), AppError> {
    let path = crate::paths::jev_api_key_path();
    if raw.trim().is_empty() {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(raw.trim().as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

fn call(conn: &Connection, state: Value, questions: Value) -> Result<Value, AppError> {
    let cfg = settings(conn);
    if !cfg.enabled {
        return Err(AppError::Validation("Jev is disabled in Settings".into()));
    }
    let (key, _) = credential().ok_or_else(|| {
        AppError::Validation(
            "Jev has no TypeSafe API key; add one in Settings or TYPESAFE_API_KEY".into(),
        )
    })?;
    let response = ureq::post(ENDPOINT)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(12))
        .send_json(json!({"model": cfg.model, "state": state, "questions": questions}))
        .map_err(|e| AppError::General(format!("Jev request failed: {e}")))?;
    response
        .into_json::<Value>()
        .map_err(|e| AppError::General(format!("Jev returned unreadable JSON: {e}")))
}

fn answers(value: &Value) -> Result<&Map<String, Value>, AppError> {
    value
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| AppError::General("Jev response did not contain answers".into()))
}

fn noul(map: &Map<String, Value>, key: &str) -> Result<f64, AppError> {
    map.get(key)
        .and_then(|v| v.get("noul"))
        .and_then(Value::as_f64)
        .ok_or_else(|| AppError::General(format!("Jev answer '{key}' was not a noul")))
}

fn score(map: &Map<String, Value>, key: &str) -> Result<f64, AppError> {
    map.get(key)
        .and_then(|v| v.get("score"))
        .and_then(Value::as_f64)
        .ok_or_else(|| AppError::General(format!("Jev answer '{key}' was not a score")))
}

/// Which model answers the capture questions.
///
/// Capture only. Readiness, review, `jev:` tripwires and reranking stay on Jev:
/// capture is the one judgment measured against both (2026-10-06, 270 tasks,
/// `examples/jev_vs_decisions.rs`), and the others have no labels to measure on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureEngine {
    /// OpenAI's Decisions API. The default: on the measurement it matched Jev
    /// on project, beat it on priority, and it carries OpenAI's API terms
    /// (30-day abuse logs, no stored state) where TypeSafe states no
    /// deletion window.
    #[default]
    Decisions,
    Jev,
}

impl CaptureEngine {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "decisions" => Some(Self::Decisions),
            "jev" => Some(Self::Jev),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Decisions => "decisions",
            Self::Jev => "jev",
        }
    }
}

/// The bars one engine's capture answers are read against.
///
/// Per engine because the two are not calibrated alike: Decisions' predicates
/// run hot, so at Jev's bars it asserted 2.7x as many tags and called 8% of
/// ordinary tasks too vague. A bar moved for one must not move for the other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptureBars {
    /// A project pick below this confidence is no pick.
    pub project: f64,
    pub tag: f64,
    /// `todo` needs a clarification probability BELOW this.
    pub todo_clarify: f64,
    /// At or above this, the brief is too vague to hand an unattended agent.
    pub too_vague: f64,
}

/// Jev's bars, unchanged.
///
/// `too_vague` began at 0.65, borrowed from `todo_clarify`. Measured against the
/// live model on 2026-09-20 that is far too low for THIS question — Jev rates
/// ordinary, perfectly actionable requests as quite vague:
///
/// | text | `needs_clarification` |
/// |---|---|
/// | `fix the login bug in pocketbuddy before friday` | 0.83 |
/// | `rerun the seo report for globex` | 0.79 |
/// | `buy milk` | 0.52 |
/// | `do the thing` | **0.94** |
///
/// Only genuine non-briefs separate, so the bar sits above them. The cost of
/// being wrong is one advisory line: nothing refuses on it.
pub const JEV_BARS: CaptureBars =
    CaptureBars { project: 0.55, tag: 0.72, todo_clarify: 0.65, too_vague: 0.90 };

/// Decisions' bars, tuned 2026-10-06 on half of 270 labeled tasks and checked
/// on the other half (split by a hash of the task id).
///
/// - `project` 0.70: the lowest bar keeping precision at or above Jev's on the
///   tuning half (70.4%), and it brings unlabeled tasks wrongly given a
///   project down to Jev's rate. Held out: 81.0% precision at 48.3% coverage
///   (Jev: 73.9% at 57.5%) — more precise, some coverage traded for it.
/// - `tag` 0.96: 1.01 tags asserted per task on the tuning half against Jev's
///   0.99; held out, 0.98 for both. At 0.72 it was 2.6.
/// - `too_vague` OFF (a bar no probability reaches). Decisions'
///   clarification answer does not separate non-briefs when asked among the
///   capture questions: "do the thing" 0.98, "stuff" 0.42, while "fix the
///   pocketbuddy budget sync bug" reads 1.00 and "fix the login bug in
///   pocketbuddy before friday" 0.98 (2026-10-06, repeatable). Every bar that
///   would catch the first catches the actionable ones too, so the advisory is
///   not raised from it at all rather than raised at random. Held out it
///   flags 0%, as Jev's 0.90 does; Jev, the fallback, still flags at its bar.
/// - `todo_clarify` keeps Jev's 0.65; it was not retuned.
pub const DECISIONS_BARS: CaptureBars =
    CaptureBars { project: 0.70, tag: 0.96, todo_clarify: 0.65, too_vague: f64::INFINITY };

impl CaptureEngine {
    pub fn bars(self) -> CaptureBars {
        match self {
            Self::Decisions => DECISIONS_BARS,
            Self::Jev => JEV_BARS,
        }
    }
}

const PROJECT_INSTRUCTIONS: &str =
    "Which existing project best fits this task? Choose none when the fit is unclear.";
const PRIORITY_INSTRUCTIONS: &str =
    "Choose the task priority. P0 is urgent and important; P3 is ordinary backlog work.";
const STATUS_INSTRUCTIONS: &str =
    "Is this only captured for later triage or is it already actionable?";
const CLARIFY_INSTRUCTIONS: &str =
    "The task is too ambiguous to act on without asking a clarifying question.";

fn tag_instructions(tag: &str) -> String {
    format!("The tag '{tag}' meaningfully applies to this task.")
}

/// The choice sets, keyed exactly as both engines are asked them.
fn capture_choices(request: &CaptureRequest) -> [(&'static str, &'static str, Map<String, Value>); 3] {
    let mut project = Map::new();
    project.insert("none".into(), json!("No listed project clearly applies"));
    for (i, p) in request.projects.iter().take(40).enumerate() {
        project.insert(format!("p{i}"), json!(p));
    }
    let as_map = |v: Value| v.as_object().cloned().unwrap_or_default();
    [
        ("project", PROJECT_INSTRUCTIONS, project),
        (
            "priority",
            PRIORITY_INSTRUCTIONS,
            as_map(json!({"p0":"Immediate critical work","p1":"High priority","p2":"Normal planned work","p3":"Low priority or backlog"})),
        ),
        (
            "status",
            STATUS_INSTRUCTIONS,
            as_map(json!({"inbox":"Needs later triage","todo":"Actionable now"})),
        ),
    ]
}

fn capture_tags(request: &CaptureRequest) -> impl Iterator<Item = (usize, &String)> {
    request.tags.iter().take(20).enumerate()
}

fn jev_capture_questions(request: &CaptureRequest) -> Value {
    let mut qs = Map::new();
    for (name, instructions, criteria) in capture_choices(request) {
        qs.insert(name.into(), json!({"type":"choice", "instructions":instructions, "criteria":criteria}));
    }
    qs.insert(
        "needs_clarification".into(),
        json!({"type":"noul", "instructions":CLARIFY_INSTRUCTIONS}),
    );
    for (i, tag) in capture_tags(request) {
        qs.insert(format!("tag_{i}"), json!({"type":"noul", "instructions":tag_instructions(tag)}));
    }
    Value::Object(qs)
}

/// The same questions in Decisions' spelling: `choice` keeps its keys as
/// `value`s, `noul` becomes `predicate`.
fn decisions_capture_questions(request: &CaptureRequest) -> Value {
    let mut qs: Vec<Value> = capture_choices(request)
        .into_iter()
        .map(|(name, instructions, criteria)| {
            let choices: Vec<Value> = criteria
                .into_iter()
                .map(|(value, description)| json!({"value":value, "description":description}))
                .collect();
            json!({"type":"choice", "name":name, "instructions":instructions, "choices":choices})
        })
        .collect();
    qs.push(json!({"type":"predicate", "name":"needs_clarification", "instructions":CLARIFY_INSTRUCTIONS}));
    for (i, tag) in capture_tags(request) {
        qs.push(json!({"type":"predicate", "name":format!("tag_{i}"), "instructions":tag_instructions(tag)}));
    }
    Value::Array(qs)
}

/// One engine's capture answers, each question independently present or not.
///
/// `None` is "no judgment": the answer was a refusal, missing, or not the shape
/// asked for. One bad answer must cost that one question, never the capture.
#[derive(Debug, Default, Clone, PartialEq)]
struct CaptureAnswers {
    project: Option<(String, f64)>,
    priority: Option<(String, f64)>,
    status: Option<(String, f64)>,
    needs_clarification: Option<f64>,
    tags: Vec<Option<f64>>,
}

/// A choice answer is `{"choice": key, "confidence": x}` in both APIs. A key
/// outside the offered set is malformed, and so is no judgment.
fn choice_of(answer: Option<&Value>, offered: &Map<String, Value>) -> Option<(String, f64)> {
    let answer = answer?;
    let key = answer.get("choice")?.as_str()?;
    offered.contains_key(key).then(|| {
        (key.to_string(), answer.get("confidence").and_then(Value::as_f64).unwrap_or(0.0))
    })
}

fn probability(answer: Option<&Value>, field: &str) -> Option<f64> {
    answer?.get(field)?.as_f64().filter(|p| (0.0..=1.0).contains(p))
}

/// Jev answers are an object keyed by question; a noul carries `noul`.
fn parse_jev_capture(response: &Value, request: &CaptureRequest) -> Result<CaptureAnswers, AppError> {
    let a = answers(response)?;
    let [project, priority, status] = capture_choices(request);
    Ok(CaptureAnswers {
        project: choice_of(a.get(project.0), &project.2),
        priority: choice_of(a.get(priority.0), &priority.2),
        status: choice_of(a.get(status.0), &status.2),
        needs_clarification: probability(a.get("needs_clarification"), "noul"),
        tags: capture_tags(request)
            .map(|(i, _)| probability(a.get(&format!("tag_{i}")), "noul"))
            .collect(),
    })
}

/// Decisions answers are an array carrying `name`; a predicate carries
/// `probability`, and a refusal is `{"type":"refusal"}` with neither.
fn parse_decisions_capture(
    response: &Value,
    request: &CaptureRequest,
) -> Result<CaptureAnswers, AppError> {
    let list = response
        .get("answers")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::General("Decisions response did not contain answers".into()))?;
    let by_name: HashMap<&str, &Value> = list
        .iter()
        .filter(|a| a.get("type").and_then(Value::as_str) != Some("refusal"))
        .filter_map(|a| Some((a.get("name")?.as_str()?, a)))
        .collect();
    let get = |name: &str| by_name.get(name).copied();
    let [project, priority, status] = capture_choices(request);
    Ok(CaptureAnswers {
        project: choice_of(get(project.0), &project.2),
        priority: choice_of(get(priority.0), &priority.2),
        status: choice_of(get(status.0), &status.2),
        needs_clarification: probability(get("needs_clarification"), "probability"),
        tags: capture_tags(request)
            .map(|(i, _)| probability(get(&format!("tag_{i}")), "probability"))
            .collect(),
    })
}

/// Read answers against an engine's bars. Every "no judgment" lands on the
/// value code would have chosen alone: no project, P3 (the create default),
/// `inbox`, no tag, not vague.
fn read_capture(
    request: &CaptureRequest,
    answers: &CaptureAnswers,
    engine: CaptureEngine,
) -> CaptureAssessment {
    let bars = engine.bars();
    let project = answers
        .project
        .as_ref()
        .filter(|(_, confidence)| *confidence >= bars.project)
        .and_then(|(key, _)| key.strip_prefix('p')?.parse::<usize>().ok())
        .and_then(|i| request.projects.get(i).cloned());
    let priority = answers
        .priority
        .as_ref()
        .and_then(|(key, _)| key.strip_prefix('p')?.parse().ok())
        .unwrap_or(db::tasks::PRIORITY_LOWEST);
    // An unanswered clarification question cannot vouch for `todo`.
    let clear = answers.needs_clarification.is_some_and(|p| p < bars.todo_clarify);
    let todo = answers.status.as_ref().is_some_and(|(key, _)| key == "todo") && clear;
    let tags = capture_tags(request)
        .filter(|(i, _)| answers.tags.get(*i).copied().flatten().is_some_and(|p| p >= bars.tag))
        .map(|(_, tag)| tag.clone())
        .collect();
    let needs_clarification = answers.needs_clarification.unwrap_or(0.0);
    let confidence = [&answers.project, &answers.priority, &answers.status]
        .into_iter()
        .flatten()
        .map(|(_, c)| *c)
        .reduce(f64::min)
        .unwrap_or(0.0);
    CaptureAssessment {
        project,
        priority,
        status: if todo { "todo" } else { "inbox" }.into(),
        tags,
        needs_clarification,
        confidence,
        too_vague: needs_clarification >= bars.too_vague,
        engine: Some(engine),
    }
}

fn decisions_call(request: &CaptureRequest) -> Result<Value, AppError> {
    let (key, _) = decisions_credential().ok_or_else(|| {
        AppError::Validation("Decisions has no OpenAI API key (OPENAI_API_KEY)".into())
    })?;
    let input = json!({"task": request.text}).to_string();
    let response = ureq::post(DECISIONS_ENDPOINT)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        // p95 was 0.2 s when measured. A slow answer is a dead one here, and
        // Jev still has to fit behind it before the capture falls to code.
        .timeout(std::time::Duration::from_secs(6))
        .send_json(json!({
            "model": DECISIONS_MODEL,
            "input": input,
            "questions": decisions_capture_questions(request),
        }))
        .map_err(|e| AppError::General(format!("Decisions request failed: {e}")))?;
    response
        .into_json::<Value>()
        .map_err(|e| AppError::General(format!("Decisions returned unreadable JSON: {e}")))
}

fn assess_capture_with(
    conn: &Connection,
    request: &CaptureRequest,
    engine: CaptureEngine,
) -> Result<CaptureAssessment, AppError> {
    let answers = match engine {
        CaptureEngine::Decisions => parse_decisions_capture(&decisions_call(request)?, request)?,
        CaptureEngine::Jev => parse_jev_capture(
            &call(conn, json!({"task": request.text}), jev_capture_questions(request))?,
            request,
        )?,
    };
    Ok(read_capture(request, &answers, engine))
}

/// Capture's judgment, from the configured engine.
///
/// Decisions falls back to Jev, and an `Err` from here falls back to code in
/// every caller — capture must never break on a provider.
pub fn assess_capture(
    conn: &Connection,
    request: &CaptureRequest,
) -> Result<Option<CaptureAssessment>, AppError> {
    let cfg = settings(conn);
    if !cfg.enabled || !cfg.capture {
        return Ok(None);
    }
    match cfg.capture_engine {
        CaptureEngine::Jev => assess_capture_with(conn, request, CaptureEngine::Jev).map(Some),
        CaptureEngine::Decisions => match assess_capture_with(conn, request, CaptureEngine::Decisions) {
            Ok(a) => Ok(Some(a)),
            Err(e) => {
                log::warn!("capture: Decisions unavailable, trying Jev: {e}");
                assess_capture_with(conn, request, CaptureEngine::Jev).map(Some)
            }
        },
    }
}

fn task_state(conn: &Connection, task: &Task) -> Value {
    let comments = db::comments::for_task(conn, &task.id)
        .unwrap_or_default()
        .into_iter()
        .rev()
        .take(8)
        .map(|c| json!({"author":c.author,"body":c.body}))
        .collect::<Vec<_>>();
    json!({
        "title": task.title,
        "body": task.body,
        "project": task.project,
        "prompt": task.prompt,
        "workspace": task.repo_path,
        "recent_comments": comments
    })
}

pub fn assess_readiness(
    conn: &Connection,
    task: &Task,
) -> Result<Option<ReadinessAssessment>, AppError> {
    let cfg = settings(conn);
    if !cfg.enabled || !cfg.readiness {
        return Ok(None);
    }
    let response = call(
        conn,
        task_state(conn, task),
        json!({
            "clear_outcome":{"type":"noul","instructions":"The requested end result is concrete enough for a coding agent to know when it is finished."},
            "enough_context":{"type":"noul","instructions":"The supplied task state contains enough context to begin without first asking the user a question."},
            "unresolved_decision":{"type":"noul","instructions":"A material product or implementation decision must be made by the user before work can safely begin."},
            "bounded":{"type":"noul","instructions":"This is one bounded unit of work rather than several independent projects."}
        }),
    )?;
    let a = answers(&response)?;
    let clear = noul(a, "clear_outcome")?;
    let context = noul(a, "enough_context")?;
    let decision = noul(a, "unresolved_decision")?;
    let bounded = noul(a, "bounded")?;
    let mut reasons = Vec::new();
    if clear < 0.65 {
        reasons.push("the expected outcome is unclear");
    }
    if context < 0.60 {
        reasons.push("the task is missing context");
    }
    if decision > 0.65 {
        reasons.push("a user decision is still open");
    }
    if bounded < 0.60 {
        reasons.push("the task appears to contain several pieces of work");
    }
    let should_block = !reasons.is_empty();
    Ok(Some(ReadinessAssessment {
        label: if should_block {
            "needs_context"
        } else {
            "ready"
        }
        .into(),
        clear_outcome: clear,
        enough_context: context,
        unresolved_decision: decision,
        bounded,
        should_block,
        note: if should_block {
            reasons.join("; ")
        } else {
            "The task looks ready to delegate.".into()
        },
    }))
}

pub fn review_run(
    conn: &Connection,
    task: &Task,
    handoff: &str,
) -> Result<Option<RunReview>, AppError> {
    let cfg = settings(conn);
    if !cfg.enabled || !cfg.review || handoff.trim().is_empty() {
        return Ok(None);
    }
    let response = call(
        conn,
        json!({"task":task_state(conn, task),"handoff":handoff}),
        json!({
            "addresses_request":{"type":"noul","instructions":"The handoff directly addresses the task's requested outcome."},
            "verification_present":{"type":"noul","instructions":"The handoff reports concrete verification or explains why verification was not possible."},
            "unresolved_questions":{"type":"noul","instructions":"The handoff leaves a material unresolved question or required follow-up."}
        }),
    )?;
    let a = answers(&response)?;
    let addresses = noul(a, "addresses_request")?;
    let verification = noul(a, "verification_present")?;
    let unresolved = noul(a, "unresolved_questions")?;
    let mut flags = Vec::new();
    if addresses < 0.65 {
        flags.push("the handoff may not address the requested outcome");
    }
    if verification < 0.60 {
        flags.push("verification is missing or unclear");
    }
    if unresolved > 0.65 {
        flags.push("material questions or follow-up remain");
    }
    Ok(Some(RunReview {
        addresses_request: addresses,
        verification_present: verification,
        unresolved_questions: unresolved,
        needs_attention: !flags.is_empty(),
        note: if flags.is_empty() {
            "No review flags.".into()
        } else {
            flags.join("; ")
        },
    }))
}

pub fn condition_holds(conn: &Connection, task: &Task, condition: &str) -> Result<f64, AppError> {
    let response = call(
        conn,
        task_state(conn, task),
        json!({
            "holds":{"type":"noul","instructions":format!("This condition is currently satisfied: {condition}")}
        }),
    )?;
    noul(answers(&response)?, "holds")
}

/// Re-rank an already bounded lexical shortlist. Retrieval remains local and
/// deterministic; Jev only changes the order when recall enhancement is on.
pub fn rerank_tasks(conn: &Connection, query: &str, tasks: &mut [Task]) -> Result<(), AppError> {
    let cfg = settings(conn);
    if !cfg.enabled || !cfg.recall || tasks.len() < 2 {
        return Ok(());
    }
    let state = json!({
        "query": query,
        "candidates": tasks.iter().enumerate().map(|(i,t)| json!({"id":i,"title":t.title,"body":t.body,"project":t.project})).collect::<Vec<_>>()
    });
    let mut questions = Map::new();
    for i in 0..tasks.len() {
        questions.insert(
            format!("candidate_{i}"),
            json!({
                "type":"score",
                "instructions":format!("How relevant is candidates[{i}] to the query?"),
                "criteria":["Unrelated","Weakly related","Relevant","Direct match"]
            }),
        );
    }
    let response = call(conn, state, Value::Object(questions))?;
    let a = answers(&response)?;
    let ranking: HashMap<String, f64> = (0..tasks.len())
        .map(|i| {
            (
                tasks[i].id.clone(),
                score(a, &format!("candidate_{i}")).unwrap_or(0.0),
            )
        })
        .collect();
    tasks.sort_by(|left, right| {
        ranking
            .get(&right.id)
            .unwrap_or(&0.0)
            .partial_cmp(ranking.get(&left.id).unwrap_or(&0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(())
}

/// Put the most useful cited handoffs/comments first inside each retrospective
/// bucket. The evidence itself and its citations are untouched.
pub fn rerank_retrospective(
    conn: &Connection,
    report: &mut db::retrospective::Retrospective,
) -> Result<(), AppError> {
    let cfg = settings(conn);
    if !cfg.enabled || !cfg.recall {
        return Ok(());
    }
    rerank_evidence(conn, &mut report.learned, "useful lesson for future work")?;
    rerank_evidence(
        conn,
        &mut report.ruled_out,
        "useful record of an approach to avoid",
    )?;
    Ok(())
}

fn rerank_evidence(
    conn: &Connection,
    evidence: &mut [db::retrospective::RetrospectiveEvidence],
    purpose: &str,
) -> Result<(), AppError> {
    if evidence.len() < 2 {
        return Ok(());
    }
    let count = evidence.len().min(40);
    let state = json!({
        "purpose": purpose,
        "evidence": evidence.iter().take(count).enumerate().map(|(i,e)| json!({"id":i,"task":e.title,"text":e.text,"source":e.source})).collect::<Vec<_>>()
    });
    let mut questions = Map::new();
    for i in 0..count {
        questions.insert(
            format!("evidence_{i}"),
            json!({
                "type":"score",
                "instructions":format!("How useful is evidence[{i}] for the stated purpose?"),
                "criteria":["Low signal","Some signal","Useful","Highly reusable"]
            }),
        );
    }
    let response = call(conn, state, Value::Object(questions))?;
    let a = answers(&response)?;
    let ranking: HashMap<(String, String, String), f64> = evidence
        .iter()
        .take(count)
        .enumerate()
        .map(|(i, e)| {
            (
                (e.task_id.clone(), e.source.clone(), e.text.clone()),
                score(a, &format!("evidence_{i}")).unwrap_or(0.0),
            )
        })
        .collect();
    evidence.sort_by(|left, right| {
        let right_key = (
            right.task_id.clone(),
            right.source.clone(),
            right.text.clone(),
        );
        let left_key = (left.task_id.clone(), left.source.clone(), left.text.clone());
        ranking
            .get(&right_key)
            .unwrap_or(&0.0)
            .partial_cmp(ranking.get(&left_key).unwrap_or(&0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;

    #[test]
    fn disabled_is_the_safe_default() {
        let conn = test_conn();
        let value = settings(&conn);
        assert!(!value.enabled);
        assert!(value.capture && value.readiness && value.review && value.recall);
        assert_eq!(value.capture_engine, CaptureEngine::Decisions);
    }

    #[test]
    fn local_settings_round_trip_without_a_key() {
        let conn = test_conn();
        let desired = SaveSettings {
            settings: Settings {
                enabled: true,
                model: "jev-test".into(),
                capture: false,
                readiness: true,
                review: false,
                recall: true,
                capture_engine: CaptureEngine::Jev,
            },
            api_key: None,
        };
        save_settings(&conn, &desired).unwrap();
        let got = settings(&conn);
        assert!(got.enabled);
        assert_eq!(got.model, "jev-test");
        assert!(!got.capture && got.readiness && !got.review && got.recall);
        assert_eq!(got.capture_engine, CaptureEngine::Jev);
    }

    fn request() -> CaptureRequest {
        CaptureRequest {
            text: "fix the dmarc record".into(),
            projects: vec!["CXVentures".into(), "pocketbuddy".into()],
            tags: vec!["seo".into(), "security".into()],
        }
    }

    #[test]
    fn both_engines_are_asked_the_same_questions() {
        let r = request();
        let jev = jev_capture_questions(&r);
        let dec = decisions_capture_questions(&r);
        let dec = dec.as_array().unwrap();
        assert_eq!(jev.as_object().unwrap().len(), dec.len());
        for q in dec {
            let name = q["name"].as_str().unwrap();
            let j = &jev[name];
            assert_eq!(j["instructions"], q["instructions"], "{name}");
            match q["type"].as_str().unwrap() {
                "choice" => {
                    let keys: Vec<&str> = q["choices"].as_array().unwrap().iter().map(|c| c["value"].as_str().unwrap()).collect();
                    let jkeys: Vec<&str> = j["criteria"].as_object().unwrap().keys().map(String::as_str).collect();
                    assert_eq!(keys, jkeys, "{name}");
                }
                "predicate" => assert_eq!(j["type"], "noul"),
                other => panic!("unexpected {other}"),
            }
        }
    }

    #[test]
    fn a_decisions_refusal_costs_one_question_not_the_capture() {
        let r = request();
        let response = json!({"answers":[
            {"type":"choice","name":"project","choice":"p0","confidence":0.9},
            {"type":"refusal","name":"priority"},
            {"type":"choice","name":"status","choice":"todo","confidence":0.8},
            {"type":"predicate","name":"needs_clarification","probability":0.1},
            {"type":"predicate","name":"tag_0","probability":0.99},
            {"type":"refusal","name":"tag_1"}
        ]});
        let answers = parse_decisions_capture(&response, &r).unwrap();
        assert_eq!(answers.priority, None);
        assert_eq!(answers.tags, vec![Some(0.99), None]);
        let a = read_capture(&r, &answers, CaptureEngine::Decisions);
        assert_eq!(a.project.as_deref(), Some("CXVentures"));
        assert_eq!(a.priority, db::tasks::PRIORITY_LOWEST, "no judgment = the create default");
        assert_eq!(a.status, "todo");
        assert_eq!(a.tags, vec!["seo".to_string()]);
        assert_eq!(a.confidence, 0.8, "a refused question does not drag confidence to 0");
        assert_eq!(a.engine, Some(CaptureEngine::Decisions));
    }

    #[test]
    fn malformed_answers_are_no_judgment() {
        let r = request();
        let response = json!({"answers":[
            {"type":"choice","name":"project","choice":"p7","confidence":0.99},
            {"type":"choice","name":"priority"},
            {"type":"choice","name":"status","choice":"todo","confidence":0.9},
            {"type":"predicate","name":"needs_clarification","probability":"high"},
            {"type":"predicate","name":"tag_0","probability":1.7}
        ]});
        let a = read_capture(&r, &parse_decisions_capture(&response, &r).unwrap(), CaptureEngine::Decisions);
        assert_eq!(a.project, None, "a key outside the offered set is no pick");
        assert_eq!(a.priority, db::tasks::PRIORITY_LOWEST);
        assert_eq!(a.status, "inbox", "an unanswered clarification cannot vouch for todo");
        assert!(a.tags.is_empty() && !a.too_vague);
        assert!(parse_decisions_capture(&json!({"error":"x"}), &r).is_err(), "no answers at all is a failure, so the caller falls back");
    }

    #[test]
    fn jev_degrades_per_question_too() {
        let r = request();
        let response = json!({"answers":{
            "project":{"type":"choice","choice":"p1","confidence":0.6},
            "status":{"type":"choice","choice":"todo","confidence":0.7},
            "needs_clarification":{"type":"noul","noul":0.2},
            "tag_1":{"type":"noul","noul":0.75}
        }});
        let a = read_capture(&r, &parse_jev_capture(&response, &r).unwrap(), CaptureEngine::Jev);
        assert_eq!(a.project.as_deref(), Some("pocketbuddy"));
        assert_eq!(a.priority, db::tasks::PRIORITY_LOWEST);
        assert_eq!(a.status, "todo");
        assert_eq!(a.tags, vec!["security".to_string()]);
    }

    #[test]
    fn bars_are_per_engine() {
        let r = request();
        let answers = CaptureAnswers {
            project: Some(("p0".into(), 0.6)),
            priority: Some(("p2".into(), 0.9)),
            status: Some(("inbox".into(), 0.9)),
            needs_clarification: Some(0.95),
            tags: vec![Some(0.8), None],
        };
        let jev = read_capture(&r, &answers, CaptureEngine::Jev);
        let dec = read_capture(&r, &answers, CaptureEngine::Decisions);
        assert_eq!(jev.project.as_deref(), Some("CXVentures"));
        assert_eq!(dec.project, None, "0.6 clears Jev's 0.55, not Decisions' 0.70");
        assert_eq!(jev.tags, vec!["seo".to_string()]);
        assert!(dec.tags.is_empty(), "0.8 clears Jev's 0.72, not Decisions' 0.96");
        assert!(jev.too_vague && !dec.too_vague, "0.95 clears Jev's 0.90; Decisions never flags");
        let certain = CaptureAnswers { needs_clarification: Some(1.0), ..answers };
        assert!(!read_capture(&r, &certain, CaptureEngine::Decisions).too_vague);
        assert_eq!(jev.priority, 2);
    }

    #[test]
    fn unknown_engine_in_meta_reads_as_the_default() {
        let conn = test_conn();
        db::meta::set(&conn, KEY_CAPTURE_ENGINE, "gpt-9").unwrap();
        assert_eq!(settings(&conn).capture_engine, CaptureEngine::Decisions);
        db::meta::set(&conn, KEY_CAPTURE_ENGINE, "jev").unwrap();
        assert_eq!(settings(&conn).capture_engine, CaptureEngine::Jev);
    }

    #[test]
    fn an_old_client_saving_settings_without_an_engine_still_parses() {
        let v: SaveSettings = serde_json::from_value(json!({
            "enabled": true, "model": "jev-1.13.0", "capture": true,
            "readiness": true, "review": true, "recall": true
        })).unwrap();
        assert_eq!(v.settings.capture_engine, CaptureEngine::Decisions);
    }
}
