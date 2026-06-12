import type { Database } from "better-sqlite3";

export function applyPursuitMigration(database: Database): void {
  database.exec(`
    CREATE TABLE IF NOT EXISTS pursuits (
      id TEXT PRIMARY KEY,
      parent_run_id TEXT,
      pursuit_goal TEXT NOT NULL,
      leg_goal_mode TEXT NOT NULL CHECK (leg_goal_mode IN ('dynamic', 'predefined')),
      leg_goals TEXT,
      status TEXT NOT NULL CHECK (status IN ('NotStarted', 'Running', 'Success', 'Failed', 'Cancelled')),
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      closed_at TEXT
    );

    CREATE TABLE IF NOT EXISTS legs (
      id TEXT PRIMARY KEY,
      pursuit_id TEXT NOT NULL REFERENCES pursuits(id),
      sequence INTEGER NOT NULL,
      origin TEXT NOT NULL CHECK (origin IN ('initial', 'next_leg_goal', 'predefined')),
      leg_goal TEXT NOT NULL,
      leg_goal_version INTEGER NOT NULL,
      leg_goal_provenance TEXT NOT NULL,
      is_leg_goal_mutatable INTEGER NOT NULL CHECK (is_leg_goal_mutatable IN (0, 1)),
      next_leg_goal TEXT,
      max_attempts INTEGER NOT NULL,
      status TEXT NOT NULL CHECK (status IN ('NotStarted', 'Running', 'Success', 'Failed', 'Cancelled')),
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      UNIQUE (pursuit_id, sequence)
    );

    CREATE TABLE IF NOT EXISTS attempts (
      id TEXT PRIMARY KEY,
      pursuit_id TEXT NOT NULL REFERENCES pursuits(id),
      leg_id TEXT NOT NULL REFERENCES legs(id),
      sequence INTEGER NOT NULL,
      leg_goal_version INTEGER NOT NULL,
      status TEXT NOT NULL CHECK (status IN ('NotStarted', 'Running', 'Success', 'Failed', 'Cancelled')),
      failure_reasons TEXT NOT NULL,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      UNIQUE (leg_id, sequence)
    );

    CREATE TABLE IF NOT EXISTS plans (
      id TEXT PRIMARY KEY,
      pursuit_id TEXT NOT NULL REFERENCES pursuits(id),
      leg_id TEXT NOT NULL REFERENCES legs(id),
      attempt_id TEXT NOT NULL REFERENCES attempts(id),
      agent_run_id TEXT,
      status TEXT NOT NULL CHECK (status IN ('NotStarted', 'Running', 'Success', 'Failed', 'Cancelled')),
      declared_leg_goal TEXT,
      declared_next_leg_goal TEXT,
      leg_goal_version INTEGER NOT NULL,
      planner_summary TEXT,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS work_items (
      key TEXT PRIMARY KEY,
      id TEXT NOT NULL,
      pursuit_id TEXT NOT NULL REFERENCES pursuits(id),
      leg_id TEXT NOT NULL REFERENCES legs(id),
      attempt_id TEXT NOT NULL REFERENCES attempts(id),
      plan_id TEXT NOT NULL REFERENCES plans(id),
      agent_name TEXT NOT NULL,
      agent_run_id TEXT,
      status TEXT NOT NULL CHECK (status IN ('NotStarted', 'Running', 'Success', 'Failed', 'Blocked', 'Cancelled')),
      title TEXT NOT NULL,
      spec TEXT NOT NULL,
      depends_on TEXT NOT NULL,
      leg_goal_version INTEGER NOT NULL,
      worker_summary TEXT,
      worker_outcome TEXT,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS work_item_dependency_edges (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      pursuit_id TEXT NOT NULL REFERENCES pursuits(id),
      leg_id TEXT NOT NULL REFERENCES legs(id),
      attempt_id TEXT NOT NULL REFERENCES attempts(id),
      work_item_key TEXT NOT NULL REFERENCES work_items(key),
      work_item_id TEXT NOT NULL,
      depends_on_work_item_id TEXT NOT NULL,
      leg_goal_version INTEGER NOT NULL,
      created_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS launch_queue (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      pursuit_id TEXT NOT NULL REFERENCES pursuits(id),
      kind TEXT NOT NULL CHECK (kind IN ('plan', 'work_item')),
      entity_id TEXT NOT NULL,
      state TEXT NOT NULL CHECK (state IN ('queued', 'claimed')),
      launch_token TEXT,
      created_at TEXT NOT NULL
    );
  `);
}
