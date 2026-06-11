import { HookConfigEntrySchema, type HookConfigEntry } from "@eos/tool";
import { z } from "zod";

import { loadEntriesFile, withDefaultCwd } from "./config-file.js";

const DEFAULT_HOOK_CONFIG_PATH = ".eos-agents/hooks.json";

/**
 * Load the operator hook config: a JSON array of `HookConfigEntry`
 * (tool events only; notification trigger rules live in their own file).
 */
export function loadHookConfig(path = DEFAULT_HOOK_CONFIG_PATH): HookConfigEntry[] {
  return loadEntriesFile(path, "hook config", z.array(HookConfigEntrySchema), (entry, cwd) => ({
    ...entry,
    hooks: entry.hooks.map((hook) => withDefaultCwd(hook, cwd)),
  }));
}
