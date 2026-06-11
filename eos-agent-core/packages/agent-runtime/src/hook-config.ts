import { readFileSync } from "node:fs";
import { basename, dirname, resolve } from "node:path";

import {
  HookConfigEntrySchema,
  TriggerRuleEntrySchema,
  type HookConfigEntry,
  type TriggerRuleEntry,
} from "@eos/tool";
import { z } from "zod";

/** One hooks.json entry: a tool-scoped hook or a loop-lifecycle trigger rule. */
export type HookConfigFileEntry = HookConfigEntry | TriggerRuleEntry;

const HookConfigSchema = z.array(
  z.discriminatedUnion("event", [HookConfigEntrySchema, ...TriggerRuleEntrySchema.options]),
);

const DEFAULT_HOOK_CONFIG_PATH = ".eos-agents/hooks.json";

/**
 * Load the operator hook config: a JSON array of `HookConfigEntry` and
 * `TriggerRuleEntry`, discriminated by `event`. A missing file means no
 * hooks; anything else malformed is a startup error naming the Zod issues -
 * config errors fail loudly at `createAgentRuntime`, never silently mid-run.
 */
export function loadHookConfig(path = DEFAULT_HOOK_CONFIG_PATH): HookConfigFileEntry[] {
  let raw: string;
  try {
    raw = readFileSync(path, "utf8");
  } catch (error) {
    // Node fs boundary: ENOENT is the documented "no hooks" case.
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw new Error(`hook config ${path} is not readable`, { cause: error });
  }
  let json: unknown;
  try {
    json = JSON.parse(raw);
  } catch (error) {
    throw new Error(`hook config ${path} is not valid JSON`, { cause: error });
  }
  const parsed = HookConfigSchema.safeParse(json);
  if (!parsed.success) {
    throw new Error(
      `hook config ${path} is invalid: ${parsed.error.issues
        .map((issue) => `${issue.path.map(String).join(".")}: ${issue.message}`)
        .join("; ")}`,
    );
  }
  const cwd = commandCwdFor(path);
  return parsed.data.map((entry) => ({
    ...entry,
    hooks: entry.hooks.map((hook) => (hook.cwd === undefined ? { ...hook, cwd } : hook)),
  }));
}

/** The runtime split: tool events feed the hook engine, trigger events the trigger engine. */
export function splitHookConfig(entries: readonly HookConfigFileEntry[]): {
  hooks: HookConfigEntry[];
  triggers: TriggerRuleEntry[];
} {
  const hooks: HookConfigEntry[] = [];
  const triggers: TriggerRuleEntry[] = [];
  for (const entry of entries) {
    if (entry.event === "TurnCompleted" || entry.event === "IdleParked") {
      triggers.push(entry);
    } else {
      hooks.push(entry);
    }
  }
  return { hooks, triggers };
}

function commandCwdFor(path: string): string {
  const configDir = dirname(resolve(path));
  return basename(configDir) === ".eos-agents" ? dirname(configDir) : configDir;
}
