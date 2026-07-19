CREATE TABLE accounts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    email TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    team TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Bearer tokens, stored as SHA-256 digests so a database leak does not leak
-- live sessions.
CREATE TABLE tokens (
    token_hash TEXT PRIMARY KEY NOT NULL,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Opaque, versioned settings documents. One row per (account, scope);
-- scope is "global" or a platform name — the server never inspects the data.
CREATE TABLE settings_blobs (
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    scope TEXT NOT NULL,
    version INTEGER NOT NULL,
    data TEXT NOT NULL,
    updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (account_id, scope)
);
