import { describe, expect, it } from "vitest";

import {
  pursuitIdFrom,
  toolUseIdFrom,
  type PlannerOutcomePayload,
  type PursuitHandle,
  type PursuitSettlement,
  type SubmissionResult,
} from "@eos/contracts";
import { BackgroundSessionSupervisor } from "@eos/background";
import { NotificationInbox } from "@eos/notification";
import { scriptedRunState } from "@eos/testkit";

import { ToolNameSchema, type ToolCallContext } from "../src/contract.js";
import { snapshotRunState } from "../src/run-state.js";
import { cancelBackgroundSessionTool } from "../src/tools/background/cancel-background-session.js";
import { pursuitTools } from "../src/tools/pursuit/delegate-pursuit.js";
import {
  plannerStructureError,
  submitPlannerOutcomeTool,
  submitWorkerOutcomeTool,
} from "../src/tools/submission/index.js";
import { live, tick } from "./support.js";

const ctx = (): ToolCallContext => ({
  meta: Object.freeze({
    tool_use_id: toolUseIdFrom("tu_pursuit"),
    tool_name: ToolNameSchema.parse("test_caller"),
    run: snapshotRunState(scriptedRunState()),
  }),
  signal: live(),
});

function scriptedPursuit(id: string): {
  pursuit: PursuitHandle;
  resolveTerminal: (terminal: PursuitSettlement) => void;
  cancelled: string[];
} {
  let resolveTerminal!: (terminal: PursuitSettlement) => void;
  const cancelled: string[] = [];
  const terminal = new Promise<PursuitSettlement>((resolve) => {
    resolveTerminal = resolve;
  });
  const pursuit: PursuitHandle = {
    pursuit_id: pursuitIdFrom(id),
    settle: () => terminal,
    cancel: async (reason = "cancelled") => {
      cancelled.push(reason);
      resolveTerminal({ status: "Cancelled", summary: reason });
      await Promise.resolve();
    },
  };
  return { pursuit, resolveTerminal, cancelled };
}

function workItem(id: string, dependsOn: string[] = []) {
  return {
    id,
    agent_name: "worker",
    title: `item ${id}`,
    spec: `spec ${id}`,
    depends_on: dependsOn,
  };
}

describe("delegate_pursuit", () => {
  it("registers the session before returning and reports the pursuit id", async () => {
    const inbox = new NotificationInbox();
    const supervisor = new BackgroundSessionSupervisor(inbox);
    const scripted = scriptedPursuit("p-1");
    const [tool] = pursuitTools(() => {
      expect(supervisor.listBackgroundSessions()).toHaveLength(0);
      return Promise.resolve(scripted.pursuit);
    }, supervisor);

    const outcome = await tool.execute({ pursuit_goal: "ship it" }, ctx());
    expect(outcome.isError ?? false).toBe(false);
    expect(outcome.content).toEqual({ pursuit_id: "p-1" });
    expect(supervisor.listBackgroundSessions()[0]).toMatchObject({
      type: "pursuit",
      id: "p-1",
      status: "running",
      description: "ship it",
    });
  });

  it("rejects a second delegation while a pursuit session is open", async () => {
    const inbox = new NotificationInbox();
    const supervisor = new BackgroundSessionSupervisor(inbox);
    const first = scriptedPursuit("p-1");
    const [tool] = pursuitTools(() => Promise.resolve(first.pursuit), supervisor);

    await tool.execute({ pursuit_goal: "first" }, ctx());
    expect((await tool.execute({ pursuit_goal: "second" }, ctx())).isError).toBe(true);

    first.resolveTerminal({ status: "Success", summary: "done" });
    await tick();
    expect(
      (await tool.execute({ pursuit_goal: "third" }, ctx())).isError,
      "undelivered settlement still guards",
    ).toBe(true);

    inbox.drain();
    expect(
      (await tool.execute({ pursuit_goal: "fourth" }, ctx())).isError ?? false,
    ).toBe(false);
  });

  it("cancel_background_session accepts type pursuit and awaits the handle cascade", async () => {
    const inbox = new NotificationInbox();
    const supervisor = new BackgroundSessionSupervisor(inbox);
    const scripted = scriptedPursuit("p-1");
    const [tool] = pursuitTools(() => Promise.resolve(scripted.pursuit), supervisor);
    await tool.execute({ pursuit_goal: "ship it" }, ctx());

    const cancel = cancelBackgroundSessionTool(supervisor);
    const outcome = await cancel.execute(
      { type: "pursuit", id: "p-1", reason: "wrong direction" },
      ctx(),
    );
    expect(outcome.isError ?? false).toBe(false);
    expect(scripted.cancelled).toEqual(["wrong direction"]);
  });
});

describe("submission tools", () => {
  it.each([
    ["duplicate ids", [workItem("a"), workItem("a")], 'duplicate work item id "a"'],
    [
      "dependency cycles",
      [workItem("a", ["b"]), workItem("b", ["a"])],
      "dependency cycle",
    ],
    ["self cycles", [workItem("a", ["a"])], "dependency cycle"],
  ])("rejects %s in-run", async (_name, workItems, expected) => {
    const payload: PlannerOutcomePayload = {
      summary: "plan",
      work_items: workItems,
    };
    expect(plannerStructureError(payload)).toContain(expected);

    const submitted: PlannerOutcomePayload[] = [];
    const tool = submitPlannerOutcomeTool({
      kind: "planner",
      submit: (accepted) => {
        submitted.push(accepted);
        return Promise.resolve<SubmissionResult>({ ok: true });
      },
    });
    const outcome = await tool.execute(payload, ctx());
    expect(outcome.isError).toBe(true);
    expect(submitted).toHaveLength(0);
  });

  it("allows nonlocal depends_on through to the pursuit binding", async () => {
    const payload: PlannerOutcomePayload = {
      summary: "plan",
      work_items: [workItem("a", ["prior-success"])],
    };
    expect(plannerStructureError(payload)).toBeUndefined();
    const tool = submitPlannerOutcomeTool({
      kind: "planner",
      submit: () => Promise.resolve({ ok: true }),
    });
    const outcome = await tool.execute(payload, ctx());
    expect(outcome.isError ?? false).toBe(false);
  });

  it("a bound planner submission awaits the binding and surfaces its error", async () => {
    const results: SubmissionResult[] = [
      { ok: false, error: "predefined leg goals cannot be refocused" },
      { ok: true },
    ];
    const tool = submitPlannerOutcomeTool({
      kind: "planner",
      submit: () => Promise.resolve(results.shift() ?? { ok: true }),
    });
    const payload: PlannerOutcomePayload = {
      summary: "plan",
      leg_goal: "slice",
      work_items: [workItem("a")],
    };

    expect((await tool.execute(payload, ctx())).isError).toBe(true);
    expect((await tool.execute(payload, ctx())).isError ?? false).toBe(false);
  });

  it("bound and unbound worker submissions preserve payload behavior", async () => {
    const okTool = submitWorkerOutcomeTool({
      kind: "worker",
      submit: () => Promise.resolve({ ok: true }),
    });
    expect(
      (
        await okTool.execute(
          { summary: "done", is_pass: true, outcome: "all good" },
          ctx(),
        )
      ).isError ?? false,
    ).toBe(false);

    const worker = await submitWorkerOutcomeTool().execute(
      { summary: "did it", is_pass: false, outcome: "details" },
      ctx(),
    );
    expect(worker.content).toEqual({
      summary: "did it",
      is_pass: false,
      outcome: "details",
    });
  });
});
