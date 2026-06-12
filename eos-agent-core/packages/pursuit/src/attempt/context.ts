import type { AttemptFailureReason } from "@eos/contracts";

import type { EntityFieldFile } from "../work-item/context.js";
import type { AttemptState } from "./state.js";

/**
 * Attempt-owned field files (§4): the accepted planner summary as
 * `plan_summary.md`, `failure_reasons.md` on failed attempts, and the derived
 * `outcome.md` once the attempt closes `Success` or `Failed`.
 */
export function attemptFieldFiles(attempt: AttemptState): EntityFieldFile[] {
  const files: EntityFieldFile[] = [];
  if (attempt.plan.summary !== null) {
    files.push({ name: "plan_summary.md", content: attempt.plan.summary });
  }
  if (attempt.status === "Failed" && attempt.failureReasons.length > 0) {
    files.push({
      name: "failure_reasons.md",
      content: attempt.failureReasons
        .map((reason) => `- ${formatAttemptFailureReason(reason)}`)
        .join("\n"),
    });
  }
  if (attempt.status === "Success" || attempt.status === "Failed") {
    files.push({ name: "outcome.md", content: composeAttemptOutcome(attempt) });
  }
  return files;
}

/**
 * §5.1: the attempt outcome is a render-time projection over the work
 * items in planner order - statuses and worker summaries only. Work-item
 * `outcome.md` content and attempt failure reasons stay separate facts.
 */
export function composeAttemptOutcome(attempt: AttemptState): string {
  if (attempt.workItems.length === 0) {
    return "# Attempt outcome\n(no work items)";
  }
  const rows = attempt.workItems.map(
    (item) =>
      `- work_item_${item.id} [${item.status}]: ${item.summary ?? "(no summary)"}`,
  );
  return ["# Attempt outcome", ...rows].join("\n");
}

export function formatAttemptFailureReason(reason: AttemptFailureReason): string {
  if (reason.kind === "failed" && reason.work_item_id !== null) {
    return `work_item_${reason.work_item_id} [Failed]: ${reason.summary ?? reason.outcome ?? reason.message ?? "(no summary)"}`;
  }
  if (
    reason.kind === "blocked_by_failed_dependency" &&
    reason.work_item_id !== null
  ) {
    return `work_item_${reason.work_item_id} [Blocked]: ${reason.summary ?? reason.message ?? blockedByText(reason.blocked_by)}`;
  }
  if (reason.kind === "context_composition_failed") {
    if (reason.work_item_id !== null) {
      return `work_item_${reason.work_item_id} [Context composition failed]: ${reason.message ?? "(no message)"}`;
    }
    return `planner [Context composition failed]: ${reason.message ?? "(no message)"}`;
  }
  return `planner [Failed]: ${reason.message ?? "(no message)"}`;
}

function blockedByText(blockedBy: readonly string[] | undefined): string {
  if (blockedBy === undefined || blockedBy.length === 0) {
    return "blocked by failed dependency";
  }
  return `blocked by ${blockedBy.map((id) => `work_item_${id}`).join(", ")}`;
}

/**
 * The superseded declaration files riding a drifted attempt (§2.8): only
 * the attempt whose plan made the now-superseded declaration carries them.
 */
export function supersededDeclarationFiles(attempt: AttemptState): EntityFieldFile[] {
  const files: EntityFieldFile[] = [];
  if (attempt.plan.declaredLegGoal !== null) {
    files.push({ name: "leg_goal.md", content: attempt.plan.declaredLegGoal });
  }
  if (attempt.plan.declaredNextLegGoal !== null) {
    files.push({ name: "next_leg_goal.md", content: attempt.plan.declaredNextLegGoal });
  }
  return files;
}
