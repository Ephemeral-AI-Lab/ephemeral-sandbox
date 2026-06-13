import {
  agentOutcomeToolName,
  createAgentOutcomeFn,
  type Agent,
  type AgentOutcomeFn,
  type AgentSdk,
  type AgentSpec,
  type HookEntry,
  type SubmitCtx,
  type ToolDefinition,
} from "eos-agent-sdk";
import type { z } from "zod";
import {
  cancelBackgroundTask,
  listBackgroundTasks,
  readAgentRun,
  readWorkflowDocs,
  sandboxTools,
} from "../tools/index.js";

import { AdvisorPassRegistry } from "./advisor-pass-registry.js";
import {
  ADVISOR_AGENT_NAME,
  SUBMIT_ADVISOR_OUTCOME,
  askAdvisor,
  requireAdvisoryPass,
} from "../tools/agent/ask-advisor.js";
import { runSubagent } from "../tools/agent/run-subagent.js";
import type { AgentProfile } from "../config/profile-loader.js";
import type { AgentProfileRegistry } from "../config/profiles.js";
import type { WorkflowHub } from "../workflows/hub.js";

/**
 * Host-owned terminal binding: the SDK outcome contract plus the advisory
 * prompt that guards its submissions. The host stores the prompt only; terminal
 * semantics stay inside the opaque SDK value.
 */
export interface AgentOutcomeFnWithAdvisory<T> {
  kind: "with_advisory";
  outcomeFn: AgentOutcomeFn<T>;
  advisoryPrompt: string;
}

/** The single constructor for the advisory binding; stamps `kind` in one place. */
export function withAdvisory<T>(
  outcomeFn: AgentOutcomeFn<T>,
  advisoryPrompt: string,
): AgentOutcomeFnWithAdvisory<T> {
  return { kind: "with_advisory", outcomeFn, advisoryPrompt };
}

export function createAgentOutcomeFnWithAdvisory<T>(spec: {
  name: string;
  description?: string;
  schema: z.ZodType<T>;
  onSubmit?: (payload: T, ctx: SubmitCtx) => Promise<{ accept: T } | { reject: string }>;
  advisoryPrompt: string;
}): AgentOutcomeFnWithAdvisory<T> {
  const { advisoryPrompt, ...outcome } = spec;
  return withAdvisory(createAgentOutcomeFn(outcome), advisoryPrompt);
}

export interface AgentFactory {
  create<T = string>(
    name: string,
    agentOutcomeFn?: AgentOutcomeFn<T> | AgentOutcomeFnWithAdvisory<T>,
  ): Agent<T>;
}

/**
 * The only place a host profile becomes an SDK `AgentSpec`. Threaded by
 * dependency injection (the hub↔factory cycle forbids a global): tools that
 * launch agents close over the returned factory. One `AdvisorPassRegistry` is
 * shared between every injected `ask_advisor` and its gate.
 */
export function buildAgentFactory(
  sdk: AgentSdk,
  profiles: AgentProfileRegistry,
  recordsDir: string,
  workflowHub: WorkflowHub,
): AgentFactory {
  validateAdvisorProfile(profiles);
  const passes = new AdvisorPassRegistry();

  const factory: AgentFactory = {
    create<T = string>(
      name: string,
      agentOutcomeFn?: AgentOutcomeFn<T> | AgentOutcomeFnWithAdvisory<T>,
    ): Agent<T> {
      const profile = profiles.require(name);
      const tools = selectOrdinaryTools(profile, factory, recordsDir, workflowHub);
      let systemPrompt = profile.system_prompt;

      if (profile.workflows.length > 0) {
        const view = workflowHub.forProfile(profile.workflows as readonly [string, ...string[]]);
        tools.push(...view.tools(factory), readWorkflowDocs(view));
        systemPrompt = `${view.promptFragment()}\n\n${systemPrompt}`;
      }

      const hooks: HookEntry[] = [];
      let outcomeFn: AgentOutcomeFn<T> | undefined;
      if (agentOutcomeFn !== undefined) {
        if (isAdvisoryBinding(agentOutcomeFn)) {
          outcomeFn = agentOutcomeFn.outcomeFn;
          if (profile.terminal_tool === undefined) {
            throw new Error(`profile "${name}" has an advisory binding but no terminal_tool`);
          }
          tools.push(askAdvisor(factory, agentOutcomeFn.advisoryPrompt, passes));
          hooks.push(requireAdvisoryPass({ toolName: profile.terminal_tool, passes }));
        } else {
          outcomeFn = agentOutcomeFn;
        }
      }
      // Terminal-name read-back is T-erased (the name never depends on T);
      // the SDK's accessor is typed AgentOutcomeFn<unknown> for exactly this.
      assertTerminalBinding(profile, outcomeFn as AgentOutcomeFn<unknown> | undefined);

      const spec: AgentSpec<T> = {
        name: profile.name,
        llm: profile.llm_client_id,
        systemPrompt,
        tools,
        ...(outcomeFn !== undefined && { agentOutcomeFn: outcomeFn }),
        ...(profile.max_turns !== undefined && { maxTurns: profile.max_turns }),
        ...(hooks.length > 0 && { hooks }),
      };
      return sdk.createAgent<T>(spec);
    },
  };
  return factory;
}

function isAdvisoryBinding<T>(
  binding: AgentOutcomeFn<T> | AgentOutcomeFnWithAdvisory<T>,
): binding is AgentOutcomeFnWithAdvisory<T> {
  // The opaque SDK outcome fn carries no `kind`; the host advisory binding does.
  return "kind" in binding;
}

function assertTerminalBinding(
  profile: AgentProfile,
  outcomeFn: AgentOutcomeFn<unknown> | undefined,
): void {
  if (profile.terminal_tool !== undefined) {
    if (outcomeFn === undefined) {
      throw new Error(`profile "${profile.name}" declares terminal_tool but no terminal binding was provided`);
    }
    const bound = agentOutcomeToolName(outcomeFn);
    if (bound !== profile.terminal_tool) {
      throw new Error(
        `profile "${profile.name}" terminal_tool "${profile.terminal_tool}" does not match binding tool "${bound}"`,
      );
    }
  } else if (outcomeFn !== undefined) {
    throw new Error(`profile "${profile.name}" has no terminal_tool but a terminal binding was provided`);
  }
}

function selectOrdinaryTools(
  profile: AgentProfile,
  factory: AgentFactory,
  recordsDir: string,
  hub: WorkflowHub,
): ToolDefinition[] {
  const available = new Map<string, ToolDefinition>();
  for (const tool of [
    listBackgroundTasks,
    cancelBackgroundTask,
    readAgentRun(recordsDir),
    ...sandboxTools(),
  ]) {
    available.set(tool.name, tool);
  }
  if (profile.subagents.length > 0) {
    available.set(
      "run_subagent",
      runSubagent(factory, profile.subagents as readonly [string, ...string[]]),
    );
  }

  const injected = hub.declaredToolNames();
  const selected: ToolDefinition[] = [];
  const seen = new Set<string>();
  for (const name of profile.allowed_tools) {
    if (name === "ask_advisor" || name === "read_workflow_docs" || injected.has(name)) {
      throw new Error(`profile "${profile.name}" lists factory-injected tool "${name}" in allowed_tools`);
    }
    if (seen.has(name)) {
      throw new Error(`profile "${profile.name}" lists duplicate tool "${name}"`);
    }
    const tool = available.get(name);
    if (!tool) throw new Error(`profile "${profile.name}" lists unknown tool "${name}"`);
    seen.add(name);
    selected.push(tool);
  }
  return selected;
}

/**
 * Advisory is host meta-policy on the terminal binding: every advisory binding
 * launches the `advisor` profile bound to `submit_advisor_outcome`, so that
 * profile must exist and declare exactly that terminal tool (spec §15).
 */
function validateAdvisorProfile(profiles: AgentProfileRegistry): void {
  const advisor = profiles.list().find((profile) => profile.name === ADVISOR_AGENT_NAME);
  if (advisor === undefined) {
    throw new Error(`advisory requires an "${ADVISOR_AGENT_NAME}" profile, which is not configured`);
  }
  if (advisor.terminal_tool !== SUBMIT_ADVISOR_OUTCOME) {
    throw new Error(
      `"${ADVISOR_AGENT_NAME}" profile terminal_tool must be "${SUBMIT_ADVISOR_OUTCOME}"`,
    );
  }
}
