import type { z } from "zod";

import {
  HookOutputSchema,
  combineHookOutputs,
  type CombinedHookOutcome,
  type HookCommand,
  type HookConfigEntry,
  type HookOutput,
  type HookPayload,
} from "./protocol.js";
import { spawnJsonCommand } from "../spawn.js";

/** One hook's settled run: a structured output, or passthrough + warning. */
interface HookRunResult {
  output: HookOutput;
  warning?: string;
}

/** The kernel's fold plus the non-blocking failures of this event's hooks. */
export interface HookRunSummary extends CombinedHookOutcome {
  /** Operator-facing; lands under the result's `metadata.hook_warnings`. */
  warnings: string[];
}

/**
 * Runs operator-configured hooks for one event: all matching hooks in
 * `Promise.all`, each through its adapter, folded by the precedence
 * kernel. Never throws - per-hook failures become passthrough warnings.
 * Three distinct channels, never collapsed: structured decision (stdout
 * JSON), model feedback (exit 2 stderr), operator warning (everything else).
 */
export class HookEngine {
  readonly #entries: HookConfigEntry[];

  constructor(entries: HookConfigEntry[]) {
    this.#entries = entries;
  }

  async run(payload: HookPayload, signal: AbortSignal): Promise<HookRunSummary> {
    const commands = this.#entries
      .filter((entry) => entry.event === payload.event)
      .filter(
        (entry) => entry.matcher === undefined || entry.matcher === payload.tool_name,
      )
      .flatMap((entry) => entry.hooks);
    const settled = await Promise.all(
      commands.map((command) => runHook(command, payload, signal)),
    );
    const warnings = settled
      .map((result) => result.warning)
      .filter((warning): warning is string => warning !== undefined);
    return {
      ...combineHookOutputs(
        payload.event,
        settled.map((result) => result.output),
      ),
      warnings,
    };
  }
}

function passthrough(warning?: string): HookRunResult {
  return warning === undefined ? { output: {} } : { output: {}, warning };
}

async function runHook(
  command: HookCommand,
  payload: HookPayload,
  signal: AbortSignal,
): Promise<HookRunResult> {
  if (command.type === "callback") {
    try {
      return validateHookOutput(await command.run(payload, signal), "callback hook output");
    } catch (error) {
      return passthrough(`callback hook failed: ${errorMessage(error)}`);
    }
  }
  // A synchronous spawn() fault rejects the command promise; map it to a
  // warning so HookEngine.run keeps its never-throws contract.
  return runCommandHook(command, payload, signal).catch((error: unknown) =>
    passthrough(`hook command failed: ${errorMessage(error)}`),
  );
}

/**
 * The JS-script pluggability over the shared spawn core: per-hook timeout
 * derived from the call's signal (a cancelled run kills its hooks). Exit 0
 * stdout parses as `HookOutput` (mismatch = passthrough + warning); exit 2
 * denies with stderr as the model-visible reason; anything else is
 * passthrough + warning.
 */
async function runCommandHook(
  command: Extract<HookCommand, { type: "command" }>,
  payload: HookPayload,
  signal: AbortSignal,
): Promise<HookRunResult> {
  const settled = await spawnJsonCommand(command, payload, signal);
  if (settled.kind === "spawn_error") {
    return passthrough(`hook command failed to spawn: ${settled.message}`);
  }
  if (settled.kind === "aborted") {
    return passthrough("hook command aborted (timeout or run abort)");
  }
  if (settled.code === 2) {
    return {
      output: {
        decision: "deny",
        reason: settled.stderr.trim() || "hook command exited 2",
      },
    };
  }
  if (settled.code !== 0) {
    return passthrough(
      `hook command exited ${String(settled.code)}: ${settled.stderr.trim() || "(no stderr)"}`,
    );
  }
  const trimmed = settled.stdout.trim();
  if (!trimmed) return passthrough();
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return passthrough(`hook stdout was not JSON: ${trimmed.slice(0, 200)}`);
  }
  const checked = HookOutputSchema.safeParse(parsed);
  return checked.success
    ? { output: checked.data }
    : invalidHookOutput("hook stdout", checked.error);
}

function validateHookOutput(output: unknown, source: string): HookRunResult {
  const checked = HookOutputSchema.safeParse(output);
  return checked.success ? { output: checked.data } : invalidHookOutput(source, checked.error);
}

function invalidHookOutput(
  source: string,
  error: z.ZodError,
): HookRunResult {
  return passthrough(
    `${source} did not match HookOutput: ${error.issues
      .map((issue) => issue.message)
      .join("; ")}`,
  );
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
