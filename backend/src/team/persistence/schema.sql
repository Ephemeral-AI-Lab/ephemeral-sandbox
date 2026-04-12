-- Team coordination schema (Section 14.4).
-- Run once during bootstrap. Partitions are created per-run by partitions.py.

CREATE EXTENSION IF NOT EXISTS ltree;

-- Task Center backing store
CREATE TABLE IF NOT EXISTS task_notes (
    id          UUID NOT NULL DEFAULT gen_random_uuid(),
    team_run_id TEXT NOT NULL,
    task_id     TEXT NOT NULL,
    agent_name  TEXT NOT NULL,
    content     TEXT NOT NULL,
    scope_ltree ltree[] DEFAULT '{}',
    created_at  TIMESTAMPTZ DEFAULT NOW(),
    PRIMARY KEY (id, team_run_id)
) PARTITION BY LIST (team_run_id);

-- Ledger backing store
CREATE TABLE IF NOT EXISTS file_changes (
    id          BIGSERIAL NOT NULL,
    team_run_id TEXT NOT NULL,
    file_path   TEXT NOT NULL,
    path_ltree  ltree NOT NULL DEFAULT '',
    agent_id    TEXT NOT NULL,
    edit_type   TEXT DEFAULT 'edit',
    old_hash    TEXT DEFAULT '',
    new_hash    TEXT DEFAULT '',
    created_at  TIMESTAMPTZ DEFAULT NOW(),
    PRIMARY KEY (id, team_run_id)
) PARTITION BY LIST (team_run_id);

-- Task queue (dispatcher backing store)
CREATE TABLE IF NOT EXISTS tasks (
    id              TEXT NOT NULL,
    team_run_id     TEXT NOT NULL,
    agent_name      TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending',
    task            TEXT NOT NULL,
    deps            TEXT[] DEFAULT '{}',
    scope_paths     TEXT[] DEFAULT '{}',
    scope_ltree     ltree[] DEFAULT '{}',
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

-- Exploration cache (shared across runs, not partitioned)
CREATE TABLE IF NOT EXISTS exploration_memory (
    cache_key    TEXT PRIMARY KEY,
    scope_paths  TEXT[] NOT NULL,
    content_hash TEXT NOT NULL,
    notes        JSONB NOT NULL,
    created_at   TIMESTAMPTZ DEFAULT NOW(),
    accessed_at  TIMESTAMPTZ DEFAULT NOW()
);

-- Indexes are created per-partition automatically when partitions are created.
-- These template indexes guide what each partition gets:
--   task_notes: (task_id), GiST(scope_ltree), BRIN(created_at), GIN(tsvector)
--   file_changes: GiST(path_ltree), BRIN(created_at)
--   tasks: (team_run_id, status), (team_run_id, depth, created_at)
