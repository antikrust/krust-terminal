# SERVER-DESIGN.md — editing rules for `crates/account_server`

Instructions for anyone (human or AI) who touches the account/settings-sync
server. The API reference and deployment guide live in
[docs/account-server.md](docs/account-server.md); the client-side account
plan lives in [ACCOUNT-DESIGN.md](ACCOUNT-DESIGN.md) (the server is its
phase 5 backend). **This file is the contract for changing the server
without breaking its point.** The point: a personal-scale service that one
person can deploy, back up, and reason about in an afternoon.

## The invariants — do not break these

1. **Standalone, forever.** The crate depends on workspace utility deps plus
   `argon2` only. It must never depend on `app/` or any warp UI/model crate,
   and must build and run alone on a bare Linux box (`cargo build -p
   account_server --release` → one binary, bundled SQLite, no runtime deps).
   If a change needs types shared with the client, define a tiny wire type
   here and mirror it client-side — do not link the crates.

2. **Settings are opaque.** The server stores `{version, data}` JSON
   documents per (account, scope) and must never inspect, validate, or
   migrate the contents of `data`. Any "the server should understand
   setting X" idea is wrong by design — which settings exist, sync, or get
   pruned is decided entirely in the client (`SyncToCloud` declarations).

3. **Versioned writes stay versioned.** Every mutation of a settings
   document goes through `base_version` optimistic concurrency and the
   `409 {error, version, data}` conflict shape. Clients merge from that
   body. Never add a "force write" path, and never change the 409 body
   without a `/v2`.

4. **Security floor.** Passwords: argon2id via `argon2`'s defaults, never
   anything homemade. Tokens: 32 random bytes, stored **only** as SHA-256
   digests (`token_hash`) — the plaintext token exists once, in the signup/
   login response. Login must stay timing-equalized for unknown emails
   (`DUMMY_HASH` must remain a parseable argon2id string). Never log
   emails-with-passwords, tokens, or settings payloads.

5. **`/v1` is a contract.** The (future) in-app signup and the settings
   syncer transport are written against it. Additive changes (new endpoints,
   new optional response fields) are fine; changed semantics or removed
   fields require new `/v2` routes with `/v1` left working.

6. **The deployment hardening is part of the design.** The systemd unit in
   docs/account-server.md (`DynamicUser`, `ProtectSystem=strict`, state-dir
   confinement, non-web port) exists because the target box runs live sites
   (nginx, SQL) that this service must be *physically unable* to touch. If
   something fails under the sandbox, fix the service to live inside its
   state directory — do not loosen the unit.

7. **Lean is a feature.** One SQLite file in WAL mode behind a mutex is the
   intended concurrency story at personal scale. Do not introduce connection
   pools, async ORMs, background workers, or split services without an
   actual observed scale problem stated by the user.

## Known gaps — deliberate, not oversights

Listed so nobody "helpfully" adds them unprompted; each needs an explicit
product decision first:

- **No TLS** — plain HTTP on a bare IP by explicit decision ("just the IP
  for now"). The upgrade path is config-only (nginx TLS proxy in front,
  bind flips to 127.0.0.1) and is documented; it is the *only* sanctioned
  nginx interaction.
- **No token expiry or rotation** — tokens live until logout.
- **No rate limiting / lockout** — acceptable at current exposure; revisit
  if the service ever fronts real users.
- **No email verification or password reset** — accounts are cheap; the
  database is one copyable file.
- **Teams is one nullable string** on the account (`team`), matching the
  Account-page design ("teams is just one setting"). No membership tables,
  roles, or invites unless the product asks.

## Working on it

- Tests are round-trips against the real router + a temp-file SQLite
  (`src/lib_tests.rs`, `tower::ServiceExt::oneshot`). Every new endpoint or
  behavior change gets one; no mocking.
- Run them with `cargo test -p account_server`.
- Migrations are embedded diesel migrations (`migrations/`); add new ones,
  never edit shipped ones — the deployed database upgrades in place on
  restart.
- Config is env-only (`ACCOUNT_SERVER_BIND` / `_PORT` / `_DB`) so the
  systemd unit remains the whole deployment story. Resist config files.

## How the client will attach (when phase 5 lands)

- Signup/signin form → `POST /v1/signup` / `/v1/login`; token rides the
  existing `Credentials`/`AuthToken` header path (`Credentials::ApiKey`
  first for end-to-end testing).
- `cloud_preferences_syncer` keeps its payload construction; only its
  transport (Warp's cloud-objects GraphQL) is replaced with
  `GET/PUT /v1/settings/{scope}`, the 409 body feeding the existing
  local/remote reconciliation.
- Account page `team` field ↔ `GET/PUT /v1/account`.
- Do none of this before ACCOUNT-DESIGN.md phases 1–3 (the `is_logged_in()`
  decoupling) — wiring a real backend while ~25 feature gates still key off
  login state would disable half the app for signed-out users.
