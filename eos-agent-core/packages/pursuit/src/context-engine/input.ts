import type {
  AttemptId,
  LegId,
  PlanId,
  PlannerContextInput,
  PursuitContextAttempt,
  PursuitContextLeg,
  PursuitContextSnapshot,
  WorkItemId,
  WorkerContextInput,
} from "@eos/contracts";

import { composeAttemptOutcome } from "../attempt/context.js";
import type { AttemptState } from "../attempt/state.js";
import { composeLegOutcome } from "../leg/context.js";
import type { LegState } from "../leg/state.js";
import { composePursuitOutcome } from "../pursuit/context.js";
import { attemptDirPath, legDirName, pursuitRootPath } from "./projection/paths.js";
import type { PursuitTree } from "../pursuit-tree.js";

export interface PlannerLaunchLocator {
  legId: LegId;
  attemptId: AttemptId;
  planId: PlanId;
}

export interface WorkerLaunchLocator {
  legId: LegId;
  attemptId: AttemptId;
  workItemId: WorkItemId;
}

export function buildPlannerContextInput(
  tree: PursuitTree,
  locator: PlannerLaunchLocator,
): PlannerContextInput {
  return {
    kind: "planner",
    pursuit_context: snapshotPursuitContext(tree),
    current: {
      pursuit_id: tree.pursuit.id,
      leg_id: locator.legId,
      attempt_id: locator.attemptId,
      plan_id: locator.planId,
    },
  };
}

export function buildWorkerContextInput(
  tree: PursuitTree,
  locator: WorkerLaunchLocator,
): WorkerContextInput {
  return {
    kind: "worker",
    pursuit_context: snapshotPursuitContext(tree),
    current: {
      pursuit_id: tree.pursuit.id,
      leg_id: locator.legId,
      attempt_id: locator.attemptId,
      work_item_id: locator.workItemId,
    },
  };
}

export function snapshotPursuitContext(tree: PursuitTree): PursuitContextSnapshot {
  const root = pursuitRootPath(tree.pursuit.id);
  return {
    pursuit: {
      id: tree.pursuit.id,
      goal: tree.pursuit.pursuitGoal,
      leg_goal_mode: tree.pursuit.legGoalMode,
      predefined_leg_count:
        tree.pursuit.legGoalMode === "predefined" ? tree.pursuit.legGoals.length : null,
      status: tree.pursuit.status,
      context_path: root,
      outcome: tree.pursuit.status === "Running" || tree.pursuit.status === "NotStarted"
        ? null
        : composePursuitOutcome(tree.pursuit, tree.legs),
      legs: tree.legs.map((leg) => snapshotLeg(root, leg)),
    },
  };
}

function snapshotLeg(root: string, leg: LegState): PursuitContextLeg {
  return {
    id: leg.id,
    sequence: leg.sequence,
    origin: leg.origin,
    status: leg.status,
    leg_goal: leg.legGoal,
    leg_goal_version: leg.legGoalVersion,
    leg_goal_provenance: leg.legGoalProvenance,
    is_leg_goal_mutatable: leg.isLegGoalMutatable,
    next_leg_goal: leg.nextLegGoal,
    max_attempts: leg.maxAttempts,
    context_path: `${root}/${legDirName(leg.id)}`,
    outcome:
      leg.status === "Success" || leg.status === "Failed"
        ? composeLegOutcome(leg)
        : null,
    attempts: leg.attempts.map((attempt) => snapshotAttempt(root, leg, attempt)),
  };
}

function snapshotAttempt(
  root: string,
  leg: LegState,
  attempt: AttemptState,
): PursuitContextAttempt {
  const attemptPath = `${root}/${attemptDirPath(leg, attempt)}`;
  return {
    id: attempt.id,
    sequence: attempt.sequence,
    status: attempt.status,
    failure_reasons: [...attempt.failureReasons],
    is_consistent_with_leg_goal: attempt.isConsistentWithLegGoal,
    context_path: attemptPath,
    outcome:
      attempt.status === "Success" || attempt.status === "Failed"
        ? composeAttemptOutcome(attempt)
        : null,
    leg_goal_version: attempt.legGoalVersion,
    plan: {
      id: attempt.plan.id,
      status: attempt.plan.status,
      declared_leg_goal: attempt.plan.declaredLegGoal,
      declared_next_leg_goal: attempt.plan.declaredNextLegGoal,
      summary: attempt.plan.summary,
      agent_run_id: attempt.plan.agentRunId,
      leg_goal_version: attempt.plan.legGoalVersion,
    },
    work_items: attempt.workItems.map((item) => ({
      id: item.id,
      agent_name: item.agentName,
      title: item.title,
      spec: item.spec,
      depends_on: [...item.dependsOn],
      status: item.status,
      summary: item.summary,
      outcome: item.outcome,
      agent_run_id: item.agentRunId,
      context_path: `${attemptPath}/work_item_${item.id}`,
      leg_goal_version: item.legGoalVersion,
    })),
  };
}
