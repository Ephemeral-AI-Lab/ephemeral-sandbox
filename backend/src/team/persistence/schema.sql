-- Team coordination schema.
-- Run once during bootstrap. Partitions are created per-run by partitions.py.
-- No PostgreSQL extensions required.

-- Task queue (dispatcher backing store) — the only PG-backed team table.
CREATE TABLE IF NOT EXISTS tasks (
    id              TEXT NOT NULL,
    team_run_id     TEXT NOT NULL,
    agent_name      TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending',
    objective       TEXT NOT NULL,
    description     TEXT DEFAULT '',
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
    blocker_id      TEXT,
    fired_by_task_id TEXT,
    pause_checkpoint TEXT,
    pause_verdict   TEXT,
    PRIMARY KEY (id, team_run_id)
) PARTITION BY LIST (team_run_id);

-- Indexes are created per-partition by partitions.py:
--   tasks: (status, pending_dep_count, depth, created_at), (parent_id, status)

-- Blocker records — durable state for the Conductor.
CREATE TABLE IF NOT EXISTS blockers (
    id                  TEXT NOT NULL,
    team_run_id         TEXT NOT NULL,
    status              TEXT NOT NULL DEFAULT 'assessing',
    reason              TEXT NOT NULL,
    root_cause_paths    TEXT[] DEFAULT '{}',
    initiating_task_id  TEXT NOT NULL,
    suggestion          TEXT,
    fix_task_id         TEXT,
    declared_by         TEXT,
    fix_summary         TEXT,
    pending_assessments INT DEFAULT 0,
    created_at          DOUBLE PRECISION NOT NULL,
    resolved_at         DOUBLE PRECISION,
    PRIMARY KEY (id, team_run_id)
);

CREATE INDEX IF NOT EXISTS idx_blockers_active
    ON blockers (team_run_id, status)
    WHERE status NOT IN ('resolved', 'failed');
