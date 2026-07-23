//! Settings sync against the account server.
//!
//! The transport already exists and is tested: `account_client`'s
//! `get_settings` / `put_settings` talk to the server's `GET/PUT
//! /v1/settings/{scope}`, which stores an opaque JSON blob per scope with
//! optimistic-concurrency versioning. This module is the glue that turns the
//! app's local settings into that blob and back, using the `SettingsManager`
//! primitives (`is_current_value_syncable`, `read_local_setting_value`,
//! `update_setting_with_storage_key`).
//!
//! ## What it does (and deliberately doesn't, yet)
//!
//! A sync runs when you sign in and at startup if a session was restored:
//!   1. GET the `global` scope. If present, apply its values locally
//!      (server-authoritative for keys it contains), remembering the version.
//!   2. Serialize the full local syncable state and PUT it, so keys the server
//!      didn't have are uploaded. The result is the union of both sides, which
//!      converges across devices over successive sign-ins.
//!
//! There is intentionally **no push-on-every-change** here yet: that needs a
//! change subscription guarded against the echo of applying remote values, and
//! is easy to get subtly wrong without a live server to test against. Sync at
//! sign-in already delivers cross-device settings; continuous push is a
//! follow-up. Scope is `global` only for now (platform scopes later).

use ::settings::SettingsManager;
use account_client::{AccountClient, ScopeDocument, SettingsWrite};
use serde_json::{Map, Value};
use derpui::{AppContext, Entity, ModelContext, SingletonEntity};

use super::AccountSession;

const GLOBAL_SCOPE: &str = "global";

/// Singleton that owns settings-sync state (just the last-seen version, for
/// optimistic concurrency).
pub struct AccountSettingsSync {
    /// Version of the `global` scope from our last successful sync, used as the
    /// `base_version` of the next write. `0` means "no document yet".
    base_version: i32,
}

impl AccountSettingsSync {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let me = Self { base_version: 0 };
        // If a session was restored at startup, pull settings right away.
        if AccountSession::as_ref(ctx).is_signed_in() {
            me.sync(ctx);
        }
        me
    }

    /// Runs a sync: pull the remote `global` scope, apply it, then push the
    /// merged local state. Best-effort — any failure is logged and dropped; the
    /// next sign-in tries again.
    pub fn sync(&self, ctx: &mut ModelContext<Self>) {
        let Some(token) = AccountSession::as_ref(ctx).token() else {
            return;
        };
        let client = match AccountSession::client(ctx) {
            Ok(client) => client,
            Err(err) => {
                log::warn!("account settings sync: no client: {err}");
                return;
            }
        };

        let _ = ctx.spawn(
            async move {
                let settings = client.get_settings(&token).await?;
                let remote = settings.scopes.get(GLOBAL_SCOPE).cloned();
                Ok::<_, account_client::AccountError>((client, token, remote))
            },
            Self::on_remote_fetched,
        );
    }

    fn on_remote_fetched(
        &mut self,
        result: Result<
            (AccountClient, String, Option<ScopeDocument>),
            account_client::AccountError,
        >,
        ctx: &mut ModelContext<Self>,
    ) {
        let (client, token, remote) = match result {
            Ok(value) => value,
            Err(err) => {
                log::warn!("account settings sync: fetch failed: {err}");
                return;
            }
        };

        // 1. Apply the remote values locally (server wins for keys it holds).
        let base_version = if let Some(doc) = remote {
            apply_blob(&doc.data, ctx);
            doc.version
        } else {
            0
        };
        self.base_version = base_version;

        // 2. Push the full local syncable state, so local-only keys are uploaded
        //    and the server ends up with the union.
        let data = Value::Object(local_syncable_blob(ctx));
        let _ = ctx.spawn(
            async move {
                let outcome = client
                    .put_settings(&token, GLOBAL_SCOPE, base_version, &data)
                    .await?;
                Ok::<_, account_client::AccountError>(outcome)
            },
            Self::on_pushed,
        );
    }

    fn on_pushed(
        &mut self,
        result: Result<SettingsWrite, account_client::AccountError>,
        ctx: &mut ModelContext<Self>,
    ) {
        match result {
            Ok(SettingsWrite::Written { version }) => {
                self.base_version = version;
            }
            Ok(SettingsWrite::Conflict { version, data }) => {
                // Someone wrote between our fetch and push. Apply their document
                // and adopt its version; the next sync reconciles any local-only
                // keys again.
                apply_blob(&data, ctx);
                self.base_version = version;
            }
            Err(err) => {
                log::warn!("account settings sync: push failed: {err}");
            }
        }
    }
}

impl Entity for AccountSettingsSync {
    type Event = ();
}

impl SingletonEntity for AccountSettingsSync {}

/// Collects the current values of every syncable setting into a `{storage_key:
/// value}` JSON object.
fn local_syncable_blob(ctx: &AppContext) -> Map<String, Value> {
    let manager = SettingsManager::as_ref(ctx);
    let mut blob = Map::new();
    for key in manager.all_storage_keys() {
        if manager.is_current_value_syncable(key, ctx).unwrap_or(false) {
            if let Ok(Some(value)) = manager.read_local_setting_value(key, ctx) {
                blob.insert(key.clone(), Value::String(value));
            }
        }
    }
    blob
}

/// Applies a `{storage_key: value}` blob to local settings, marking each write
/// as coming from cloud sync. Unknown keys and non-string values are skipped.
fn apply_blob(data: &Value, ctx: &mut ModelContext<AccountSettingsSync>) {
    let Some(object) = data.as_object() else {
        return;
    };
    SettingsManager::handle(ctx).update(ctx, |manager, ctx| {
        for (key, value) in object {
            if let Some(value) = value.as_str() {
                if let Err(err) =
                    manager.update_setting_with_storage_key(key, value.to_owned(), true, ctx)
                {
                    log::debug!("account settings sync: skipped {key}: {err}");
                }
            }
        }
    });
}
