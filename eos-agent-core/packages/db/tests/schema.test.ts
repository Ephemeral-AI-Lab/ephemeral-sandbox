import { describe, expect, it } from "vitest";
import { sql } from "kysely";

import { pursuitIdFrom, workItemIdFrom } from "@eos/contracts";

import { createPursuitDatabase } from "../src/index.js";

describe("pursuit database schema", () => {
  it("allows caller-agnostic pursuits without an agent parent run", async () => {
    const db = createPursuitDatabase(":memory:");

    await sql`
      INSERT INTO pursuits (
        id, parent_run_id, pursuit_goal, leg_goal_mode, leg_goals,
        status, created_at, updated_at, closed_at
      )
      VALUES ('p-standalone', NULL, 'ship it', 'dynamic', NULL, 'Running', 'now', 'now', NULL)
    `.execute(db);

    await expect(
      db
        .selectFrom("pursuits")
        .select("parent_run_id")
        .where("id", "=", pursuitIdFrom("p-standalone"))
        .executeTakeFirstOrThrow(),
    ).resolves.toEqual({ parent_run_id: null });
  });

  it("rejects Blocked for non-work-item entity statuses", async () => {
    const db = createPursuitDatabase(":memory:");

    await expect(
      sql`
        INSERT INTO pursuits (
          id, parent_run_id, pursuit_goal, leg_goal_mode, leg_goals,
          status, created_at, updated_at, closed_at
        )
        VALUES (
          'p-1', 'parent', 'ship it', 'dynamic', NULL,
          'Blocked', 'now', 'now', NULL
        )
      `.execute(db),
    ).rejects.toThrow();
  });

  it("allows planner work-item ids to be reused across leg-goal versions", async () => {
    const db = createPursuitDatabase(":memory:");

    await sql`
      INSERT INTO pursuits (
        id, parent_run_id, pursuit_goal, leg_goal_mode, leg_goals,
        status, created_at, updated_at, closed_at
      )
      VALUES ('p-1', 'parent', 'ship it', 'dynamic', NULL, 'Running', 'now', 'now', NULL)
    `.execute(db);
    await sql`
      INSERT INTO legs (
        id, pursuit_id, sequence, origin, leg_goal, leg_goal_version,
        leg_goal_provenance, is_leg_goal_mutatable, next_leg_goal, max_attempts,
        status, created_at, updated_at
      )
      VALUES (
        'leg-1', 'p-1', 1, 'initial', 'ship it', 2,
        'declared by attempt_attempt-2 planner', 1, NULL, 3,
        'Running', 'now', 'now'
      )
    `.execute(db);
    for (const [attemptId, sequence, version] of [
      ["attempt-1", 1, 1],
      ["attempt-2", 2, 2],
    ] as const) {
      const planId = `plan-${String(sequence)}`;
      const workItemKey = `leg-1:${String(version)}:same`;
      await sql`
        INSERT INTO attempts (
          id, pursuit_id, leg_id, sequence, leg_goal_version, status,
          failure_reasons, created_at, updated_at
        )
        VALUES (
          ${attemptId}, 'p-1', 'leg-1', ${sequence}, ${version}, 'Running',
          '[]', 'now', 'now'
        )
      `.execute(db);
      await sql`
        INSERT INTO plans (
          id, pursuit_id, leg_id, attempt_id, agent_run_id, status,
          declared_leg_goal, declared_next_leg_goal, leg_goal_version,
          planner_summary, created_at, updated_at
	        )
	        VALUES (
	          ${planId}, 'p-1', 'leg-1', ${attemptId}, NULL, 'Success',
	          NULL, NULL, ${version}, 'planned', 'now', 'now'
	        )
      `.execute(db);
      await sql`
        INSERT INTO work_items (
          key, id, pursuit_id, leg_id, attempt_id, plan_id, agent_name,
          agent_run_id, status, title, spec, depends_on, leg_goal_version,
          worker_summary, worker_outcome, created_at, updated_at
	        )
	        VALUES (
	          ${workItemKey}, 'same', 'p-1', 'leg-1', ${attemptId},
	          ${planId}, 'worker', NULL, 'NotStarted', 'same', 'same',
          '[]', ${version}, NULL, NULL, 'now', 'now'
        )
      `.execute(db);
    }

    const rows = await db
      .selectFrom("work_items")
      .select(["id", "key"])
      .where("id", "=", workItemIdFrom("same"))
      .execute();

    expect(rows.map((row) => row.key).sort()).toEqual([
      "leg-1:1:same",
      "leg-1:2:same",
    ]);
  });
});
