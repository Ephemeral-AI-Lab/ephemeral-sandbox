import { describe, expect, it } from "vitest";

import {
  agentRunIdFrom,
  attemptIdFrom,
  planIdFrom,
  pursuitIdFrom,
  workItemIdFrom,
} from "@eos/contracts";

import {
  allMessageText,
  harness,
  plannerPayload,
  until,
  workerPayload,
  workItem,
} from "./support.js";

describe("pursuit creation and planner declarations", () => {
  it("rejects creation payloads whose explicit leg_goal_mode conflicts with their shape", async () => {
    const h = harness();

    await expect(
      h.service.createPursuit(
        {
          pursuit_goal: "ship it",
          leg_goal_mode: "dynamic",
          leg_goals: ["parser"],
        },
        agentRunIdFrom("parent-run"),
      ),
    ).rejects.toThrow("leg_goal_mode dynamic does not match predefined payload shape");
  });

  it("creates a dynamic first leg with pursuit_goal as leg_goal and accepts keep payloads", async () => {
    const h = harness();
    const pursuit = await h.create("ship the parser");

    expect(h.launches).toHaveLength(1);
    expect(h.launches[0].agentName).toBe("planner");
    expect(h.launches[0].options?.parent).toBe("parent-run");

    const initial = await h.tree(pursuit.pursuit_id);
    expect(initial.pursuit.pursuitGoal).toBe("ship the parser");
    expect(initial.pursuit.legGoalMode).toBe("dynamic");
    expect(initial.legs[0]).toMatchObject({
      legGoal: "ship the parser",
      legGoalVersion: 1,
      nextLegGoal: null,
      isLegGoalMutatable: true,
    });
    expect(allMessageText(h.launches[0].messages)).toContain(
      "# Current leg goal\nship the parser",
    );

    const accepted = await h.launches[0].submitPlanner(plannerPayload());
    expect(accepted.ok).toBe(true);
    expect(h.launches, "root work item launched").toHaveLength(2);
    const afterPlan = await h.tree(pursuit.pursuit_id);
    expect(afterPlan.legs[0].attempts[0].plan.declaredLegGoal).toBeNull();
    expect(afterPlan.legs[0].attempts[0].workItems[0].title).toBe(
      "implement the leg",
    );
  });

  it("lets non-agent callers create, launch without a parent, cancel, and settle", async () => {
    const h = harness();

    const pursuit = await h.service.createPursuit({
      pursuit_goal: "standalone call",
    });

    expect(h.launches[0].options?.parent).toBeUndefined();
    await expect(
      h.db
        .selectFrom("pursuits")
        .select("parent_run_id")
        .where("id", "=", pursuit.pursuit_id)
        .executeTakeFirstOrThrow(),
    ).resolves.toMatchObject({ parent_run_id: null });

    await pursuit.cancel("operator stopped it");

    await expect(pursuit.settle()).resolves.toMatchObject({
      status: "Cancelled",
      summary: "operator stopped it",
    });
  });

  it("predefined mode rejects planner leg-goal declarations and promotes fixed legs", async () => {
    const h = harness();
    const pursuit = await h.create("ship all", { legGoals: ["parser", "printer"] });

    const initial = await h.tree(pursuit.pursuit_id);
    expect(initial.pursuit.legGoalMode).toBe("predefined");
    expect(initial.legs[0].legGoal).toBe("parser");
    expect(initial.legs[0].nextLegGoal).toBe("printer");

    const rejected = await h.launches[0].submitPlanner(
      plannerPayload({ leg_goal: "new parser" }),
    );
    expect(rejected.ok).toBe(false);
    if (!rejected.ok) expect(rejected.error).toContain("predefined");
    expect(
      (await h.tree(pursuit.pursuit_id)).legs[0].attempts[0].workItems,
      "correctable declaration error does not materialize work",
    ).toHaveLength(0);

    await h.launches[0].submitPlanner(plannerPayload());
    await h.launches[1].submitWorker(workerPayload());

    const promoted = await h.tree(pursuit.pursuit_id);
    expect(promoted.legs).toHaveLength(2);
    expect(promoted.legs[1]).toMatchObject({
      origin: "predefined",
      legGoal: "printer",
      isLegGoalMutatable: false,
    });
  });
});

describe("scheduler dependency and failure behavior", () => {
  it("launches same-attempt dependents only after direct dependencies succeed", async () => {
    const h = harness();
    await h.create("ship graph");
    await h.launches[0].submitPlanner(
      plannerPayload({
        work_items: [workItem("base"), workItem("dependent", ["base"])],
      }),
    );

    expect(h.launches.map((launch) => launch.options?.submission?.kind)).toEqual([
      "planner",
      "worker",
    ]);
    expect(allMessageText(h.launches[1].messages)).not.toContain("dependent");

    await h.launches[1].submitWorker(workerPayload({ summary: "base done" }));

    expect(h.launches.map((launch) => launch.options?.submission?.kind)).toEqual([
      "planner",
      "worker",
      "worker",
    ]);
    expect(allMessageText(h.launches[2].messages)).toContain("base done");
  });

  it("closes an attempt successfully only after every work item succeeds", async () => {
    const h = harness();
    const pursuit = await h.create("ship all work");
    await h.launches[0].submitPlanner(
      plannerPayload({ work_items: [workItem("a"), workItem("b")] }),
    );

    await h.launches[1].submitWorker(workerPayload({ summary: "a done" }));
    expect((await h.tree(pursuit.pursuit_id)).legs[0].attempts[0].status).toBe(
      "Running",
    );

    await h.launches[2].submitWorker(workerPayload({ summary: "b done" }));
    const tree = await h.tree(pursuit.pursuit_id);
    expect(tree.legs[0].attempts[0].status).toBe("Success");
    expect(tree.pursuit.status).toBe("Success");
  });

  it("blocks only not-started dependents and waits for unrelated running work before failing", async () => {
    const h = harness();
    const pursuit = await h.create("ship graph", { maxAttempts: 2 });
    await h.launches[0].submitPlanner(
      plannerPayload({
        work_items: [
          workItem("root"),
          workItem("dependent", ["root"]),
          workItem("unrelated"),
        ],
      }),
    );
    expect(h.launches.map((launch) => launch.agentName)).toEqual([
      "planner",
      "worker",
      "worker",
    ]);

    await h.launches[1].submitWorker(
      workerPayload({ is_pass: false, summary: "root failed" }),
    );
    const afterRootFailure = await h.tree(pursuit.pursuit_id);
    const attempt = afterRootFailure.legs[0].attempts[0];
    expect(attempt.status, "attempt stays open while unrelated work runs").toBe(
      "Running",
    );
    expect(
      attempt.workItems.map((item) => [String(item.id), item.status]).sort(),
    ).toEqual([
      ["dependent", "Blocked"],
      ["root", "Failed"],
      ["unrelated", "Running"],
    ]);

    await h.launches[2].submitWorker(workerPayload());
    const closed = await h.tree(pursuit.pursuit_id);
    expect(closed.legs[0].attempts[0].status).toBe("Failed");
    expect(closed.legs[0].attempts[0].failureReasons).toEqual([
      {
        work_item_id: "root",
        kind: "failed",
        message: null,
        summary: "root failed",
        outcome: "the leg is implemented",
      },
      {
        work_item_id: "dependent",
        kind: "blocked_by_failed_dependency",
        message: "blocked by work_item_root",
        summary: "blocked by work_item_root",
        outcome: "blocked by work_item_root",
        blocked_by: ["root"],
      },
    ]);
    expect(closed.legs[0].attempts, "retry created after failed close").toHaveLength(2);
    expect(h.launches.at(-1)?.agentName).toBe("planner");
  });

  it("propagates dependency blocks transitively until stable", async () => {
    const h = harness();
    const pursuit = await h.create("ship graph", { maxAttempts: 1 });
    await h.launches[0].submitPlanner(
      plannerPayload({
        work_items: [
          workItem("root"),
          workItem("middle", ["root"]),
          workItem("leaf", ["middle"]),
        ],
      }),
    );

    await h.launches[1].submitWorker(
      workerPayload({ is_pass: false, summary: "root failed" }),
    );

    const attempt = (await h.tree(pursuit.pursuit_id)).legs[0].attempts[0];
    expect(attempt.workItems.map((item) => [String(item.id), item.status])).toEqual([
      ["root", "Failed"],
      ["middle", "Blocked"],
      ["leaf", "Blocked"],
    ]);
    expect(attempt.failureReasons).toEqual([
      {
        work_item_id: "root",
        kind: "failed",
        message: null,
        summary: "root failed",
        outcome: "the leg is implemented",
      },
      {
        work_item_id: "middle",
        kind: "blocked_by_failed_dependency",
        message: "blocked by work_item_root",
        summary: "blocked by work_item_root",
        outcome: "blocked by work_item_root",
        blocked_by: ["root"],
      },
      {
        work_item_id: "leaf",
        kind: "blocked_by_failed_dependency",
        message: "blocked by work_item_middle",
        summary: "blocked by work_item_middle",
        outcome: "blocked by work_item_middle",
        blocked_by: ["middle"],
      },
    ]);
  });

  it("rechecks a claimed worker after context composition and skips stale launches", async () => {
    const state: { service?: ReturnType<typeof harness>["service"] } = {};
    const h = harness({
      compose: async (_agentName, input) => {
        if (input.kind === "worker" && input.current.work_item_id === "dependent") {
          await state.service?.cancel(
            pursuitIdFrom(input.current.pursuit_id),
            "cancel before stale launch",
          );
        }
        return [{ role: "user", content: [{ type: "text", text: "context" }] }];
      },
    });
    state.service = h.service;
    const pursuit = await h.create("ship graph");
    await h.launches[0].submitPlanner(
      plannerPayload({
        work_items: [workItem("base"), workItem("dependent", ["base"])],
      }),
    );

    await h.launches[1].submitWorker(workerPayload({ summary: "base done" }));

    expect(h.launches, "dependent claim was not launched after cancellation").toHaveLength(
      2,
    );
    expect((await h.tree(pursuit.pursuit_id)).pursuit.status).toBe("Cancelled");
  });

  it("records failed and blocked reasons when dependency propagation closes immediately", async () => {
    const h = harness();
    const pursuit = await h.create("ship graph", { maxAttempts: 1 });
    await h.launches[0].submitPlanner(
      plannerPayload({
        work_items: [workItem("root"), workItem("dependent", ["root"])],
      }),
    );

    await h.launches[1].submitWorker(
      workerPayload({ is_pass: false, summary: "root failed", outcome: "root broke" }),
    );

    const attempt = (await h.tree(pursuit.pursuit_id)).legs[0].attempts[0];
    expect(attempt.status).toBe("Failed");
    expect(attempt.failureReasons).toEqual([
      {
        work_item_id: "root",
        kind: "failed",
        message: null,
        summary: "root failed",
        outcome: "root broke",
      },
      {
        work_item_id: "dependent",
        kind: "blocked_by_failed_dependency",
        message: "blocked by work_item_root",
        summary: "blocked by work_item_root",
        outcome: "blocked by work_item_root",
        blocked_by: ["root"],
      },
    ]);
    expect(attempt.workItems.find((item) => item.id === "dependent")).toMatchObject({
      status: "Blocked",
      summary: "blocked by work_item_root",
      outcome: "blocked by work_item_root",
    });
  });

  it("allows retry plans to depend on successful prior-attempt work in the same leg-goal version", async () => {
    const h = harness();
    await h.create("ship retry", { maxAttempts: 2 });
    await h.launches[0].submitPlanner(
      plannerPayload({ work_items: [workItem("base"), workItem("breaker")] }),
    );
    await h.launches[1].submitWorker(workerPayload({ summary: "base done" }));
    await h.launches[2].submitWorker(
      workerPayload({ is_pass: false, summary: "breaker failed" }),
    );

    const retryPlanner = h.launches[3];
    const accepted = await retryPlanner.submitPlanner(
      plannerPayload({ work_items: [workItem("followup", ["base"])] }),
    );
    expect(accepted.ok).toBe(true);
    expect(h.launches.at(-1)?.options?.submission?.kind).toBe("worker");
    expect(allMessageText(h.launches.at(-1)?.messages ?? [])).toContain(
      "base done",
    );
  });

  it("renders dependency outcomes from the matching leg-goal version when ids repeat", async () => {
    const h = harness();
    await h.create("ship retry", { maxAttempts: 4 });
    await h.launches[0].submitPlanner(
      plannerPayload({ work_items: [workItem("base"), workItem("breaker")] }),
    );
    await h.launches[1].submitWorker(workerPayload({ summary: "old base done" }));
    await h.launches[2].submitWorker(
      workerPayload({ is_pass: false, summary: "old breaker failed" }),
    );

    await h.launches[3].submitPlanner(
      plannerPayload({
        leg_goal: "new leg goal",
        work_items: [workItem("base"), workItem("breaker")],
      }),
    );
    await h.launches[4].submitWorker(workerPayload({ summary: "new base done" }));
    await h.launches[5].submitWorker(
      workerPayload({ is_pass: false, summary: "new breaker failed" }),
    );

    const accepted = await h.launches[6].submitPlanner(
      plannerPayload({ work_items: [workItem("followup", ["base"])] }),
    );

    expect(accepted.ok).toBe(true);
    const workerText = allMessageText(h.launches.at(-1)?.messages ?? []);
    expect(workerText).toContain("new base done");
    expect(workerText).not.toContain("old base done");
  });

  it("closes failed after compose failures exhaust the attempt budget", async () => {
    const h = harness({ compose: () => Promise.reject(new Error("script exploded")) });
    const pursuit = await h.create("doomed", { maxAttempts: 2 });

    await until(async () => {
      const tree = await h.tree(pursuit.pursuit_id);
      return tree.pursuit.status === "Failed";
    }, "pursuit failed after compose failures");

    const tree = await h.tree(pursuit.pursuit_id);
    expect(tree.legs[0].attempts).toHaveLength(2);
    expect(tree.legs[0].attempts[0].failureReasons).toEqual([
      {
        work_item_id: null,
        kind: "context_composition_failed",
        message: "context_script_error: script exploded",
        summary: null,
        outcome: null,
      },
    ]);
    await expect(pursuit.settle()).resolves.toMatchObject({ status: "Failed" });
  });

  it("treats planner launch failure without submission as planner death", async () => {
    const h = harness();
    const pursuit = await h.create("doomed", { maxAttempts: 1 });

    h.launches[0].settle({ status: "failed" });

    await until(async () => {
      const tree = await h.tree(pursuit.pursuit_id);
      return tree.pursuit.status === "Failed";
    }, "pursuit failed after planner launch death");

    const attempt = (await h.tree(pursuit.pursuit_id)).legs[0].attempts[0];
    expect(attempt.plan.summary).toBeNull();
    expect(attempt.failureReasons).toEqual([
      {
        work_item_id: null,
        kind: "planner_failed",
        message: "run settled 'failed' without a submission",
        summary: null,
        outcome: null,
      },
    ]);
  });
});

describe("planner payload dependency validation", () => {
  it.each([
    [
      "unknown worker agent",
      () =>
        plannerPayload({
          work_items: [{ ...workItem("a"), agent_name: "missing" }],
        }),
      "unknown worker agent",
    ],
    [
      "unknown dependency id",
      () => plannerPayload({ work_items: [workItem("a", ["missing"])] }),
      'depends_on unknown id "missing"',
    ],
  ])("rejects %s without consuming the attempt", async (_name, payload, expected) => {
    const h = harness();
    const pursuit = await h.create("validate");

    const rejected = await h.launches[0].submitPlanner(payload());

    expect(rejected.ok).toBe(false);
    if (!rejected.ok) expect(rejected.error).toContain(expected);
    const attempt = (await h.tree(pursuit.pursuit_id)).legs[0].attempts[0];
    expect(attempt.status).toBe("Running");
    expect(attempt.workItems).toHaveLength(0);
    expect((await h.tree(pursuit.pursuit_id)).legs[0].attempts).toHaveLength(1);
  });

  it("rejects duplicate work-item ids from a prior same-version attempt", async () => {
    const h = harness();
    const pursuit = await h.create("validate", { maxAttempts: 2 });
    await h.launches[0].submitPlanner(
      plannerPayload({ work_items: [workItem("base"), workItem("breaker")] }),
    );
    await h.launches[1].submitWorker(workerPayload({ summary: "base done" }));
    await h.launches[2].submitWorker(
      workerPayload({ is_pass: false, summary: "breaker failed" }),
    );

    const rejected = await h.launches[3].submitPlanner(
      plannerPayload({ work_items: [workItem("base")] }),
    );

    expect(rejected.ok).toBe(false);
    if (!rejected.ok) expect(rejected.error).toContain("current leg goal version");
    expect((await h.tree(pursuit.pursuit_id)).legs[0].attempts[1].workItems).toHaveLength(
      0,
    );
  });

  it("rejects replacement leg_goal submissions that depend on prior work", async () => {
    const h = harness();
    await h.create("validate", { maxAttempts: 2 });
    await h.launches[0].submitPlanner(
      plannerPayload({ work_items: [workItem("base"), workItem("breaker")] }),
    );
    await h.launches[1].submitWorker(workerPayload({ summary: "base done" }));
    await h.launches[2].submitWorker(
      workerPayload({ is_pass: false, summary: "breaker failed" }),
    );

    const rejected = await h.launches[3].submitPlanner(
      plannerPayload({
        leg_goal: "new goal",
        work_items: [workItem("followup", ["base"])],
      }),
    );

    expect(rejected.ok).toBe(false);
    if (!rejected.ok) expect(rejected.error).toContain("replacement leg_goal");
  });

  it("rejects dependencies on superseded earlier leg-goal versions", async () => {
    const h = harness();
    await h.create("validate", { maxAttempts: 3 });
    await h.launches[0].submitPlanner(plannerPayload({ work_items: [workItem("old")] }));
    await h.launches[1].submitWorker(
      workerPayload({ is_pass: false, summary: "old failed" }),
    );
    await h.launches[2].submitPlanner(
      plannerPayload({ leg_goal: "new goal", work_items: [workItem("new")] }),
    );
    await h.launches[3].submitWorker(
      workerPayload({ is_pass: false, summary: "new failed" }),
    );

    const rejected = await h.launches[4].submitPlanner(
      plannerPayload({ work_items: [workItem("followup", ["old"])] }),
    );

    expect(rejected.ok).toBe(false);
    if (!rejected.ok) expect(rejected.error).toContain("superseded leg-goal version");
  });

  it("rejects dependencies on work items from another leg", async () => {
    const h = harness();
    await h.create("validate");
    await h.launches[0].submitPlanner(
      plannerPayload({ next_leg_goal: "next", work_items: [workItem("base")] }),
    );
    await h.launches[1].submitWorker(workerPayload({ summary: "base done" }));

    const rejected = await h.launches[2].submitPlanner(
      plannerPayload({ work_items: [workItem("followup", ["base"])] }),
    );

    expect(rejected.ok).toBe(false);
    if (!rejected.ok) expect(rejected.error).toContain("another leg");
  });

  it("rejects dependencies on work items from future attempts", async () => {
    const h = harness();
    const pursuit = await h.create("validate");
    const tree = await h.tree(pursuit.pursuit_id);
    const leg = tree.legs[0];
    const futureAttemptId = attemptIdFrom("future-attempt");
    const futurePlanId = planIdFrom("future-plan");
    const futureWorkItemId = workItemIdFrom("future");
    const now = new Date().toISOString();

    await h.db
      .insertInto("attempts")
      .values({
        id: futureAttemptId,
        pursuit_id: pursuit.pursuit_id,
        leg_id: leg.id,
        sequence: 2,
        leg_goal_version: leg.legGoalVersion,
        status: "Running",
        failure_reasons: "[]",
        created_at: now,
        updated_at: now,
      })
      .execute();
    await h.db
      .insertInto("plans")
      .values({
        id: futurePlanId,
        pursuit_id: pursuit.pursuit_id,
        leg_id: leg.id,
        attempt_id: futureAttemptId,
        agent_run_id: null,
        status: "Success",
        declared_leg_goal: null,
        declared_next_leg_goal: null,
        leg_goal_version: leg.legGoalVersion,
        planner_summary: "future planned",
        created_at: now,
        updated_at: now,
      })
      .execute();
    await h.db
      .insertInto("work_items")
      .values({
        key: `${leg.id}:${String(leg.legGoalVersion)}:${futureWorkItemId}`,
        id: futureWorkItemId,
        pursuit_id: pursuit.pursuit_id,
        leg_id: leg.id,
        attempt_id: futureAttemptId,
        plan_id: futurePlanId,
        agent_name: "worker",
        agent_run_id: null,
        status: "Success",
        title: "future",
        spec: "future",
        depends_on: "[]",
        leg_goal_version: leg.legGoalVersion,
        worker_summary: "future done",
        worker_outcome: "future done",
        created_at: now,
        updated_at: now,
      })
      .execute();

    const rejected = await h.launches[0].submitPlanner(
      plannerPayload({ work_items: [workItem("followup", ["future"])] }),
    );

    expect(rejected.ok).toBe(false);
    if (!rejected.ok) expect(rejected.error).toContain("future attempt");
  });
});

describe("dynamic refocus", () => {
  it("increments leg_goal_version, clears omitted next_leg_goal, and supersedes older attempts", async () => {
    const h = harness();
    const pursuit = await h.create("whole goal", { maxAttempts: 3 });
    await h.launches[0].submitPlanner(
      plannerPayload({ next_leg_goal: "later", work_items: [workItem("old")] }),
    );
    await h.launches[1].submitWorker(
      workerPayload({ is_pass: false, summary: "old failed" }),
    );

    const retry = h.launches[2];
    await retry.submitPlanner(
      plannerPayload({ leg_goal: "narrowed goal", work_items: [workItem("old")] }),
    );

    const tree = await h.tree(pursuit.pursuit_id);
    const leg = tree.legs[0];
    expect(leg.legGoal).toBe("narrowed goal");
    expect(leg.legGoalVersion).toBe(2);
    expect(leg.nextLegGoal, "omitted successor cleared during refocus").toBeNull();
    expect(leg.attempts[0].isConsistentWithLegGoal).toBe(false);
    expect(leg.attempts[1].isConsistentWithLegGoal).toBe(true);
    expect(leg.attempts[1].workItems[0].id).toBe("old");
  });

  it("preserves standing next_leg_goal when retry payload omits both goal fields", async () => {
    const h = harness();
    const pursuit = await h.create("whole goal", { maxAttempts: 3 });
    await h.launches[0].submitPlanner(
      plannerPayload({ next_leg_goal: "later", work_items: [workItem("old")] }),
    );
    await h.launches[1].submitWorker(
      workerPayload({ is_pass: false, summary: "old failed" }),
    );

    await h.launches[2].submitPlanner(plannerPayload({ work_items: [workItem("new")] }));

    const leg = (await h.tree(pursuit.pursuit_id)).legs[0];
    expect(leg.nextLegGoal).toBe("later");
  });
});
