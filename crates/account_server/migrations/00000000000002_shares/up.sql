-- Shared objects: one row per thing someone handed to someone else.
--
-- Unlike settings_blobs (private to one account), a share is addressed by an
-- unguessable id and is readable without authentication — that is the point:
-- the owner sends a link and the recipient needs no account. `data` is
-- ciphertext the client encrypted with a key that never reaches this server,
-- so a database leak yields nothing readable.
CREATE TABLE shares (
    id TEXT PRIMARY KEY NOT NULL,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    -- Which kind of Library object this is ("workflow", "notebook", ...).
    -- Stored so the importing client knows how to parse the plaintext; the
    -- server never interprets it beyond a length check.
    kind TEXT NOT NULL,
    -- Human label for listings. Deliberately NOT encrypted so an owner can see
    -- what they have shared without holding every key; keep secrets out of it.
    name TEXT NOT NULL,
    data TEXT NOT NULL,
    -- The sharer's team at the moment of sharing, denormalized on purpose:
    -- leaving a team later should not retroactively re-scope old shares.
    team TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX shares_account_id ON shares (account_id);
