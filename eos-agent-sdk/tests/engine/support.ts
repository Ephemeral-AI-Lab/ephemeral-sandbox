import {
  toolUseIdFrom,
  type ContentBlock,
  type JsonObject,
  type Message,
  type UserMessage,
} from "../../src/contracts/index.js";
import type {
  LlmClient,
  LlmRequest,
  LlmStreamEvent,
  LlmStreamOptions,
  StopReason,
  UsageSnapshot,
} from "../../src/llm-client/index.js";
import { NotificationInbox, type Notifier } from "../../src/notification/index.js";
import type { BackgroundTaskSupervisor } from "../../src/background/index.js";

import {
  runAgentLoop,
  type TaskRegistryGate,
  type TerminationMode,
  type TurnFacts,
} from "../../src/engine/agent-loop.js";
import { Conversation, type ConversationRecord } from "../../src/engine/conversation.js";
import { RunHandle, type AgentEvent } from "../../src/engine/run-handle.js";
import type {
  ToolBatchContext,
  ToolExecutor,
  ToolUseBlock,
} from "../../src/engine/tool-executor.js";
import { agentRunIdFrom } from "../../src/contracts/index.js";

// --- scripted provider client (engine-local; testkit depends on tool) ---

export type ScriptedTurn = (
  request: LlmRequest,
  signal: AbortSignal | undefined,
) => AsyncIterable<LlmStreamEvent>;

class ScriptedClient implements LlmClient {
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

export function scripted(events: LlmStreamEvent[]): ScriptedTurn {
  return async function* () {
    for (const event of events) {
      await Promise.resolve();
      yield event;
    }
  };
}

export function hanging(onStart?: () => void): ScriptedTurn {
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

const USAGE: UsageSnapshot = { input_tokens: 1, output_tokens: 1 };

export function complete(
  message: Message,
  stop_reason: StopReason = "end_turn",
): LlmStreamEvent {
  return { type: "assistant_message_complete", message, usage: USAGE, stop_reason };
}

export function assistant(...content: ContentBlock[]): Message {
  return { role: "assistant", content };
}

export function text(value: string): ContentBlock {
  return { type: "text", text: value };
}

export function toolUse(
  id: string,
  name: string,
  input: JsonObject = {},
): Extract<ContentBlock, { type: "tool_use" }> {
  return { type: "tool_use", tool_use_id: toolUseIdFrom(id), name, input };
}

export function user(value: string): UserMessage {
  return { role: "user", content: [{ type: "text", text: value }] };
}

// --- stub task registry gate ---------------------------------------------

class StubTasks implements TaskRegistryGate {
  #open = 0;
  #changes = 0;
  #wakers = new Set<() => void>();
  disposedWith: string | undefined;

  setOpen(count: number): void {
    this.#open = count;
    this.#changes += 1;
    for (const wake of [...this.#wakers]) wake();
  }

  isEmpty(): boolean {
    return this.#open === 0;
  }

  count(): number {
    return this.#open;
  }

  changeCount(): number {
    return this.#changes;
  }

  waitForChange(since: number, signal: AbortSignal): Promise<void> {
    if (this.#changes !== since || signal.aborted) return Promise.resolve();
    return new Promise((resolve) => {
      const wake = (): void => {
        this.#wakers.delete(wake);
        signal.removeEventListener("abort", wake);
        resolve();
      };
      this.#wakers.add(wake);
      signal.addEventListener("abort", wake);
    });
  }

  disposeAll(reason: string): Promise<void> {
    this.disposedWith = reason;
    this.#open = 0;
    return Promise.resolve();
  }
}

// --- loop fixture ----------------------------------------------------------

export type StubHandler = (
  call: ToolUseBlock,
  batch: ToolBatchContext,
) => Promise<{
  content: string;
  is_error?: boolean;
  is_terminal?: boolean;
}>;

function stubExecutor(handlers: Record<string, StubHandler>): ToolExecutor {
  return {
    specs: () => [],
    executeBatch: async (calls, batch) => {
      const results = [];
      for (const call of calls) {
        const handler = handlers[call.name] as StubHandler | undefined;
        if (!handler) continue; // exercise the loop's normalizeBatch fill
        batch.emit({
          type: "tool_execution_started",
          tool_use_id: call.tool_use_id,
          name: call.name,
          input: call.input,
        });
        const settled = await handler(call, batch);
        const at = Date.now();
        results.push({
          tool_use_id: call.tool_use_id,
          content: settled.content,
          is_error: settled.is_error ?? false,
          is_terminal: settled.is_terminal ?? false,
          tool_start_time: at,
          tool_end_time: at,
        });
      }
      return results;
    },
  };
}

export interface LoopFixture {
  handle: RunHandle;
  inbox: NotificationInbox;
  tasks: StubTasks;
  client: ScriptedClient;
  records: ConversationRecord[];
  events: AgentEvent[];
  loop: Promise<void>;
}

export function startLoop(options: {
  turns: ScriptedTurn[];
  mode?: TerminationMode<string>;
  maxTurns?: number;
  tools?: Record<string, StubHandler>;
  onTurnBoundary?: (facts: TurnFacts) => Promise<void>;
  initial?: UserMessage[];
}): LoopFixture {
  const inbox = new NotificationInbox();
  const tasks = new StubTasks();
  const notifier: Notifier = {
    publish: (message, opts) => {
      inbox.publish(message, opts);
    },
  };
  const supervisor = {
    register: () => {
      throw new Error("stub supervisor cannot register");
    },
    list: () => [],
    cancel: () => Promise.resolve(false),
  } as unknown as BackgroundTaskSupervisor;
  const events: AgentEvent[] = [];
  const handle = new RunHandle({
    runId: agentRunIdFrom("run-fixture"),
    backgroundTaskSupervisor: supervisor,
    notifier,
    tap: (event) => {
      events.push(event);
    },
  });
  const records: ConversationRecord[] = [];
  const conversation = new Conversation(options.initial ?? [user("go")], (entry) => {
    records.push(entry);
  });
  const client = new ScriptedClient(options.turns);
  const loop = runAgentLoop<string>({
    handle,
    conversation,
    tools: stubExecutor(options.tools ?? {}),
    turnConfig: {
      client,
      model: "scripted-model",
      maxTokens: 128,
      toolSpecs: () => [],
    },
    maxTurns: options.maxTurns ?? 8,
    inbox,
    tasks,
    onTurnBoundary: options.onTurnBoundary,
    mode: options.mode ?? { kind: "text" },
  });
  return { handle, inbox, tasks, client, records, events, loop };
}

export function flushMicrotasks(rounds = 20): Promise<void> {
  let chain: Promise<void> = Promise.resolve();
  for (let i = 0; i < rounds; i += 1) chain = chain.then(() => undefined);
  return chain;
}
