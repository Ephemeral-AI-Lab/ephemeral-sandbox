import type {
  AgentRunId,
  AttemptId,
  LegGoalMode,
  LegId,
  PlanId,
  PursuitEntityRunStatus,
  PursuitId,
  WorkItemId,
  WorkItemRunStatus,
} from "@eos/contracts";
import type { Generated, Selectable } from "kysely";

export interface PursuitsTable {
  id: PursuitId;
  parent_run_id: AgentRunId | null;
  pursuit_goal: string;
  leg_goal_mode: LegGoalMode;
  /** JSON-encoded string array; null for dynamic mode. */
  leg_goals: string | null;
  status: PursuitEntityRunStatus;
  created_at: string;
  updated_at: string;
  closed_at: string | null;
}

export interface LegsTable {
  id: LegId;
  pursuit_id: PursuitId;
  sequence: number;
  origin: "initial" | "next_leg_goal" | "predefined";
  leg_goal: string;
  leg_goal_version: number;
  leg_goal_provenance: string;
  is_leg_goal_mutatable: number;
  next_leg_goal: string | null;
  max_attempts: number;
  status: PursuitEntityRunStatus;
  created_at: string;
  updated_at: string;
}

export interface AttemptsTable {
  id: AttemptId;
  pursuit_id: PursuitId;
  leg_id: LegId;
  sequence: number;
  leg_goal_version: number;
  status: PursuitEntityRunStatus;
  /** JSON-encoded AttemptFailureReason array. */
  failure_reasons: string;
  created_at: string;
  updated_at: string;
}

export interface PlansTable {
  id: PlanId;
  pursuit_id: PursuitId;
  leg_id: LegId;
  attempt_id: AttemptId;
  agent_run_id: AgentRunId | null;
  status: PursuitEntityRunStatus;
  declared_leg_goal: string | null;
  declared_next_leg_goal: string | null;
  leg_goal_version: number;
  planner_summary: string | null;
  created_at: string;
  updated_at: string;
}

export interface WorkItemsTable {
  /** Internal storage/launch key scoped by leg goal version. */
  key: string;
  id: WorkItemId;
  pursuit_id: PursuitId;
  leg_id: LegId;
  attempt_id: AttemptId;
  plan_id: PlanId;
  agent_name: string;
  agent_run_id: AgentRunId | null;
  status: WorkItemRunStatus;
  title: string;
  spec: string;
  /** JSON-encoded WorkItemId array. */
  depends_on: string;
  leg_goal_version: number;
  worker_summary: string | null;
  worker_outcome: string | null;
  created_at: string;
  updated_at: string;
}

export interface WorkItemDependencyEdgesTable {
  id: Generated<number>;
  pursuit_id: PursuitId;
  leg_id: LegId;
  attempt_id: AttemptId;
  work_item_key: string;
  work_item_id: WorkItemId;
  depends_on_work_item_id: WorkItemId;
  leg_goal_version: number;
  created_at: string;
}

export interface LaunchQueueTable {
  id: Generated<number>;
  pursuit_id: PursuitId;
  kind: "plan" | "work_item";
  entity_id: string;
  state: "queued" | "claimed";
  launch_token: string | null;
  created_at: string;
}

export interface PursuitDatabase {
  pursuits: PursuitsTable;
  legs: LegsTable;
  attempts: AttemptsTable;
  plans: PlansTable;
  work_items: WorkItemsTable;
  work_item_dependency_edges: WorkItemDependencyEdgesTable;
  launch_queue: LaunchQueueTable;
}

export type PursuitRow = Selectable<PursuitsTable>;
export type LegRow = Selectable<LegsTable>;
export type AttemptRow = Selectable<AttemptsTable>;
export type PlanRow = Selectable<PlansTable>;
export type WorkItemRow = Selectable<WorkItemsTable>;
export type WorkItemDependencyEdgeRow = Selectable<WorkItemDependencyEdgesTable>;
export type LaunchQueueRow = Selectable<LaunchQueueTable>;
