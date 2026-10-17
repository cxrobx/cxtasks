//! [`Peer`] over HTTP.
//!
//! Blocking on purpose. Every caller — the Tauri command, the runner's tick —
//! already has a thread and a `MutexGuard<Connection>` in hand, and an async
//! client would mean an executor in a crate that does not otherwise need one.
//!
//! **No TLS.** The endpoint lives on a tailnet, which is already encrypted and
//! has no certificate to present, and it is bound to a tailnet address
//! precisely so that it is not reachable from anywhere a certificate would
//! matter. That is a deployment fact rather than a shortcut, and it is written
//! down beside the dependency that assumes it.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::AppError;
use crate::sync::leases::Lease;
use crate::sync::outbox::{Intent, PushResult};
use crate::sync::pair::{Peer, PeerInfo};
use crate::sync::snapshot::Snapshot;

pub struct HttpPeer {
    base: String,
    token: String,
    timeout: Duration,
}

impl HttpPeer {
    pub fn new(base_url: &str, token: &str) -> Self {
        Self {
            base: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            // Generous, because the snapshot is the big one and a NAS under
            // load is slow rather than broken. The UI never waits on any of
            // this — reconcile runs off the interaction path — so a long
            // timeout costs patience nobody is spending.
            timeout: Duration::from_secs(120),
        }
    }

    fn agent(&self) -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout_read(self.timeout)
            .timeout_write(self.timeout)
            .build()
    }

    /// Unwrap the `{ "ok": … } | { "err": … }` envelope every route uses.
    ///
    /// The error branch matters as much as the success one: a peer's refusal is
    /// a sentence written for a person — "that node was already seeded from …"
    /// — and turning it into "HTTP 409" at the transport would throw away the
    /// only part that says what to do.
    fn unwrap_envelope<T: DeserializeOwned>(raw: Value) -> Result<T, AppError> {
        if let Some(err) = raw.get("err").and_then(Value::as_str) {
            return Err(AppError::Validation(err.to_string()));
        }
        let ok = raw
            .get("ok")
            .ok_or_else(|| AppError::General("peer sent neither ok nor err".into()))?;
        serde_json::from_value(ok.clone())
            .map_err(|e| AppError::General(format!("could not decode peer response: {e}")))
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, AppError> {
        let resp = self
            .agent()
            .get(&format!("{}{path}", self.base))
            .set("Authorization", &format!("Bearer {}", self.token))
            .call();
        Self::finish(resp)
    }

    fn post<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T, AppError> {
        let resp = self
            .agent()
            .post(&format!("{}{path}", self.base))
            .set("Authorization", &format!("Bearer {}", self.token))
            .send_json(body.clone());
        Self::finish(resp)
    }

    /// A raw body, for the one route that does not answer JSON.
    ///
    /// Bounded by the same cap the importer enforces, so a peer cannot make
    /// this end allocate more than an attachment could ever be. `read_to_end`
    /// on an unbounded reader is how a hostile — or merely broken — peer turns
    /// a sync into an OOM.
    fn get_bytes(&self, path: &str) -> Result<Vec<u8>, AppError> {
        let resp = self
            .agent()
            .get(&format!("{}{path}", self.base))
            .set("Authorization", &format!("Bearer {}", self.token))
            .call();
        match resp {
            Ok(r) => {
                use std::io::Read;
                let cap = crate::attachments::MAX_IMAGE_BYTES;
                let mut buf = Vec::new();
                r.into_reader()
                    .take(cap + 1)
                    .read_to_end(&mut buf)
                    .map_err(|e| AppError::General(format!("could not read the body: {e}")))?;
                if buf.len() as u64 > cap {
                    return Err(AppError::Validation(format!(
                        "{path} is larger than the {cap}-byte attachment limit"
                    )));
                }
                Ok(buf)
            }
            // A refusal answers the ordinary envelope, so the peer's sentence
            // survives even though success does not look like JSON at all.
            Err(ureq::Error::Status(_, r)) => {
                let raw: Value = r.into_json().unwrap_or(Value::Null);
                Self::unwrap_envelope::<Value>(raw).map(|_| Vec::new())
            }
            Err(e) => Err(AppError::General(format!("could not reach the node: {e}"))),
        }
    }

    fn post_bytes<T: DeserializeOwned>(&self, path: &str, body: &[u8]) -> Result<T, AppError> {
        let resp = self
            .agent()
            .post(&format!("{}{path}", self.base))
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Content-Type", "application/octet-stream")
            .send_bytes(body);
        Self::finish(resp)
    }

    fn finish<T: DeserializeOwned>(
        resp: Result<ureq::Response, ureq::Error>,
    ) -> Result<T, AppError> {
        match resp {
            Ok(r) => {
                let raw: Value = r
                    .into_json()
                    .map_err(|e| AppError::General(format!("peer sent malformed JSON: {e}")))?;
                Self::unwrap_envelope(raw)
            }
            // A refusal carries a body, and that body is the message. Reading it
            // rather than reporting the status is what makes the peer's reasons
            // reach the screen.
            Err(ureq::Error::Status(_, r)) => {
                let raw: Value = r.into_json().unwrap_or(Value::Null);
                Self::unwrap_envelope(raw)
            }
            Err(e) => Err(AppError::General(format!("could not reach the node: {e}"))),
        }
    }
}

impl Peer for HttpPeer {
    fn info(&self) -> Result<PeerInfo, AppError> {
        self.get("/sync/info")
    }

    fn seed(&self, snapshot: &Snapshot, from_host: &str) -> Result<(), AppError> {
        let body = serde_json::json!({ "snapshot": snapshot, "from_host": from_host });
        let _: Value = self.post("/sync/seed", &body)?;
        Ok(())
    }

    fn request_lease(&self, host: &str, size: i64) -> Result<Lease, AppError> {
        self.post("/sync/lease", &serde_json::json!({ "host": host, "size": size }))
    }

    fn snapshot(&self) -> Result<Snapshot, AppError> {
        self.get("/sync/snapshot")
    }

    fn push(&self, intents: &[Intent]) -> Result<PushResult, AppError> {
        self.post("/sync/push", &serde_json::json!({ "intents": intents }))
    }

    fn wanted_blobs(&self) -> Result<Vec<String>, AppError> {
        self.get("/attachments/missing")
    }

    fn get_blob(&self, stored_name: &str) -> Result<Vec<u8>, AppError> {
        self.get_bytes(&blob_path(stored_name)?)
    }

    fn put_blob(&self, stored_name: &str, bytes: &[u8]) -> Result<(), AppError> {
        let _: Value = self.post_bytes(&blob_path(stored_name)?, bytes)?;
        Ok(())
    }
}

/// The route for one managed file.
///
/// Validated HERE as well as at the server, and not out of politeness: a name
/// with a `/` in it would silently address a completely different route, so the
/// check that keeps a traversal off the peer's disk is also the check that
/// keeps this client from asking the wrong question. No percent-encoding is
/// needed once the name is known to be `[A-Za-z0-9._-]`, which is the second
/// reason to validate before building the URL rather than after.
fn blob_path(stored_name: &str) -> Result<String, AppError> {
    if !crate::attachments::is_safe_stored_name(stored_name) {
        return Err(AppError::Validation(format!(
            "{stored_name:?} is not a managed attachment filename"
        )));
    }
    Ok(format!("/attachments/{stored_name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_err_envelope_becomes_the_peers_own_sentence() {
        let raw = serde_json::json!({
            "err": "this node was already seeded from other-mac; refusing to re-seed it"
        });
        let err = HttpPeer::unwrap_envelope::<Value>(raw).unwrap_err().to_string();
        assert!(
            err.contains("already seeded from other-mac"),
            "the peer's reason must survive the transport, got: {err}"
        );
    }

    #[test]
    fn an_ok_envelope_is_unwrapped() {
        let raw = serde_json::json!({ "ok": { "lo": 10, "hi": 20 } });
        let lease: Lease = HttpPeer::unwrap_envelope(raw).unwrap();
        assert_eq!(lease, Lease { lo: 10, hi: 20 });
    }

    #[test]
    fn a_response_with_neither_field_is_an_error_rather_than_a_default() {
        let raw = serde_json::json!({ "something": "else" });
        assert!(HttpPeer::unwrap_envelope::<Value>(raw).is_err());
    }

    /// A trailing slash on the configured URL must not produce `//sync/info`.
    #[test]
    fn a_trailing_slash_is_trimmed() {
        let p = HttpPeer::new("http://node:8787/", "t");
        assert_eq!(p.base, "http://node:8787");
    }

    /// A name that could address a different route never becomes a URL.
    ///
    /// The server refuses this too, and both refusals are wanted: this one
    /// stops a `..` from being asked FOR, and that one stops it from being
    /// served. Either alone leaves the other end trusting a stranger.
    #[test]
    fn a_stored_name_that_is_not_a_plain_filename_never_reaches_the_wire() {
        for bad in ["../../etc/passwd", "a/b.png", "..", ".hidden.png", "a\\b.png", ""] {
            assert!(blob_path(bad).is_err(), "{bad:?} must not become a URL");
        }
        assert_eq!(
            blob_path("2f3a4b5c-0000-1111-2222-333344445555.png").unwrap(),
            "/attachments/2f3a4b5c-0000-1111-2222-333344445555.png"
        );
    }
}
