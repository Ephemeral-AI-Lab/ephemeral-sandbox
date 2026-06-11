import { TriggerOutputSchema, type TriggerCommandRunner } from "@eos/notifications";

import { spawnJsonCommand } from "./spawn.js";

/**
 * The spawn-backed implementation of the `@eos/notifications` trigger
 * runner seam, reusing this package's shared command-spawn mechanics
 * (shell spawn, payload JSON + newline on stdin, per-command timeout) —
 * the same dependency direction as the tool layer implementing the
 * engine's `ToolExecutor` port. Never rejects: every failure — spawn
 * fault, timeout, nonzero exit, bad JSON, schema mismatch — settles as a
 * `warning` and the firing is dropped.
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
