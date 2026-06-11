import {
  mintAgentRunId,
  sandboxIdFrom,
  type AgentRunId,
  type BackgroundSessionSnapshot,
  type Message,
} from "@eos/contracts";
import {
  startAgentRun,
  type AgentRunHandle,
} from "@eos/engine";
import { BackgroundSessionSupervisor } from "@eos/background";
import {
  NotificationInbox,
  NotificationTriggerEngine,
  triggerRuleAppliesTo,
  type TriggerRuleEntry,
} from "@eos/notification";
import {
  BACKGROUND_TOOL_NAMES,
  AGENT_TOOL_NAMES,
  HookEngine,
  TERMINAL_TOOL_NAMES,
  agentTools,
  backgroundTools,
  buildToolExecutor,
  runTriggerCommand,
  snapshotRunState,
  terminalToolDefinitions,
  type AgentRunState,
  type ToolDefinition,
} from "@eos/tool";

import {
  loadAgentProfileRegistry,
  selectProfileDefinitions,
  type AgentProfileRegistry,
  type KnownToolNames,
} from "./agent-profile-registry.js";
import { loadHookConfig } from "./hook-config.js";
import {
  loadLlmClientRegistry,
  type LlmClientRegistry,
} from "./llm-client-registry.js";
import { loadNotificationRules } from "./notification-rules-config.js";
import { RunRegistry, type RunSummary } from "./run-registry.js";
import {
  RunLog,
  readTranscriptFile as readTranscriptBytes,
  type TranscriptRead,
} from "./transcript.js";

/** Process-level dependencies, bound once at `createAgentRuntime` (§2.3). */
export interface AgentRuntimeDependencies {
  /** Default: `.eos-agents/profiles`. */
  agentProfilesDir?: string;
  /** Default: `.eos-agents/llm_clients.json`. */
  llmClientsPath?: string;
  /** Optional in-memory test override; wins over `llmClientsPath`. */
  llmClients?: LlmClientRegistry;
  /** Already-built process-level tools (the backend-family seam, §10). */
  baseTools?: readonly ToolDefinition[];
  /** Default: `.eos-agents/hooks.json`. */
  hookConfigPath?: string;
  /** Default: `.eos-agents/notification_rules.json`; rules apply to every run. */
  notificationRulesPath?: string;
  /** Transcript root. */
  dataDir: string;
}

export type UserMessage = Message & { role: "user" };

/** In-process input (camelCase; never serialized). */
export interface StartRunParams {
  agentName: string;
  /** Ordered user messages; the system prompt stays profile data (§2.9). */
  initialMessages: readonly [UserMessage, ...UserMessage[]];
  /** Caller cancellation scope (§2.10). */
  signal?: AbortSignal;
}

export interface StartedRun {
  runId: AgentRunId;
  /**
   * Steer / interrupt / outcome. Its event stream is already consumed by
   * the runtime's transcript subscriber (decision 5); iterating it throws.
   */
  handle: AgentRunHandle;
  transcriptPath: string;
}

/** The §9 public API. */
export interface AgentRuntime {
  startRun(params: StartRunParams): StartedRun;
  listRuns(): readonly RunSummary[];
}

/**
 * Bind the process-level dependencies: load and statically validate agent
 * profiles, llm clients, and hook config. Config errors fail loudly here,
 * before any run can start.
 */
export function createAgentRuntime(dependencies: AgentRuntimeDependencies): AgentRuntime {
  const agentProfiles = loadAgentProfileRegistry(
    dependencies.agentProfilesDir ?? ".eos-agents/profiles",
    knownToolNames(dependencies.baseTools ?? []),
  );
  const llmClients =
    dependencies.llmClients ??
    loadLlmClientRegistry(dependencies.llmClientsPath ?? ".eos-agents/llm_clients.json");
  // Profiles resolve before engine start (§2.8); a dangling llm_client_id
  // reference is a startup error, never a mid-run one.
  for (const profile of agentProfiles.list()) llmClients.require(profile.llm_client_id);
  // Two operator files, two event families (04.9 §5): hooks.json feeds the
  // hook engine, notification_rules.json the per-run trigger engine.
  return createRuntime({
    dataDir: dependencies.dataDir,
    baseTools: dependencies.baseTools ?? [],
    agentProfiles,
    llmClients,
    hookEngine: new HookEngine(loadHookConfig(dependencies.hookConfigPath)),
    triggerRules: loadNotificationRules(dependencies.notificationRulesPath),
  });
}

/**
 * The static name universe for profile validation: each runtime-owned tool
 * family's exported constant plus every base definition's name, split by
 * terminality. A base name that collides with a family tool (or another
 * base tool) would silently shadow it at selection time, so collisions are
 * a startup error like every other config fault.
 */
function knownToolNames(baseTools: readonly ToolDefinition[]): KnownToolNames {
  const ordinary = new Set<string>([...AGENT_TOOL_NAMES, ...BACKGROUND_TOOL_NAMES]);
  const terminal = new Set<string>(TERMINAL_TOOL_NAMES);
  for (const definition of baseTools) {
    const name: string = definition.name;
    if (ordinary.has(name) || terminal.has(name)) {
      throw new Error(
        `baseTools name "${name}" collides with a runtime tool family or another base tool`,
      );
    }
    (definition.isTerminal ? terminal : ordinary).add(name);
  }
  return { ordinary, terminal };
}

interface RuntimeContext {
  dataDir: string;
  baseTools: readonly ToolDefinition[];
  agentProfiles: AgentProfileRegistry;
  llmClients: LlmClientRegistry;
  /** One engine per runtime: hook commands are stateless processes (§7). */
  hookEngine: HookEngine;
  /** Shared rule list; the trigger engine itself is per run (04.9 §7). */
  triggerRules: readonly TriggerRuleEntry[];
}

interface StartRunContext {
  /** Internal only, never public input. */
  parent?: AgentRunId;
}

function createRuntime(ctx: RuntimeContext): AgentRuntime {
  const registry = new RunRegistry();
  /** Per-run write barriers, keyed by transcript path: reads await the queue. */
  const transcriptBarriers = new Map<string, () => Promise<void>>();

  const readTranscriptFile = async (
    path: string,
    offset: number,
    maxBytes: number,
  ): Promise<TranscriptRead> => {
    await transcriptBarriers.get(path)?.();
    return readTranscriptBytes(path, offset, maxBytes);
  };

  // The §4 wiring order IS the spec (decision 2): per-run inbox/supervisor
  // pair, profile-resolved engine input, runtime-owned transcript consumer,
  // and one atomic registration after everything that can fail.
  function startRun(params: StartRunParams, context: StartRunContext = {}): StartedRun {
    const profile = ctx.agentProfiles.require(params.agentName);
    const llm = ctx.llmClients.require(profile.llm_client_id);
    if (profile.agent_kind === "main" && context.parent !== undefined) {
      throw new Error("main profiles can only be started externally");
    }

    const runId = mintAgentRunId();
    const inbox = new NotificationInbox();
    const supervisor = new BackgroundSessionSupervisor(inbox);
    const runLog = new RunLog(ctx.dataDir, {
      run_id: runId,
      agent_name: profile.name,
      agent_kind: profile.agent_kind,
      ...(context.parent !== undefined && { parent: context.parent }),
      llm_client_id: profile.llm_client_id,
      model_id: llm.model_id,
      reasoning_effort: llm.reasoning_effort,
      max_turns: profile.max_turns,
    });
    const transcriptPath = runLog.transcriptPath;

    const runState: AgentRunState = {
      run_id: runId,
      kind: profile.agent_kind,
      parent: context.parent,
      agent_name: profile.name,
      // Placeholder until the sandbox family phase binds real sandboxes.
      sandbox_id: sandboxIdFrom(runId),
      transcript_path: transcriptPath,
      workspace: { isIsolated: false },
    };

    const terminalDefinitions = terminalToolDefinitions();
    const advisorPrompts = advisorPromptLookup([...ctx.baseTools, ...terminalDefinitions]);
    const availableDefinitions = [
      ...ctx.baseTools,
      ...agentTools(
        {
          startRun: (next) => startRun(next, { parent: runId }),
          transcriptPathOf: (target) => registry.transcriptPathOf(target),
          readTranscriptFile,
          advisorPromptFor: (toolName) => advisorPrompts.get(toolName),
        },
        supervisor,
      ),
      ...backgroundTools(supervisor),
      ...terminalDefinitions,
    ];
    const definitions = selectProfileDefinitions(profile, availableDefinitions);
    validateAdvisoryToolAccess(profile, definitions);

    // Both operator-script payload families carry the same projection:
    // hook payloads through the executor, trigger payloads at fire time.
    const listBackgroundSessions = (): BackgroundSessionSnapshot[] =>
      supervisor.listBackgroundSessions().map(backgroundSessionSnapshot);

    // No inbox parameter (decision 11): hook context rides result metadata
    // and the engine publishes it; tools and the executor never see the inbox.
    const tools = buildToolExecutor({
      runState,
      definitions,
      hookEngine: ctx.hookEngine,
      hookPayloadFacts: () => ({ background_sessions: listBackgroundSessions() }),
    });

    const observer = new NotificationTriggerEngine({
      rules: ctx.triggerRules.filter((rule) =>
        triggerRuleAppliesTo(rule, {
          agent_name: profile.name,
          agent_kind: profile.agent_kind,
        }),
      ),
      runCommand: runTriggerCommand,
      inbox,
      listBackgroundSessions,
      runSnapshot: () => snapshotRunState(runState),
      terminalTool: profile.terminal_tool,
    });

    const handle = startAgentRun({
      llmClient: llm.client,
      tools,
      notifications: inbox,
      background: supervisor,
      observer,
      model: llm.model_id,
      reasoningEffort: llm.reasoning_effort,
      systemPrompt: profile.system_prompt,
      maxTurns: profile.max_turns,
      signal: params.signal,
      initialMessages: [...params.initialMessages],
    });

    for (const message of params.initialMessages) {
      runLog.appendUser("initial", message);
    }
    let finished = false;
    // Decision 5: the runtime is the stream's single consumer.
    const consumed = (async () => {
      for await (const event of handle.events) runLog.append(event);
    })();
    transcriptBarriers.set(transcriptPath, async () => {
      // After finish the buffered tail may still be draining off the stream.
      if (finished) await consumed;
      await runLog.flush();
    });
    registry.add(runState, handle);
    void handle.outcome.finally(() => {
      finished = true;
      // The one authoritative flush trigger (§6): the stream's whole tail
      // is on disk before the registry reports the run finished.
      void consumed
        .then(() => runLog.flush())
        // A write failure resurfaces on the next read; finishing is bookkeeping.
        .catch(() => undefined)
        .finally(() => {
          registry.finish(runId);
        });
    });

    return { runId, handle, transcriptPath };
  }

  return {
    startRun: (params) => startRun(params),
    listRuns: () => registry.list(),
  };
}

/** Explicit projection so a new `BackgroundSessionRow` field never leaks into payloads. */
function backgroundSessionSnapshot(row: {
  type: string;
  id: string;
  status: BackgroundSessionSnapshot["status"];
  started_at: string;
  summary?: string;
  description?: string;
}): BackgroundSessionSnapshot {
  const session: BackgroundSessionSnapshot = {
    type: row.type,
    id: row.id,
    status: row.status,
    started_at: row.started_at,
  };
  if (row.summary !== undefined) session.summary = row.summary;
  if (row.description !== undefined) session.description = row.description;
  return session;
}

function advisorPromptLookup(
  definitions: readonly ToolDefinition[],
): ReadonlyMap<string, string> {
  const prompts = new Map<string, string>();
  for (const definition of definitions) {
    if (!definition.isAdvisoryRequired) continue;
    if (definition.advisorPrompt === undefined) {
      throw new Error(`tool ${definition.name} requires advisory but has no advisorPrompt`);
    }
    prompts.set(definition.name, definition.advisorPrompt);
  }
  return prompts;
}

function validateAdvisoryToolAccess(
  profile: { name: string },
  definitions: readonly ToolDefinition[],
): void {
  const requiresAdvisory = definitions.some(
    (definition) => definition.isAdvisoryRequired,
  );
  if (!requiresAdvisory) return;
  if (definitions.some((definition) => definition.name === "ask_advisor")) return;
  throw new Error(
    `profile "${profile.name}" selects advisory-required tools but cannot call ask_advisor`,
  );
}
