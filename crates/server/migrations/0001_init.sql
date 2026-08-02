-- Gridline server schema.
--
-- `events` and `consents` are append-only: an event log whose rows can be
-- rewritten cannot be used to trace a routine's provenance, and a consent
-- record that can be overwritten cannot answer "what had they agreed to on
-- the day this row was captured?". Current consent is the latest row.

CREATE TABLE users (
    id         TEXT PRIMARY KEY,
    token_hash TEXT UNIQUE NOT NULL,
    is_admin   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE TABLE consents (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    actor_id             TEXT NOT NULL,
    mode                 TEXT NOT NULL,
    consent_text_version TEXT NOT NULL,
    granted_at           TEXT NOT NULL,
    revoked_at           TEXT
);

CREATE INDEX idx_consents_actor ON consents (actor_id, id);

CREATE TABLE events (
    event_id       TEXT PRIMARY KEY,
    actor_id       TEXT NOT NULL,
    session_id     TEXT NOT NULL,
    workbook_id    TEXT NOT NULL,
    seq            INTEGER NOT NULL,
    ts_ms          INTEGER NOT NULL,
    action         TEXT NOT NULL,
    payload        TEXT NOT NULL,
    context        TEXT NOT NULL,
    client_version TEXT NOT NULL,
    received_at    TEXT NOT NULL
);

CREATE INDEX idx_events_actor_session_seq ON events (actor_id, session_id, seq);
CREATE INDEX idx_events_received_at ON events (received_at);
-- Sessionization walks one actor's events in wall-clock order.
CREATE INDEX idx_events_actor_ts ON events (actor_id, ts_ms, seq);

CREATE TABLE workbooks (
    id         TEXT PRIMARY KEY,
    actor_id   TEXT NOT NULL,
    name       TEXT NOT NULL,
    state      TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_workbooks_actor ON workbooks (actor_id, updated_at);

CREATE TABLE routines (
    id                      TEXT PRIMARY KEY,
    workbook_id             TEXT NOT NULL,
    actor_id                TEXT NOT NULL,
    summary                 TEXT NOT NULL,
    body                    TEXT NOT NULL,
    estimated_minutes_saved REAL NOT NULL,
    support                 INTEGER NOT NULL,
    status                  TEXT NOT NULL DEFAULT 'proposed',
    created_at              TEXT NOT NULL
);

CREATE INDEX idx_routines_workbook ON routines (actor_id, workbook_id, estimated_minutes_saved);
