-- Chat messages. One global room for now: every signed-in account sees every
-- message. The `team` column is snapshotted at post time (like shares.team) so
-- team-scoped rooms can filter on it later without re-scoping old messages;
-- nothing filters on it yet.
CREATE TABLE messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    -- The author's email, denormalized so a listing needs no join and leaving
    -- an account does not erase who said what.
    author_email TEXT NOT NULL,
    body TEXT NOT NULL,
    -- The author's team at post time; NULL for the global room.
    team TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX messages_id ON messages (id);
