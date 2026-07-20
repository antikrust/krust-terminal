//! The fork's own account layer.
//!
//! Creating an account against our account server ([`account_client`]) unlocks
//! exactly two things, by design (see ACCOUNT-DESIGN.md):
//!   1. settings sync (wired later — phase E), and
//!   2. turning swear/misery mode off — the one account-gated feature, checked
//!      via [`has_account`].
//!
//! Everything else in the app works with no account at all.
//!
//! ## Relationship to the (still-present) fake auth
//!
//! Upstream Warp's `AuthState`/`Credentials` machinery is still in the tree and
//! still boots a fake local "test user" (see ACCOUNTS.md). This module is
//! deliberately kept *separate* from it for now: signing in here does not touch
//! `AuthState`, so it cannot destabilize the ~140 files that read it, and the
//! fake-auth boot cannot clobber our token. When the fake auth is finally
//! removed (phase D), this session becomes the source of truth and its token
//! moves onto the existing `Credentials::Bearer` path.
//!
//! ## Why a process-global flag
//!
//! [`can_disable_flavour_settings`](crate::settings::ai::AISettings::can_disable_flavour_settings)
//! is consulted by the swear/misery getters, some of which are called from hot
//! paths in files another session actively edits. Rather than thread an
//! `AppContext` through all of them, "is an account active" is published as a
//! process-global flag that any code can read without a context. [`AccountSession`]
//! keeps it in sync.

// The signup/login UI that drives this module's public API (`sign_up`,
// `log_in`, `log_out`, `set_server`, `AccountEvent`, …) lands in a later pass;
// until then those seams are legitimately unused. `has_account`,
// `set_account_active`, and `new` are already live (the swear/misery gate and
// the startup registration). Remove this allow when the UI wires the rest up.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};

use account_client::{Account, AccountClient, AccountError, ServerConfig};
use warp_core::user_preferences::GetUserPreferences;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

pub mod signup_modal;

/// Registers the account modal's key bindings. Call once at startup.
pub fn init(app: &mut AppContext) {
    signup_modal::init(app);
}

/// Private-preference keys. These live in the platform-native store, never in
/// the user-visible settings file.
const SERVER_URL_KEY: &str = "account.server_url";
const SERVER_FINGERPRINT_KEY: &str = "account.server_fingerprint";
const TOKEN_KEY: &str = "account.token";
const EMAIL_KEY: &str = "account.email";

/// Process-global "an account is active" flag. See the module docs for why this
/// is a context-free global rather than a settings read.
static ACCOUNT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether the user is currently signed in to an account. This is the single
/// gate for the one account-only capability: turning swear/misery mode off.
/// With no account, it returns `false` and those settings stay locked on.
pub fn has_account() -> bool {
    ACCOUNT_ACTIVE.load(Ordering::Relaxed)
}

fn set_account_active(active: bool) {
    ACCOUNT_ACTIVE.store(active, Ordering::Relaxed);
}

/// Emitted so a view (the signup/login UI, the account settings row) can react
/// to sign-in state changes without polling. `AccountError` is not `Clone`, so
/// failures are carried as their display message.
#[derive(Debug, Clone)]
pub enum AccountEvent {
    /// A signup or login succeeded; carries the freshly fetched account.
    SignedIn(Account),
    /// The user signed out (locally cleared; the server token is best-effort
    /// revoked).
    SignedOut,
    /// A signup/login attempt failed. The string is user-facing.
    Failed(String),
}

/// The account we're signed in as, plus the bearer token authenticating it.
#[derive(Clone)]
struct ActiveSession {
    token: String,
    account: Account,
}

/// Singleton that owns the account session and talks to the account server.
///
/// Async work follows the app's standard pattern: a `&self` method builds a
/// client and `ctx.spawn`s the request, and the completion callback (`&mut
/// self`) writes the result back and emits an [`AccountEvent`].
pub struct AccountSession {
    session: Option<ActiveSession>,
}

impl AccountSession {
    /// Restores a persisted session at startup so a signed-in user stays signed
    /// in across restarts (and keeps the swear/misery toggle available).
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let session = match (read_pref(ctx, TOKEN_KEY), read_pref(ctx, EMAIL_KEY)) {
            (Some(token), Some(email)) => Some(ActiveSession {
                token,
                account: Account { email, team: None },
            }),
            _ => None,
        };
        if session.is_some() {
            set_account_active(true);
        }
        Self { session }
    }

    /// Whether a session is currently active.
    pub fn is_signed_in(&self) -> bool {
        self.session.is_some()
    }

    /// The signed-in account, if any.
    pub fn account(&self) -> Option<&Account> {
        self.session.as_ref().map(|s| &s.account)
    }

    /// The configured server URL (for a settings/UI field), if set.
    pub fn server_url(ctx: &AppContext) -> Option<String> {
        read_pref(ctx, SERVER_URL_KEY)
    }

    /// Records the account server's address and (optionally) pinned certificate
    /// fingerprint. If the fingerprint is `None`, the first connection pins
    /// whatever the server presents (trust on first use) and stores it.
    pub fn set_server(ctx: &AppContext, url: &str, fingerprint: Option<&str>) {
        write_pref(ctx, SERVER_URL_KEY, url.trim());
        match fingerprint.map(str::trim).filter(|f| !f.is_empty()) {
            Some(fp) => write_pref(ctx, SERVER_FINGERPRINT_KEY, fp),
            None => remove_pref(ctx, SERVER_FINGERPRINT_KEY),
        }
    }

    /// Creates an account and signs in.
    pub fn sign_up(&self, email: String, password: String, ctx: &mut ModelContext<Self>) {
        self.authenticate(email, password, true, ctx);
    }

    /// Signs in to an existing account.
    pub fn log_in(&self, email: String, password: String, ctx: &mut ModelContext<Self>) {
        self.authenticate(email, password, false, ctx);
    }

    fn authenticate(
        &self,
        email: String,
        password: String,
        signup: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        let client = match Self::client(ctx) {
            Ok(client) => client,
            Err(err) => {
                ctx.emit(AccountEvent::Failed(err.to_string()));
                return;
            }
        };
        let had_pin = read_pref(ctx, SERVER_FINGERPRINT_KEY).is_some();
        let _ = ctx.spawn(
            async move {
                let token = if signup {
                    client.signup(&email, &password).await?
                } else {
                    client.login(&email, &password).await?
                };
                let account = client.get_account(&token).await?;
                // Trust-on-first-use: report the fingerprint we just pinned (if
                // we didn't already have one) so it can be persisted.
                let learned_fingerprint = if had_pin {
                    None
                } else {
                    client.observed_fingerprint()
                };
                Ok::<_, AccountError>((token, account, learned_fingerprint))
            },
            Self::on_authenticated,
        );
    }

    fn on_authenticated(
        &mut self,
        result: Result<(String, Account, Option<String>), AccountError>,
        ctx: &mut ModelContext<Self>,
    ) {
        match result {
            Ok((token, account, learned_fingerprint)) => {
                write_pref(ctx, TOKEN_KEY, &token);
                write_pref(ctx, EMAIL_KEY, &account.email);
                if let Some(fingerprint) = learned_fingerprint {
                    write_pref(ctx, SERVER_FINGERPRINT_KEY, &fingerprint);
                }
                self.session = Some(ActiveSession {
                    token,
                    account: account.clone(),
                });
                set_account_active(true);
                ctx.emit(AccountEvent::SignedIn(account));
            }
            Err(err) => ctx.emit(AccountEvent::Failed(err.to_string())),
        }
    }

    /// Signs out: clears the local session immediately (so the swear/misery gate
    /// closes at once) and best-effort revokes the token server-side. The server
    /// URL and pinned fingerprint are kept so the next login is one step.
    pub fn log_out(&mut self, ctx: &mut ModelContext<Self>) {
        if let (Ok(client), Some(session)) = (Self::client(ctx), self.session.as_ref()) {
            let token = session.token.clone();
            let _ = ctx.spawn(
                async move {
                    let _ = client.logout(&token).await;
                },
                |_this: &mut Self, _res: (), _ctx: &mut ModelContext<Self>| {},
            );
        }
        self.session = None;
        set_account_active(false);
        remove_pref(ctx, TOKEN_KEY);
        remove_pref(ctx, EMAIL_KEY);
        ctx.emit(AccountEvent::SignedOut);
    }

    /// Builds a client from the stored server config, or an error if no server
    /// has been configured yet.
    fn client(ctx: &AppContext) -> Result<AccountClient, AccountError> {
        let base_url = read_pref(ctx, SERVER_URL_KEY).ok_or_else(|| {
            AccountError::Config(
                "no account server configured — set its address first".to_owned(),
            )
        })?;
        AccountClient::new(ServerConfig {
            base_url,
            pinned_fingerprint: read_pref(ctx, SERVER_FINGERPRINT_KEY),
        })
    }
}

impl Entity for AccountSession {
    type Event = AccountEvent;
}

impl SingletonEntity for AccountSession {}

// --- private preference helpers -------------------------------------------

/// Reads a non-empty private preference, treating any backend error or an empty
/// stored value as "absent".
fn read_pref(ctx: &AppContext, key: &str) -> Option<String> {
    ctx.private_user_preferences()
        .read_value(key)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
}

fn write_pref(ctx: &AppContext, key: &str, value: &str) {
    if let Err(err) = ctx
        .private_user_preferences()
        .write_value(key, value.to_owned())
    {
        log::warn!("account: failed to persist {key}: {err}");
    }
}

fn remove_pref(ctx: &AppContext, key: &str) {
    let _ = ctx.private_user_preferences().remove_value(key);
}
