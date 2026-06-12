import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

import { ContextScriptOutputSchema } from "@eos/contracts";
import { executeJsonCommand } from "@eos/scripts";
import { eosAgentsPath } from "@eos/testkit";

const SCRIPT_ROOT = eosAgentsPath("pursuit/scripts");

function plannerInput() {
  return {
    kind: "planner",
    pursuit_context: {
      pursuit: {
        id: "p-1",
        goal: "ship it",
        leg_goal_mode: "dynamic",
        predefined_leg_count: null,
        status: "Running",
        context_path: "pursuit_p-1",
        outcome: null,
        legs: [
          {
            id: "leg-1",
            sequence: 1,
            origin: "initial",
            status: "Running",
            leg_goal: "ship parser",
            leg_goal_version: 1,
            leg_goal_provenance: "inherited from pursuit goal",
            is_leg_goal_mutatable: true,
            next_leg_goal: "ship printer",
            max_attempts: 2,
            context_path: "pursuit_p-1/leg_leg-1",
            outcome: null,
            attempts: [
              {
                id: "attempt-1",
                sequence: 1,
                status: "Running",
                failure_reasons: [],
                is_consistent_with_leg_goal: true,
                context_path: "pursuit_p-1/leg_leg-1/attempt_attempt-1",
                outcome: null,
                leg_goal_version: 1,
                plan: {
                  id: "plan-1",
                  status: "Running",
                  declared_leg_goal: null,
                  declared_next_leg_goal: null,
                  summary: null,
                  agent_run_id: null,
                  leg_goal_version: 1,
                },
                work_items: [] as Record<string, unknown>[],
              },
            ],
          },
        ],
      },
    },
    current: {
      pursuit_id: "p-1",
      leg_id: "leg-1",
      attempt_id: "attempt-1",
      plan_id: "plan-1",
    },
  };
}

function workerInput() {
  const input = plannerInput();
  const leg = input.pursuit_context.pursuit.legs[0];
  leg.attempts[0].work_items = [
    {
      id: "work-1",
      agent_name: "worker",
      title: "Parser",
      spec: "Implement parser",
      depends_on: [],
      status: "Running",
      summary: null,
      outcome: null,
      agent_run_id: null,
      context_path: `${leg.attempts[0].context_path}/work_item_work-1`,
      leg_goal_version: 1,
    },
  ];
  return {
    kind: "worker",
    pursuit_context: input.pursuit_context,
    current: {
      pursuit_id: "p-1",
      leg_id: "leg-1",
      attempt_id: "attempt-1",
      work_item_id: "work-1",
    },
  };
}

function repeatedDependencyWorkerInput() {
  const input = plannerInput();
  const baseLeg = input.pursuit_context.pursuit.legs[0];
  const baseAttempt = baseLeg.attempts[0];
  const leg = baseLeg as {
    leg_goal_version: number;
    attempts: Record<string, unknown>[];
  };
  leg.leg_goal_version = 2;
  leg.attempts = [
    {
      ...baseAttempt,
      id: "attempt-old",
      sequence: 1,
      status: "Failed",
      is_consistent_with_leg_goal: false,
      leg_goal_version: 1,
      context_path: "pursuit_p-1/leg_leg-1/superseded/attempt_attempt-old",
      plan: {
        ...baseAttempt.plan,
        id: "plan-old",
        status: "Success",
        summary: "old plan",
        leg_goal_version: 1,
      },
      work_items: [
        {
          id: "base",
          agent_name: "worker",
          title: "Old base",
          spec: "Old base",
          depends_on: [],
          status: "Success",
          summary: "old base done",
          outcome: "old base done",
          agent_run_id: null,
          context_path:
            "pursuit_p-1/leg_leg-1/superseded/attempt_attempt-old/work_item_base",
          leg_goal_version: 1,
        },
      ],
    },
    {
      ...baseAttempt,
      id: "attempt-current",
      sequence: 2,
      status: "Running",
      is_consistent_with_leg_goal: true,
      leg_goal_version: 2,
      context_path: "pursuit_p-1/leg_leg-1/attempt_attempt-current",
      plan: {
        ...baseAttempt.plan,
        id: "plan-current",
        status: "Success",
        summary: "current plan",
        leg_goal_version: 2,
      },
      work_items: [
        {
          id: "base",
          agent_name: "worker",
          title: "Current base",
          spec: "Current base",
          depends_on: [],
          status: "Success",
          summary: "current base done",
          outcome: "current base done",
          agent_run_id: null,
          context_path: "pursuit_p-1/leg_leg-1/attempt_attempt-current/work_item_base",
          leg_goal_version: 2,
        },
        {
          id: "follow",
          agent_name: "worker",
          title: "Follow",
          spec: "Use base",
          depends_on: ["base"],
          status: "Running",
          summary: null,
          outcome: null,
          agent_run_id: null,
          context_path:
            "pursuit_p-1/leg_leg-1/attempt_attempt-current/work_item_follow",
          leg_goal_version: 2,
        },
      ],
    },
  ];
  return {
    kind: "worker",
    pursuit_context: input.pursuit_context,
    current: {
      pursuit_id: "p-1",
      leg_id: "leg-1",
      attempt_id: "attempt-current",
      work_item_id: "follow",
    },
  };
}

async function runScript(name: string, input: unknown): Promise<string> {
  const result = await executeJsonCommand(
    { command: `"${process.execPath}" "${resolve(SCRIPT_ROOT, name)}"` },
    input,
  );
  expect(result.kind).toBe("exited");
  if (result.kind !== "exited") return "";
  expect(result.code).toBe(0);
  const output = ContextScriptOutputSchema.parse(JSON.parse(result.stdout));
  return output.initial_messages
    .flatMap((message) => message.content)
    .filter((block) => block.type === "text")
    .map((block) => block.text)
    .join("\n");
}

describe("active pursuit context scripts", () => {
  it("renders dynamic planner standing-successor guidance", async () => {
    const text = await runScript("planner.cjs", plannerInput());

    expect(text).toContain("# Standing next_leg_goal\nship printer");
    expect(text).toContain("Omitting both preserves any standing next_leg_goal");
    expect(text).toContain("Success means the full effective leg_goal is achieved");
  });

  it("keeps workers inside their assigned work item and leg goal", async () => {
    const text = await runScript("worker.cjs", workerInput());

    expect(text).toContain("Stay inside the current leg_goal and this work item");
    expect(text).toContain("Do not plan new legs, change leg_goal, or decide next_leg_goal");
  });

  it("renders dependency outcomes from the current non-superseded leg-goal version", async () => {
    const text = await runScript("worker.cjs", repeatedDependencyWorkerInput());

    expect(text).toContain("current base done");
    expect(text).not.toContain("old base done");
  });
});
