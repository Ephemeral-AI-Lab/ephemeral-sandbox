-- Team coordination schema.
-- Run once during bootstrap. Partitions are created per-run by partitions.py.
-- No PostgreSQL extensions required.

-- Task queue (dispatcher backing store) — the only PG-backed team table.
CREATE TABLE IF NOT EXISTS tasks (
    id              TEXT NOT NULL,
    team_run_id     TEXT NOT NULL,
    agent_name      TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending',
    task            TEXT NOT NULL,
    deps            TEXT[] DEFAULT '{}',
    scope_paths     TEXT[] DEFAULT '{}',
    scope_ltree     TEXT[] DEFAULT '{}',
    cascade_policy  TEXT DEFAULT 'cancel',
    parent_id       TEXT,
    root_id         TEXT DEFAULT '',
    depth           INT DEFAULT 0,
    pending_dep_count INT DEFAULT 0,
    retry_count     INT DEFAULT 0,
    max_retries     INT DEFAULT 2,
    agent_run_id    TEXT,
    created_at      TIMESTAMPTZ DEFAULT NOW(),
    started_at      TIMESTAMPTZ,
    finished_at     TIMESTAMPTZ,
    failure_reason  TEXT,
    PRIMARY KEY (id, team_run_id)
) PARTITION BY LIST (team_run_id);

-- Indexes are created per-partition by partitions.py:
--   tasks: (status, pending_dep_count, depth, created_at), (parent_id, status)
