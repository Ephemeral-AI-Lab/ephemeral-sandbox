import { z } from "zod";

import type { AgentRunSnapshot } from "../contract.js";
import { CommandHookSchema, type HookBackgroundSession } from "./protocol.js";
import { spawnJsonCommand } from "./spawn.js";

/**
 * Notification trigger rules (Phase 04.9): loop-lifecycle rules sharing
 * `.eos-agents/hooks.json` with the tool-scoped hook entries. Triggers are
 * notification-only — scripts inform, the model acts; no trigger can deny,
 * rewrite state, or cancel anything.
 */
export const TriggerRuleEntrySchema = z.discriminatedUnion("event", [
  z.object({
    event: z.literal("TurnCompleted"),
    hooks: z.array(CommandHookSchema).min(1),
  }),
  z.object({
    event: z.literal("IdleParked"),
    /** Park lifetime before the rule fires; one shot per park entry. */
    timeout_ms: z.number().int().positive(),
    hooks: z.array(CommandHookSchema).min(1),
  }),
]);
export type TriggerRuleEntry = z.infer<typeof TriggerRuleEntrySchema>;

export type TriggerCommand = z.infer<typeof CommandHookSchema>;

/** Serialized mirror of the engine's `TurnFacts`; same two axes. */
export interface TurnCompletedFacts {
  /** Budget axis. */
  turn: number;
  /** Budget axis. */
  max_turns: number;
  /** Shape axis; 0 means bare text. */
  tool_calls: number;
  /** Shape axis. */
  live_sessions: number;
  /** Shape axis. */
  has_pending_steers: boolean;
}

export interface IdleTimeoutFacts {
  idle_elapsed_ms: number;
  timeout_ms: number;
}

/** What a trigger script receives (JSON over stdin; snake_case: crosses the process boundary). */
export interface TriggerPayload {
  /** Config subscribes to "IdleParked"; the occurrence is "IdleTimeout". */
  event: "TurnCompleted" | "IdleTimeout";
  facts: TurnCompletedFacts | IdleTimeoutFacts;
  run: AgentRunSnapshot;
  /** From the profile; scripts never hardcode submit_* names. */
  terminal_tool: string;
  /** Running plus settled-but-undelivered, as in tool hook payloads. */
  background_sessions: readonly HookBackgroundSession[];
}

/**
 * What a trigger script may answer. Strict: `decision` and `updatedInput`
 * are meaningless here and are rejected, not stripped. Empty stdout, `{}`,
 * or an absent `notification` means skip.
 */
export const TriggerOutputSchema = z.strictObject({
  notification: z.string().min(1).optional(),
});
export type TriggerOutput = z.infer<typeof TriggerOutputSchema>;

/** One command's settled run: a reminder to publish, a failure to log, or neither (skip). */
export interface TriggerCommandRun {
  notification?: string;
  /** Operator-facing; the firing is dropped. */
  warning?: string;
}

/** The runner seam `NotificationTriggerEngine` calls; spawn-backed by default, stubbed in tests. */
export type TriggerCommandRunner = (
  command: TriggerCommand,
  payload: TriggerPayload,
) => Promise<TriggerCommandRun>;

/**
 * The spawn-backed runner over the shared hook command mechanics (shell
 * spawn, payload JSON + newline on stdin, per-command timeout). Never
 * rejects: every failure — spawn fault, timeout, nonzero exit, bad JSON,
 * schema mismatch — settles as a `warning` and the firing is dropped.
 */
export const runTriggerCommand: TriggerCommandRunner = async (command, payload) => {
  let settled;
  try {
    settled = await spawnJsonCommand(command, payload);
  } catch (error) {
    return {
      warning: `trigger command failed: ${error instanceof Error ? error.message : String(error)}`,
    };
  }
  if (settled.kind === "spawn_error") {
    return { warning: `trigger command failed to spawn: ${settled.message}` };
  }
  if (settled.kind === "aborted") {
    return { warning: "trigger command timed out" };
  }
  if (settled.code !== 0) {
    return {
      warning: `trigger command exited ${String(settled.code)}: ${settled.stderr.trim() || "(no stderr)"}`,
    };
  }
  const trimmed = settled.stdout.trim();
  if (!trimmed) return {};
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return { warning: `trigger stdout was not JSON: ${trimmed.slice(0, 200)}` };
  }
  const checked = TriggerOutputSchema.safeParse(parsed);
  if (!checked.success) {
    return {
      warning: `trigger stdout did not match TriggerOutput: ${checked.error.issues
        .map((issue) => issue.message)
        .join("; ")}`,
    };
  }
  return checked.data.notification === undefined
    ? {}
    : { notification: checked.data.notification };
};
