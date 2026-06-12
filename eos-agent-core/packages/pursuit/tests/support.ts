import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  agentRunIdFrom,
  mintAgentRunId,
  type AgentRunId,
  type InitialUserMessage,
  type PlannerOutcomePayload,
  type PursuitHandle,
  type PursuitId,
  type SubmissionResult,
  type WorkerOutcomePayload,
} from "@eos/contracts";
import { createPursuitDatabase, type PursuitDb } from "@eos/db";

import {
  defaultComposeLaunchContext,
  type ComposeLaunchContext,
} from "../src/context-engine/composer.js";
import type {
  AgentLaunchOptions,
  AgentLaunchPort,
  LaunchSettlement,
} from "../src/agent-launcher.js";
import { loadPursuitTree, type PursuitTree } from "../src/pursuit-tree.js";
import { PursuitService, type PursuitServiceDependencies } from "../src/service.js";

const PARENT_RUN = agentRunIdFrom("parent-run");

export interface ScriptedLaunch {
  agentName: string;
  messages: readonly InitialUserMessage[];
  options: AgentLaunchOptions | undefined;
  runId: AgentRunId;
  settle(settlement: LaunchSettlement): void;
  submitPlanner(payload: PlannerOutcomePayload): Promise<SubmissionResult>;
  submitWorker(payload: WorkerOutcomePayload): Promise<SubmissionResult>;
}

export interface Harness {
  db: PursuitDb;
  service: PursuitService;
  launches: ScriptedLaunch[];
  contextRoot: string;
  create(
    pursuitGoal?: string,
    options?: { maxAttempts?: number; legGoals?: readonly [string, ...string[]] },
  ): Promise<PursuitHandle>;
  tree(pursuitId: PursuitId): Promise<PursuitTree>;
}

export function harness(
  overrides: Partial<PursuitServiceDependencies> & {
    compose?: ComposeLaunchContext;
  } = {},
): Harness {
  const db = createPursuitDatabase(":memory:");
  const contextRoot = mkdtempSync(join(tmpdir(), "eos-pursuit-ctx-"));
  const launches: ScriptedLaunch[] = [];

  const port: AgentLaunchPort = {
    launch(agentName, initialMessages, options) {
      let resolve!: (settlement: LaunchSettlement) => void;
      const outcome = new Promise<LaunchSettlement>((settle) => {
        resolve = settle;
      });
      const launch: ScriptedLaunch = {
        agentName,
        messages: initialMessages,
        options,
        runId: mintAgentRunId(),
        settle: resolve,
        submitPlanner: (payload) => {
          const binding = options?.submission;
          if (binding?.kind !== "planner") {
            throw new Error(`launch of ${agentName} carries no planner binding`);
          }
          return binding.submit(payload);
        },
        submitWorker: (payload) => {
          const binding = options?.submission;
          if (binding?.kind !== "worker") {
            throw new Error(`launch of ${agentName} carries no worker binding`);
          }
          return binding.submit(payload);
        },
      };
      launches.push(launch);
      return {
        runId: launch.runId,
        outcome,
        interrupt: () => undefined,
      };
    },
  };

  const service = new PursuitService({
    db,
    port,
    compose: overrides.compose ?? defaultComposeLaunchContext,
    contextRoot,
    plannerAgentName: "planner",
    isRegisteredWorkerAgent: (name) => name === "worker",
    logMirrorFailure: () => undefined,
    ...overrides,
  });

  return {
    db,
    service,
    launches,
    contextRoot,
    create: (pursuitGoal = "ship the feature", options = {}) =>
      service.createPursuit(
        {
          pursuit_goal: pursuitGoal,
          ...(options.legGoals !== undefined && {
            leg_goals: [...options.legGoals] as [string, ...string[]],
          }),
          ...(options.maxAttempts !== undefined && {
            max_attempts: options.maxAttempts,
          }),
        },
        PARENT_RUN,
      ),
    tree: async (pursuitId) => {
      const tree = await loadPursuitTree(db, pursuitId);
      if (!tree) throw new Error(`pursuit ${pursuitId} not found`);
      return tree;
    },
  };
}

export function plannerPayload(
  overrides: Partial<PlannerOutcomePayload> = {},
): PlannerOutcomePayload {
  return {
    summary: "planned the leg",
    work_items: [
      {
        id: "w1",
        agent_name: "worker",
        title: "implement the leg",
        spec: "write the code for the leg",
        depends_on: [],
      },
    ],
    ...overrides,
  };
}

export function workItem(
  id: string,
  dependsOn: readonly string[] = [],
): PlannerOutcomePayload["work_items"][number] {
  return {
    id,
    agent_name: "worker",
    title: `item ${id}`,
    spec: `spec ${id}`,
    depends_on: [...dependsOn],
  };
}

export function workerPayload(
  overrides: Partial<WorkerOutcomePayload> = {},
): WorkerOutcomePayload {
  return {
    summary: "did the work",
    is_pass: true,
    outcome: "the leg is implemented",
    ...overrides,
  };
}

export async function until(
  check: () => boolean | Promise<boolean>,
  label = "condition",
): Promise<void> {
  for (let attempt = 0; attempt < 500; attempt += 1) {
    if (await check()) return;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  throw new Error(`timed out waiting for ${label}`);
}

function messageText(message: InitialUserMessage): string {
  return message.content
    .filter((block) => block.type === "text")
    .map((block) => block.text)
    .join("\n");
}

export function allMessageText(messages: readonly InitialUserMessage[]): string {
  return messages.map(messageText).join("\n---\n");
}
