import { join } from "node:path";

import { HookConfigEntrySchema, type HookConfigEntry } from "@eos/tool";
import { z } from "zod";

import { loadEntriesFile, withDefaultCwd } from "./config-file.js";
import { eosAgentsRoot } from "./config-root.js";

/**
 * Load the operator hook config: a JSON array of `HookConfigEntry`
 * (tool events only; notification trigger rules live in their own file).
 */
export function loadHookConfig(path = join(eosAgentsRoot(), "hooks.json")): HookConfigEntry[] {
  return loadEntriesFile(path, "hook config", z.array(HookConfigEntrySchema), (entry, cwd) => ({
    ...entry,
    hooks: entry.hooks.map((hook) => withDefaultCwd(hook, cwd)),
  }));
}
