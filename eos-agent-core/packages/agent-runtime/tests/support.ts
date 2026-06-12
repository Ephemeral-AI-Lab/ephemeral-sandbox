import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  toolUseIdFrom,
  type ContentBlock,
  type JsonObject,
  type Message,
} from "@eos/contracts";
import type {
  LlmClient,
  LlmRequest,
  LlmStreamEvent,
  LlmStreamOptions,
  StopReason,
  UsageSnapshot,
} from "@eos/llm-client";

import type { LlmClientRegistry } from "../src/llm-client-registry.js";
import type { UserMessage } from "../src/runtime.js";
import type {
  EventLine,
  ResultLine,
  TranscriptLine,
} from "../src/transcript.js";

// --- scripted provider client (local copy of the engine's double) -------------

/** One scripted provider turn; receives the request and the run's signal. */
export type ScriptedTurn = (
  request: LlmRequest,
  signal: AbortSignal | undefined,
) => AsyncIterable<LlmStreamEvent>;

/** In-process `LlmClient` double: one script per provider call, in order. */
export class MockLlmClient implements LlmClient {
  readonly requests: LlmRequest[] = [];
  readonly #turns: ScriptedTurn[];

  constructor(turns: ScriptedTurn[]) {
    this.#turns = turns;
  }

  streamMessage(
    request: LlmRequest,
    options?: LlmStreamOptions,
  ): AsyncIterable<LlmStreamEvent> {
    const script = this.#turns.at(this.requests.length);
    this.requests.push(request);
    if (!script) {
      throw new Error(`unscripted provider call ${String(this.requests.length)}`);
    }
    return script(request, options?.signal);
  }
}

/** A turn that yields the given events, a microtask apart, then completes. */
export function scriptedTurn(events: LlmStreamEvent[]): ScriptedTurn {
  return async function* () {
    for (const event of events) {
      await Promise.resolve();
      yield event;
    }
  };
}

/** A turn whose events are built from the live request (dynamic run ids). */
export function dynamicTurn(
  build: (request: LlmRequest) => LlmStreamEvent[],
): ScriptedTurn {
  return async function* (request) {
    for (const event of build(request)) {
      await Promise.resolve();
      yield event;
    }
  };
}

/** A turn that waits for `release`, then yields its events. */
export function gatedTurn(
  release: Promise<void>,
  events: LlmStreamEvent[],
): ScriptedTurn {
  return async function* () {
    await release;
    for (const event of events) {
      await Promise.resolve();
      yield event;
    }
  };
}

/** A turn that signals `onStart`, then hangs until the run's signal aborts. */
export function hangingTurn(onStart?: () => void): ScriptedTurn {
  // eslint-disable-next-line require-yield -- the stream dies before yielding
  return async function* (_request, signal) {
    onStart?.();
    await new Promise<never>((_resolve, reject) => {
      const fail = (): void => {
        reject(new Error("aborted"));
      };
      if (!signal) return;
      if (signal.aborted) fail();
      else signal.addEventListener("abort", fail, { once: true });
    });
  };
}

// --- event and message builders ------------------------------------------------

const USAGE: UsageSnapshot = { input_tokens: 1, output_tokens: 1 };

export function complete(
  message: Message,
  stop_reason: StopReason = "end_turn",
): LlmStreamEvent {
  return { type: "assistant_message_complete", message, usage: USAGE, stop_reason };
}

export function assistantMessage(...content: ContentBlock[]): Message {
  return { role: "assistant", content };
}

export function textBlock(text: string): ContentBlock {
  return { type: "text", text };
}

export function toolUseBlock(
  id: string,
  name: string,
  input: JsonObject = {},
): Extract<ContentBlock, { type: "tool_use" }> {
  return { type: "tool_use", tool_use_id: toolUseIdFrom(id), name, input };
}

/** A `StartRunParams.initialMessages` entry from raw text. */
export function userMessage(text: string): UserMessage {
  return { role: "user", content: [{ type: "text", text }] };
}

/** The last `tool_result` block in the request, most recent first. */
export function lastToolResult(
  request: LlmRequest,
): Extract<ContentBlock, { type: "tool_result" }> {
  for (const message of [...request.messages].reverse()) {
    for (const block of [...message.content].reverse()) {
      if (block.type === "tool_result") return block;
    }
  }
  throw new Error("no tool_result block in the request");
}

/** The last `tool_result` block content in the request, parsed as JSON. */
export function lastToolResultJson(request: LlmRequest): JsonObject {
  return JSON.parse(lastToolResult(request).content) as JsonObject;
}

// --- in-memory llm client registry ----------------------------------------------

/** Bindings over mock clients; `model-<id>` / `low` stand in for config. */
export function llmRegistry(
  clients: Partial<Record<string, LlmClient>>,
): LlmClientRegistry {
  return {
    require(llmClientId) {
      const client = clients[llmClientId];
      if (client === undefined) {
        throw new Error(`unknown llm client id "${llmClientId}"`);
      }
      return {
        id: llmClientId,
        model_id: `model-${llmClientId}`,
        reasoning_effort: "low",
        client,
      };
    },
  };
}

/** Narrow a JSON value to its string, failing the test otherwise. */
export function asString(value: unknown): string {
  if (typeof value !== "string") {
    throw new Error(`expected a string, got ${typeof value}`);
  }
  return value;
}

// --- fixtures --------------------------------------------------------------------

export function tempDir(prefix: string): string {
  return mkdtempSync(join(tmpdir(), prefix));
}

export interface ProfileSpec {
  name: string;
  kind: "main" | "planner" | "worker" | "advisor" | "subagent";
  llmClientId: string;
  allowed?: readonly string[];
  /** `null` writes no terminal_tool line at all (text-mode profile). */
  terminal?: string | null;
  maxTurns?: number;
  body?: string;
  /** Required for planner/worker kinds; fixtures usually inject it. */
  pursuitContextScript?: string;
}

/** Write `<dir>/<name>.md` in the §4 profile format. */
export function writeProfile(dir: string, spec: ProfileSpec): string {
  const allowed = (spec.allowed ?? []).map((tool) => `  - ${tool}`).join("\n");
  const terminal =
    spec.terminal === null ? undefined : (spec.terminal ?? `submit_${spec.kind}_outcome`);
  const content = [
    "---",
    `name: ${spec.name}`,
    `description: ${spec.name} profile`,
    `llm_client_id: ${spec.llmClientId}`,
    `max_turns: ${String(spec.maxTurns ?? 8)}`,
    `agent_kind: ${spec.kind}`,
    `allowed_tools:${allowed ? `\n${allowed}` : " []"}`,
    ...(terminal === undefined ? [] : [`terminal_tool: ${terminal}`]),
    ...(spec.pursuitContextScript
      ? [`pursuit_context_script: ${spec.pursuitContextScript}`]
      : []),
    "---",
    "",
    spec.body ?? `You are ${spec.name}.`,
    "",
  ].join("\n");
  const path = join(dir, `${spec.name}.md`);
  writeFileSync(path, content);
  return path;
}

/** An unsigned JWT whose payload the codex loader decodes. */
export function mintJwt(payload: Record<string, unknown>): string {
  const encode = (value: unknown): string =>
    Buffer.from(JSON.stringify(value)).toString("base64url");
  return `${encode({ alg: "none" })}.${encode(payload)}.sig`;
}

/** A valid codex JWT payload; spread overrides to break one claim at a time. */
export function codexJwtPayload(
  overrides: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    exp: Math.floor(Date.now() / 1000) + 3600,
    "https://api.openai.com/auth": { chatgpt_account_id: "acct_1" },
    ...overrides,
  };
}

// --- transcript assertions ----------------------------------------------------

function readJsonLines<T>(path: string): T[] {
  return readFileSync(path, "utf8")
    .split("\n")
    .filter((line) => line.length > 0)
    .map((line) => JSON.parse(line) as T);
}

export function readTranscriptLines(path: string): TranscriptLine[] {
  return readJsonLines<TranscriptLine>(path);
}

export function readEventLines(path: string): EventLine[] {
  return readJsonLines<EventLine>(path);
}

export function readResultLines(path: string): ResultLine[] {
  return readJsonLines<ResultLine>(path);
}

export function must<T>(value: T | undefined | null): T {
  if (value === undefined || value === null) {
    throw new Error("expected a value to be present");
  }
  return value;
}
