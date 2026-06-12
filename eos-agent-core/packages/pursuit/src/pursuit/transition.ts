import {
  isPursuitEntityTerminal,
  type AgentRunId,
  type CreatePursuitInput,
  type LegGoalMode,
  type PursuitId,
} from "@eos/contracts";
import type { PursuitTransaction } from "@eos/db";

import { cancelLeg, createLeg } from "../leg/transition.js";
import { encodeStringList, type PursuitTree } from "../pursuit-tree.js";

export interface CreatePursuitInit {
  pursuitId: PursuitId;
  parentRunId?: AgentRunId | null;
  input: CreatePursuitInput;
  maxAttempts: number;
}

export async function createPursuitRows(
  trx: PursuitTransaction,
  init: CreatePursuitInit,
): Promise<void> {
  const legGoals = init.input.leg_goals ?? [];
  const legGoalMode: LegGoalMode = legGoals.length === 0 ? "dynamic" : "predefined";
  const firstLegGoal =
    legGoalMode === "dynamic" ? init.input.pursuit_goal : requireLegGoal(legGoals, 0);
  const now = new Date().toISOString();
  await trx
    .insertInto("pursuits")
    .values({
      id: init.pursuitId,
      parent_run_id: init.parentRunId,
      pursuit_goal: init.input.pursuit_goal,
      leg_goal_mode: legGoalMode,
      leg_goals: legGoalMode === "predefined" ? encodeStringList(legGoals) : null,
      status: "Running",
      created_at: now,
      updated_at: now,
      closed_at: null,
    })
    .execute();
  await createLeg(trx, init.pursuitId, {
    sequence: 1,
    origin: "initial",
    legGoal: firstLegGoal,
    legGoalProvenance:
      legGoalMode === "dynamic"
        ? "inherited from pursuit goal"
        : "predefined leg_goal[1]",
    isLegGoalMutatable: legGoalMode === "dynamic",
    nextLegGoal: legGoalMode === "predefined" ? (legGoals[1] ?? null) : null,
    maxAttempts: init.maxAttempts,
  });
}

export async function reconcilePursuit(
  trx: PursuitTransaction,
  _tree: PursuitTree,
  pursuitId: PursuitId,
): Promise<void> {
  const pursuit = await trx
    .selectFrom("pursuits")
    .select("status")
    .where("id", "=", pursuitId)
    .executeTakeFirst();
  if (!pursuit || isPursuitEntityTerminal(pursuit.status)) return;

  const legs = await trx
    .selectFrom("legs")
    .select("status")
    .where("pursuit_id", "=", pursuitId)
    .orderBy("sequence")
    .execute();
  const last = legs.at(-1);
  if (!last || (last.status !== "Success" && last.status !== "Failed")) return;

  const now = new Date().toISOString();
  await trx
    .updateTable("pursuits")
    .set({ status: last.status, updated_at: now, closed_at: now })
    .where("id", "=", pursuitId)
    .execute();
}

export async function cancelPursuit(
  trx: PursuitTransaction,
  pursuitId: PursuitId,
): Promise<void> {
  const pursuit = await trx
    .selectFrom("pursuits")
    .select("status")
    .where("id", "=", pursuitId)
    .executeTakeFirst();
  if (!pursuit) return;
  if (!isPursuitEntityTerminal(pursuit.status)) {
    const now = new Date().toISOString();
    await trx
      .updateTable("pursuits")
      .set({ status: "Cancelled", updated_at: now, closed_at: now })
      .where("id", "=", pursuitId)
      .execute();
  }
  const legs = await trx
    .selectFrom("legs")
    .select("id")
    .where("pursuit_id", "=", pursuitId)
    .execute();
  for (const leg of legs) await cancelLeg(trx, leg.id);
}

function requireLegGoal(legGoals: readonly string[], index: number): string {
  const goal = legGoals.at(index);
  if (goal === undefined) {
    throw new Error(`missing predefined leg goal at index ${String(index)}`);
  }
  return goal;
}
