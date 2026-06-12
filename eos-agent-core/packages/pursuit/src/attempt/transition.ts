import {
  type AttemptFailureReason,
  isPursuitEntityTerminal,
  mintAttemptId,
  type AttemptId,
  type LegId,
  type PursuitId,
  type WorkItemRunStatus,
} from "@eos/contracts";
import type { PursuitTransaction } from "@eos/db";

import { reconcileLeg } from "../leg/transition.js";
import { cancelPlan, createPlan } from "../plan/transition.js";
import { encodeFailureReasons, type PursuitTree } from "../pursuit-tree.js";
import { cancelWorkItem } from "../work-item/transition.js";

export interface AttemptScope {
  pursuitId: PursuitId;
  legId: LegId;
}

export async function createAttempt(
  trx: PursuitTransaction,
  scope: AttemptScope,
  sequence: number,
): Promise<AttemptId> {
  const leg = await trx
    .selectFrom("legs")
    .select("leg_goal_version")
    .where("id", "=", scope.legId)
    .executeTakeFirstOrThrow();
  const id = mintAttemptId();
  const now = new Date().toISOString();
  await trx
    .insertInto("attempts")
    .values({
      id,
      pursuit_id: scope.pursuitId,
      leg_id: scope.legId,
      sequence,
      leg_goal_version: leg.leg_goal_version,
      status: "NotStarted",
      failure_reasons: encodeFailureReasons([]),
      created_at: now,
      updated_at: now,
    })
    .execute();
  await createPlan(trx, { ...scope, attemptId: id }, leg.leg_goal_version);
  return id;
}

export interface AttemptRef {
  pursuitId: PursuitId;
  legId: LegId;
  attemptId: AttemptId;
}

export interface AttemptSettlementContext {
  failureReasons?: readonly AttemptFailureReason[];
}

export function plannerFailureReason(message: string): AttemptFailureReason {
  return {
    work_item_id: null,
    kind: message.startsWith("context_script_error:")
      ? "context_composition_failed"
      : "planner_failed",
    message,
    summary: null,
    outcome: null,
  };
}

export async function propagateDependencyBlocks(
  trx: PursuitTransaction,
  attemptId: AttemptId,
): Promise<void> {
  const attempt = await trx
    .selectFrom("attempts")
    .select(["leg_id", "leg_goal_version"])
    .where("id", "=", attemptId)
    .executeTakeFirst();
  if (!attempt) return;

  let changed = true;
  while (changed) {
    changed = false;
    const allVersionItems = await trx
      .selectFrom("work_items")
      .select(["key", "id", "attempt_id", "status", "depends_on"])
      .where("leg_id", "=", attempt.leg_id)
      .where("leg_goal_version", "=", attempt.leg_goal_version)
      .execute();
    const statusOf = new Map(
      allVersionItems.map((item) => [String(item.id), item.status]),
    );
    for (const item of allVersionItems) {
      if (item.attempt_id !== attemptId || item.status !== "NotStarted") continue;
      const dependsOn = decodeDependsOn(item.depends_on);
      const blockedBy = dependsOn.filter((id) => dependencyBlocks(statusOf.get(id)));
      if (blockedBy.length === 0) continue;
      const summary = blockedSummary(blockedBy);
      await trx
        .updateTable("work_items")
        .set({
          status: "Blocked",
          worker_summary: summary,
          worker_outcome: summary,
          updated_at: new Date().toISOString(),
        })
        .where("key", "=", item.key)
        .execute();
      changed = true;
    }
  }
}

export async function reconcileAttemptStatus(
  trx: PursuitTransaction,
  tree: PursuitTree,
  ref: AttemptRef,
  context: AttemptSettlementContext = {},
): Promise<void> {
  const attempt = await trx
    .selectFrom("attempts")
    .selectAll()
    .where("id", "=", ref.attemptId)
    .executeTakeFirst();
  if (!attempt || isPursuitEntityTerminal(attempt.status)) return;

  const plan = await trx
    .selectFrom("plans")
    .select(["id", "status"])
    .where("attempt_id", "=", ref.attemptId)
    .executeTakeFirst();
  const items = await trx
    .selectFrom("work_items")
    .select(["id", "status", "worker_summary", "worker_outcome", "depends_on"])
    .where("attempt_id", "=", ref.attemptId)
    .execute();
  const versionItems = await trx
    .selectFrom("work_items")
    .select(["id", "status"])
    .where("leg_id", "=", attempt.leg_id)
    .where("leg_goal_version", "=", attempt.leg_goal_version)
    .execute();
  const statusOf = new Map(versionItems.map((item) => [String(item.id), item.status]));

  let next: "Success" | "Failed" | undefined;
  let failureReasons: readonly AttemptFailureReason[] = [];
  if (plan?.status === "Failed") {
    next = "Failed";
    failureReasons =
      context.failureReasons ?? [plannerFailureReason("planner failed without a submission")];
  } else if (
    plan?.status === "Success" &&
    items.length > 0 &&
    items.every((item) => item.status === "Success")
  ) {
    next = "Success";
  } else if (
    plan?.status === "Success" &&
    items.some((item) => item.status === "Failed" || item.status === "Blocked") &&
    items.every((item) => item.status !== "Running" && item.status !== "NotStarted")
  ) {
    next = "Failed";
    failureReasons = itemFailureReasons(items, statusOf);
    if (failureReasons.length === 0) failureReasons = context.failureReasons ?? [];
  }

  if (next === undefined) return;

  const now = new Date().toISOString();
  await trx
    .updateTable("attempts")
    .set({
      status: next,
      failure_reasons: encodeFailureReasons(failureReasons),
      updated_at: now,
    })
    .where("id", "=", ref.attemptId)
    .execute();

  if (next === "Failed") {
    const leg = await trx
      .selectFrom("legs")
      .select(["status", "max_attempts"])
      .where("id", "=", ref.legId)
      .executeTakeFirst();
    const attemptCount = await trx
      .selectFrom("attempts")
      .select(trx.fn.countAll<number>().as("count"))
      .where("leg_id", "=", ref.legId)
      .executeTakeFirst();
    const spent = attemptCount?.count ?? 0;
    if (
      leg &&
      !isPursuitEntityTerminal(leg.status) &&
      spent < leg.max_attempts
    ) {
      await createAttempt(trx, { pursuitId: ref.pursuitId, legId: ref.legId }, spent + 1);
      return;
    }
  }

  await reconcileLeg(trx, tree, {
    pursuitId: ref.pursuitId,
    legId: ref.legId,
  });
}

export async function cancelAttempt(
  trx: PursuitTransaction,
  attemptId: AttemptId,
): Promise<void> {
  const attempt = await trx
    .selectFrom("attempts")
    .select("status")
    .where("id", "=", attemptId)
    .executeTakeFirst();
  if (!attempt) return;
  if (!isPursuitEntityTerminal(attempt.status)) {
    await trx
      .updateTable("attempts")
      .set({ status: "Cancelled", updated_at: new Date().toISOString() })
      .where("id", "=", attemptId)
      .execute();
  }
  const plans = await trx
    .selectFrom("plans")
    .select("id")
    .where("attempt_id", "=", attemptId)
    .execute();
  for (const plan of plans) await cancelPlan(trx, plan.id);
  const items = await trx
    .selectFrom("work_items")
    .select("key")
    .where("attempt_id", "=", attemptId)
    .execute();
  for (const item of items) await cancelWorkItem(trx, item.key);
}

function dependencyBlocks(status: WorkItemRunStatus | undefined): boolean {
  return status === "Failed" || status === "Blocked";
}

function blockedSummary(blockedBy: readonly string[]): string {
  return `blocked by ${blockedBy.map((id) => `work_item_${id}`).join(", ")}`;
}

function itemFailureReasons(
  items: readonly {
    id: string;
    status: WorkItemRunStatus;
    worker_summary: string | null;
    worker_outcome: string | null;
    depends_on: string;
  }[],
  statusOf: ReadonlyMap<string, WorkItemRunStatus>,
): AttemptFailureReason[] {
  return items
    .filter((item) => item.status === "Failed" || item.status === "Blocked")
    .map((item) => {
      if (item.status === "Blocked") {
        const blockedBy = decodeDependsOn(item.depends_on).filter((id) =>
          dependencyBlocks(statusOf.get(id)),
        );
        return {
          work_item_id: item.id,
          kind: "blocked_by_failed_dependency" as const,
          message: blockedBy.length > 0 ? blockedSummary(blockedBy) : null,
          summary: item.worker_summary,
          outcome: item.worker_outcome,
          ...(blockedBy.length > 0 && { blocked_by: blockedBy }),
        };
      }
      if (isContextCompositionFailure(item.worker_summary, item.worker_outcome)) {
        return {
          work_item_id: item.id,
          kind: "context_composition_failed" as const,
          message: item.worker_summary ?? item.worker_outcome,
          summary: item.worker_summary,
          outcome: item.worker_outcome,
        };
      }
      return {
        work_item_id: item.id,
        kind: "failed" as const,
        message: null,
        summary: item.worker_summary,
        outcome: item.worker_outcome,
      };
    });
}

function isContextCompositionFailure(
  summary: string | null,
  outcome: string | null,
): boolean {
  return (
    summary?.startsWith("context_script_error:") === true ||
    outcome?.startsWith("context_script_error:") === true
  );
}

function decodeDependsOn(raw: string): string[] {
  const parsed: unknown = JSON.parse(raw);
  return Array.isArray(parsed) ? parsed.map(String) : [];
}
