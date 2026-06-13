import { readFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";

import type { HookEntry, LlmClientConfig } from "eos-agent-sdk";
import type { z } from "zod";

import type { WorkflowConfig } from "../workflows/contract.js";
import { loadHookConfig } from "./hook-config.js";
import { loadLlmClients } from "./llm-client-config.js";
import { loadAgentProfiles, type AgentProfileRegistry } from "./profiles.js";
import { loadWorkflowConfigs } from "./workflow-config.js";

/**
 * The single composition-root config value: every host config parsed into the
 * shapes the SDK and host wiring consume. `configRoot` is the absolute
 * `.eos-agents` directory (from `eosAgentsRoot()`); per-path resolution of
 * scripts/store/context stays in their consumers, keyed off the config base.
 */
export interface EosConfig {
  llmClients: LlmClientConfig;
  hooks: HookEntry[];
  recordsDir: string;
  profiles: AgentProfileRegistry;
  workflows: WorkflowConfig[];
}

export function loadEosConfig(configRoot: string): EosConfig {
  const root = resolve(configRoot);
  return {
    llmClients: loadLlmClients(join(root, "llm_clients.json")),
    hooks: loadHookConfig(join(root, "hooks.json")),
    recordsDir: join(root, "runs"),
    profiles: loadAgentProfiles(join(root, "profile")),
    workflows: loadWorkflowConfigs(join(root, "workflow")),
  };
}

/**
 * The mechanics shared by the operator config loaders (`hooks.json`,
 * `notification_rules.json`): a missing file means no entries; anything
 * else malformed is a startup error naming the Zod issues - config errors
 * fail loudly at `createAgentRuntime`, never silently mid-run. `fillCwd`
 * gives commands without a `cwd` the config's owning directory (the repo
 * root for a `.eos-agents` config).
 */
export function loadEntriesFile<E>(
  path: string,
  label: string,
  schema: z.ZodType<E[]>,
  fillCwd: (entry: E, cwd: string) => E,
): E[] {
  let raw: string;
  try {
    raw = readFileSync(path, "utf8");
  } catch (error) {
    // Node fs boundary: ENOENT is the documented "no entries" case.
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw new Error(`${label} ${path} is not readable`, { cause: error });
  }
  let json: unknown;
  try {
    json = JSON.parse(raw);
  } catch (error) {
    throw new Error(`${label} ${path} is not valid JSON`, { cause: error });
  }
  const parsed = schema.safeParse(json);
  if (!parsed.success) {
    throw new Error(
      `${label} ${path} is invalid: ${parsed.error.issues
        .map((issue) => `${issue.path.map(String).join(".")}: ${issue.message}`)
        .join("; ")}`,
    );
  }
  const cwd = commandCwdFor(path);
  return parsed.data.map((entry) => fillCwd(entry, cwd));
}

export function withDefaultCwd<C extends { cwd?: string }>(command: C, cwd: string): C {
  return command.cwd === undefined ? { ...command, cwd } : command;
}

/** One-line `path: message; …` summary of a Zod error, for startup diagnostics. */
export function zodIssues(error: z.ZodError): string {
  return error.issues
    .map((issue) => `${issue.path.map(String).join(".")}: ${issue.message}`)
    .join("; ");
}

function commandCwdFor(path: string): string {
  const configDir = dirname(resolve(path));
  return basename(configDir) === ".eos-agents" ? dirname(configDir) : configDir;
}
