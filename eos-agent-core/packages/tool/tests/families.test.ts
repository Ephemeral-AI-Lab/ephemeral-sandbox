import { describe, expect, it } from "vitest";

import { toolUseIdFrom } from "@eos/contracts";
import { BackgroundSessionSupervisor } from "@eos/background";
import { NotificationInbox } from "@eos/notification";
import { scriptedRunState, scriptedBackgroundSessionHandle } from "@eos/testkit";

import {
  ToolNameSchema,
  type ToolCallContext,
  type ToolDefinition,
} from "../src/contract.js";
import { snapshotRunState } from "../src/run-state.js";
import { backgroundTools } from "../src/index.js";
import { submissionTool } from "../src/tools/submission/index.js";
import { live, must, tick } from "./support.js";

function setup(): {
  inbox: NotificationInbox;
  supervisor: BackgroundSessionSupervisor;
  list: ToolDefinition;
  cancel: ToolDefinition;
} {
  const inbox = new NotificationInbox();
  const supervisor = new BackgroundSessionSupervisor(inbox);
  const [list, cancel] = backgroundTools(supervisor);
  return { inbox, supervisor, list, cancel };
}

const ctx = (): ToolCallContext => ({
  meta: Object.freeze({
    tool_use_id: toolUseIdFrom("tu_t"),
    tool_name: ToolNameSchema.parse("test_caller"),
    run: snapshotRunState(scriptedRunState()),
  }),
  signal: live(),
});

const register = (
  supervisor: BackgroundSessionSupervisor,
  type: string,
  id: string,
  describe?: string,
): ReturnType<typeof scriptedBackgroundSessionHandle> => {
  const session = scriptedBackgroundSessionHandle(describe);
  supervisor.registerBackgroundSession({ type, id }, session.handle);
  return session;
};

describe("background tool family", () => {
  it("lists running and undelivered sessions as rows", async () => {
    const { supervisor, list } = setup();
    register(supervisor, "command", "c1", "npm test");
    const second = register(supervisor, "subagent", "r2");
    second.settle({ status: "completed", summary: "explored" });
    await tick();
    const outcome = await list.execute({}, ctx());
    const rows = outcome.content as { id: string; status: string }[];
    expect(rows).toHaveLength(2);
    expect(must(rows.at(0))).toMatchObject({
      type: "command",
      id: "c1",
      status: "running",
      description: "npm test",
    });
    expect(must(rows.at(1))).toMatchObject({
      type: "subagent",
      id: "r2",
      status: "completed",
      summary: "explored",
    });
  });

  it("cancels exactly the named session by (type, id) (§15.9)", async () => {
    const { supervisor, cancel } = setup();
    const first = register(supervisor, "command", "c1");
    const second = register(supervisor, "command", "c2");
    const outcome = await cancel.execute(
      { type: "command", id: "c2", reason: "wrong branch" },
      ctx(),
    );
    expect(outcome.isError ?? false).toBe(false);
    expect(outcome.content).toContain("command:c2 cancelled");
    expect(second.cancelled).toEqual(["wrong branch"]);
    expect(first.cancelled, "the sibling session is untouched").toEqual([]);
  });

  it("errors on an unknown ref (§15.9)", async () => {
    const { cancel } = setup();
    const outcome = await cancel.execute({ type: "command", id: "ghost" }, ctx());
    expect(outcome.isError).toBe(true);
    expect(outcome.content).toBe("no background session command:ghost");
  });

  it("notes an already-terminal session as a no-op (§15.9)", async () => {
    const { supervisor, cancel } = setup();
    const session = register(supervisor, "command", "c1");
    session.settle({ status: "completed", summary: "done" });
    await tick();
    const outcome = await cancel.execute({ type: "command", id: "c1" }, ctx());
    expect(outcome.isError ?? false, "a no-op is not an error").toBe(false);
    expect(outcome.content).toContain("already settled (completed)");
    expect(session.cancelled, "no teardown for a settled session").toEqual([]);
  });
});

describe("submission tool family", () => {
  it("is terminal by construction, one definition per kind", () => {
    const tool = submissionTool("worker");
    expect(tool.name).toBe("submit_worker_outcome");
    expect(tool.isTerminal).toBe(true);
    expect(tool.availableInIsolatedWorkspace).toBe(false);
  });

  it("marks only planner, worker, and main submissions as advisory-required", () => {
    for (const kind of ["main", "planner", "worker"] as const) {
      const tool = submissionTool(kind);
      expect(tool.isAdvisoryRequired, kind).toBe(true);
      expect(typeof tool.advisorPrompt, kind).toBe("string");
      expect(tool.advisorPrompt?.length, kind).toBeGreaterThan(0);
    }
    expect(submissionTool("advisor")).toMatchObject({
      isAdvisoryRequired: false,
    });
    expect(submissionTool("subagent")).toMatchObject({
      isAdvisoryRequired: false,
    });
  });

  it("returns the parsed outcome object as the terminal content", async () => {
    const submit = submissionTool("planner");
    const outcome = await submit.execute({ summary: "plan ready" }, ctx());
    expect(outcome.content).toEqual({ summary: "plan ready" });
  });

  it("does not own background-session submission policy", async () => {
    const { inbox, supervisor } = setup();
    const submit = submissionTool("main");
    const session = register(supervisor, "command", "c1");

    const whileRunning = await submit.execute({ summary: "all done" }, ctx());
    expect(whileRunning.isError ?? false).toBe(false);

    session.settle({ status: "completed", summary: "ok" });
    await tick();
    const whileUndelivered = await submit.execute({ summary: "all done" }, ctx());
    expect(whileUndelivered.isError ?? false).toBe(false);

    inbox.drain();
    const afterDelivery = await submit.execute(
      { summary: "all done", payload: { commits: 2 } },
      ctx(),
    );
    expect(afterDelivery.isError ?? false).toBe(false);
    expect(afterDelivery.content).toEqual({
      summary: "all done",
      payload: { commits: 2 },
    });
  });
});
