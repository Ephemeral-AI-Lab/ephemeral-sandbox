import { join } from "node:path";

import { TriggerRuleEntrySchema, type TriggerRuleEntry } from "@eos/notification";
import { z } from "zod";

import { loadEntriesFile, withDefaultCwd } from "./config-file.js";
import { eosAgentsRoot } from "./config-root.js";

/**
 * Load the operator notification rules (Phase 04.9): a JSON array of
 * `TriggerRuleEntry` with a `rules` command list, sharing the operator
 * config-file mechanics. The rules apply to every agent run the runtime
 * starts, narrowed per run by the optional `agent_name`/`agent_kind`
 * matchers.
 */
export function loadNotificationRules(
  path = join(eosAgentsRoot(), "notification_rules.json"),
): TriggerRuleEntry[] {
  return loadEntriesFile(
    path,
    "notification rules config",
    z.array(TriggerRuleEntrySchema),
    (entry, cwd) => ({
      ...entry,
      rules: entry.rules.map((rule) => withDefaultCwd(rule, cwd)),
    }),
  );
}
