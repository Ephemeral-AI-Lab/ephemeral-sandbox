// The module surface: the authoring contract (defineTool), the terminal
// contract factory (createAgentOutcomeFn), the callback hook engine, and
// the executor assembly. Pipeline and batch-executor internals stay
// module-private behind buildToolExecutor. The module ships ZERO tool
// implementations — every tool is host-authored.
export type {
  ToolCallContext,
  ToolDefinition,
  ToolResult,
} from "./contract.js";
export { defineTool, type ToolDefinitionInit } from "./define.js";
export {
  HookEngine,
  type HookDecision,
  type HookEntry,
  type HookMatcher,
  type ToolCallFacts,
} from "./hooks.js";
export {
  createAgentOutcomeFn,
  unwrapAgentOutcomeFn,
  type AgentOutcomeFn,
  type SubmitCtx,
} from "./outcome.js";
export type { TerminalGate } from "./terminal.js";
export { buildToolExecutor } from "./toolset.js";
