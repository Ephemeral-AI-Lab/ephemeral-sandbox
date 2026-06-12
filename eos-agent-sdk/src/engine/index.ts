// The run mechanism: handle + events, conversation, provider turn, the
// loop spine, and the injected tool-executor port. The runtime package
// assembles these per run; nothing here reads config or touches disk.
export {
  runAgentLoop,
  type TerminationMode,
  type TurnFacts,
} from "./agent-loop.js";
export { Conversation, type ConversationRecord } from "./conversation.js";
export {
  RunHandle,
  type AgentEvent,
  type AgentEventBody,
  type AgentOutcome,
  type AgentRunError,
  type AgentRunHandle,
} from "./run-handle.js";
export type {
  ToolBatchContext,
  ToolExecutor,
  ToolUseBlock,
} from "./tool-executor.js";
