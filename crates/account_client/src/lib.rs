//! Client for the fork's account + settings-sync server (`account_server`).
//!
//! A thin, runtime-agnostic wrapper over the server's bearer-token REST API
//! (see `docs/account-server.md`). It owns two concerns the rest of the app
//! should not have to think about:
//!
//! * **transport security** — the server is a bare `IP:port` with a self-signed
//!   certificate, so this client pins that certificate by fingerprint instead
//!   of trusting a CA (see [`tls`]). A password is never sent to a server whose
//!   certificate does not match the pin.
//! * **status-code semantics** — each endpoint's meaningful failures (an email
//!   already taken, a bad password, a settings version conflict) become typed
//!   [`AccountError`] / result variants rather than raw HTTP codes.
//!
//! The returned session token maps onto the app's existing
//! `Credentials::Bearer` path, so nothing downstream needs a new auth concept.

use std::sync::{Arc, Mutex};

use serde::Deserialize;

mod tls;
pub use tls::{fingerprint_eq, fingerprint_of_der};

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

/// How to reach the account server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Base URL, e.g. `https://203.0.113.4:10101`. `https` engages certificate
    /// pinning; `http` (local testing only) does not.
    pub base_url: String,
    /// The certificate fingerprint to pin (`A1:B2:…`, any casing/separators).
    /// `None` trusts the first certificate seen and records it via
    /// [`AccountClient::observed_fingerprint`] so it can be pinned afterwards.
    pub pinned_fingerprint: Option<String>,
}

/// A logged-in account, as the server sees it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Account {
    pub email: String,
    /// The single free-form team field; `None` when the account is on no team.
    #[serde(default)]
    pub team: Option<String>,
}

/// One scope's settings document (`global`, a platform name, …).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ScopeDocument {
    /// Monotonic version; pass it back as `base_version` on the next write.
    pub version: i32,
    /// Opaque client-owned payload. The server never interprets it.
    pub data: serde_json::Value,
}

/// Every scope the account has stored, keyed by scope name.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub scopes: std::collections::HashMap<String, ScopeDocument>,
}

/// The outcome of a settings write. A conflict is an expected, recoverable
/// result — the caller merges its change into the returned document and writes
/// again with the returned version as the new `base_version`.
#[derive(Debug, Clone)]
pub enum SettingsWrite {
    /// The write landed; `version` is the new document version.
    Written { version: i32 },
    /// Another machine wrote first. The server returned the live document so
    /// the caller can merge without a second round trip.
    Conflict { version: i32, data: serde_json::Value },
}

/// A typed failure from an account operation.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    /// The request never reached the server, or the reply was unreadable.
    #[error("network error: {0}")]
    Network(String),
    /// Signup for an email that already has an account (`409`).
    #[error("an account already exists for that email")]
    AccountExists,
    /// Login with the wrong email or password (`401`).
    #[error("wrong email or password")]
    InvalidCredentials,
    /// The bearer token was missing, invalid, or revoked (`401` on an
    /// authenticated route).
    #[error("not signed in (token missing, invalid, or expired)")]
    Unauthorized,
    /// The server rejected the input (`400`), e.g. a malformed email or a
    /// password below the minimum length.
    #[error("{0}")]
    BadRequest(String),
    /// Any other non-success status.
    #[error("server error (HTTP {status}): {message}")]
    Server { status: u16, message: String },
    /// The server's certificate did not match the pinned fingerprint. The
    /// request body was NOT sent.
    #[error("{0}")]
    Pinning(String),
    /// The client could not be constructed (bad URL, TLS init failure).
    #[error("{0}")]
    Config(String),
}

/// A client bound to one server. Cheap to clone (shares the underlying
/// connection pool); construct once and reuse.
#[derive(Clone)]
pub struct AccountClient {
    http: reqwest::Client,
    /// Base URL with any trailing slash trimmed.
    base: String,
    /// The pinned fingerprint, kept so a connection failure can be reported as
    /// a pin mismatch rather than a generic network error.
    expected_fingerprint: Option<String>,
    /// Fingerprint of the certificate last presented by the server.
    observed: Arc<Mutex<Option<String>>>,
}

impl AccountClient {
    /// Builds a client for `config`. Fails only on an unusable URL or TLS
    /// initialization error — no network I/O happens here.
    pub fn new(config: ServerConfig) -> Result<Self, AccountError> {
        let base = config.base_url.trim().trim_end_matches('/').to_owned();
        if base.is_empty() {
            return Err(AccountError::Config("server URL is empty".to_owned()));
        }
        let parsed = url::Url::parse(&base)
            .map_err(|e| AccountError::Config(format!("invalid server URL {base:?}: {e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(AccountError::Config(format!(
                "server URL must be http or https, got {:?}",
                parsed.scheme()
            )));
        }

        let observed = Arc::new(Mutex::new(None));
        let http = tls::pinned_client(config.pinned_fingerprint.clone(), observed.clone())?;

        Ok(Self {
            http,
            base,
            expected_fingerprint: config.pinned_fingerprint,
            observed,
        })
    }

    /// The fingerprint of the certificate the server last presented, if any
    /// connection has been attempted. In trust-on-first-use mode, persist this
    /// after a successful call and feed it back as `pinned_fingerprint` so every
    /// later connection is verified.
    pub fn observed_fingerprint(&self) -> Option<String> {
        self.observed.lock().ok().and_then(|slot| slot.clone())
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base, path.trim_start_matches('/'))
    }

    /// Liveness probe. Also the cheapest way to establish (trust-on-first-use)
    /// or verify (pinned) the certificate before any credential is sent.
    pub async fn health(&self) -> Result<(), AccountError> {
        let resp = self.send(self.http.get(self.url("v1/health"))).await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(self.unexpected(resp).await)
        }
    }

    /// Creates an account and returns its session token. `409` → the email is
    /// taken; `400` → the server rejected the email or password.
    pub async fn signup(&self, email: &str, password: &str) -> Result<String, AccountError> {
        let resp = self
            .send(self.http.post(self.url("v1/signup")).json(&Credentials {
                email: email.to_owned(),
                password: password.to_owned(),
            }))
            .await?;
        match resp.status().as_u16() {
            200 | 201 => Ok(self.token(resp).await?),
            409 => Err(AccountError::AccountExists),
            400 => Err(AccountError::BadRequest(self.message(resp).await)),
            _ => Err(self.unexpected(resp).await),
        }
    }

    /// Signs in and returns a session token. `401` → wrong email or password.
    pub async fn login(&self, email: &str, password: &str) -> Result<String, AccountError> {
        let resp = self
            .send(self.http.post(self.url("v1/login")).json(&Credentials {
                email: email.to_owned(),
                password: password.to_owned(),
            }))
            .await?;
        match resp.status().as_u16() {
            200 | 201 => Ok(self.token(resp).await?),
            401 => Err(AccountError::InvalidCredentials),
            400 => Err(AccountError::BadRequest(self.message(resp).await)),
            _ => Err(self.unexpected(resp).await),
        }
    }

    /// Revokes `token` server-side. Succeeds even if the token was already gone.
    pub async fn logout(&self, token: &str) -> Result<(), AccountError> {
        let resp = self
            .send(self.http.post(self.url("v1/logout")).bearer_auth(token))
            .await?;
        match resp.status().as_u16() {
            200 | 204 => Ok(()),
            401 => Err(AccountError::Unauthorized),
            _ => Err(self.unexpected(resp).await),
        }
    }

    /// Fetches the signed-in account (email + team).
    pub async fn get_account(&self, token: &str) -> Result<Account, AccountError> {
        let resp = self
            .send(self.http.get(self.url("v1/account")).bearer_auth(token))
            .await?;
        self.json_or_auth_error(resp).await
    }

    /// Updates the account's team field (`None` clears it).
    pub async fn set_team(&self, token: &str, team: Option<&str>) -> Result<Account, AccountError> {
        let resp = self
            .send(
                self.http
                    .put(self.url("v1/account"))
                    .bearer_auth(token)
                    .json(&PutAccount {
                        team: team.map(str::to_owned),
                    }),
            )
            .await?;
        self.json_or_auth_error(resp).await
    }

    /// Fetches all of the account's settings scopes.
    pub async fn get_settings(&self, token: &str) -> Result<Settings, AccountError> {
        let resp = self
            .send(self.http.get(self.url("v1/settings")).bearer_auth(token))
            .await?;
        self.json_or_auth_error(resp).await
    }

    /// Writes one scope. `base_version` is the version this write is based on
    /// (`0` to create). A version conflict returns [`SettingsWrite::Conflict`]
    /// with the live document rather than an error.
    pub async fn put_settings(
        &self,
        token: &str,
        scope: &str,
        base_version: i32,
        data: &serde_json::Value,
    ) -> Result<SettingsWrite, AccountError> {
        let resp = self
            .send(
                self.http
                    .put(self.url(&format!("v1/settings/{scope}")))
                    .bearer_auth(token)
                    .json(&PutSettings {
                        base_version,
                        data: data.clone(),
                    }),
            )
            .await?;
        match resp.status().as_u16() {
            200 | 201 => {
                let body: VersionBody = self.decode(resp).await?;
                Ok(SettingsWrite::Written {
                    version: body.version,
                })
            }
            409 => {
                let body: ConflictBody = self.decode(resp).await?;
                Ok(SettingsWrite::Conflict {
                    version: body.version,
                    data: body.data,
                })
            }
            401 => Err(AccountError::Unauthorized),
            400 => Err(AccountError::BadRequest(self.message(resp).await)),
            _ => Err(self.unexpected(resp).await),
        }
    }

    // ---- internals -------------------------------------------------------

    async fn send(&self, builder: reqwest::RequestBuilder) -> Result<reqwest::Response, AccountError> {
        builder.send().await.map_err(|e| self.transport_error(e))
    }

    /// Turns a reqwest transport error into the most specific variant we can.
    /// If the verifier recorded a certificate that does not match the pin, the
    /// failure is definitively a pin mismatch (that is the only way an observed
    /// fingerprint can diverge from the expected one) — report it as such rather
    /// than as a generic network error, however reqwest classified it.
    fn transport_error(&self, e: reqwest::Error) -> AccountError {
        if let (Some(expected), Some(observed)) =
            (&self.expected_fingerprint, self.observed_fingerprint())
        {
            if !fingerprint_eq(expected, &observed) {
                return AccountError::Pinning(format!(
                    "the server's certificate ({observed}) does not match the pinned \
                     fingerprint ({expected}); refusing to connect"
                ));
            }
        }
        AccountError::Network(e.to_string())
    }

    async fn token(&self, resp: reqwest::Response) -> Result<String, AccountError> {
        let body: TokenBody = self.decode(resp).await?;
        Ok(body.token)
    }

    /// Decodes a JSON body, mapping `401` to [`AccountError::Unauthorized`] and
    /// anything else non-success to [`AccountError::Server`]. Used by the
    /// authenticated GET/PUT routes that share these two failure modes.
    async fn json_or_auth_error<T: serde::de::DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, AccountError> {
        match resp.status().as_u16() {
            200 | 201 => self.decode(resp).await,
            401 => Err(AccountError::Unauthorized),
            _ => Err(self.unexpected(resp).await),
        }
    }

    async fn decode<T: serde::de::DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, AccountError> {
        resp.json::<T>()
            .await
            .map_err(|e| AccountError::Network(format!("could not read server response: {e}")))
    }

    /// Reads the server's `{ "error": "…" }` message, falling back to the raw
    /// status when the body is not that shape.
    async fn message(&self, resp: reqwest::Response) -> String {
        let status = resp.status();
        match resp.json::<ErrorBody>().await {
            Ok(body) => body.error,
            Err(_) => format!("HTTP {status}"),
        }
    }

    async fn unexpected(&self, resp: reqwest::Response) -> AccountError {
        let status = resp.status().as_u16();
        AccountError::Server {
            status,
            message: self.message(resp).await,
        }
    }
}

// ---- wire types ----------------------------------------------------------

#[derive(serde::Serialize)]
struct Credentials {
    email: String,
    password: String,
}

#[derive(serde::Serialize)]
struct PutAccount {
    team: Option<String>,
}

#[derive(serde::Serialize)]
struct PutSettings {
    base_version: i32,
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct TokenBody {
    token: String,
}

#[derive(Deserialize)]
struct VersionBody {
    version: i32,
}

#[derive(Deserialize)]
struct ConflictBody {
    version: i32,
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
}
