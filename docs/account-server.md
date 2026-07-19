# account-server: lean account + settings sync

The "real service" behind ACCOUNT-DESIGN.md phase 5: a single-binary Rust
server (crates/account_server) that stores accounts and **opaque, versioned
settings documents**. It is our own version of what Warp's cloud did for
settings sync, shrunk to one box: the client keeps its cloud-objects
machinery and will simply point its transport here instead of Warp's GraphQL
backend.

Design rules (from the 2026-07-19 discussion):

- **Lean**: axum + one SQLite file. No external database, no nginx
  involvement, no domain — just `IP:port` on a **non-web port**
  (default `10101`).
- **Encrypted without ceremony**: TLS using a self-signed certificate the
  service mints itself on first boot; clients pin its fingerprint rather than
  trusting a CA. No Let's Encrypt, no domain validation, no renewal. See
  [TLS and pinning](#tls-and-pinning).
- **Robust where it counts**: argon2id password hashes; bearer tokens stored
  as SHA-256 digests (a DB leak leaks no sessions); settings writes carry
  `base_version` and 409 with the live document on conflict, so no machine
  ever silently clobbers another.
- **Ignorant by design**: the server never learns what a setting is. "Less
  settings" is a client-side decision; the payload here is a JSON blob per
  scope (`global` plus one per platform).
- **Teams is one field**: a nullable `team` string on the account, matching
  the Account-page design.
- **Shares are capabilities**: a shared Library object is addressed by an
  unguessable id and readable without an account, because the payload is
  ciphertext the client encrypted with a key that never reaches this server.

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
| `POST /v1/shares` | `{kind, name, data}` (auth) | `201 {id, kind, name, team, created_at}` |
| `GET /v1/shares` | — (auth) | `{shares: [{id, kind, name, team, created_at}]}` — your own, payloads omitted |
| `GET /v1/shares/{id}` | — (**no auth**) | `{id, kind, name, data, created_at}`; `404` if unknown or revoked |
| `DELETE /v1/shares/{id}` | — (auth, owner only) | `204`; `404` for unknown *or* someone else's |

Conventions: `base_version: 0` means "creating the first document".
Scopes are free-form (≤64 chars); the client uses `global` plus platform
names. Bodies are capped at 1 MiB.

For shares: `kind` (≤32 chars) is the Library object type the importing
client switches on (`workflow`, `notebook`, `env_vars`, `rule`, …); `name`
(≤200 chars) is a plaintext label so an owner can manage their own shares —
**keep secrets out of it**, it is the one part of a share this server can
read. `data` is opaque; the client encrypts it and carries the key in the
share link's fragment, which browsers and this server never see. `team` is
snapshotted at creation, so leaving a team later does not re-scope shares
you already made. Deleting is the revocation mechanism — there is no expiry.

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

Smoke test from anywhere (`-k` because the certificate is self-signed by
design — see below for checking the fingerprint instead of ignoring it):

```bash
curl -k https://<server-ip>:10101/v1/health
curl -k -X POST https://<server-ip>:10101/v1/signup \
  -H 'content-type: application/json' \
  -d '{"email":"you@example.com","password":"…"}'
```

Backups: the entire state is one file —
`/var/lib/account-server/account-server.sqlite3` (plus its `-wal` sidecar).
Copy it, and you have everything. The certificate and key sit next to it as
`cert.pem` / `key.pem`; back those up too, because replacing them makes every
pinned client refuse to connect until it re-pins.

## TLS and pinning

Traffic is encrypted, but not via a certificate authority. On first boot the
service generates a self-signed certificate into its state directory and logs
the fingerprint clients pin:

```
[INFO] generated a self-signed certificate in /var/lib/account-server
[INFO] account-server listening on https://0.0.0.0:10101
[INFO] certificate fingerprint (SHA-256): 4B:C0:4B:5D:…
```

The client records that fingerprint on first connect and refuses anything
else afterwards — SSH's host-key model. This buys full TLS 1.3 encryption
with no domain, no Let's Encrypt, no renewal, and no proxy, and it cannot be
forged by a rogue CA. Verify a server's fingerprint yourself with:

```bash
echo | openssl s_client -connect <server-ip>:10101 2>/dev/null \
  | openssl x509 -fingerprint -sha256 -noout
```

Two things to know. The certificate is valid until 2120 on purpose:
rotation breaks every pinned client, so it should be a deliberate act rather
than an annual surprise. And pinning does not protect the *very first*
connection — if that one is intercepted, the client pins the attacker, which
is why the fingerprint is printed at startup for out-of-band comparison.

`ACCOUNT_SERVER_TLS=0` serves plain HTTP instead. That is for local testing
or for terminating TLS on the same host; never expose it, since bearer tokens
ride every request.

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
