import { mkdirSync } from "node:fs";
import { dirname, join } from "node:path";

import type { JsonObject, JsonValue } from "@eos/contracts";
import type { UsageSnapshot } from "@eos/llm-client";
import { eosAgentsPath } from "@eos/testkit";
import { defineTool, type ToolDefinition } from "@eos/tool";
import { z } from "zod";
import { describe, expect, it } from "vitest";

import type {
  LlmClientBinding,
  LlmClientRegistry,
} from "../src/llm-client-registry.js";
import { createAgentRuntime, type AgentRuntime } from "../src/runtime.js";
import {
  readEventLines,
  readResultLines,
  tempDir,
  userMessage,
  writeProfile,
  type ProfileSpec,
} from "../tests/support.js";
import {
  CODEX_CLIENT_ID,
  loadConfiguredCodexRuntime,
} from "./support/codex-runtime.js";
import { finishedRun } from "./support/fixtures.js";

const FINISH_TOOL = "finish_tau_bench_task";

const RETAIL_SYSTEM = [
  "You are a retail support agent. Use tools to help the user.",
  "Always verify the user's identity with name plus order id before any mutation.",
  "Never invent order ids, user ids, emails, statuses, prices, or addresses.",
  "If a request is outside your tools or violates policy, refuse honestly.",
  `When the task is complete or refused, call ${FINISH_TOOL} exactly once.`,
  "Do not write final prose instead of calling the finish tool.",
].join("\n");

const codex = loadConfiguredCodexRuntime();

if (!codex.available) {
  console.warn(`agent-runtime tau-bench-lite cache e2e skipped: ${codex.reason}`);
}

interface UserRow {
  name: string;
  email: string;
}

interface OrderRow {
  userId: string;
  status: string;
  address: string;
  item: string;
  price: number;
}

interface RefundRow {
  orderId: string;
  reason: string;
  amount: number;
}

interface WorldState {
  users: Partial<Record<string, UserRow>>;
  orders: Partial<Record<string, OrderRow>>;
  refunds: Partial<Record<string, RefundRow>>;
}

interface TauTask {
  id: string;
  description: string;
  userMessage: string;
  check(db: WorldState): boolean;
}

interface TauRunMetrics {
  taskId: string;
  status: "completed" | "cancelled" | "failed";
  pass: boolean;
  turns: number;
  toolCalls: number;
  cacheHitRate: number;
  usage: UsageSnapshot;
}

function codexBinding(): LlmClientBinding {
  if (!codex.available) {
    throw new Error("unreachable: the suite is skipped without credentials");
  }
  return codex.binding;
}

function retailSeed(): WorldState {
  return {
    users: {
      u_ari: { name: "Ari Chen", email: "ari@example.com" },
      u_bo: { name: "Bo Wang", email: "bo@example.com" },
      u_cai: { name: "Cai Lin", email: "cai@example.com" },
      u_dev: { name: "Dev Patel", email: "dev@example.com" },
    },
    orders: {
      o_1001: {
        userId: "u_ari",
        status: "shipped",
        address: "1 Elm St, SF, CA 94110",
        item: "wool sweater M",
        price: 89,
      },
      o_1002: {
        userId: "u_bo",
        status: "processing",
        address: "22 Oak Rd, NYC, NY 10001",
        item: "running shoes 10",
        price: 140,
      },
      o_1003: {
        userId: "u_cai",
        status: "delivered",
        address: "9 Pine Ave, Austin, TX 78701",
        item: "desk lamp",
        price: 55,
      },
      o_1004: {
        userId: "u_dev",
        status: "processing",
        address: "4 Maple Ln, Seattle, WA 98101",
        item: "kettle",
        price: 45,
      },
      o_1005: {
        userId: "u_ari",
        status: "delivered",
        address: "1 Elm St, SF, CA 94110",
        item: "notebook pack",
        price: 22,
      },
    },
    refunds: {},
  };
}

const TASKS: readonly TauTask[] = [
  {
    id: "t01_address_happy",
    description: "Change the shipping address of a processing order.",
    userMessage:
      "I am Bo Wang, user id u_bo. Change the shipping address on order o_1002 to 5 Birch Rd, NYC, NY 10001.",
    check: (db) => db.orders.o_1002?.address === "5 Birch Rd, NYC, NY 10001",
  },
  {
    id: "t02_address_not_allowed",
    description: "Refuse an address change for a shipped order.",
    userMessage:
      "I am Ari Chen, user id u_ari. Change the address on order o_1001 to 99 New St, SF, CA.",
    check: (db) => db.orders.o_1001?.address === "1 Elm St, SF, CA 94110",
  },
  {
    id: "t03_cancel_processing",
    description: "Cancel a processing order.",
    userMessage: "I am Dev Patel, user id u_dev. Cancel order o_1004.",
    check: (db) => db.orders.o_1004?.status === "cancelled",
  },
  {
    id: "t04_refund_delivered",
    description: "Refund a delivered order.",
    userMessage:
      "I am Cai Lin, user id u_cai. Refund order o_1003 because the lamp arrived broken.",
    check: (db) =>
      db.orders.o_1003?.status === "refunded" &&
      db.refunds.o_1003?.amount === 55,
  },
  {
    id: "t05_refund_not_delivered",
    description: "Do not refund a processing order.",
    userMessage:
      "I am Bo Wang, user id u_bo. Refund order o_1002 right now because I changed my mind.",
    check: (db) => {
      const status = db.orders.o_1002?.status;
      return (status === "processing" || status === "cancelled") && !db.refunds.o_1002;
    },
  },
  {
    id: "t06_multi_order_lookup",
    description: "List a user's order history without invalid cancellations.",
    userMessage:
      "I am Ari Chen, user id u_ari. Show every order on file for me, then cancel any of my orders that are still processing.",
    check: (db) =>
      db.orders.o_1001?.status === "shipped" &&
      db.orders.o_1005?.status === "delivered",
  },
  {
    id: "t07_wrong_identity",
    description: "Do not mutate when name and order ownership mismatch.",
    userMessage:
      "I am Bo Wang. Cancel order o_1004 for me. I do not know a different user id.",
    check: (db) => db.orders.o_1004?.status === "processing",
  },
  {
    id: "t08_address_then_cancel",
    description: "User changes from address update to cancellation.",
    userMessage:
      "I am Dev Patel, user id u_dev. I first wanted order o_1004 sent to 7 Fir St, Seattle, WA 98101, but I changed my mind. Cancel order o_1004 instead.",
    check: (db) => db.orders.o_1004?.status === "cancelled",
  },
];

function singleClientRegistry(binding: LlmClientBinding): LlmClientRegistry {
  return {
    require(llmClientId) {
      if (llmClientId !== binding.id) {
        throw new Error(`unknown llm client id "${llmClientId}"`);
      }
      return binding;
    },
  };
}

function runtimeForTask(
  binding: LlmClientBinding,
  task: TauTask,
  db: WorldState,
): AgentRuntime {
  const root = tempDir(`eos-tau-cache-${task.id}-`);
  const profilesDir = join(root, "profiles");
  mkdirSync(profilesDir, { recursive: true });
  writeProfile(profilesDir, taskProfile(task));
  return createAgentRuntime({
    agentProfilesDir: profilesDir,
    llmClients: singleClientRegistry(binding),
    baseTools: [...retailTools(db), finishTool()],
    hookConfigPath: eosAgentsPath("tests/hooks/none.json"),
    notificationRulesPath: eosAgentsPath("tests/notification-rules/none.json"),
    dataDir: join(root, "data"),
  });
}

function taskProfile(task: TauTask): ProfileSpec {
  return {
    name: task.id,
    kind: "main",
    llmClientId: CODEX_CLIENT_ID,
    allowed: [
      "lookup_order",
      "lookup_user",
      "update_address",
      "cancel_order",
      "refund_order",
      "list_user_orders",
    ],
    terminal: FINISH_TOOL,
    maxTurns: 10,
    body: [
      RETAIL_SYSTEM,
      "",
      `Benchmark task: ${task.description}`,
      "Use the finish tool after the business action, refusal, or no-op is complete.",
      "The finish summary should be short; the DB state is the benchmark result.",
    ].join("\n"),
  };
}

function retailTools(db: WorldState): ToolDefinition[] {
  return [
    defineTool({
      name: "lookup_order",
      description:
        "Look up an order by id. Returns { orderId, userId, status, address, item, price } or { error }.",
      input: z.object({ orderId: z.string().min(1) }),
      execute: ({ orderId }) => Promise.resolve({ content: lookupOrder(db, orderId) }),
    }),
    defineTool({
      name: "lookup_user",
      description: "Look up a user by id. Returns { userId, name, email } or { error }.",
      input: z.object({ userId: z.string().min(1) }),
      execute: ({ userId }) => Promise.resolve({ content: lookupUser(db, userId) }),
    }),
    defineTool({
      name: "update_address",
      description:
        "Update the shipping address on an order only when status is processing.",
      input: z.object({ orderId: z.string().min(1), address: z.string().min(1) }),
      execute: ({ orderId, address }) =>
        Promise.resolve({ content: updateAddress(db, orderId, address) }),
    }),
    defineTool({
      name: "cancel_order",
      description: "Cancel an order only when status is processing.",
      input: z.object({ orderId: z.string().min(1) }),
      execute: ({ orderId }) => Promise.resolve({ content: cancelOrder(db, orderId) }),
    }),
    defineTool({
      name: "refund_order",
      description: "Issue a refund only on a delivered order.",
      input: z.object({ orderId: z.string().min(1), reason: z.string().min(1) }),
      execute: ({ orderId, reason }) =>
        Promise.resolve({ content: refundOrder(db, orderId, reason) }),
    }),
    defineTool({
      name: "list_user_orders",
      description: "List every order belonging to a user id.",
      input: z.object({ userId: z.string().min(1) }),
      execute: ({ userId }) =>
        Promise.resolve({ content: { orders: listUserOrders(db, userId) } }),
    }),
  ];
}

function finishTool(): ToolDefinition {
  return defineTool({
    name: FINISH_TOOL,
    description: "Finish the tau-bench-lite task with a short summary.",
    input: z.object({ summary: z.string().min(1) }),
    isTerminal: true,
    execute: (input) => Promise.resolve({ content: { summary: input.summary } }),
  });
}

function lookupOrder(db: WorldState, orderId: string): JsonObject {
  const row = db.orders[orderId];
  return row === undefined ? { error: "order not found" } : { orderId, ...row };
}

function lookupUser(db: WorldState, userId: string): JsonObject {
  const row = db.users[userId];
  return row === undefined ? { error: "user not found" } : { userId, ...row };
}

function updateAddress(
  db: WorldState,
  orderId: string,
  address: string,
): JsonObject {
  const row = db.orders[orderId];
  if (row === undefined) return { error: "order not found" };
  if (row.status !== "processing") return { error: `cannot edit: status=${row.status}` };
  row.address = address;
  return { ok: true, orderId, newAddress: address };
}

function cancelOrder(db: WorldState, orderId: string): JsonObject {
  const row = db.orders[orderId];
  if (row === undefined) return { error: "order not found" };
  if (row.status !== "processing") return { error: `cannot cancel: status=${row.status}` };
  row.status = "cancelled";
  return { ok: true, orderId, status: "cancelled" };
}

function refundOrder(db: WorldState, orderId: string, reason: string): JsonObject {
  const row = db.orders[orderId];
  if (row === undefined) return { error: "order not found" };
  if (row.status !== "delivered") return { error: `cannot refund: status=${row.status}` };
  db.refunds[orderId] = { orderId, reason, amount: row.price };
  row.status = "refunded";
  return { ok: true, orderId, amount: row.price };
}

function listUserOrders(db: WorldState, userId: string): JsonValue[] {
  return Object.entries(db.orders).flatMap(([orderId, row]) =>
    row?.userId === userId ? [{ orderId, ...row }] : [],
  );
}

async function runTask(
  binding: LlmClientBinding,
  task: TauTask,
): Promise<TauRunMetrics> {
  const db = structuredClone(retailSeed());
  const runtime = runtimeForTask(binding, task, db);
  const run = runtime.startRun({
    agentName: task.id,
    initialMessages: [userMessage(task.userMessage)],
  });
  const outcome = await run.handle.outcome;
  await finishedRun(runtime, task.id);

  const runDir = dirname(run.transcriptPath);
  const events = readEventLines(join(runDir, "events.jsonl"));
  const result = readResultLines(join(runDir, "result.jsonl")).at(0);
  if (result === undefined) throw new Error(`missing result for ${task.id}`);
  const toolCalls = events.filter((line) => line.type === "tool_completed").length;
  const pass = outcome.status === "completed" && task.check(db);
  const metrics: TauRunMetrics = {
    taskId: task.id,
    status: outcome.status,
    pass,
    turns: result.turns,
    toolCalls,
    cacheHitRate: result.cache_hit_rate,
    usage: result.usage,
  };
  logTaskMetrics(metrics);
  return metrics;
}

function selectedTasks(env: NodeJS.ProcessEnv = process.env): TauTask[] {
  const raw = env.EOS_TAU_BENCH_TASKS;
  if (raw === undefined) return [...TASKS];
  const ids = new Set(raw.split(",").map((id) => id.trim()).filter(Boolean));
  const tasks = TASKS.filter((task) => ids.has(task.id));
  if (tasks.length !== ids.size) {
    throw new Error(`EOS_TAU_BENCH_TASKS contains unknown ids: ${raw}`);
  }
  return tasks;
}

function addUsage(total: UsageSnapshot, usage: UsageSnapshot): UsageSnapshot {
  return {
    input_tokens: total.input_tokens + usage.input_tokens,
    output_tokens: total.output_tokens + usage.output_tokens,
    cache_read_input_tokens:
      (total.cache_read_input_tokens ?? 0) + (usage.cache_read_input_tokens ?? 0),
    cache_creation_input_tokens:
      (total.cache_creation_input_tokens ?? 0) +
      (usage.cache_creation_input_tokens ?? 0),
  };
}

function cacheHitRate(usage: UsageSnapshot): number {
  const read = usage.cache_read_input_tokens ?? 0;
  const denominator =
    usage.input_tokens + read + (usage.cache_creation_input_tokens ?? 0);
  return denominator > 0 ? read / denominator : 0;
}

function logTaskMetrics(metrics: TauRunMetrics): void {
  console.log(
    [
      `[tau-cache] ${metrics.taskId}`,
      `status=${metrics.status}`,
      `pass=${String(metrics.pass)}`,
      `turns=${String(metrics.turns)}`,
      `tool_calls=${String(metrics.toolCalls)}`,
      `cache=${formatRate(metrics.cacheHitRate)}`,
      `input=${String(metrics.usage.input_tokens)}`,
      `cache_read=${String(metrics.usage.cache_read_input_tokens ?? 0)}`,
      `output=${String(metrics.usage.output_tokens)}`,
    ].join(" "),
  );
}

function formatRate(value: number): string {
  return `${(value * 100).toFixed(1)}%`;
}

describe.skipIf(!codex.available)("tau-bench-lite retail cache over live codex (e2e)", () => {
  it(
    "runs the retail task set and reports token-weighted cache hit rate",
    { timeout: 900_000 },
    async () => {
      const binding = codexBinding();
      const results: TauRunMetrics[] = [];
      for (const task of selectedTasks()) {
        results.push(await runTask(binding, task));
      }

      const total = results.reduce<UsageSnapshot>(
        (usage, result) => addUsage(usage, result.usage),
        { input_tokens: 0, output_tokens: 0 },
      );
      const passCount = results.filter((result) => result.pass).length;
      const aggregate = cacheHitRate(total);
      console.log(
        [
          "[tau-cache] summary",
          `tasks=${String(results.length)}`,
          `pass_rate=${formatRate(passCount / results.length)}`,
          `cache=${formatRate(aggregate)}`,
          `input=${String(total.input_tokens)}`,
          `cache_read=${String(total.cache_read_input_tokens ?? 0)}`,
          `output=${String(total.output_tokens)}`,
        ].join(" "),
      );

      expect(results.length, "at least one tau-bench-lite task ran").toBeGreaterThan(0);
      expect(aggregate, "aggregate cache rate is bounded").toBeGreaterThanOrEqual(0);
      expect(aggregate, "aggregate cache rate is bounded").toBeLessThanOrEqual(1);
    },
  );
});
