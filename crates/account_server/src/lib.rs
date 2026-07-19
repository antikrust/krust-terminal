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

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod schema;
use schema::{accounts, settings_blobs, tokens};

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
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

/// Serves until ctrl-c / SIGTERM.
pub async fn serve(state: AppState, addr: SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log::info!("account-server listening on {addr}");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
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

async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
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

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
