import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { assistantText, type JsonObject, type Message } from "@eos/contracts";
import type { AgentRunOutcome } from "@eos/engine";
import { scriptedTool } from "@eos/testkit";
import { defineTool, type HookConfigEntry, type ToolDefinition } from "@eos/tool";
import { z } from "zod";

import type { RunSummary } from "../../src/run-registry.js";
import { createAgentRuntime, type AgentRuntime } from "../../src/runtime.js";
import { tempDir, writeProfile, type ProfileSpec } from "../../tests/support.js";

// --- live-prompt fixtures --------------------------------------------------

/** Shared system-prompt body: directive enough to keep live runs on rails. */
export const TERSE_BODY = [
  "You are a terse test agent.",
  "Follow the user's numbered instructions exactly and in order.",
  "Make at most one tool call per assistant turn and write no prose.",
].join(" ");

/** Like `TERSE_BODY` but without the one-call rule, for batch scenarios. */
export const PARALLEL_BODY = [
  "You are a terse test agent.",
  "Follow the user's numbered instructions exactly and in order.",
  "Write no prose.",
].join(" ");

/** A subagent that parks on the mocked `wait` tool until it is cancelled. */
export const SLEEPER_BODY = [
  "You are the sleeper.",
  'Immediately call wait with {"ms": 120000}.',
  'After it returns, call submit_subagent_outcome with summary "slept".',
].join(" ");

const TERMINAL_SUBMISSION_TOOL_NAMES = [
  "submit_main_outcome",
  "submit_planner_outcome",
  "submit_worker_outcome",
  "submit_advisor_outcome",
  "submit_subagent_outcome",
] as const;

const ADVISORY_REQUIRED_SUBMISSION_TOOL_NAMES = new Set<string>([
  "submit_main_outcome",
  "submit_planner_outcome",
  "submit_worker_outcome",
]);

export function noOpenBackgroundSessionsHookPath(): string {
  return join(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../../.eos-agents/hooks/no-open-background-sessions.cjs",
  );
}

export function requireAdvisoryPassHookPath(): string {
  return join(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../../.eos-agents/hooks/require-advisory-pass.cjs",
  );
}

export function noOpenBackgroundSessionsHookEntries(): HookConfigEntry[] {
  const command = `node ${JSON.stringify(noOpenBackgroundSessionsHookPath())}`;
  return TERMINAL_SUBMISSION_TOOL_NAMES.map((matcher) => ({
    event: "PreToolUse",
    matcher,
    hooks: [{ type: "command", command }],
  }));
}

export function requireAdvisoryPassHookEntries(): HookConfigEntry[] {
  const command = `node ${JSON.stringify(requireAdvisoryPassHookPath())}`;
  return [
    {
      event: "PreToolUse",
      hooks: [{ type: "command", command }],
    },
  ];
}

/** A subagent that settles immediately with a fixed summary. */
export const HELPER_BODY = [
  "You are the helper.",
  "Immediately call submit_subagent_outcome exactly once with summary set to",
  'exactly "helper finished". Do not call any other tool.',
].join(" ");

export function advisoryReadyProfile(spec: ProfileSpec): ProfileSpec {
  const terminal = spec.terminal ?? `submit_${spec.kind}_outcome`;
  if (!ADVISORY_REQUIRED_SUBMISSION_TOOL_NAMES.has(terminal)) return spec;
  const allowed = spec.allowed ?? [];
  if (allowed.includes("ask_advisor")) return spec;
  return { ...spec, allowed: [...allowed, "ask_advisor"] };
}

// --- mocked tools ------------------------------------------------------------

export const CODEWORD = "zebra-7";

export interface LookupCodewordTool {
  definition: ToolDefinition;
  /** Executions observed, across every run sharing the fixture. */
  calls(): number;
}

/** A deterministic lookup the live model is told to call and echo back. */
export function lookupCodewordTool(): LookupCodewordTool {
  let calls = 0;
  return {
    definition: scriptedTool({
      name: "lookup_codeword",
      description:
        "Look up the secret codeword. Takes no arguments and returns { codeword }.",
      execute: () => {
        calls += 1;
        return Promise.resolve({ content: { codeword: CODEWORD } });
      },
    }),
    calls: () => calls,
  };
}

export interface WaitTool {
  definition: ToolDefinition;
  /** Resolves when the first wait call begins executing. */
  started: Promise<void>;
  /** Wait calls aborted by their execution signal. */
  aborted(): number;
}

/**
 * A blocking tool that gives interrupt/steer tests a deterministic mid-run
 * window: it resolves after `ms` or settles early (as an error result) when
 * the call's execution signal aborts.
 */
export function waitTool(): WaitTool {
  let signalStarted!: () => void;
  const started = new Promise<void>((resolve) => {
    signalStarted = resolve;
  });
  let aborted = 0;
  return {
    definition: scriptedTool({
      name: "wait",
      description:
        'Block for the given duration, then return. Input: { "ms": number }.',
      execute: (input, ctx) =>
        new Promise((resolve) => {
          signalStarted();
          const ms = typeof input.ms === "number" ? input.ms : 60_000;
          const timer = setTimeout(() => {
            resolve({ content: { waited_ms: ms } });
          }, ms);
          ctx.signal.addEventListener(
            "abort",
            () => {
              aborted += 1;
              clearTimeout(timer);
              resolve({ content: "wait aborted", isError: true });
            },
            { once: true },
          );
        }),
    }),
    started,
    aborted: () => aborted,
  };
}

export interface ProbeWindow {
  /** Epoch ms; brackets the mocked execution body. */
  start: number;
  end: number;
}

export interface ProbeTool {
  definition: ToolDefinition;
  /** One window per completed execution, in settle order. */
  windows(): readonly ProbeWindow[];
}

/**
 * A slow deterministic tool whose recorded execution windows expose batch
 * behavior: overlapping windows prove concurrent dispatch, an empty list
 * proves a call was rejected undispatched. `exclusive` sets
 * `isBatchExecutionForbidden` for the policy scenarios.
 */
export function probeTool(
  name: string,
  delayMs: number,
  options: { exclusive?: boolean } = {},
): ProbeTool {
  const windows: ProbeWindow[] = [];
  return {
    definition: scriptedTool({
      name,
      description: `Run the ${name} probe (takes ~${String(delayMs)}ms, no arguments) and return { probe }.`,
      isBatchExecutionForbidden: options.exclusive,
      execute: async () => {
        const start = Date.now();
        await new Promise((resolve) => setTimeout(resolve, delayMs));
        windows.push({ start, end: Date.now() });
        return { content: { probe: name } };
      },
    }),
    windows: () => windows,
  };
}

/**
 * A terminal stand-in for engine-direct runs. A real `summary` schema (not
 * the permissive scripted-tool object) so the live model sees the field in
 * the tool spec and the submission stays assertable.
 */
export function finishTaskTool(): ToolDefinition {
  return defineTool({
    name: "finish_task",
    description:
      "Finish the task with a one-line summary. Terminal: a successful call ends the run.",
    input: z.object({ summary: z.string().min(1) }),
    isTerminal: true,
    execute: (input) => Promise.resolve({ content: { summary: input.summary } }),
  });
}

// --- runtime fixture ----------------------------------------------------------

export interface RuntimeFixtureOptions {
  /** The REAL on-disk llm clients config (or a corrupted copy). */
  llmClientsPath: string;
  profiles: readonly ProfileSpec[];
  baseTools?: ToolDefinition[];
  /** `HookConfigEntry[]` JSON, written to the fixture's `hooks.json`. */
  hookEntries?: unknown;
}

export interface RuntimeFixture {
  runtime: AgentRuntime;
  dataDir: string;
}

/** Temp profiles + data dir over the configured llm clients. */
export function runtimeFixture(options: RuntimeFixtureOptions): RuntimeFixture {
  const root = tempDir("eos-agent-runtime-e2e-");
  const profilesDir = join(root, "profiles");
  mkdirSync(profilesDir, { recursive: true });
  for (const spec of options.profiles) {
    writeProfile(profilesDir, advisoryReadyProfile(spec));
  }
  const hookConfigPath = join(root, "hooks.json");
  if (options.hookEntries !== undefined) {
    writeFileSync(hookConfigPath, JSON.stringify(options.hookEntries));
  }
  const dataDir = join(root, "data");
  const runtime = createAgentRuntime({
    agentProfilesDir: profilesDir,
    llmClientsPath: options.llmClientsPath,
    baseTools: options.baseTools,
    hookConfigPath,
    dataDir,
  });
  return { runtime, dataDir };
}

// --- polling -------------------------------------------------------------------

/** Poll `check` until true; live settle times are network-bound. */
export async function until(
  label: string,
  check: () => boolean,
  timeoutMs = 60_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    if (check()) return;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

/** The registry row for one agent name, if that run was ever started. */
export function runOf(runtime: AgentRuntime, agentName: string): RunSummary | undefined {
  return runtime.listRuns().find((run) => run.agent_name === agentName);
}

/** Wait until the named run is registered and finished, then return its row. */
export async function finishedRun(
  runtime: AgentRuntime,
  agentName: string,
  timeoutMs = 60_000,
): Promise<RunSummary> {
  await until(
    `run "${agentName}" to finish`,
    () => runOf(runtime, agentName)?.status === "finished",
    timeoutMs,
  );
  const run = runOf(runtime, agentName);
  if (run === undefined) throw new Error(`run "${agentName}" disappeared`);
  return run;
}

// --- provider-history assertions ---------------------------------------------

/** Index of the first user message whose text contains `needle`, or -1. */
export function userMessageIndex(llm: readonly Message[], needle: string): number {
  return llm.findIndex(
    (message) => message.role === "user" && assistantText(message).includes(needle),
  );
}

/** Every drained `session_settled` notification message, in arrival order. */
export function sessionSettledMessages(llm: readonly Message[]): Message[] {
  return llm.filter(
    (message) =>
      message.role === "user" &&
      assistantText(message).includes('"session_settled"'),
  );
}

export interface ToolResultView {
  tool_use_id: string;
  content: string;
  is_error: boolean;
}

/** Every `tool_result` block across the provider history, in order. */
export function toolResultsIn(llm: readonly Message[]): ToolResultView[] {
  const results: ToolResultView[] = [];
  for (const message of llm) {
    for (const block of message.content) {
      if (block.type === "tool_result") {
        results.push({
          tool_use_id: block.tool_use_id,
          content: block.content,
          is_error: block.is_error,
        });
      }
    }
  }
  return results;
}

/** `tool_use` ids with no answering `tool_result`; must be empty at finish. */
export function unansweredToolUses(llm: readonly Message[]): string[] {
  const answered = new Set(toolResultsIn(llm).map((result) => result.tool_use_id));
  const unanswered: string[] = [];
  for (const message of llm) {
    for (const block of message.content) {
      if (block.type === "tool_use" && !answered.has(block.tool_use_id)) {
        unanswered.push(block.tool_use_id);
      }
    }
  }
  return unanswered;
}

/** Narrow a completed outcome's submission to its object payload. */
export function submissionOf(outcome: AgentRunOutcome): JsonObject {
  if (outcome.status !== "completed") {
    throw new Error(`expected a completed outcome, got ${outcome.status}`);
  }
  const submission = outcome.submission;
  if (
    typeof submission !== "object" ||
    submission === null ||
    Array.isArray(submission)
  ) {
    throw new Error(`expected an object submission, got ${JSON.stringify(submission)}`);
  }
  return submission;
}
