import { describe, expect, it } from "vitest";

import { BackgroundSupervisor } from "../src/background/supervisor.js";
import {
  NotificationInbox,
  systemNotificationMessage,
} from "../src/notification-inbox.js";
import { must, sessionHandle, tick } from "./support.js";

const REF = { type: "command", id: "c1" };

function setup(): { inbox: NotificationInbox; supervisor: BackgroundSupervisor } {
  const inbox = new NotificationInbox();
  return { inbox, supervisor: new BackgroundSupervisor(inbox) };
}

describe("BackgroundSupervisor", () => {
  it("publishes one settlement, marks delivered on drain, then evicts (§15.6)", async () => {
    const { inbox, supervisor } = setup();
    const session = sessionHandle({ describe: "npm test" });
    supervisor.register(REF, session.handle);
    expect(supervisor.liveCount(), "running counts live").toBe(1);
    expect(supervisor.openCount(), "running counts open").toBe(1);
    const runningRow = must(supervisor.list().at(0));
    expect(runningRow).toMatchObject({
      type: "command",
      id: "c1",
      status: "running",
      description: "npm test",
    });
    expect(Date.parse(runningRow.started_at), "started_at is a timestamp").not.toBeNaN();

    session.settle({ status: "completed", summary: "all green" });
    await tick();
    expect(supervisor.liveCount(), "settled is no longer live").toBe(0);
    expect(supervisor.openCount(), "undelivered stays open").toBe(1);
    expect(must(supervisor.list().at(0))).toMatchObject({
      status: "completed",
      summary: "all green",
    });

    const drained = inbox.drain();
    expect(drained).toEqual([
      systemNotificationMessage({
        type: "session_settled",
        session: { type: "command", id: "c1" },
        status: "completed",
        summary: "all green",
      }),
    ]);
    expect(supervisor.openCount(), "delivered then evicted").toBe(0);
    expect(supervisor.list()).toEqual([]);
    expect(inbox.drain(), "no second delivery").toEqual([]);
  });

  it("maps a rejected settled promise to failed with the error as summary (§15.6)", async () => {
    const { inbox, supervisor } = setup();
    const session = sessionHandle();
    supervisor.register(REF, session.handle);
    session.fail(new Error("child run exploded"));
    await tick();
    expect(must(supervisor.list().at(0))).toMatchObject({
      status: "failed",
      summary: "child run exploded",
    });
    expect(inbox.drain()).toEqual([
      systemNotificationMessage({
        type: "session_settled",
        session: { type: "command", id: "c1" },
        status: "failed",
        summary: "child run exploded",
      }),
    ]);
  });

  it("publishes cancelled and ignores the late natural settle (§15.7)", async () => {
    const { inbox, supervisor } = setup();
    const session = sessionHandle();
    supervisor.register(REF, session.handle);
    expect(await supervisor.cancel(REF, "user asked")).toBe(true);
    expect(session.cancelled, "teardown invoked with the reason").toEqual([
      "user asked",
    ]);
    session.settle({ status: "completed", summary: "too late" });
    await tick();
    expect(must(supervisor.list().at(0)), "cancel won the race").toMatchObject({
      status: "cancelled",
      summary: "user asked",
    });
    expect(inbox.drain(), "exactly one settlement notification").toEqual([
      systemNotificationMessage({
        type: "session_settled",
        session: { type: "command", id: "c1" },
        status: "cancelled",
        summary: "user asked",
      }),
    ]);
  });

  it("returns false for unknown refs and already-settled sessions", async () => {
    const { supervisor } = setup();
    expect(await supervisor.cancel({ type: "command", id: "ghost" }, "x")).toBe(false);
    const session = sessionHandle();
    supervisor.register(REF, session.handle);
    session.settle({ status: "completed", summary: "done" });
    await tick();
    expect(await supervisor.cancel(REF, "x"), "settled is not cancellable").toBe(false);
    expect(session.cancelled, "teardown never invoked").toEqual([]);
  });

  it("dispose cancels running sessions, publishes nothing, and latches (§15.6, §15.22)", async () => {
    const { inbox, supervisor } = setup();
    const first = sessionHandle();
    const second = sessionHandle();
    supervisor.register(REF, first.handle);
    supervisor.register({ type: "subagent", id: "r2" }, second.handle);
    await supervisor.dispose("run finished");
    expect(first.cancelled).toEqual(["run finished"]);
    expect(second.cancelled).toEqual(["run finished"]);
    expect(supervisor.liveCount()).toBe(0);
    expect(inbox.drain(), "dispose publishes nothing").toEqual([]);

    const late = sessionHandle();
    supervisor.register({ type: "command", id: "late" }, late.handle);
    expect(late.cancelled, "late registration cancelled by the latch").toEqual([
      "run finished",
    ]);
    expect(
      supervisor.list().some((row) => row.id === "late"),
      "nothing registered after the latch",
    ).toBe(false);

    late.fail(new Error("ignored"));
    await tick();
    expect(inbox.drain(), "a latched handle's rejection publishes nothing").toEqual([]);
  });
});
