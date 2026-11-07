//! Due reminders pushed to phones through APNs.
//!
//! # Why the server, when the phone already schedules its own
//!
//! The phone's local reminders only know about due dates the phone has SEEN,
//! and iOS keeps an app suspended for days. A due date set on the Mac, or by an
//! agent through the MCP, reached the phone only when the app was next opened —
//! which is exactly when a reminder is no longer needed. This node is the one
//! place every due date passes through, and it is always up, so it is the one
//! place that can say "this is due now" on time.
//!
//! # Shape
//!
//! - A phone registers its APNs device token with `register_push_device`
//!   (intercepted in `rpc.rs`, never reaching the core verb table). The reply
//!   says whether this node can push; while it can, the phone stops scheduling
//!   its own reminders so nothing arrives twice.
//! - [`run`] wakes every [`TICK`], finds open tasks whose `due_at` passed within
//!   [`WINDOW`], and pushes each ONCE per due date.
//!
//! # Where the state lives
//!
//! `push.db`, beside the canonical database and deliberately NOT in it. A
//! device token is a fact about this node (the only one with a key), and a
//! table in `cxtasks.db` would have to be classified for sync, migrated, and
//! kept off the wire — three chances to leak a token to every Mac. A separate
//! file cannot travel.
//!
//! # At most once
//!
//! The `(task, due_at)` row is written BEFORE the push is sent. A crash between
//! the two means a missed reminder, never a repeated one, and a due date that
//! moves gets a fresh row, which is what re-arms it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use serde_json::{json, Value};

use crate::ServerState;

/// How often the loop looks for newly due tasks. The reminder lands at most
/// this late.
pub const TICK: Duration = Duration::from_secs(20);

/// How far back a due date still earns a push. Covers a restart or an outage
/// of a few minutes; anything older is overdue, which the app shows, and a
/// push for it would only be noise — on a first deploy, a flood of it.
pub const WINDOW: chrono::Duration = chrono::Duration::minutes(15);

/// Apple accepts a provider token for an hour and refuses one refreshed more
/// often than every 20 minutes. 50 sits inside both.
const JWT_LIFETIME_SECS: u64 = 50 * 60;

/// The team every CX app ships under; one APNs key serves all of them.
const DEFAULT_TEAM_ID: &str = "CCYV5HQZCM";
const DEFAULT_TOPIC: &str = "app.cxtasks";

pub struct Push {
    key: EcdsaKeyPair,
    key_id: String,
    team_id: String,
    topic: String,
    store: Mutex<rusqlite::Connection>,
    jwt: Mutex<Option<(String, u64)>>,
    http: reqwest::Client,
}

/// One task that came due, as the payload needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct DueTask {
    pub id: String,
    pub reference: String,
    pub title: String,
    pub due_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub token: String,
    pub environment: String,
}

impl Push {
    /// `None` when push is not configured, which is a normal state: a node
    /// without `CXTASKS_APNS_KEY_FILE` simply does not push, and every phone
    /// keeps its local reminders. A key that is SET but unusable is logged
    /// loudly and also yields `None` — a reminder feature is not a reason to
    /// refuse to serve the task list.
    pub fn from_env(data_dir: &Path) -> Option<Arc<Self>> {
        let key_file = std::env::var("CXTASKS_APNS_KEY_FILE").ok().filter(|v| !v.is_empty())?;
        match Self::load(data_dir, Path::new(&key_file)) {
            Ok(p) => {
                log::info!("push: on (topic {}, key {})", p.topic, p.key_id);
                Some(Arc::new(p))
            }
            Err(e) => {
                log::error!("push: OFF — {e:#}");
                None
            }
        }
    }

    fn load(data_dir: &Path, key_file: &Path) -> Result<Self> {
        let key_id = std::env::var("CXTASKS_APNS_KEY_ID")
            .ok()
            .filter(|v| !v.is_empty())
            .context("CXTASKS_APNS_KEY_ID is not set")?;
        let team_id = std::env::var("CXTASKS_APNS_TEAM_ID")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_TEAM_ID.into());
        let topic = std::env::var("CXTASKS_APNS_TOPIC")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_TOPIC.into());
        let pem = std::fs::read_to_string(key_file)
            .with_context(|| format!("could not read {}", key_file.display()))?;
        let key = parse_p8(&pem)?;
        let store = open_store(&data_dir.join("push.db"))?;
        let http = reqwest::Client::builder()
            // APNs speaks HTTP/2 only; HTTP/1.1 is refused outright.
            .http2_prior_knowledge()
            .timeout(Duration::from_secs(15))
            .build()
            .context("could not build the APNs client")?;
        Ok(Self {
            key,
            key_id,
            team_id,
            topic,
            store: Mutex::new(store),
            jwt: Mutex::new(None),
            http,
        })
    }

    /// Remember a phone. Re-registering refreshes it, so a token that rotates
    /// (a restore, a reinstall) replaces nothing by hand.
    pub fn register(&self, token: &str, environment: &str, name: Option<&str>) -> Result<()> {
        validate_device(token, environment)?;
        let conn = lock(&self.store);
        register_device(&conn, token, environment, name, &now_db())
    }

    pub fn unregister(&self, token: &str) -> Result<()> {
        let conn = lock(&self.store);
        conn.execute("DELETE FROM devices WHERE token = ?1", [token])?;
        Ok(())
    }

    fn devices(&self) -> Result<Vec<Device>> {
        list_devices(&lock(&self.store))
    }

    fn jwt(&self) -> Result<String> {
        let now = unix_now();
        let mut cached = lock(&self.jwt);
        if let Some((token, issued)) = cached.as_ref() {
            if now.saturating_sub(*issued) < JWT_LIFETIME_SECS {
                return Ok(token.clone());
            }
        }
        let token = make_jwt(&self.key, &self.key_id, &self.team_id, now)?;
        *cached = Some((token.clone(), now));
        Ok(token)
    }

    /// One push to one phone. Returns Apple's verdict as a short string for the
    /// log; a token Apple says is dead is forgotten here.
    async fn send(&self, device: &Device, task: &DueTask) -> String {
        let jwt = match self.jwt() {
            Ok(j) => j,
            Err(e) => return format!("no provider token: {e:#}"),
        };
        let req = build_request(device, task, &self.topic, &jwt);
        let mut builder = self.http.post(&req.url);
        for (k, v) in &req.headers {
            builder = builder.header(*k, v);
        }
        let response = match builder.body(req.body.to_string()).send().await {
            Ok(r) => r,
            Err(e) => return format!("network: {e}"),
        };
        let status = response.status().as_u16();
        let reason = response
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v.get("reason").and_then(Value::as_str).map(String::from))
            .unwrap_or_default();
        match (status, reason.as_str()) {
            (200, _) => "sent".into(),
            (410, _) | (400, "BadDeviceToken") | (400, "DeviceTokenNotForTopic") => {
                let _ = self.unregister(&device.token);
                format!("{status} {reason}: device forgotten")
            }
            (403, "ExpiredProviderToken") => {
                *lock(&self.jwt) = None;
                format!("{status} {reason}: provider token reset")
            }
            _ => format!("{status} {reason}"),
        }
    }

    /// Ask Apple whether key, team and topic are accepted, without reaching a
    /// phone: a push to a token no device owns comes back `400 BadDeviceToken`
    /// when all three are good, and `403` when they are not.
    pub async fn check(&self, environment: &str) -> String {
        let device = Device { token: "0".repeat(64), environment: environment.into() };
        let task = DueTask {
            id: "check".into(),
            reference: "T0".into(),
            title: "push check".into(),
            due_at: now_iso(),
        };
        // Through `send`, minus its forgetting: the fake token was never stored.
        self.send(&device, &task).await
    }
}

/// The loop. Runs for the life of the process.
pub async fn run(state: Arc<ServerState>, push: Arc<Push>) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if let Err(e) = tick(&state, &push).await {
            log::warn!("push: tick failed: {e:#}");
        }
    }
}

async fn tick(state: &ServerState, push: &Push) -> Result<()> {
    let now = chrono::Utc::now();
    let due = {
        let conn = lock(&state.db);
        due_tasks(&conn, now)?
    };
    if due.is_empty() {
        return Ok(());
    }
    let devices = push.devices()?;
    for task in due {
        // Claimed before sending: at most once. With no phone registered the
        // claim still lands, so registering later does not replay old dues.
        let claimed = {
            let conn = lock(&push.store);
            claim(&conn, &task, &now_db())?
        };
        if !claimed {
            continue;
        }
        for device in &devices {
            let verdict = push.send(device, &task).await;
            log::info!(
                "push: {} → {}… ({}): {verdict}",
                task.reference,
                &device.token[..8.min(device.token.len())],
                device.environment
            );
        }
    }
    Ok(())
}

// ── pure parts, tested below ────────────────────────────────────────────────

/// Open tasks whose due date passed within [`WINDOW`] of `now`.
///
/// Both operands go through `datetime()` (invariant 1): a due date written by
/// another path in another format must still compare as a time, not as text.
pub fn due_tasks(conn: &rusqlite::Connection, now: chrono::DateTime<chrono::Utc>) -> Result<Vec<DueTask>> {
    let upper = iso(now);
    let lower = iso(now - WINDOW);
    let closed = cxtasks_core::db::tasks::CLOSED_STATUSES
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id, short_id, title, due_at FROM tasks
          WHERE due_at IS NOT NULL
            AND datetime(due_at) <= datetime(?1) AND datetime(due_at) > datetime(?2)
            AND deleted_at IS NULL AND archived_at IS NULL
            AND status NOT IN ({closed})
          ORDER BY due_at"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([&upper, &lower], |r| {
        let short: Option<i64> = r.get(1)?;
        Ok(DueTask {
            id: r.get(0)?,
            reference: short.map(|n| format!("T{n}")).unwrap_or_default(),
            title: r.get(2)?,
            due_at: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub struct ApnsRequest {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Value,
}

pub fn build_request(device: &Device, task: &DueTask, topic: &str, jwt: &str) -> ApnsRequest {
    let host = if device.environment == "development" {
        "api.sandbox.push.apple.com"
    } else {
        "api.push.apple.com"
    };
    let expires = unix_now() + 3600;
    // The reference is the title (it is what the local reminder showed, and
    // what a T-number search finds); a task with none falls back to the title.
    let title = if task.reference.is_empty() { task.title.clone() } else { task.reference.clone() };
    ApnsRequest {
        url: format!("https://{host}/3/device/{}", device.token),
        headers: vec![
            ("authorization", format!("bearer {jwt}")),
            ("apns-topic", topic.to_string()),
            ("apns-push-type", "alert".into()),
            ("apns-priority", "10".into()),
            // A due reminder an hour late is noise; let Apple drop it.
            ("apns-expiration", expires.to_string()),
            // A re-send for the same task replaces the banner, never stacks.
            ("apns-collapse-id", format!("due-{}", task.id)),
        ],
        body: json!({
            "aps": {
                "alert": { "title": title, "body": task.title },
                "sound": "default",
                "thread-id": "due",
            },
            // `NotificationRouter` resolves this the same way it resolves a
            // local reminder's `ref`, so a tap opens the task.
            "ref": task.reference,
        }),
    }
}

pub fn validate_device(token: &str, environment: &str) -> Result<()> {
    if !(32..=200).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("a device token is hex, 32 to 200 characters");
    }
    if environment != "development" && environment != "production" {
        bail!("environment must be 'development' or 'production'");
    }
    Ok(())
}

/// An Apple `.p8` is PEM-wrapped PKCS#8 for a P-256 key.
pub fn parse_p8(pem: &str) -> Result<EcdsaKeyPair> {
    // Accept a key carried through an env file with literal `\n`s too.
    let pem = pem.replace("\\n", "\n");
    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .map(str::trim)
        .collect();
    let der = STANDARD.decode(body).context("the APNs key is not valid base64")?;
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &SystemRandom::new())
        .map_err(|e| anyhow::anyhow!("the APNs key is not a P-256 PKCS#8 key: {e}"))
}

/// The provider token: ES256 over `{alg, kid}.{iss, iat}`.
pub fn make_jwt(key: &EcdsaKeyPair, key_id: &str, team_id: &str, iat: u64) -> Result<String> {
    let header = URL_SAFE_NO_PAD.encode(json!({ "alg": "ES256", "kid": key_id }).to_string());
    let claims = URL_SAFE_NO_PAD.encode(json!({ "iss": team_id, "iat": iat }).to_string());
    let signing_input = format!("{header}.{claims}");
    let sig = key
        .sign(&SystemRandom::new(), signing_input.as_bytes())
        .map_err(|_| anyhow::anyhow!("could not sign the provider token"))?;
    Ok(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref())))
}

pub fn open_store(path: &PathBuf) -> Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(path)
        .with_context(|| format!("could not open {}", path.display()))?;
    init_store(&conn)?;
    Ok(conn)
}

pub fn init_store(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS devices (
             token       TEXT PRIMARY KEY,
             environment TEXT NOT NULL,
             name        TEXT,
             created_at  TEXT NOT NULL,
             seen_at     TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS sent (
             task_id TEXT NOT NULL,
             due_at  TEXT NOT NULL,
             sent_at TEXT NOT NULL,
             PRIMARY KEY (task_id, due_at)
         );",
    )?;
    Ok(())
}

pub fn register_device(
    conn: &rusqlite::Connection,
    token: &str,
    environment: &str,
    name: Option<&str>,
    now: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO devices (token, environment, name, created_at, seen_at)
              VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(token) DO UPDATE
              SET environment = excluded.environment, name = excluded.name, seen_at = excluded.seen_at",
        rusqlite::params![token, environment, name, now],
    )?;
    Ok(())
}

pub fn list_devices(conn: &rusqlite::Connection) -> Result<Vec<Device>> {
    let mut stmt = conn.prepare("SELECT token, environment FROM devices ORDER BY created_at")?;
    let rows = stmt.query_map([], |r| Ok(Device { token: r.get(0)?, environment: r.get(1)? }))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// True the first time a `(task, due_at)` is seen; false ever after.
pub fn claim(conn: &rusqlite::Connection, task: &DueTask, now: &str) -> Result<bool> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO sent (task_id, due_at, sent_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![task.id, task.due_at, now],
    )?;
    Ok(n == 1)
}

fn iso(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn now_iso() -> String {
    iso(chrono::Utc::now())
}

fn now_db() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{KeyPair, UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};

    fn tasks_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        cxtasks_core::db::schema::initialize(&conn).unwrap();
        conn
    }

    fn insert(conn: &rusqlite::Connection, short: i64, status: &str, due: Option<&str>, archived: bool) {
        conn.execute(
            "INSERT INTO tasks (id, short_id, title, status, due_at, archived_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), datetime('now'))",
            rusqlite::params![
                format!("id-{short}"),
                short,
                format!("task {short}"),
                status,
                due,
                archived.then_some("2026-11-01 00:00:00")
            ],
        )
        .unwrap();
    }

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc)
    }

    #[test]
    fn only_open_tasks_that_just_came_due() {
        let conn = tasks_db();
        insert(&conn, 1, "todo", Some("2026-11-07T03:20:00.000Z"), false); // due 1 min ago
        insert(&conn, 2, "todo", Some("2026-11-07T03:22:00.000Z"), false); // not yet
        insert(&conn, 3, "todo", Some("2026-11-07T02:00:00.000Z"), false); // long overdue
        insert(&conn, 4, "done", Some("2026-11-07T03:20:00.000Z"), false); // closed
        insert(&conn, 5, "todo", Some("2026-11-07T03:20:00.000Z"), true); // archived
        insert(&conn, 6, "inbox", None, false); // no date
        insert(&conn, 7, "in_review", Some("2026-11-07T03:21:00.000Z"), false); // due exactly now
        let due = due_tasks(&conn, at("2026-11-07T03:21:00.000Z")).unwrap();
        let refs: Vec<_> = due.iter().map(|t| t.reference.as_str()).collect();
        assert_eq!(refs, ["T1", "T7"]);
    }

    #[test]
    fn a_due_date_is_pushed_once_and_a_moved_one_again() {
        let store = rusqlite::Connection::open_in_memory().unwrap();
        init_store(&store).unwrap();
        let mut t = DueTask { id: "a".into(), reference: "T1".into(), title: "x".into(), due_at: "2026-11-07T03:20:00.000Z".into() };
        assert!(claim(&store, &t, "now").unwrap());
        assert!(!claim(&store, &t, "now").unwrap(), "the second tick must not re-send");
        t.due_at = "2026-11-08T03:20:00.000Z".into();
        assert!(claim(&store, &t, "now").unwrap(), "a new due date re-arms the push");
    }

    #[test]
    fn registering_twice_keeps_one_device() {
        let store = rusqlite::Connection::open_in_memory().unwrap();
        init_store(&store).unwrap();
        let tok = "ab".repeat(32);
        register_device(&store, &tok, "development", Some("Duo"), "t1").unwrap();
        register_device(&store, &tok, "production", Some("Duo"), "t2").unwrap();
        assert_eq!(list_devices(&store).unwrap(), vec![Device { token: tok, environment: "production".into() }]);
    }

    #[test]
    fn device_input_is_checked() {
        assert!(validate_device(&"ab".repeat(32), "development").is_ok());
        assert!(validate_device("not-hex", "development").is_err());
        assert!(validate_device(&"ab".repeat(32), "staging").is_err());
    }

    #[test]
    fn the_provider_token_verifies_against_the_key() {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(pkcs8.as_ref())
        );
        let key = parse_p8(&pem).unwrap();
        // Also as an env file carries it, with literal backslash-n.
        assert!(parse_p8(&pem.replace('\n', "\\n")).is_ok());

        let jwt = make_jwt(&key, "ABC123DEFG", "CCYV5HQZCM", 1_790_000_000).unwrap();
        let parts: Vec<_> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header, json!({ "alg": "ES256", "kid": "ABC123DEFG" }));
        let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims, json!({ "iss": "CCYV5HQZCM", "iat": 1_790_000_000u64 }));
        let sig = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, key.public_key().as_ref())
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .expect("signature must verify");
    }

    #[test]
    fn the_request_targets_the_right_gateway_and_carries_the_ref() {
        let task = DueTask { id: "uuid-1".into(), reference: "T635".into(), title: "Reminder test".into(), due_at: String::new() };
        let dev = Device { token: "ab".repeat(32), environment: "development".into() };
        let req = build_request(&dev, &task, "app.cxtasks", "JWT");
        assert!(req.url.starts_with("https://api.sandbox.push.apple.com/3/device/"));
        assert!(req.headers.contains(&("apns-topic", "app.cxtasks".into())));
        assert!(req.headers.contains(&("authorization", "bearer JWT".into())));
        assert!(req.headers.contains(&("apns-collapse-id", "due-uuid-1".into())));
        assert_eq!(req.body["ref"], "T635");
        assert_eq!(req.body["aps"]["alert"]["title"], "T635");
        assert_eq!(req.body["aps"]["alert"]["body"], "Reminder test");

        let prod = Device { environment: "production".into(), ..dev };
        assert!(build_request(&prod, &task, "app.cxtasks", "JWT").url.starts_with("https://api.push.apple.com/"));
    }
}
