# Account system audit

Written 2026-07-19. Scope: how identity, login, workspaces and billing are wired
into this fork today, and what a from-scratch account system would have to
replace.

**Read [`ACCOUNTS.md`](ACCOUNTS.md) first** (written 2026-07-16). It covers the
*narrative*: what upstream built, why the fork fakes login, what breaks on the
wire, and the destructive-logout warning. This file is the *code-level* half —
module-by-module inventory, hook counts, dead-surface list, and a phased
refactor plan. Where they disagree, this file is newer. They should probably be
merged once the refactor direction is settled.

Companion docs: [`docs/local-drive.md`](docs/local-drive.md),
[`docs/local-harness-lifecycle.md`](docs/local-harness-lifecycle.md).

---

## 1. TL;DR — where the fork already stands

The fork has **already cut the cord on real authentication**, but has *not* cut
the cord on the account *model*. Every launch is a hard-coded fake user:

```rust
// crates/warp_server_auth/src/auth_state.rs:165
/// Always true in this fork: there's no account system to authenticate
/// against, so every launch logs in as a permanent local fake user.
fn should_use_test_user() -> bool {
    true
}
```

That single line short-circuits `AuthState::initialize`, so the API-key path,
the `WARP_USER_SECRET` path and the secure-storage path are all dead code in
practice. The app boots as `User::test()`:

| Field | Value |
|---|---|
| `local_id` | `TEST_USER_UID` = `"test_user_uid"` |
| `metadata.email` | `TEST_USER_EMAIL` = `"test_user@warp.dev"` |
| `display_name` / `photo_url` | `None` |
| `is_onboarded` | `true` |
| `anonymous_user_type` | `None` (so `is_user_anonymous()` is false) |
| `principal_type` | `User` |
| credentials | `Credentials::Test` under `test`/`integration_tests`/`skip_login`, else `Credentials::SessionCookie` |

Consequences that matter for a refactor:

- `is_logged_in()` is **always true** (it only checks `credentials.is_some()`).
- `is_anonymous_or_logged_out()` is **always false**. All ~50 call sites that
  gate features on it are permanently in the "full account" branch.
- The login-gated-feature machinery (`attempt_login_gated_feature`, the auth
  modal, the anonymous-user upsells) is unreachable in normal operation but is
  still fully compiled and still owns a lot of UI.
- Because the fake user is not anonymous, `personal_object_limits()` would have
  been `None` anyway; the fork additionally hard-returns `None` from it
  (`auth_state.rs:449`) to kill the Drive storage banner.

**The honest summary: identity is already fake, but ~7,700 lines of auth code
and ~6,200 lines of workspace/billing code still exist to serve it.** A rewrite
is mostly a *deletion* exercise with a small local-identity core to build.

---

## 2. Module map

### 2.1 `crates/warp_server_auth` — the identity core (~1,300 LOC)

Platform-agnostic, no UI. This is the piece worth keeping (in reduced form).

| File | LOC | Role |
|---|---:|---|
| `auth_state.rs` | 594 | `AuthState` (the god object) + `AuthStateProvider` singleton |
| `credentials.rs` | 246 | `Credentials` / `AuthToken` / `LoginToken` enums |
| `user.rs` (+ `user/persistence.rs`) | 221 | `User`, `UserMetadata`, `AnonymousUserType`, `PersonalObjectLimits`, `PrincipalType` |
| `user_uid.rs` | 88 | `UserUid` — interned string newtype (`lasso::ThreadedRodeo`) |
| `anonymous_id.rs` | 35 | Stable random UUID for logged-out telemetry |

`AuthState` is a `RwLock<Option<User>>` + `RwLock<Option<Credentials>>` +
`AtomicBool` (needs_reauth) + anonymous UUID. It exposes ~35 read accessors.
It is reached everywhere via the `AuthStateProvider` singleton model:

```rust
AuthStateProvider::as_ref(ctx).get()   // -> &Arc<AuthState>
```

**Notable coupling:** `AuthState` implements
`warp_managed_secrets::ActorProvider`, so the managed-secrets crate depends on
the user id. Any replacement must keep supplying an actor uid.

### 2.2 `app/src/auth` — the login UI and orchestration (~6,400 LOC)

| File | LOC | Role |
|---|---:|---|
| `login_slide.rs` | 1309 | Full-screen login step in onboarding |
| `auth_view_body.rs` | 1105 | Login modal body (providers, email, SSO) |
| `auth_manager.rs` | 889 | `AuthManager` singleton — the *only* thing allowed to mutate `AuthState` |
| `auth_view_shared_helpers.rs` | 602 | Shared login UI chrome |
| `paste_auth_token_modal.rs` | 441 | Manual token paste fallback |
| `auth_override_warning_body.rs` | 419 | "You're signing in as a different user" flow |
| `auth_view_modal.rs` | 397 | Modal wrapper / `AuthViewVariant` |
| `mod.rs` | 324 | `log_out()` / `maybe_log_out()` — the global teardown |
| `login_failure_notification.rs` | 165 | |
| `auth_override_warning_modal.rs` | 139 | |
| `login_error_modal.rs` | 133 | |
| `web_handoff.rs` | 131 | wasm-only browser handoff |
| `needs_sso_link_view.rs` | 103 | |
| `user_properties.rs` | 70 | GraphQL `UserOutput` → `User` conversion |

`AuthManager` public surface (the seam a rewrite would reimplement or delete):

```
initialize_user_from_auth_payload   resume_interrupted_auth_payload
initialize_user_from_session_cookie refresh_user
authorize_device                    set_needs_reauth
create_anonymous_user               attempt_login_gated_feature
anonymous_user_hit_drive_object_limit
initiate_anonymous_user_linking     open_url_maybe_with_anonymous_token
copy_anonymous_user_linking_url_to_clipboard
sign_up_url  sign_in_url  upgrade_url  login_options_url  link_sso_url
set_user_onboarded  log_out
```

`AuthManagerEvent` has 9 variants (`AuthComplete`, `AuthFailed`,
`CreateAnonymousUserFailed`, `SkippedLogin`, `NeedsReauth`,
`AttemptedLoginGatedFeature`, `LoginOverrideDetected`, `MintCustomTokenFailed`,
`ReceivedDeviceAuthorizationCode`). Under the fork, only a rewrite's own
equivalents of `AuthComplete` / `log_out` are meaningful.

`LoginGatedFeature` is just `type LoginGatedFeature = &'static str;` — a
stringly-typed tag passed into `attempt_login_gated_feature`. Cheap to delete.

### 2.3 `app/src/workspaces` — the billing/entitlement model (~6,200 LOC)

This is where "account" stops meaning identity and starts meaning
**entitlements**, and it is the harder half of the refactor.

| File | LOC | Role |
|---|---:|---|
| `user_workspaces.rs` | 1768 | `UserWorkspaces` singleton — current workspace, ~40 `is_X_enabled()` gates |
| `gql_convert.rs` | 1214 | GraphQL → `Workspace`/`Team` conversion |
| `workspace.rs` | 987 | `Workspace`, `BillingMetadata`, `CustomerType`, `Tier`, usage entries |
| `update_manager.rs` | 531 | `TeamUpdateManager` — polls workspace metadata; team CRUD |
| `team.rs` | 166 | |
| `user_profiles.rs` | 103 | |

`CustomerType` has 11 variants (`Free`, `Turbo`, `SelfServe`, `Prosumer`,
`Legacy`, `Enterprise`, `Business`, `Lightspeed`, `Build`, `BuildMax`,
`Unknown`). `BillingMetadata` carries `tier`, `customer_type`,
`delinquency_status`, `service_agreements`, `ai_overages`.

**The entitlement gates are the real hooks.** `UserWorkspaces` exposes a long
tail of policy questions the rest of the app asks constantly:

```
is_active_ai_allowed            ai_allowed_for_current_team
is_prompt_suggestions_toggleable is_code_suggestions_toggleable
is_next_command_enabled          is_git_operations_ai_enabled
is_voice_enabled                 is_byo_api_key_enabled
are_member_byo_keys_allowed      is_custom_inference_enabled
are_member_byo_endpoints_allowed is_aws_bedrock_*  (5 fns)
is_gemini_enterprise_*  (4 fns)  is_ai_autonomy_allowed
ai_autonomy_settings             sandboxed_agent_settings
is_at_tier_limit_for_object_type is_at_tier_limit_for_some_warp_drive_objects
has_capacity_for_shared_notebooks has_capacity_for_shared_workflows
```

Each of these is a decision point that, post-refactor, becomes either a local
setting or a constant `true`.

### 2.4 Settings UI

- `app/src/settings_view/main_page.rs` (1,171 LOC) — the **Account page**.
  Widgets: `AccountWidget` (avatar/email/plan badge/upgrade link/Stripe portal),
  `SettingsSyncWidget`, `IapCredentialsWidget` (staging-only),
  `VersionInfoWidget`, `LogoutWidget`, and — as of this change —
  `BillingAndUsageWidget`.
- `app/src/settings_view/billing_and_usage_page.rs` (3,624) +
  `billing_and_usage_page_v2.rs` (2,243) + `billing_and_usage/` helpers —
  the two billing implementations, selected by
  `FeatureFlag::BillingAndUsagePageV2` and the workspace's plan.
- `app/src/settings_view/teams_page.rs` — team membership/invites.

**Change landed in this session:** Billing & usage is no longer a standalone
settings page. `BillingAndUsageDispatchView` dropped its `PageType`,
`SettingsPageMeta` and `SettingsPageViewHandle::BillingAndUsage` variant, and
now renders its active child directly as the final widget of the Account page.
`SettingsSection::BillingAndUsage` survives as a **navigation alias**: callers
that still ask for it (there are 8 — buy-credits banner, prompt alerts, slash
command, `warp://` URI handler, workspace action) are redirected to
`SettingsSection::Account` and scrolled to `BILLING_AND_USAGE_WIDGET_ID`.

---

## 3. Every hook, by category

Counts from `grep` across `app/src` + `crates`:

| Hook | Sites | What it gates |
|---|---:|---|
| `AuthStateProvider` referenced | 140 files | everything |
| `AuthManager` referenced | 62 files | login flows, upsells |
| `.user_id()` | 77 | object ownership, telemetry, secrets actor, sync scoping |
| `is_anonymous_or_logged_out()` | 50 | feature gating + upsell UI |
| `.is_logged_in()` | 25 | onboarding routing, polling start/stop |

### 3.1 Identity hooks (must be replaced, not deleted)

- **Object ownership.** `UserUid` is stamped on Drive objects, notebooks,
  workflows, env-var collections. `UserWorkspaces::personal_drive(ctx)` returns
  an `Owner` derived from the user id.
- **Managed secrets.** `AuthState as ActorProvider` supplies `actor_uid`.
- **Telemetry.** `app/src/server/telemetry/` stamps user id + anonymous id.
- **Crash reporting.** user id tag.
- **SQLite scoping.** `log_out()` currently *deletes the whole database*
  (`persistence::remove(...)`) because per-user scoping was never implemented —
  see the `TODO: Implement per-user scoping of sqlite3` in `auth/mod.rs:218`.

### 3.2 Entitlement hooks (candidates for deletion → constants)

Everything in §2.3's gate list, plus:

- `app/src/ai/` — 12 files consult auth/workspace state for model access.
- `app/src/terminal/buy_credits_banner.rs`, `inline_banner/prompt_suggestions.rs`,
  `ai/blocklist/prompt/prompt_alert.rs` — credit-exhaustion upsells that deep-link
  to billing.
- `app/src/billing/shared_objects_creation_denied_modal.rs`.
- `app/src/workspace/view/build_plan_migration_modal.rs`.

### 3.3 Server hooks (already largely neutralised in-fork)

- `ServerApi::get_or_refresh_access_token()` — called at ~14 sites in
  `server_api.rs`. With `Credentials::Test`/`SessionCookie` the bearer token is
  `AuthToken::NoAuth`.
- Cloud-object sync: already replaced by `local_object_client.rs`; the websocket
  listener has a fork master switch (`cloud_objects/listener.rs:40`) and notes
  that *"cloud-object sync can never authenticate under this fork's
  credentials"*.
- ~70 GraphQL operations exist under `crates/graphql/src/api/`. Account-relevant
  ones: `get_user`, `get_user_settings`, `update_user_settings`,
  `get_workspaces_metadata_for_user`, `update_workspace_settings`,
  `create_anonymous_user`, `mint_custom_token`, `set_user_is_onboarded`,
  `stripe_billing_portal`, `purchase_addon_credits`, `get_request_limit_info`,
  `get_ai_overages_for_workspace`, `get_conversation_usage`, `api_keys`,
  `generate_api_key`, `expire_api_key`, plus the whole team/invite family.

### 3.4 Onboarding

`app/src/root_view.rs` drives `AuthOnboardingState`, which has a `LoginSlide`
variant. Routing keys off `auth_state.is_logged_in()` (line 1689) — which is now
permanently true, so the login slide is effectively unreachable in the main app.
`crates/onboarding` is a standalone preview binary for the slides; the fork
already patched its `main.rs` to call `warp_core::features::mark_initialized()`
so slide rendering doesn't panic on flag access.

---

## 4. Unreachable-in-fork surface

Code that still compiles and ships but cannot execute given
`should_use_test_user() == true`.

**"Unreachable" is not "delete me."** Two of these are load-bearing for the
stated end goal (an optional, in-app account backed by our own cloud later), and
should be *kept*:

- **Firebase** is Warp's identity *provider*, not a Warp-specific concept. What
  is Warp-specific is only the two token-exchange URLs
  (`securetoken.googleapis.com`, `identitytoolkit.googleapis.com`) and the
  `/proxy/*` fallbacks. The surrounding shape — long-lived `LoginToken`
  exchanged for a short-lived `AuthToken`, refresh-on-expiry, persist to
  Keychain — is exactly what any own-cloud auth needs. Keep
  `Credentials`/`AuthToken`/`LoginToken` and the refresh plumbing; swap the
  URLs when there's something to point them at.
- **API-key auth** (`Credentials::ApiKey`, prefix `wk-`) is headless/service
  authentication: `--api-key` / `WARP_API_KEY`, consumed by `crates/warp_cli`
  and `crates/warp_tui`, backed by `PrincipalType::ServiceAccount` and the
  `api_keys` / `generate_api_key` / `expire_api_key` GraphQL ops plus the
  "Oz Cloud API Keys" settings page. It's how a CLI or CI agent authenticates
  without a browser. Keep it — it is the cheapest possible first cloud
  integration, and needs no login UI at all.

Genuinely disposable (Warp-server-specific, no role in the target design):

1. **Anonymous-user machinery** — `AnonymousUserType`, `create_anonymous_user`,
   `initiate_anonymous_user_linking`, `mint_custom_token`, the renotification
   timer (`ANONYMOUS_USER_NOTIFICATION_BLOCK_TIMER`, 7 days), every
   `is_anonymous_*` accessor, and the upsell UI they drive. This exists so
   logged-out users still get *Warp's* cloud features; under "the app just works
   without an account" it has no purpose.
2. **Billing / workspaces / teams** — §2.3 in full, plus the billing settings UI
   and the credit-exhaustion upsells.
3. **The browser-handoff login UI** — `login_slide.rs`, `auth_view_body.rs`,
   `auth_view_shared_helpers.rs`, `paste_auth_token_modal.rs`,
   `auth_override_warning_*`, `needs_sso_link_view.rs`. These implement
   *warp.dev's* OAuth redirect dance. In-app signup replaces them wholesale.
4. **Device authorization flow** — `authorize_device`,
   `ReceivedDeviceAuthorizationCode`.

Ambiguous, decide later:

- **Secure-storage persistence** (`PersistedUser::{from,remove}_secure_storage`,
  `PersistAction::{Persist,Remove}`). Currently only `DoNothing` is reachable.
  Needed again the moment a real account can hold a real token — keep.
- **`needs_reauth`** — meaningless against a test credential, meaningful against
  any real token. Keep.
- **Remote-server bearer handoff** (`apply_remote_server_auth_context`) — used by
  the remote-server daemon path, not by accounts. Orthogonal; leave alone.

Realistic deletion budget once the disposable list is done: **~8–9k lines**
(auth UI + anonymous + billing + workspaces), while the ~1,300-line
`warp_server_auth` core mostly survives.

### 4.1 Reference: the old "everything unreachable" list

For completeness, the full set of code paths that cannot execute today —
including the ones marked keep above:

1. **All of `app/src/auth`'s UI** — login slide, auth modal, SSO link view,
   token paste modal, override warning, login error/failure notifications.
2. **Anonymous-user machinery** — `AnonymousUserType`, `create_anonymous_user`,
   `initiate_anonymous_user_linking`, `mint_custom_token`, the renotification
   timer (`ANONYMOUS_USER_NOTIFICATION_BLOCK_TIMER`, 7 days), and every
   `is_anonymous_*` accessor.
3. **Firebase** — `FirebaseAuthTokens`, `FirebaseToken::{Refresh,Custom}`, the
   `securetoken.googleapis.com` / `identitytoolkit.googleapis.com` URL builders
   and the server-side proxy fallbacks.
4. **Device authorization flow** — `authorize_device`,
   `ReceivedDeviceAuthorizationCode`.
5. **Secure-storage persistence** — `PersistedUser::{from,remove}_secure_storage`,
   `PersistAction::{Persist,Remove}` (only `DoNothing` is reachable, since
   `Credentials::Test`/`SessionCookie` both map there).
6. **`needs_reauth`** — nothing can invalidate a test credential.
7. **API-key auth** — `Credentials::ApiKey`, `api_key_owner_type`,
   `is_api_key_authenticated`, the `API_KEY_PREFIX` handling. (Note: the
   `--api-key` launch arg is *accepted* by `AuthState::initialize` but the
   test-user check runs first, so it is silently ignored.)
8. **Remote-server bearer handoff** — `apply_remote_server_auth_context`.

That is roughly **5,000–6,000 lines** removable with no behaviour change, before
touching entitlements.

---

## 5. Refactor plan

### Phase 0 — the target model (decided 2026-07-19)

**Accounts are optional and additive. The app is fully functional with no
account at all.** Signup happens *in the app*, never via a website. An account,
when present, unlocks cloud services — which are no-ops for now.

This is materially different from "rip out accounts". The shape:

```
        ┌─ no account (default) ──────────────────────────────┐
        │  local identity: display name, avatar. No server.    │
        │  Everything works. Local AI auth is the user's own   │
        │  business (their CLI, their keys) — we don't see it. │
        └──────────────────────────────────────────────────────┘
                              │  optional, in-app
                              ▼
        ┌─ account ────────────────────────────────────────────┐
        │  our credentials → our cloud. No-op today.           │
        │  Reuses Credentials/AuthToken/LoginToken as-is.      │
        └──────────────────────────────────────────────────────┘
```

Three consequences that drive everything downstream:

1. **`is_logged_in()` must stop being a feature gate.** Today it is
   permanently-true load-bearing scaffolding: ~25 call sites treat it as
   "may I use this feature". In the target model it means only "do I have a
   cloud identity", which is *false* for most users, and no local feature may
   depend on it. This is the single riskiest part of the refactor — flipping
   `is_logged_in()` to honest values without first decoupling the gates would
   disable half the app.
2. **Local AI auth is out of scope, permanently.** The local harness
   (`claude`, `codex`) authenticates itself via the user's own CLI config. The
   account system must never learn about it, store it, or gate on it.
3. **`log_out()` becomes non-destructive.** With no account, there is nothing to
   log out *of*; with an account, logging out drops a token. Neither justifies
   deleting the SQLite database. The wipe goes away, and with it the per-user
   scoping TODO — local data belongs to the machine, not the cloud identity.

Local identity is a settings-backed struct, not an account:

```rust
struct LocalIdentity {
    display_name: String,           // defaults to $USER
    avatar: Option<PathBuf>,
    // no id, no email, no credentials
}
```

`UserUid` survives only as the *cloud* identity, `Option`-shaped, absent by
default. Anything currently keying local data off `user_id()` (Drive objects,
managed-secrets `actor_uid`, telemetry) needs a machine-local id instead — the
existing `anonymous_id` UUID is already exactly that and can be promoted.

Monetisation note, for whoever wires the paywall later: the intended paid
feature is **turning swear mode off**. Which means the entitlement check runs on
a *cosmetic copy setting*, not on any core capability — a pleasant constraint,
since it means no code path that does real work ever needs to ask about billing.

### Phase 1 — shrink the seam (no deletions)

Make `AuthState`'s surface small enough to reimplement. Today ~35 accessors are
called ad hoc. Collapse the entitlement questions out of it:

- Introduce `Entitlements` as a separate singleton with the ~25 `is_X_enabled()`
  answers, initially delegating to `UserWorkspaces`.
- Leave `AuthState` holding *only* identity: id, display name, email, avatar,
  onboarded flag.

This is mechanical and independently reviewable, and it makes phases 2–4 safe.

### Phase 2 — split local identity from cloud identity

Introduce `LocalIdentity` (always present) alongside `AuthState` (cloud, now
honestly `Option`). Repoint the local consumers — Drive ownership,
`ActorProvider::actor_uid`, telemetry, crash tags — at a machine-local id
promoted from the existing `anonymous_id`. Keep `AuthStateProvider` as the
accessor name so the 140 dependent files don't churn.

**Do not flip `should_use_test_user()` to `false` until this phase is complete
and phase 3 has landed.** Until then it is the only thing keeping the ~25
`is_logged_in()` gates satisfied.

### Phase 3 — constant-fold the entitlements

Replace `Entitlements`' delegating bodies with local settings or `true`. Once
nothing reads `UserWorkspaces`' gate functions, `workspaces/` collapses to
whatever team/space concept (if any) survives — likely none, since Drive is
already local-only. At the end of this phase `is_logged_in()` gates nothing, and
the test user can finally be retired.

### Phase 4 — delete

In dependency order: billing UI → upsell banners/modals → `workspaces/` →
the browser-handoff login UI → anonymous-user machinery → the billing/team/
anonymous GraphQL operations.

**Not deleted:** `Credentials` and its `Firebase`/`ApiKey`/`Bearer` variants,
the token-refresh plumbing, secure-storage persistence, `needs_reauth`. These
are the substrate for phase 5.

### Phase 5 — in-app signup (the actual new work)

Everything above is subtraction; this is the only addition.

**Status — started (transport + session layers built and tested):**

- `crates/account_client` — a fingerprint-pinned REST client for the account
  server (`docs/account-server.md`), unit- + integration-tested over a real TLS
  handshake against an in-process server.
- `app/src/account` — an `AccountSession` singleton: in-app signup/login/logout,
  token persisted to the platform-native private store, server address +
  pinned fingerprint stored alongside it, and a process-global "account active"
  flag restored at startup.
- The one account gate is **live**: `AISettings::can_disable_flavour_settings()`
  reads that flag, so turning swear/misery mode off requires an account.
- Kept deliberately **separate from `AuthState`/the fake auth** for now (so it
  can't destabilize the ~140 files that read it, and the fake-auth boot can't
  clobber the token). The token uses `Credentials::Bearer` semantics already
  (opaque server token → `Authorization: Bearer`); folding it onto the real
  `AuthState` credential happens when the fake auth is removed (phase D/4).

**Still to do:** the native signup/login **UI** (email+password modal) and the
server-address/fingerprint settings fields — `AccountSession::{set_server,
sign_up, log_in, log_out}` are the seams it drives; moving the token from the
private store to the Keychain (`PersistedUser` secure storage); and phase E
settings sync. No browser handoff, no `warp://` redirect, no Firebase URLs —
the same `Credentials` → `AuthToken` → request-header path the code already has.

### Phase 6 — settings page

The Account page becomes: local identity (editable name/avatar), a signed-out
"Create account" affordance, settings-sync toggle, version info. Billing & usage
and teams go. `SettingsSection::BillingAndUsage` and `Teams` get removed from
the enum along with their `FromStr` arms and `warp://` URI mappings — at which
point the alias redirect added in this session can be deleted along with
`BILLING_AND_USAGE_WIDGET_ID`.

---

## 6. Traps

- **`log_out()` used to delete the SQLite database.** The fork removed that
  wipe (non-destructive logout — phase 0 landed). The now-orphaned
  `persistence::remove` / `sqlite::remove` are retained behind
  `#[allow(dead_code)]` in case per-user scoping is ever implemented.
- **`UserUid` is interned globally and never freed** (`lasso::ThreadedRodeo`,
  `LazyLock`). Fine for a handful of ids; do not start minting them per-request.
- **`UserUid::default()` interns `""`** specifically to avoid a panic in
  `as_str()`. Preserve that if you reimplement it.
- **`is_anonymous_or_logged_out()` deliberately uses `unwrap_or(true)`**, not
  `unwrap_or_default()`, to fail closed during the transient
  credentials-without-user window. If identity becomes synchronous and always
  present, that window vanishes — but don't "simplify" the unwrap while the old
  async path still exists.
- **Billing v1 vs v2 selection depends on both a feature flag and plan type**
  (`BillingAndUsageDispatchView::workspace_uses_v2`). `workspace.is_none()`
  ⇒ v2. Deleting v1 first is safe; deleting v2 first is not.
- **The `--api-key` launch arg is silently ignored** because the test-user check
  precedes it. If anything in the tooling passes it expecting it to work, that
  expectation is already broken.
- **`personal_object_limits()` hard-returns `None`** — a fork-local override, not
  upstream behaviour. Don't "restore" it by reading the field.
- **Concurrent-edit hazard, 2026-07-19:** other Claude Code sessions were
  editing `app/src/settings_view/mod.rs` at the same time as this audit (a
  Warp Drive settings-page removal). Re-read that file before trusting any line
  numbers quoted here for it.

---

## 7. Rough sizing

| Area | LOC | Disposition |
|---|---:|---|
| `crates/warp_server_auth` | ~1,300 | shrink to ~200 (identity only) |
| `app/src/auth` | ~6,400 | delete ~6,000, keep a thin local-identity model |
| `app/src/workspaces` | ~6,200 | delete nearly all |
| billing settings UI | ~7,000 | delete |
| teams settings UI | ~1,500 | delete |
| account GraphQL ops | ~25 files | delete |

Order of operations matters more than total size: phases 1–2 are the only ones
that touch 100+ files, and both are rename-shaped rather than logic-shaped.
