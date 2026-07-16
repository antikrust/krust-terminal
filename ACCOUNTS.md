# Accounts in 0term

Everything we know about how accounts, authentication, and the "user" concept
work in this fork of Warp's open-source release — what upstream built, what the
fork changed, what is currently broken-on-purpose, and where we want to take it.

Written 2026-07-16. File paths are current as of that date; function names are
given instead of line numbers where possible.

## TL;DR

- Upstream Warp requires a cloud account (Firebase-backed) for almost all AI
  features. This fork **fakes the login**: every launch boots as a permanent
  local test user (`test_user@warp.dev`), no network auth ever happens.
- All requests to Warp's servers therefore go out **unauthenticated** and fail
  with `Unauthorized: User not in context`. The local Claude/Codex harness
  doesn't care; a few upstream code paths needed silencing (see below).
- **Do not click "Log out"** — upstream's logout deletes the local sqlite
  database ("Logout v0" wipes sessions so accounts don't leak into each other).
  Ripping that out is on the roadmap.
- End goal: replace the whole account system with a **local profile** (display
  name + optional per-provider API keys), no server, no Firebase, no billing.

## The fork's change (all of it)

One function. `crates/warp_server_auth/src/auth_state.rs`:

```rust
fn should_use_test_user() -> bool {
    true
}
```

Upstream returns true only for `cfg(test)`, `skip_login`/`test-util` feature
builds, and the Integration channel. With it always-true,
`AuthState::initialize()` short-circuits at step 1 of its normal order:

1. Test user            ← we always stop here now
2. Provided API key (`--api-key` style injection)
3. `WARP_USER_SECRET` (a **compile-time** `option_env!`, baked into the binary)
4. Persisted user from secure storage (Keychain on macOS)

The test user is `User::test()` (`crates/warp_server_auth/src/user.rs`):

| field        | value                                   |
|--------------|-----------------------------------------|
| uid          | `test_user_uid` (`user_uid.rs` const)    |
| email        | `test_user@warp.dev` (`user_uid.rs`)     |
| onboarded    | `true` (skips onboarding UI)             |
| anonymous    | no — a "real" account as far as UI goes  |

Credentials are set to `Credentials::SessionCookie` (the fork also removed the
`cfg` gates around `test_credentials()`; in a normal desktop build the `Test`
variant doesn't exist, and `SessionCookie` is the variant that maps to
**`AuthToken::NoAuth`** — see `crates/warp_server_client/src/auth/session.rs`
and `credentials.rs`: every token/header accessor returns `None`). That is why
the app is "logged in" locally but every server call is anonymous.

## What "logged in" gates, and why faking it works

`AuthState::is_logged_in()` is just `credentials.is_some()` — so the fake
SessionCookie satisfies every gate:

- `AgentDriver::new` (`app/src/ai/agent_sdk/driver.rs`) returns
  `AgentDriverError::NotLoggedIn` without it. This is the driver used for
  cloud/Oz agents; our local harness bypasses it entirely, but the gate must
  pass for assorted UI to consider agents usable.
- `HarnessAvailabilityModel::refresh()` (`app/src/ai/harness_availability.rs`)
  no-ops when logged out; when "logged in" it calls
  `get_available_harnesses()` against the server — which now fails and just
  logs `Failed to fetch available harnesses`. Harmless; the model picker task
  will stop calling it altogether.
- The root view decides between auth screens and the workspace based on auth
  state; the fake user lands us in the workspace.
- The user menu (avatar, top right) shows the email and a Log out item when a
  non-anonymous user exists — hence `test_user@warp.dev` in the corner.

## What breaks (and what we silenced)

With `AuthToken::NoAuth` on the wire:

- **Every GraphQL query/mutation** against `app.warp.dev` returns
  `Unauthorized: User not in context`. Sources: conversation metadata fetches,
  harness availability, billing/workspace info (`UserWorkspaces`), Warp Drive,
  telemetry uploads, changelog fetches.
- **Websockets** (CloudObjects live-sync) would retry forever; the fork
  hard-disables the listener via `CLOUD_OBJECTS_LISTENER_ENABLED = false` in
  `app/src/server/cloud_objects/listener.rs`. Flip it back if real auth ever
  returns.
- **Post-run metadata fetch**: local-harness conversations carry a placeholder
  (empty) server token; `mark_response_stream_completed_successfully`
  (`app/src/ai/blocklist/history_model.rs`) skips the fetch for empty tokens
  instead of spamming errors.

Everything else just logs a warning and moves on; the app is fully usable.

## The upstream account machinery (for reference / removal planning)

Where it all lives:

- `crates/warp_server_auth/` — `AuthState` (current user + credentials),
  `Credentials` enum (`Firebase(FirebaseAuthTokens)` with id+refresh token,
  `ApiKey`, `Bearer`, `SessionCookie`, `Test`), `User`/`UserMetadata`,
  persistence to secure storage (`PersistedUser`), anonymous-id generation.
- `crates/warp_server_client/src/auth/` — turning credentials into request
  auth (`AuthToken`), refresh-token exchange.
- `app/src/auth/` — the UI and orchestration:
  - `auth_manager.rs` — `AuthManager` singleton: login/logout state machine,
    CSRF (`pending_auth_state`), token refresh, `create_anonymous_user`,
    needs-reauth events.
  - `login_slide.rs`, `auth_view_modal.rs`, `auth_view_body.rs` — the login
    screens (browser-based OAuth handoff to warp.dev).
  - `paste_auth_token_modal.rs` — manual fallback: paste a token from the
    browser when the custom-URI callback fails.
  - `web_handoff.rs` — wasm/web build: derive auth from the browser session.
  - `needs_sso_link_view.rs`, `login_error_modal.rs`,
    `login_failure_notification.rs` — edge-case UI.
- Login flow on desktop (upstream): login screen → opens browser to
  warp.dev auth → server redirects to a `warp://` URI with a token → app
  exchanges it for Firebase credentials → persisted to Keychain →
  `AuthManagerEvent` swaps the root view into the workspace.
- **Anonymous users**: upstream can mint a server-side anonymous account
  (`create_anonymous_user`) so logged-out users still get cloud features; the
  user menu's "Sign up" item converts it. Irrelevant under fake auth (our test
  user is not anonymous), but the code paths are still present.
- **Billing**: `UserWorkspaces` fetches workspace + plan metadata; the user
  menu shows "Upgrade" (or "Billing and usage" on paid plans) off that. With
  the server dead it always shows "Upgrade" linking to warp.dev — rip-out
  candidate.

## ⚠ Log out is destructive

`app/src/auth/mod.rs::log_out()` (dispatched from the user menu's "Log out"):

1. sends telemetry, resets codebase indexing,
2. **deletes the sqlite database** (`persistence::remove(...)` — upstream's
   "Logout v0": no per-user scoping, so they nuke everything),
3. clears `AuthManager` state and Keychain persistence,
4. resets every AI/history/cloud model in memory.

Under fake auth there is no way back to a login screen that works — you'd
restart into the test user again, minus your local history. Until we remove or
repurpose the menu item, don't press it.

## Roadmap: proper local accounts for 0term

The direction (not yet implemented):

1. **Local profile instead of User::test()** — a settings-backed identity
   (display name, avatar initial, email optional) read at startup; the user
   menu shows it. No credentials concept at all, or a simple enum of
   per-provider API keys (Anthropic/OpenAI/local) for the model-picker work.
2. **Neuter `log_out`** — either remove the menu item or turn it into
   "Switch profile" without the database wipe.
3. **Strip the auth UI** — `app/src/auth/` login slides/modals, anonymous-user
   machinery, needs-reauth plumbing, `warp_server_auth` persistence; keep a
   thin `AuthState` shim so `is_logged_in()` keeps returning true for the many
   upstream gates (or progressively delete the gates).
4. **Stop the unauthenticated server traffic** at the source (ServerApi
   clients) rather than silencing errors one call site at a time.
5. Longer-term: per-profile data dirs so multiple local profiles get separate
   history databases (what upstream's "Logout v0" TODO wanted anyway).
