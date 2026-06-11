import { mkdirSync } from "node:fs";
import { appendFile, readFile } from "node:fs/promises";
import { join } from "node:path";

import type {
  AgentKind,
  AgentRunId,
  JsonValue,
  Message,
  ToolCallResult,
  ToolUseId,
} from "@eos/contracts";
import type { AgentEvent, AgentRunOutcome } from "@eos/engine";
import { cacheHitRate, type ReasoningEffort, type UsageSnapshot } from "@eos/llm-client";

/** Where one run's transcript lives under the runtime's data dir. */
export function runTranscriptPath(dataDir: string, runId: string): string {
  return join(dataDir, "runs", runId, "transcript.jsonl");
}

interface SequencedLine {
  seq: number;
  ts: string;
}

/** One conversation-shaping entry, before the writer stamps `seq`/`ts`. */
type TranscriptEntry =
  | { kind: "user"; origin: "initial" | "steer"; message: Message }
  | { kind: "assistant"; message: Message }
  | { kind: "tool_result"; result: ToolCallResult }
  | { kind: "notification"; text: string }
  | {
      kind: "run_finished";
      outcome_status: string;
      interrupt_reason?: string;
      submission?: JsonValue;
    };

/** One JSONL line; snake_case: serialized. */
export type TranscriptLine = SequencedLine & TranscriptEntry;

type EventEntry =
  | {
      type: "run_started";
      run_id: AgentRunId;
      agent_name: string;
      agent_kind: AgentKind;
      parent?: AgentRunId;
      llm_client_id: string;
      model_id: string;
      reasoning_effort?: ReasoningEffort;
      max_turns: number;
    }
  | { type: "turn_started"; turn: number }
  | {
      type: "turn_completed";
      turn: number;
      stop_reason?: string;
      usage: UsageSnapshot;
      cache_hit_rate: number;
    }
  | { type: "tool_started"; turn: number; tool_use_id: ToolUseId; name: string }
  | {
      type: "tool_completed";
      turn: number;
      tool_use_id: ToolUseId;
      name: string;
      is_error: boolean;
      is_terminal: boolean;
      duration_ms: number;
    }
  | { type: "run_finished"; status: "completed" | "cancelled" | "failed" };

/** One audit lifecycle line; snake_case: serialized. */
export type EventLine = SequencedLine & EventEntry;

export type ResultLine = SequencedLine & {
  run_id: AgentRunId;
  agent_name: string;
  agent_kind: AgentKind;
  parent?: AgentRunId;
  llm_client_id: string;
  model_id: string;
  status: "completed" | "cancelled" | "failed";
  interrupt_reason?: string;
  failure?: { kind: string; message: string };
  submission?: JsonValue;
  turns: number;
  usage: UsageSnapshot;
  cache_hit_rate: number;
  started_at: string;
  finished_at: string;
  duration_ms: number;
};

export interface RunLogMeta {
  run_id: AgentRunId;
  agent_name: string;
  agent_kind: AgentKind;
  parent?: AgentRunId;
  llm_client_id: string;
  model_id: string;
  reasoning_effort?: ReasoningEffort;
  max_turns: number;
}

/**
 * Per-run JSONL writer: one ordered append queue, fed by the runtime's
 * event subscriber. `events.jsonl` records lifecycle facts, `transcript.jsonl`
 * keeps the Phase 04.5 conversation artifact, and `result.jsonl` receives one
 * terminal rollup line. A failed write latches and resurfaces on every later
 * `flush()`.
 */
export class RunLog {
  readonly transcriptPath: string;
  readonly #eventsPath: string;
  readonly #resultPath: string;
  readonly #meta: RunLogMeta;
  readonly #runDir: string;
  readonly #startedAt: string;
  readonly #startedMs: number;
  #seq = 0;
  #turn = 0;
  #queue: Promise<void> = Promise.resolve();
  #directoryReady = false;
  #failure: Error | undefined;

  constructor(dataDir: string, meta: RunLogMeta) {
    this.#meta = meta;
    this.#runDir = join(dataDir, "runs", meta.run_id);
    this.#eventsPath = join(this.#runDir, "events.jsonl");
    this.transcriptPath = join(this.#runDir, "transcript.jsonl");
    this.#resultPath = join(this.#runDir, "result.jsonl");
    this.#startedMs = Date.now();
    this.#startedAt = new Date(this.#startedMs).toISOString();
    this.#appendEvent({
      type: "run_started",
      run_id: meta.run_id,
      agent_name: meta.agent_name,
      agent_kind: meta.agent_kind,
      ...(meta.parent !== undefined && { parent: meta.parent }),
      llm_client_id: meta.llm_client_id,
      model_id: meta.model_id,
      ...(meta.reasoning_effort !== undefined && {
        reasoning_effort: meta.reasoning_effort,
      }),
      max_turns: meta.max_turns,
    });
  }

  /** Append the lines for one engine event; streaming deltas are skipped. */
  append(event: AgentEvent): void {
    switch (event.type) {
      case "turn_started":
        this.#turn = event.turn;
        this.#appendEvent({ type: "turn_started", turn: event.turn });
        return;
      case "assistant_message_complete":
        this.#appendTranscript({ kind: "assistant", message: event.message });
        this.#appendEvent({
          type: "turn_completed",
          turn: this.#turn,
          ...(event.stop_reason !== undefined && { stop_reason: event.stop_reason }),
          usage: event.usage,
          cache_hit_rate: cacheHitRate(event.usage),
        });
        return;
      case "tool_execution_started":
        this.#appendEvent({
          type: "tool_started",
          turn: this.#turn,
          tool_use_id: event.tool_use_id,
          name: event.name,
        });
        return;
      case "tool_execution_completed":
        this.#appendEvent({
          type: "tool_completed",
          turn: this.#turn,
          tool_use_id: event.tool_use_id,
          name: event.name,
          is_error: event.is_error,
          is_terminal: event.is_terminal,
          duration_ms: event.tool_end_time - event.tool_start_time,
        });
        this.#appendTranscript({
          kind: "tool_result",
          result: {
            tool_use_id: event.tool_use_id,
            content: event.output,
            is_error: event.is_error,
            is_terminal: event.is_terminal,
            tool_start_time: event.tool_start_time,
            tool_end_time: event.tool_end_time,
            ...(event.metadata !== undefined && { metadata: event.metadata }),
          },
        });
        return;
      case "run_finished":
        this.#appendEvent({
          type: "run_finished",
          status: event.outcome.status,
        });
        this.#appendTranscript(runFinishedTranscriptEntry(event.outcome));
        this.#appendResult(this.#resultEntry(event.outcome));
        return;
      case "assistant_text_delta":
      case "reasoning_delta":
      case "tool_use_delta":
        return;
    }
  }

  /** Initial and (future) steered user input; the runtime is the source. */
  appendUser(origin: "initial" | "steer", message: Message): void {
    this.#appendTranscript({ kind: "user", origin, message });
  }

  /** Resolves once every line appended so far is on disk. */
  flush(): Promise<void> {
    return this.#queue.then(() => {
      if (this.#failure) throw this.#failure;
    });
  }

  #appendEvent(entry: EventEntry): void {
    this.#enqueue(this.#eventsPath, entry);
  }

  #appendTranscript(entry: TranscriptEntry): void {
    this.#enqueue(this.transcriptPath, entry);
  }

  #appendResult(entry: Omit<ResultLine, "seq" | "ts">): void {
    this.#enqueue(this.#resultPath, entry);
  }

  #enqueue(path: string, entry: object): void {
    const line: SequencedLine & object = {
      seq: this.#seq,
      ts: new Date().toISOString(),
      ...entry,
    };
    this.#seq += 1;
    this.#queue = this.#queue
      .then(() => this.#write(path, line))
      .catch((error: unknown) => {
        this.#failure ??= error instanceof Error ? error : new Error(String(error));
      });
  }

  #resultEntry(outcome: AgentRunOutcome): Omit<ResultLine, "seq" | "ts"> {
    const finishedMs = Date.now();
    return {
      run_id: this.#meta.run_id,
      agent_name: this.#meta.agent_name,
      agent_kind: this.#meta.agent_kind,
      ...(this.#meta.parent !== undefined && { parent: this.#meta.parent }),
      llm_client_id: this.#meta.llm_client_id,
      model_id: this.#meta.model_id,
      status: outcome.status,
      ...(outcome.status === "cancelled" && {
        interrupt_reason: outcome.reason,
      }),
      ...(outcome.status === "failed" && { failure: outcome.failure }),
      ...(outcome.status === "completed" &&
        outcome.submission !== undefined && { submission: outcome.submission }),
      turns: outcome.turns,
      usage: outcome.usage,
      cache_hit_rate: cacheHitRate(outcome.usage),
      started_at: this.#startedAt,
      finished_at: new Date(finishedMs).toISOString(),
      duration_ms: finishedMs - this.#startedMs,
    };
  }

  async #write(path: string, line: SequencedLine & object): Promise<void> {
    if (!this.#directoryReady) {
      mkdirSync(this.#runDir, { recursive: true });
      this.#directoryReady = true;
    }
    await appendFile(path, `${JSON.stringify(line)}\n`, "utf8");
  }
}

function runFinishedTranscriptEntry(outcome: AgentRunOutcome): TranscriptEntry {
  return {
    kind: "run_finished",
    outcome_status: outcome.status,
    ...(outcome.status === "cancelled" && { interrupt_reason: outcome.reason }),
    ...(outcome.status === "completed" &&
      outcome.submission !== undefined && { submission: outcome.submission }),
  };
}

/** One byte-offset read of a transcript file. */
export interface TranscriptRead {
  data: string;
  /** Resume offset (bytes); clamped to the file size. */
  next_offset: number;
  eof: boolean;
}

/**
 * Byte-offset reader with a per-call cap. Offsets are byte positions by
 * contract: callers resume from `next_offset`, so a chunk boundary may
 * split a line (or a multibyte character) and concatenation restores it.
 */
export async function readTranscriptFile(
  path: string,
  offset: number,
  maxBytes: number,
): Promise<TranscriptRead> {
  const buffer = await readFile(path);
  const start = Math.min(Math.max(offset, 0), buffer.length);
  const end = Math.min(buffer.length, start + Math.max(maxBytes, 0));
  const slice = buffer.subarray(start, end);
  return {
    data: slice.toString("utf8"),
    next_offset: end,
    eof: end >= buffer.length,
  };
}
