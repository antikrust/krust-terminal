//! The fork's account + settings-sync server: the "real service" behind
//! ACCOUNT-DESIGN.md phase 5, sized for one small box.
//!
//! Deliberately lean: one binary, one SQLite file, a bearer-token REST API on
//! a non-web port. The server stores **opaque, versioned settings documents**
//! per (account, scope) — it never learns what a setting is, so the client
//! owns all merge semantics and the payload can shrink as the fork prunes
//! settings. Teams are one nullable column on the account, matching the
//! product call that "teams is just one setting".
//!
//! Robustness comes from three choices, not from features:
//! - passwords are argon2id-hashed and tokens are stored as SHA-256 digests,
//!   so a database leak leaks neither credentials nor live sessions;
//! - settings PUTs carry `base_version` and 409 on mismatch (optimistic
//!   concurrency) — a stale client re-reads and re-merges instead of
//!   clobbering another machine's write;
//! - SQLite runs in WAL mode with a busy timeout, behind a mutex — plenty for
//!   a personal-scale service and immune to connection-pool complexity.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod schema;
pub mod tls;
use schema::{accounts, messages, settings_blobs, shares, tokens};

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

/// Settings documents are preference JSON, not conversation data; 1 MiB is an
/// order of magnitude above what the client ships today.
const MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    db: Arc<Mutex<SqliteConnection>>,
}

/// Opens (creating if needed) the database and returns the app state.
pub fn open_state(db_path: &Path) -> anyhow::Result<AppState> {
    let mut conn = SqliteConnection::establish(&db_path.to_string_lossy())?;
    diesel::sql_query("PRAGMA journal_mode = WAL;").execute(&mut conn)?;
    diesel::sql_query("PRAGMA busy_timeout = 5000;").execute(&mut conn)?;
    diesel::sql_query("PRAGMA foreign_keys = ON;").execute(&mut conn)?;
    conn.run_pending_migrations(MIGRATIONS)
        .map_err(|e| anyhow::anyhow!("migrations failed: {e}"))?;
    Ok(AppState {
        db: Arc::new(Mutex::new(conn)),
    })
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/signup", post(signup))
        .route("/v1/login", post(login))
        .route("/v1/logout", post(logout))
        .route("/v1/account", get(get_account).put(put_account))
        .route("/v1/settings", get(get_settings))
        .route("/v1/settings/{scope}", put(put_settings))
        .route("/v1/shares", get(list_shares).post(create_share))
        .route("/v1/shares/{id}", get(get_share).delete(delete_share))
        .route("/v1/chat", get(get_chat).post(post_chat))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

/// How the service accepts connections.
pub enum Transport {
    /// TLS using a self-signed certificate in the given directory, generated
    /// on first boot. This is the intended production setting: the client
    /// pins the certificate's fingerprint, so no CA or domain is involved.
    Tls { dir: std::path::PathBuf },
    /// Plain HTTP, for local testing or for sitting behind a TLS-terminating
    /// proxy on the same host. Anything on the network path can read traffic,
    /// including bearer tokens, so never expose this directly.
    Plaintext,
}

/// Serves until ctrl-c / SIGTERM.
pub async fn serve(state: AppState, addr: SocketAddr, transport: Transport) -> anyhow::Result<()> {
    match transport {
        Transport::Plaintext => {
            let listener = tokio::net::TcpListener::bind(addr).await?;
            log::warn!("account-server listening on http://{addr} — traffic is NOT encrypted");
            axum::serve(listener, router(state))
                .with_graceful_shutdown(shutdown_signal())
                .await?;
            Ok(())
        }
        Transport::Tls { dir } => {
            let certificate = tls::ensure_certificate(&dir, vec!["localhost".to_owned()])?;
            let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
                &certificate.cert_path,
                &certificate.key_path,
            )
            .await?;

            log::info!("account-server listening on https://{addr}");
            log::info!(
                "certificate fingerprint (SHA-256): {}",
                certificate.fingerprint
            );
            log::info!("clients pin this on first connect; compare it if one ever warns");

            // axum-server drives shutdown through a handle rather than a
            // future, so translate the same signal into a graceful stop.
            let handle = axum_server::Handle::new();
            tokio::spawn({
                let handle = handle.clone();
                async move {
                    shutdown_signal().await;
                    handle.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
                }
            });

            axum_server::bind_rustls(addr, config)
                .handle(handle)
                .serve(router(state).into_make_service())
                .await?;
            Ok(())
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CredentialsBody {
    email: String,
    password: String,
}

#[derive(Serialize)]
struct TokenBody {
    token: String,
}

#[derive(Serialize)]
struct AccountBody {
    email: String,
    team: Option<String>,
}

#[derive(Deserialize)]
struct PutAccountBody {
    team: Option<String>,
}

#[derive(Serialize)]
struct ScopeBody {
    version: i32,
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct PutSettingsBody {
    /// The version this write is based on; 0 for "no document yet".
    base_version: i32,
    data: serde_json::Value,
}

#[derive(Serialize)]
struct PutSettingsResponse {
    version: i32,
}

#[derive(Deserialize)]
struct CreateShareBody {
    kind: String,
    name: String,
    /// Ciphertext. The client encrypts before sending and keeps the key in the
    /// share link's fragment, so this server never holds a means to read it.
    data: String,
}

#[derive(Serialize)]
struct ShareSummary {
    id: String,
    kind: String,
    name: String,
    team: Option<String>,
    created_at: String,
}

#[derive(Serialize)]
struct ShareBody {
    id: String,
    kind: String,
    name: String,
    data: String,
    created_at: String,
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
}

type ApiError = (StatusCode, Json<ErrorBody>);

fn err(status: StatusCode, error: &'static str) -> ApiError {
    (status, Json(ErrorBody { error }))
}

fn internal<E: std::fmt::Display>(e: E) -> ApiError {
    log::error!("internal error: {e}");
    err(StatusCode::INTERNAL_SERVER_ERROR, "internal")
}

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

fn hash_token(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex(&bytes)
}

fn normalized_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

/// Share ids are capability URLs: 32 unguessable random bytes. Unlike auth
/// tokens they are stored in the clear, because the payload they address is
/// already ciphertext this server cannot decrypt — hashing the id would
/// protect nothing that the client-side encryption does not already protect.
fn new_share_id() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex(&bytes)
}

/// Resolves the bearer token in `headers` to an account id.
fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<i32, ApiError> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing bearer token"))?
        .to_owned();
    let mut db = state.db.lock().map_err(internal)?;
    tokens::table
        .filter(tokens::token_hash.eq(hash_token(&token)))
        .select(tokens::account_id)
        .first::<i32>(&mut *db)
        .optional()
        .map_err(internal)?
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "invalid token"))
}

fn issue_token(db: &mut SqliteConnection, account_id: i32) -> Result<String, ApiError> {
    let token = new_token();
    diesel::insert_into(tokens::table)
        .values((
            tokens::token_hash.eq(hash_token(&token)),
            tokens::account_id.eq(account_id),
        ))
        .execute(db)
        .map_err(internal)?;
    Ok(token)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "service": "account-server", "api": 1 }))
}

async fn signup(
    State(state): State<AppState>,
    Json(body): Json<CredentialsBody>,
) -> Result<(StatusCode, Json<TokenBody>), ApiError> {
    let email = normalized_email(&body.email);
    if email.is_empty() || !email.contains('@') {
        return Err(err(StatusCode::BAD_REQUEST, "invalid email"));
    }
    if body.password.len() < 8 {
        return Err(err(StatusCode::BAD_REQUEST, "password too short"));
    }
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(body.password.as_bytes(), &salt)
        .map_err(internal)?
        .to_string();

    let mut db = state.db.lock().map_err(internal)?;
    let inserted = diesel::insert_into(accounts::table)
        .values((
            accounts::email.eq(&email),
            accounts::password_hash.eq(&password_hash),
        ))
        .on_conflict_do_nothing()
        .execute(&mut *db)
        .map_err(internal)?;
    if inserted == 0 {
        return Err(err(StatusCode::CONFLICT, "account exists"));
    }
    let account_id = accounts::table
        .filter(accounts::email.eq(&email))
        .select(accounts::id)
        .first::<i32>(&mut *db)
        .map_err(internal)?;
    let token = issue_token(&mut db, account_id)?;
    Ok((StatusCode::CREATED, Json(TokenBody { token })))
}

async fn login(
    State(state): State<AppState>,
    Json(body): Json<CredentialsBody>,
) -> Result<Json<TokenBody>, ApiError> {
    let email = normalized_email(&body.email);
    let mut db = state.db.lock().map_err(internal)?;
    let row = accounts::table
        .filter(accounts::email.eq(&email))
        .select((accounts::id, accounts::password_hash))
        .first::<(i32, String)>(&mut *db)
        .optional()
        .map_err(internal)?;
    // Verify against a constant dummy hash when the account is missing so
    // login timing doesn't reveal which emails exist.
    let (account_id, password_hash) = row.unwrap_or((-1, DUMMY_HASH.to_string()));
    let parsed = PasswordHash::new(&password_hash).map_err(internal)?;
    let verified = Argon2::default()
        .verify_password(body.password.as_bytes(), &parsed)
        .is_ok();
    if !verified || account_id < 0 {
        return Err(err(StatusCode::UNAUTHORIZED, "wrong email or password"));
    }
    let token = issue_token(&mut db, account_id)?;
    Ok(Json(TokenBody { token }))
}

/// argon2id hash of an unguessable throwaway string, used only to equalize
/// login timing for unknown emails.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$wJUnbYyyEUENQZ4CU3Bhcg$Sib9jzGnCyvUJhO8SmClh8oPuF0eSNhkc9dw4qb7Sq8";

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<StatusCode, ApiError> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "missing bearer token"))?;
    let mut db = state.db.lock().map_err(internal)?;
    diesel::delete(tokens::table.filter(tokens::token_hash.eq(hash_token(token))))
        .execute(&mut *db)
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_account(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AccountBody>, ApiError> {
    let account_id = authenticate(&state, &headers)?;
    let mut db = state.db.lock().map_err(internal)?;
    let (email, team) = accounts::table
        .filter(accounts::id.eq(account_id))
        .select((accounts::email, accounts::team))
        .first::<(String, Option<String>)>(&mut *db)
        .map_err(internal)?;
    Ok(Json(AccountBody { email, team }))
}

async fn put_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PutAccountBody>,
) -> Result<Json<AccountBody>, ApiError> {
    let account_id = authenticate(&state, &headers)?;
    let mut db = state.db.lock().map_err(internal)?;
    diesel::update(accounts::table.filter(accounts::id.eq(account_id)))
        .set(accounts::team.eq(&body.team))
        .execute(&mut *db)
        .map_err(internal)?;
    let (email, team) = accounts::table
        .filter(accounts::id.eq(account_id))
        .select((accounts::email, accounts::team))
        .first::<(String, Option<String>)>(&mut *db)
        .map_err(internal)?;
    Ok(Json(AccountBody { email, team }))
}

async fn get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let account_id = authenticate(&state, &headers)?;
    let mut db = state.db.lock().map_err(internal)?;
    let rows = settings_blobs::table
        .filter(settings_blobs::account_id.eq(account_id))
        .select((
            settings_blobs::scope,
            settings_blobs::version,
            settings_blobs::data,
        ))
        .load::<(String, i32, String)>(&mut *db)
        .map_err(internal)?;
    let mut scopes = serde_json::Map::new();
    for (scope, version, data) in rows {
        let data: serde_json::Value = serde_json::from_str(&data).map_err(internal)?;
        scopes.insert(
            scope,
            serde_json::json!({ "version": version, "data": data }),
        );
    }
    Ok(Json(serde_json::json!({ "scopes": scopes })))
}

async fn put_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath(scope): UrlPath<String>,
    Json(body): Json<PutSettingsBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let account_id = authenticate(&state, &headers)?;
    if scope.is_empty() || scope.len() > 64 {
        return Err(err(StatusCode::BAD_REQUEST, "invalid scope"));
    }
    let data = serde_json::to_string(&body.data).map_err(internal)?;
    let mut db = state.db.lock().map_err(internal)?;

    let current = settings_blobs::table
        .filter(settings_blobs::account_id.eq(account_id))
        .filter(settings_blobs::scope.eq(&scope))
        .select((settings_blobs::version, settings_blobs::data))
        .first::<(i32, String)>(&mut *db)
        .optional()
        .map_err(internal)?;

    match current {
        None => {
            if body.base_version != 0 {
                return Err(err(StatusCode::CONFLICT, "version conflict"));
            }
            diesel::insert_into(settings_blobs::table)
                .values((
                    settings_blobs::account_id.eq(account_id),
                    settings_blobs::scope.eq(&scope),
                    settings_blobs::version.eq(1),
                    settings_blobs::data.eq(&data),
                ))
                .execute(&mut *db)
                .map_err(internal)?;
            Ok((
                StatusCode::OK,
                Json(serde_json::json!(PutSettingsResponse { version: 1 })),
            ))
        }
        Some((version, current_data)) => {
            if body.base_version != version {
                // Hand the current document back so the client can merge
                // without a second round trip.
                let current_data: serde_json::Value =
                    serde_json::from_str(&current_data).map_err(internal)?;
                return Ok((
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({
                        "error": "version conflict",
                        "version": version,
                        "data": current_data,
                    })),
                ));
            }
            let next = version + 1;
            diesel::update(
                settings_blobs::table
                    .filter(settings_blobs::account_id.eq(account_id))
                    .filter(settings_blobs::scope.eq(&scope))
                    .filter(settings_blobs::version.eq(version)),
            )
            .set((
                settings_blobs::version.eq(next),
                settings_blobs::data.eq(&data),
                settings_blobs::updated_at.eq(diesel::dsl::now),
            ))
            .execute(&mut *db)
            .map_err(internal)?;
            Ok((
                StatusCode::OK,
                Json(serde_json::json!(PutSettingsResponse { version: next })),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Shares
// ---------------------------------------------------------------------------

/// Caps on the two fields the server does look at. `kind` is a short tag the
/// importing client switches on; `name` is a label for listings. Neither is
/// interpreted here beyond these bounds.
const MAX_KIND_LEN: usize = 32;
const MAX_NAME_LEN: usize = 200;

async fn create_share(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateShareBody>,
) -> Result<(StatusCode, Json<ShareSummary>), ApiError> {
    let account_id = authenticate(&state, &headers)?;
    let kind = body.kind.trim().to_owned();
    let name = body.name.trim().to_owned();
    if kind.is_empty() || kind.len() > MAX_KIND_LEN {
        return Err(err(StatusCode::BAD_REQUEST, "invalid kind"));
    }
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(err(StatusCode::BAD_REQUEST, "invalid name"));
    }
    if body.data.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "empty payload"));
    }

    let id = new_share_id();
    let mut db = state.db.lock().map_err(internal)?;
    // Snapshot the sharer's team now; see the migration's note on why this is
    // denormalized rather than joined at read time.
    let team = accounts::table
        .filter(accounts::id.eq(account_id))
        .select(accounts::team)
        .first::<Option<String>>(&mut *db)
        .map_err(internal)?;

    diesel::insert_into(shares::table)
        .values((
            shares::id.eq(&id),
            shares::account_id.eq(account_id),
            shares::kind.eq(&kind),
            shares::name.eq(&name),
            shares::data.eq(&body.data),
            shares::team.eq(&team),
        ))
        .execute(&mut *db)
        .map_err(internal)?;

    let created_at = shares::table
        .filter(shares::id.eq(&id))
        .select(shares::created_at)
        .first::<chrono::NaiveDateTime>(&mut *db)
        .map_err(internal)?;

    Ok((
        StatusCode::CREATED,
        Json(ShareSummary {
            id,
            kind,
            name,
            team,
            created_at: created_at.and_utc().to_rfc3339(),
        }),
    ))
}

/// Deliberately unauthenticated: possession of the id *is* the credential, so
/// a link works for a recipient who has no account. The payload is ciphertext,
/// and the key lives in the link fragment the client never sends here.
async fn get_share(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<ShareBody>, ApiError> {
    let mut db = state.db.lock().map_err(internal)?;
    let row = shares::table
        .filter(shares::id.eq(&id))
        .select((shares::kind, shares::name, shares::data, shares::created_at))
        .first::<(String, String, String, chrono::NaiveDateTime)>(&mut *db)
        .optional()
        .map_err(internal)?;

    let (kind, name, data, created_at) =
        row.ok_or_else(|| err(StatusCode::NOT_FOUND, "no such share"))?;
    Ok(Json(ShareBody {
        id,
        kind,
        name,
        data,
        created_at: created_at.and_utc().to_rfc3339(),
    }))
}

/// Revokes a share. Scoped to the owner, so knowing an id is enough to *read*
/// a share but never enough to delete someone else's.
async fn delete_share(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<StatusCode, ApiError> {
    let account_id = authenticate(&state, &headers)?;
    let mut db = state.db.lock().map_err(internal)?;
    let deleted = diesel::delete(
        shares::table
            .filter(shares::id.eq(&id))
            .filter(shares::account_id.eq(account_id)),
    )
    .execute(&mut *db)
    .map_err(internal)?;

    if deleted == 0 {
        // Same response whether the share is missing or belongs to someone
        // else: no reason to confirm the existence of other people's ids.
        return Err(err(StatusCode::NOT_FOUND, "no such share"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Lists the caller's own shares. Payloads are omitted — a listing is for
/// managing and revoking, not for bulk-downloading ciphertext.
async fn list_shares(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    let account_id = authenticate(&state, &headers)?;
    let mut db = state.db.lock().map_err(internal)?;
    let rows = shares::table
        .filter(shares::account_id.eq(account_id))
        .order(shares::created_at.desc())
        .select((
            shares::id,
            shares::kind,
            shares::name,
            shares::team,
            shares::created_at,
        ))
        .load::<(
            String,
            String,
            String,
            Option<String>,
            chrono::NaiveDateTime,
        )>(&mut *db)
        .map_err(internal)?;

    let shares: Vec<ShareSummary> = rows
        .into_iter()
        .map(|(id, kind, name, team, created_at)| ShareSummary {
            id,
            kind,
            name,
            team,
            created_at: created_at.and_utc().to_rfc3339(),
        })
        .collect();
    Ok(Json(serde_json::json!({ "shares": shares })))
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

/// One global room for now: every signed-in account posts to and reads the same
/// stream. Team-scoped rooms arrive later by filtering on `messages.team`.
const MAX_MESSAGE_LEN: usize = 4000;
/// Cap on how many messages one poll returns.
const MAX_MESSAGES: i64 = 200;

#[derive(Deserialize)]
struct PostMessageBody {
    body: String,
}

#[derive(Deserialize)]
struct ChatQuery {
    /// Return messages with an id greater than this; omit for the whole (capped)
    /// history.
    since: Option<i32>,
}

#[derive(Serialize)]
struct MessageBody {
    id: i32,
    author_email: String,
    body: String,
    created_at: String,
}

async fn post_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PostMessageBody>,
) -> Result<(StatusCode, Json<MessageBody>), ApiError> {
    let account_id = authenticate(&state, &headers)?;
    let text = body.body.trim();
    if text.is_empty() || text.len() > MAX_MESSAGE_LEN {
        return Err(err(StatusCode::BAD_REQUEST, "invalid message"));
    }
    let mut db = state.db.lock().map_err(internal)?;
    // Snapshot the author's email and team at post time (denormalized on the row).
    let (author_email, team) = accounts::table
        .filter(accounts::id.eq(account_id))
        .select((accounts::email, accounts::team))
        .first::<(String, Option<String>)>(&mut *db)
        .map_err(internal)?;
    diesel::insert_into(messages::table)
        .values((
            messages::account_id.eq(account_id),
            messages::author_email.eq(&author_email),
            messages::body.eq(text),
            messages::team.eq(&team),
        ))
        .execute(&mut *db)
        .map_err(internal)?;
    // The single locked connection makes "the newest row" the one we just wrote.
    let (id, created_at) = messages::table
        .order(messages::id.desc())
        .select((messages::id, messages::created_at))
        .first::<(i32, chrono::NaiveDateTime)>(&mut *db)
        .map_err(internal)?;
    Ok((
        StatusCode::CREATED,
        Json(MessageBody {
            id,
            author_email,
            body: text.to_owned(),
            created_at: created_at.and_utc().to_rfc3339(),
        }),
    ))
}

/// Reads the global room. Requires a valid token (you must have an account to
/// see the room) but returns every message regardless of team — team filtering
/// arrives with team rooms.
async fn get_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ChatQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authenticate(&state, &headers)?;
    let since = query.since.unwrap_or(0);
    let mut db = state.db.lock().map_err(internal)?;
    let rows = messages::table
        .filter(messages::id.gt(since))
        .order(messages::id.asc())
        .limit(MAX_MESSAGES)
        .select((
            messages::id,
            messages::author_email,
            messages::body,
            messages::created_at,
        ))
        .load::<(i32, String, String, chrono::NaiveDateTime)>(&mut *db)
        .map_err(internal)?;
    let messages: Vec<MessageBody> = rows
        .into_iter()
        .map(|(id, author_email, body, created_at)| MessageBody {
            id,
            author_email,
            body,
            created_at: created_at.and_utc().to_rfc3339(),
        })
        .collect();
    Ok(Json(serde_json::json!({ "messages": messages })))
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
