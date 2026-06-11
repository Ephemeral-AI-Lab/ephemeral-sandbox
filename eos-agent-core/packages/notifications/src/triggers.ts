import {
  AgentKindSchema,
  type AgentKind,
  type AgentRunSnapshot,
  type BackgroundSessionSnapshot,
} from "@eos/contracts";
import { z } from "zod";

/**
 * One notification-rule command script in `notification_rules.json`:
 * spawned with `shell: true`, payload JSON + newline on stdin. Owned by
 * this package — other operator configs have look-alike command shapes,
 * but each config's schema evolves independently.
 */
export const CommandScriptSchema = z.object({
  type: z.literal("command"),
  command: z.string().min(1),
  /** Working directory for relative command paths; runtime-filled when omitted. */
  cwd: z.string().min(1).optional(),
  /** Default 60 000. */
  timeout_ms: z.number().int().positive().optional(),
});
export type CommandScript = z.infer<typeof CommandScriptSchema>;

/** Matcher axes shared by every rule kind; absent fields match every run. */
const TriggerRuleMatchers = {
  /** Exact profile name; absent matches all agents. */
  agent_name: z.string().min(1).optional(),
  /** Profile kind; absent matches all kinds. Present with `agent_name`: AND. */
  agent_kind: AgentKindSchema.optional(),
};

/**
 * Notification trigger rules (Phase 04.9): loop-lifecycle rules loaded
 * from `.eos-agents/notification_rules.json`, keyed by an inner `rules`
 * command list. Triggers are notification-only — scripts inform, the
 * model acts; no trigger can deny, rewrite state, or cancel anything.
 */
export const TriggerRuleEntrySchema = z.discriminatedUnion("event", [
  z.object({
    event: z.literal("TurnCompleted"),
    ...TriggerRuleMatchers,
    rules: z.array(CommandScriptSchema).min(1),
  }),
  z.object({
    event: z.literal("IdleParked"),
    ...TriggerRuleMatchers,
    /** Park lifetime before the rule fires; one shot per park entry. */
    timeout_ms: z.number().int().positive(),
    rules: z.array(CommandScriptSchema).min(1),
  }),
]);
export type TriggerRuleEntry = z.infer<typeof TriggerRuleEntrySchema>;

/** The matcher fold: absent fields match every run; present fields AND. */
export function triggerRuleAppliesTo(
  rule: TriggerRuleEntry,
  run: { agent_name: string; agent_kind: AgentKind },
): boolean {
  if (rule.agent_name !== undefined && rule.agent_name !== run.agent_name) return false;
  if (rule.agent_kind !== undefined && rule.agent_kind !== run.agent_kind) return false;
  return true;
}

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
  /** Running plus settled-but-undelivered sessions for this run. */
  background_sessions: readonly BackgroundSessionSnapshot[];
}

/**
 * What a trigger script may answer. Strict: `decision` and `updatedInput`
 * are meaningless here and are rejected, not stripped. Empty stdout,
 * `{}`, or an absent `notification` means skip.
 */
export const TriggerOutputSchema = z.strictObject({
  notification: z.string().min(1).optional(),
});

/** One command's settled run: a reminder to publish, a failure to log, or neither (skip). */
export interface TriggerCommandRun {
  notification?: string;
  /** Operator-facing; the firing is dropped. */
  warning?: string;
}

/**
 * The runner seam `NotificationTriggerEngine` calls. The spawn-backed
 * implementation lives with the shared command-spawn mechanics in
 * `@eos/tool` (`runTriggerCommand`), the same dependency direction as the
 * tool layer implementing the engine's `ToolExecutor` port; tests stub it.
 */
export type TriggerCommandRunner = (
  command: CommandScript,
  payload: TriggerPayload,
) => Promise<TriggerCommandRun>;
