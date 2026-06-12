import type {
  InitialUserMessage,
  PlannerContextInput,
  PursuitContextAttempt,
  PursuitContextWorkItem,
  WorkerContextInput,
} from "@eos/contracts";

import { formatAttemptFailureReason } from "../attempt/context.js";

export type ComposeLaunchContext = (
  agentName: string,
  input: PlannerContextInput | WorkerContextInput,
  signal?: AbortSignal,
) => Promise<InitialUserMessage[]>;

export const defaultComposeLaunchContext: ComposeLaunchContext = (
  _agentName,
  input,
) =>
  Promise.resolve(
    input.kind === "planner" ? plannerMessages(input) : workerMessages(input),
  );

function user(text: string): InitialUserMessage {
  return { role: "user", content: [{ type: "text", text }] };
}

function plannerMessages(input: PlannerContextInput): InitialUserMessage[] {
  const pursuit = input.pursuit_context.pursuit;
  const leg = pursuit.legs.find((candidate) => candidate.id === input.current.leg_id);
  const messages = [
    user(`# Pursuit goal\n${pursuit.goal}`),
    user(`# Current leg goal\n${leg?.leg_goal ?? ""}`),
  ];
  if (pursuit.leg_goal_mode === "predefined") {
    messages.push(
      user(
        "Success means the full effective leg_goal is achieved. Plan work items " +
          "for the current predefined leg goal. Do not submit `leg_goal` or " +
          "`next_leg_goal`; predefined pursuits own the leg sequence. If the " +
          "predefined leg_goal is too broad or wrong, plan only work that " +
          "completes the current predefined leg_goal.",
      ),
    );
  } else {
    messages.push(
      user(
        "Dynamic mode: A new dynamic leg exists only because the previous leg " +
          "closed successfully and declared next_leg_goal. Success means the " +
          "full effective leg_goal is achieved. Use the current leg goal as-is " +
          "unless it needs refocus. You may submit `leg_goal` to replace this " +
          "leg's goal, submit successor-only `next_leg_goal`, or omit both. " +
          "If you cannot achieve the full leg_goal in this leg, submit a " +
          "narrowed leg_goal and put the remainder in next_leg_goal. Clearing " +
          "`next_leg_goal` requires a replacement `leg_goal` in the same payload.",
      ),
    );
  }

  for (const attempt of leg?.attempts ?? []) {
    if (attempt.is_consistent_with_leg_goal && attempt.status === "Failed") {
      messages.push(user(failedAttemptReport(attempt)));
    }
  }
  messages.push(user("Submit the plan via submit_planner_outcome."));
  return messages;
}

function failedAttemptReport(attempt: PursuitContextAttempt): string {
  const lines = [`# Failed attempt ${String(attempt.sequence)}`];
  for (const reason of attempt.failure_reasons) {
    lines.push(`- ${formatAttemptFailureReason(reason)}`);
  }
  if (attempt.plan.summary !== null) lines.push(`plan summary: ${attempt.plan.summary}`);
  for (const item of attempt.work_items) {
    lines.push(
      `- work_item ${item.id} [${item.status}] (${item.agent_name}): ${item.title}`,
    );
    if (item.summary !== null) lines.push(`  summary: ${item.summary}`);
    if (item.outcome !== null) lines.push(`  outcome: ${item.outcome}`);
  }
  return lines.join("\n");
}

function workerMessages(input: WorkerContextInput): InitialUserMessage[] {
  const pursuit = input.pursuit_context.pursuit;
  const leg = pursuit.legs.find((candidate) => candidate.id === input.current.leg_id);
  const attempt = leg?.attempts.find(
    (candidate) => candidate.id === input.current.attempt_id,
  );
  const item = attempt?.work_items.find(
    (candidate) => candidate.id === input.current.work_item_id,
  );
  const dependencyScope =
    item === undefined
      ? []
      : (leg?.attempts ?? [])
          .filter((candidate) => candidate.is_consistent_with_leg_goal)
          .flatMap((candidate) => candidate.work_items)
          .filter(
            (candidate) => candidate.leg_goal_version === item.leg_goal_version,
          );
  const dependencies = (item?.depends_on ?? [])
    .map((id) => dependencyScope.find((candidate) => candidate.id === id))
    .filter(
      (dependency): dependency is PursuitContextWorkItem =>
        dependency?.status === "Success",
    );

  const messages = [
    user(`# Current leg goal\n${leg?.leg_goal ?? ""}`),
    user(`# Work item title\n${item?.title ?? ""}`),
    user(`# Work item spec\n${item?.spec ?? ""}`),
  ];
  if (dependencies.length > 0) messages.splice(1, 0, user(dependencyReport(dependencies)));
  messages.push(
    user(
      "Complete only this assigned work item. Do not plan, refocus, or change legs. " +
        "Do not decide next_leg_goal. Submit via submit_worker_outcome.",
    ),
  );
  return messages;
}

function dependencyReport(dependencies: readonly PursuitContextWorkItem[]): string {
  const lines = ["# Direct dependency outcomes"];
  for (const dependency of dependencies) {
    lines.push(
      `- work_item ${dependency.id} [${dependency.status}]: ${dependency.summary ?? "(no summary)"}`,
    );
    if (dependency.outcome !== null) lines.push(`  ${dependency.outcome}`);
  }
  return lines.join("\n");
}
