# account-server: lean account + settings sync

The "real service" behind ACCOUNT-DESIGN.md phase 5: a single-binary Rust
server (crates/account_server) that stores accounts and **opaque, versioned
settings documents**. It is our own version of what Warp's cloud did for
settings sync, shrunk to one box: the client keeps its cloud-objects
machinery and will simply point its transport here instead of Warp's GraphQL
backend.

Design rules (from the 2026-07-19 discussion):

- **Lean**: axum + one SQLite file. No external database, no nginx
  involvement, no domain — plain `IP:port` on a **non-web port**
  (default `10101`).
- **Robust where it counts**: argon2id password hashes; bearer tokens stored
  as SHA-256 digests (a DB leak leaks no sessions); settings writes carry
  `base_version` and 409 with the live document on conflict, so no machine
  ever silently clobbers another.
- **Ignorant by design**: the server never learns what a setting is. "Less
  settings" is a client-side decision; the payload here is a JSON blob per
  scope (`global` plus one per platform).
- **Teams is one field**: a nullable `team` string on the account, matching
  the Account-page design.

## API (v1)

All bodies are JSON. Auth is `Authorization: Bearer <token>`.

| Method & path | Body | Returns |
|---|---|---|
| `GET /v1/health` | — | `{ok, service, api}` |
| `POST /v1/signup` | `{email, password}` | `201 {token}`; `409` if the account exists |
| `POST /v1/login` | `{email, password}` | `{token}`; `401` on bad credentials (timing-equalized for unknown emails) |
| `POST /v1/logout` | — (auth) | `204`; revokes the presented token |
| `GET /v1/account` | — (auth) | `{email, team}` |
| `PUT /v1/account` | `{team}` (auth) | updated `{email, team}` |
| `GET /v1/settings` | — (auth) | `{scopes: {<scope>: {version, data}}}` |
| `PUT /v1/settings/{scope}` | `{base_version, data}` (auth) | `{version}`; `409 {version, data}` on conflict — merge and retry |

Conventions: `base_version: 0` means "creating the first document".
Scopes are free-form (≤64 chars); the client uses `global` plus platform
names. Bodies are capped at 1 MiB.

## Build

```bash
cargo build -p account_server --release
# → target/release/account-server
```

The binary must be built **for the server's platform**. therob.lol is Linux,
so either build on the box (rustup + `git clone` + the command above) or
cross-compile (`cross build -p account_server --release --target
x86_64-unknown-linux-gnu`). SQLite is bundled; the binary has no runtime
dependencies.

## Deploying to therob.lol — without touching anything else

The box runs live sites (nginx, SQL). This service must not interact with
any of it: own unix user, own directory, own port, own SQLite file. The
systemd unit below enforces that — the process can write nowhere except its
own state directory.

```ini
# /etc/systemd/system/account-server.service
[Unit]
Description=l0 account + settings sync server
After=network.target

[Service]
DynamicUser=yes
StateDirectory=account-server
WorkingDirectory=/var/lib/account-server
Environment=ACCOUNT_SERVER_PORT=10101
Environment=ACCOUNT_SERVER_BIND=0.0.0.0
Environment=ACCOUNT_SERVER_DB=/var/lib/account-server/account-server.sqlite3
ExecStart=/usr/local/bin/account-server
Restart=on-failure

# Containment: read-only OS, no home dirs, no privilege escalation, only
# the state directory is writable. Keeps the service physically unable to
# touch nginx, the SQL server, or the live sites.
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
NoNewPrivileges=yes
CapabilityBoundingSet=
RestrictAddressFamilies=AF_INET AF_INET6
MemoryMax=256M

[Install]
WantedBy=multi-user.target
```

```bash
scp target/release/account-server you@therob.lol:/usr/local/bin/account-server
ssh you@therob.lol 'sudo systemctl daemon-reload && sudo systemctl enable --now account-server'
# open the port (ufw example; skip if the box has no firewall)
ssh you@therob.lol 'sudo ufw allow 10101/tcp'
```

Smoke test from anywhere:

```bash
curl http://<server-ip>:10101/v1/health
curl -X POST http://<server-ip>:10101/v1/signup \
  -H 'content-type: application/json' \
  -d '{"email":"you@example.com","password":"…"}'
```

Backups: the entire state is one file —
`/var/lib/account-server/account-server.sqlite3` (plus its `-wal` sidecar).
Copy it, and you have everything.

## The honest caveat: plain HTTP

With no domain there is no TLS, so signup/login credentials cross the wire
in cleartext. Acceptable for now by explicit decision ("just the IP for
now"), but treat these accounts as low-value until that changes. When it
should change, **no code changes**: put nginx in front as a TLS proxy on the
existing sites' cert (`location /` → `proxy_pass http://127.0.0.1:10101;`
on some subdomain) and flip `ACCOUNT_SERVER_BIND=127.0.0.1`. That is the
one sanctioned nginx interaction, and it is opt-in later.

## Client wiring (not yet built)

Phase 5 of ACCOUNT-DESIGN.md. The mapping when we get there:

- signup/signin form → `POST /v1/signup` / `/v1/login`; the returned token
  rides the existing `Credentials`/`AuthToken` path
  (`Credentials::ApiKey` end-to-end first, as the design doc suggests).
- `cloud_preferences_syncer`'s payload construction stays; its transport
  (cloud-objects GraphQL) is replaced with `GET/PUT /v1/settings/{scope}`,
  the 409 body feeding the existing local/remote reconciliation.
- The Account page's `team` field ↔ `GET/PUT /v1/account`.
- Which settings sync at all is decided client-side (`SyncToCloud`
  declarations) — today 236 global + 13 per-platform; pruning that list
  ("less settings") never requires a server change.
