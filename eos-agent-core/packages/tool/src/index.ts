// The package surface is the authoring contract, the hook protocol, and
// the assembly entry. Pipeline and batch-executor internals (bindTool,
// toolBatchExecutor, the precedence kernel) stay package-private behind
// buildToolExecutor.
export {
  ToolNameSchema,
  type AgentRunSnapshot,
  type ToolCallContext,
  type ToolCallMeta,
  type ToolDefinition,
  type ToolName,
  type ToolOutcome,
} from "./contract.js";
export { defineTool, type ToolDefinitionInit } from "./define.js";
export {
  HookConfigEntrySchema,
  HookEventSchema,
  HookOutputSchema,
  type HookAdvisoryRequirement,
  type HookCommand,
  type HookBackgroundSession,
  type HookConfigEntry,
  type HookEvent,
  type HookOutput,
  type HookPayload,
} from "./hooks/protocol.js";
export { HookEngine } from "./hooks/runner.js";
export {
  TriggerOutputSchema,
  TriggerRuleEntrySchema,
  runTriggerCommand,
  type IdleTimeoutFacts,
  type TriggerCommand,
  type TriggerCommandRun,
  type TriggerCommandRunner,
  type TriggerOutput,
  type TriggerPayload,
  type TriggerRuleEntry,
  type TurnCompletedFacts,
} from "./hooks/triggers.js";
export { snapshotRunState, type AgentRunState } from "./run-state.js";
export { buildToolExecutor, type BuildToolExecutorInput } from "./toolset.js";
export {
  ADVISOR_AGENT_NAME,
  AGENT_TOOL_NAMES,
  agentTools,
  type AgentRunCalls,
  type AgentToolTranscriptRead,
  type AgentToolUserMessage,
  type StartAgentToolRunParams,
  type StartedAgentToolRun,
} from "./tools/agent/index.js";
export {
  BACKGROUND_TOOL_NAMES,
  backgroundTools,
} from "./tools/background/index.js";
export {
  TERMINAL_TOOL_NAMES,
  terminalToolDefinitions,
} from "./tools/submission/index.js";
