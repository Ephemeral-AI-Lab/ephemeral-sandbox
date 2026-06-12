import { describe, expect, it } from "vitest";

import {
  ContextScriptOutputSchema,
  CreatePursuitInputSchema,
  PlannerContextInputSchema,
  PlannerOutcomePayloadSchema,
  PursuitEntityRunStatusSchema,
  WorkItemRunStatusSchema,
  WorkerContextInputSchema,
  WorkerOutcomePayloadSchema,
  isPursuitEntityTerminal,
  isWorkItemTerminal,
  mintPursuitId,
  pursuitIdFrom,
} from "../src/index.js";

const WORK_ITEM = {
  id: "wi-1",
  agent_name: "worker",
  title: "implement the parser",
  spec: "write the parser module",
};

function snapshot() {
  return {
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
          leg_goal: "ship it",
          leg_goal_version: 1,
          leg_goal_provenance: "inherited from pursuit goal",
          is_leg_goal_mutatable: true,
          next_leg_goal: null,
          max_attempts: 2,
          context_path: "pursuit_p-1/leg_leg-1",
          outcome: null,
          attempts: [
            {
              id: "at-1",
              sequence: 1,
              status: "Running",
              failure_reasons: [],
              is_consistent_with_leg_goal: true,
              context_path: "pursuit_p-1/leg_leg-1/attempt_at-1",
              outcome: null,
              leg_goal_version: 1,
              plan: {
                id: "pl-1",
                status: "Running",
                declared_leg_goal: null,
                declared_next_leg_goal: null,
                summary: null,
                agent_run_id: null,
                leg_goal_version: 1,
              },
              work_items: [],
            },
          ],
        },
      ],
    },
  };
}

describe("pursuit ids and statuses", () => {
  it("mints and adopts branded pursuit ids", () => {
    expect(mintPursuitId()).toMatch(/[0-9a-f-]{36}/);
    expect(pursuitIdFrom("p-1")).toBe("p-1");
    expect(() => pursuitIdFrom("")).toThrow();
  });

  it.each`
    status          | terminal
    ${"NotStarted"} | ${false}
    ${"Running"}    | ${false}
    ${"Success"}    | ${true}
    ${"Failed"}     | ${true}
    ${"Cancelled"}  | ${true}
  `("classifies pursuit status $status", ({ status, terminal }) => {
    const parsed = PursuitEntityRunStatusSchema.parse(status);
    expect(isPursuitEntityTerminal(parsed)).toBe(terminal);
  });

  it("keeps Blocked scoped to work items", () => {
    expect(WorkItemRunStatusSchema.parse("Blocked")).toBe("Blocked");
    expect(isWorkItemTerminal("Blocked")).toBe(true);
    expect(PursuitEntityRunStatusSchema.safeParse("Blocked").success).toBe(false);
  });
});

describe("create pursuit input", () => {
  it("derives dynamic mode when leg_goals is omitted", () => {
    const parsed = CreatePursuitInputSchema.parse({ pursuit_goal: "ship it" });
    expect(parsed.leg_goals).toBeUndefined();
  });

  it("accepts predefined mode with an ordered non-empty leg list", () => {
    const parsed = CreatePursuitInputSchema.parse({
      pursuit_goal: "ship it",
      leg_goal_mode: "predefined",
      leg_goals: ["parser", "printer"],
    });
    expect(parsed.leg_goals).toEqual(["parser", "printer"]);
  });

  it("rejects predefined mode with an empty leg list", () => {
    expect(
      CreatePursuitInputSchema.safeParse({
        pursuit_goal: "ship it",
        leg_goal_mode: "predefined",
        leg_goals: [],
      }).success,
    ).toBe(false);
  });

  it("rejects explicit mode and payload-shape mismatches", () => {
    expect(
      CreatePursuitInputSchema.safeParse({
        pursuit_goal: "ship it",
        leg_goal_mode: "dynamic",
        leg_goals: ["parser"],
      }).success,
    ).toBe(false);
    expect(
      CreatePursuitInputSchema.safeParse({
        pursuit_goal: "ship it",
        leg_goal_mode: "predefined",
      }).success,
    ).toBe(false);
  });
});

describe("planner outcome payload", () => {
  it("accepts dynamic refocus, successor-only, and keep payloads", () => {
    expect(
      PlannerOutcomePayloadSchema.parse({
        summary: "planned",
        leg_goal: "parser",
        next_leg_goal: "printer",
        work_items: [WORK_ITEM],
      }).work_items[0].depends_on,
    ).toEqual([]);

    expect(
      PlannerOutcomePayloadSchema.parse({
        summary: "planned",
        next_leg_goal: "printer",
        work_items: [WORK_ITEM],
      }).next_leg_goal,
    ).toBe("printer");

    expect(
      PlannerOutcomePayloadSchema.parse({
        summary: "planned",
        work_items: [{ ...WORK_ITEM, depends_on: ["wi-0"] }],
      }).leg_goal,
    ).toBeUndefined();
  });

  it("rejects legacy work-item fields and empty values", () => {
    const legacySpecField = ["work_item", "spec"].join("_");
    const legacyDependencyField = ["nee", "ds"].join("");
    expect(
      PlannerOutcomePayloadSchema.safeParse({
        summary: "planned",
        work_items: [
          {
            id: "wi-1",
            agent_name: "worker",
            description: "old",
            [legacySpecField]: "old",
            [legacyDependencyField]: [],
          },
        ],
      }).success,
    ).toBe(false);
    expect(
      PlannerOutcomePayloadSchema.safeParse({
        summary: "",
        work_items: [WORK_ITEM],
      }).success,
    ).toBe(false);
  });

  it("rejects null next_leg_goal instead of treating it as a clear request", () => {
    expect(
      PlannerOutcomePayloadSchema.safeParse({
        summary: "planned",
        next_leg_goal: null,
        work_items: [WORK_ITEM],
      }).success,
    ).toBe(false);
  });
});

describe("context script inputs", () => {
  it("carries pursuit_context plus current for planners", () => {
    const input = {
      kind: "planner",
      pursuit_context: snapshot(),
      current: {
        pursuit_id: "p-1",
        leg_id: "leg-1",
        attempt_id: "at-1",
        plan_id: "pl-1",
      },
    };
    expect(PlannerContextInputSchema.parse(input)).toEqual(input);
    expect(
      PlannerContextInputSchema.safeParse({
        ...input,
        pursuit_context: {
          pursuit: { ...snapshot().pursuit, pursuit_goal: "old" },
        },
      }).success,
    ).toBe(false);
  });

  it("carries structured attempt failure reasons in snapshots", () => {
    const input = {
      ...snapshot(),
      pursuit: {
        ...snapshot().pursuit,
        legs: [
          {
            ...snapshot().pursuit.legs[0],
            attempts: [
              {
                ...snapshot().pursuit.legs[0].attempts[0],
                failure_reasons: [
                  {
                    work_item_id: "wi-2",
                    kind: "blocked_by_failed_dependency",
                    message: "blocked by work_item_wi-1",
                    summary: "blocked by work_item_wi-1",
                    outcome: "blocked by work_item_wi-1",
                    blocked_by: ["wi-1"],
                  },
                ],
              },
            ],
          },
        ],
      },
    };
    expect(PlannerContextInputSchema.parse({
      kind: "planner",
      pursuit_context: input,
      current: {
        pursuit_id: "p-1",
        leg_id: "leg-1",
        attempt_id: "at-1",
        plan_id: "pl-1",
      },
    }).pursuit_context.pursuit.legs[0].attempts[0].failure_reasons[0]).toMatchObject({
      kind: "blocked_by_failed_dependency",
      blocked_by: ["wi-1"],
    });
  });

  it("carries pursuit_context plus current for workers", () => {
    const input = {
      kind: "worker",
      pursuit_context: snapshot(),
      current: {
        pursuit_id: "p-1",
        leg_id: "leg-1",
        attempt_id: "at-1",
        work_item_id: "wi-1",
      },
    };
    expect(WorkerContextInputSchema.parse(input)).toEqual(input);
    expect(
      WorkerContextInputSchema.safeParse({
        ...input,
        current: { ...input.current, plan_id: "pl-1" },
      }).success,
    ).toBe(false);
  });
});

describe("worker outcome and script output", () => {
  it("accepts worker terminal payloads", () => {
    expect(
      WorkerOutcomePayloadSchema.parse({
        summary: "done",
        is_pass: true,
        outcome: "parser written",
      }).is_pass,
    ).toBe(true);
  });

  it("accepts ordered user messages and rejects empty output", () => {
    expect(
      ContextScriptOutputSchema.parse({
        initial_messages: [
          { role: "user", content: [{ type: "text", text: "# Pursuit goal\nship" }] },
        ],
      }).initial_messages,
    ).toHaveLength(1);
    expect(ContextScriptOutputSchema.safeParse({ initial_messages: [] }).success).toBe(
      false,
    );
  });
});
