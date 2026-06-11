import { JsonObjectSchema, type JsonObject, type ToolUseId } from "@eos/contracts";
import { z } from "zod";

import type { AgentRunSnapshot } from "../contract.js";

export const HookEventSchema = z.enum([
  "PreToolUse",
  "PostToolUse",
  "PostToolUseFailure",
]);
export type HookEvent = z.infer<typeof HookEventSchema>;

export interface HookBackgroundSession {
  type: string;
  id: string;
  status: "running" | "completed" | "failed" | "cancelled";
  /** ISO-8601 registration time. */
  started_at: string;
  summary?: string;
  description?: string;
}

export interface HookAdvisoryRequirement {
  required: boolean;
  advisor_prompt?: string;
}

/**
 * What a hook receives (JSON over stdin for command hooks). snake_case:
 * crosses the process boundary. Hooks receive snapshots only, never live
 * objects or ports.
 */
export interface HookPayload {
  event: HookEvent;
  tool_name: string;
  tool_input: JsonObject;
  tool_use_id: ToolUseId;
  run: AgentRunSnapshot;
  /** Running plus settled-but-undelivered sessions for this run. */
  background_sessions?: readonly HookBackgroundSession[];
  /** PreToolUse policy fact for hooks that gate advisory-required tools. */
  advisory_requirement?: HookAdvisoryRequirement;
  /** PostToolUse only (string projection of the outcome content). */
  tool_response?: string;
  /** PostToolUseFailure only. */
  error?: string;
}

/**
 * What a hook may answer. Hooks cannot rewrite tool output and there is
 * no `ask` decision - the engine is headless; advisor approval is a tool.
 */
export const HookOutputSchema = z.object({
  decision: z.enum(["allow", "deny"]).optional(),
  /** Deny: model-visible feedback. */
  reason: z.string().optional(),
  /** PreToolUse only; re-validated through the tool's own schema. */
  updatedInput: JsonObjectSchema.optional(),
  /** Published as a `hook_context` notification, never folded into the result. */
  additionalContext: z.string().optional(),
});
export type HookOutput = z.infer<typeof HookOutputSchema>;

export type HookCommand =
  | {
      /** Spawned with `shell: true`; payload JSON + newline on stdin. */
      type: "command";
      command: string;
      /** Working directory for relative command paths; runtime-filled when omitted. */
      cwd?: string;
      /** Default 60 000. */
      timeout_ms?: number;
    }
  | {
      /** In-process adapter for tests and SDK callers. */
      type: "callback";
      run(payload: HookPayload, signal: AbortSignal): Promise<HookOutput>;
    };

export interface HookConfigEntry {
  event: HookEvent;
  /** Exact tool name; absent matches all tools. Rule-content matchers are a later seam. */
  matcher?: string;
  hooks: HookCommand[];
}

/** The serializable command-hook shape shared by tool hooks and trigger rules. */
export const CommandHookSchema = z.object({
  type: z.literal("command"),
  command: z.string().min(1),
  cwd: z.string().min(1).optional(),
  timeout_ms: z.number().int().positive().optional(),
});

/** The serializable subset a hooks.json file may hold (loading is Phase 04.5). */
export const HookConfigEntrySchema = z.object({
  event: HookEventSchema,
  matcher: z.string().min(1).optional(),
  hooks: z.array(CommandHookSchema),
});

/** One event's hook outputs folded by the precedence kernel. */
export interface CombinedHookOutcome {
  /** `deny > allow > passthrough`; all hooks still run after a deny. */
  decision: "deny" | "allow" | "passthrough";
  /** Model-visible reason when denied. */
  reason?: string;
  /** Present only when exactly one hook supplied it. */
  updatedInput?: JsonObject;
  additionalContexts: string[];
}

/**
 * The precedence kernel, centralized: capability filtering per event
 * (PostToolUse/PostToolUseFailure may only add context), `deny > allow >
 * passthrough` across parallel hooks, and the single-update rule - two or
 * more updates deny deterministically instead of merge-guessing.
 */
export function combineHookOutputs(
  event: HookEvent,
  outputs: HookOutput[],
): CombinedHookOutcome {
  const additionalContexts = outputs
    .map((output) => output.additionalContext)
    .filter((context): context is string => Boolean(context));
  if (event !== "PreToolUse") {
    return { decision: "passthrough", additionalContexts };
  }
  const denials = outputs.filter((output) => output.decision === "deny");
  if (denials.length > 0) {
    const reasons = denials
      .map((denial) => denial.reason)
      .filter((reason): reason is string => Boolean(reason));
    return {
      decision: "deny",
      reason: reasons.join("; ") || "denied by PreToolUse hook",
      additionalContexts,
    };
  }
  const updates = outputs.filter((output) => output.updatedInput !== undefined);
  if (updates.length > 1) {
    return {
      decision: "deny",
      reason: `${String(updates.length)} hooks supplied conflicting updatedInput`,
      additionalContexts,
    };
  }
  const combined: CombinedHookOutcome = {
    decision: outputs.some((output) => output.decision === "allow")
      ? "allow"
      : "passthrough",
    additionalContexts,
  };
  if (updates.length === 1) combined.updatedInput = updates[0].updatedInput;
  return combined;
}
