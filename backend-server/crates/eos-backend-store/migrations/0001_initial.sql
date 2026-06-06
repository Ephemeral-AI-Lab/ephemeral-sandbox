-- backend.db schema (SPEC.md "Database Shape"). Timestamps are TEXT (sqlx `time`
-- encodes OffsetDateTime as an RFC3339-ish string). JSON columns are
-- TEXT-of-validated-JSON. Model-facing (`tool_use_id`) and daemon-facing
-- (`sandbox_invocation_id`) identities are stored as separate columns and are
-- never collapsed (AC7).

CREATE TABLE run_meta (
    request_id       TEXT PRIMARY KEY,
    status           TEXT NOT NULL,
    label            TEXT,
    client_meta_json TEXT NOT NULL DEFAULT '{}',
    created_at       TEXT NOT NULL,
    finished_at      TEXT,
    cancel_reason    TEXT
);
CREATE INDEX ix_run_meta_created_at ON run_meta(created_at);

CREATE TABLE event_log (
    request_id   TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (request_id, seq)
);

CREATE TABLE obs_event (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    request_id            TEXT,
    task_id               TEXT,
    agent_run_id          TEXT,
    tool_use_id           TEXT,
    sandbox_invocation_id TEXT,
    sandbox_id            TEXT,
    source                TEXT NOT NULL,
    kind                  TEXT NOT NULL,
    payload_json          TEXT NOT NULL,
    created_at            TEXT NOT NULL
);
CREATE INDEX ix_obs_event_request_id ON obs_event(request_id);

CREATE TABLE sandbox_call_correlation (
    request_id            TEXT NOT NULL,
    task_id               TEXT NOT NULL,
    agent_run_id          TEXT NOT NULL,
    tool_use_id           TEXT NOT NULL,
    sandbox_invocation_id TEXT NOT NULL,
    caller_id             TEXT NOT NULL,
    sandbox_id            TEXT NOT NULL,
    created_at            TEXT NOT NULL,
    PRIMARY KEY (sandbox_id, caller_id, sandbox_invocation_id)
);

CREATE TABLE audit_cursor (
    sandbox_id      TEXT PRIMARY KEY,
    last_seq        INTEGER NOT NULL,
    boot_epoch_id   INTEGER NOT NULL,
    lost_before_seq INTEGER,
    dropped_count   INTEGER NOT NULL DEFAULT 0,
    updated_at      TEXT NOT NULL
);
