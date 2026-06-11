import { TriggerRuleEntrySchema, type TriggerRuleEntry } from "@eos/notifications";
import { z } from "zod";

import { loadEntriesFile, withDefaultCwd } from "./config-file.js";

const DEFAULT_NOTIFICATION_RULES_PATH = ".eos-agents/notification_rules.json";

/**
 * Load the operator notification rules (Phase 04.9): a JSON array of
 * `TriggerRuleEntry` with a `rules` command list, sharing the operator
 * config-file mechanics. The rules apply to every agent run the runtime
 * starts, narrowed per run by the optional `agent_name`/`agent_kind`
 * matchers.
 */
export function loadNotificationRules(
  path = DEFAULT_NOTIFICATION_RULES_PATH,
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
