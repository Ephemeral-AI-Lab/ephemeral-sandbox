import { isAbsolute, resolve } from "node:path";

import { agentOutcomeToolName, type Agent, type AgentOutcomeFn } from "eos-agent-sdk";
import {
  openPursuitService,
  type ComposeLaunchContext,
  type PursuitAgents,
} from "@eos/pursuit";
import { z } from "zod";

import { withAdvisory, type AgentFactory } from "../agents/agent-factory.js";
import { configBaseDir } from "../config/config-root.js";
import { delegatePursuit } from "../tools/index.js";
import type { AgentProfileRegistry } from "../config/profiles.js";
import type { WorkflowProvider } from "./contract.js";

const PursuitWorkflowArgsSchema = z.object({
  planner: z.string().min(1),
  worker: z.string().min(1),
  store: z.string().min(1),
  context_root: z.string().min(1),
  default_max_attempts: z.number().int().positive().default(2),
});
export type PursuitWorkflowArgs = z.infer<typeof PursuitWorkflowArgsSchema>;

// Code-bound advisory prompts for the pursuit terminal tools (spec §15: one
// app-owned source; a gated terminal tool with no prompt is an error).
const PURSUIT_ADVISORY_PROMPTS = new Map<string, string>([
  [
    "submit_planner_outcome",
    "Review whether the planner's submission is coherent, complete, and safe to hand to workers.",
  ],
  [
    "submit_worker_outcome",
    "Review whether the worker's submission accurately reports the completed work and remaining risk.",
  ],
]);

/**
 * The pursuit provider: a host-side adapter over `openPursuitService`. It
 * holds the two values pursuit cannot own — the profile registry (registration
 * validation) and the script composer — and resolves config-relative paths from
 * the config base, never the cwd.
 */
export function pursuitWorkflowProvider(init: {
  profiles: AgentProfileRegistry;
  compose: ComposeLaunchContext;
}): WorkflowProvider<PursuitWorkflowArgs> {
  return {
    type: "pursuit",
    args: PursuitWorkflowArgsSchema,
    create(name, args) {
      assertPursuitProfiles(init.profiles, args);
      const service = openPursuitService({
        plannerAgentName: args.planner,
        workerAgentName: args.worker,
        storePath: resolveConfigPath(args.store),
        contextRoot: resolveConfigPath(args.context_root),
        defaultMaxAttempts: args.default_max_attempts,
        compose: init.compose,
      });
      return (agents) => [delegatePursuit(name, service, pursuitAgentsWithAdvisory(agents))];
    },
  };
}

/** Registration validation that replaced the deleted profile role/kind table (spec §9). */
function assertPursuitProfiles(profiles: AgentProfileRegistry, args: PursuitWorkflowArgs): void {
  const roles = [
    ["planner", args.planner, "submit_planner_outcome"],
    ["worker", args.worker, "submit_worker_outcome"],
  ] as const;
  for (const [role, name, terminal] of roles) {
    const profile = profiles.require(name);
    if (profile.terminal_tool !== terminal) {
      throw new Error(`pursuit ${role} profile "${name}" must declare terminal_tool "${terminal}"`);
    }
    if (profile.pursuit_context_script === undefined) {
      throw new Error(`pursuit ${role} profile "${name}" must declare pursuit_context_script`);
    }
  }
}

/**
 * Adapts the host `AgentFactory` to pursuit's narrow `PursuitAgents` slice,
 * attaching the matching advisory prompt before forwarding; pursuit only ever
 * sees plain SDK outcome functions.
 */
function pursuitAgentsWithAdvisory(agents: AgentFactory): PursuitAgents {
  return {
    create<T>(agentName: string, outcomeFn: AgentOutcomeFn<T>): Agent<T> {
      // The terminal-tool name is T-erased; the SDK accessor is AgentOutcomeFn<unknown>.
      const toolName = agentOutcomeToolName(outcomeFn as AgentOutcomeFn<unknown>);
      return agents.create(agentName, withAdvisory(outcomeFn, advisoryPromptForTerminalTool(toolName)));
    },
  };
}

function advisoryPromptForTerminalTool(toolName: string): string {
  const prompt = PURSUIT_ADVISORY_PROMPTS.get(toolName);
  if (prompt === undefined) {
    throw new Error(`no advisory prompt configured for gated terminal tool "${toolName}"`);
  }
  return prompt;
}

function resolveConfigPath(path: string): string {
  return isAbsolute(path) ? path : resolve(configBaseDir(), path);
}
