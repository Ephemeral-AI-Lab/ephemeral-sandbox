import type { AgentRunId, Message } from "@eos/contracts";
import type { BackgroundSessionSupervisor } from "@eos/background";
import type { AgentRunHandle } from "@eos/engine";

import type { ToolDefinition } from "../../contract.js";
import { askAdvisorTool } from "./ask-advisor.js";
import { readAgentRunTranscriptTool } from "./read-agent-run-transcript.js";
import { runSubagentTool } from "./run-subagent.js";

export { ADVISOR_AGENT_NAME } from "./ask-advisor.js";

/** The family's name universe: static, so profile validation needs no services. */
export const AGENT_TOOL_NAMES = [
  "run_subagent",
  "ask_advisor",
  "read_agent_run_transcript",
] as const;

export type AgentToolUserMessage = Message & { role: "user" };

export interface StartAgentToolRunParams {
  agentName: string;
  initialMessages: readonly [AgentToolUserMessage, ...AgentToolUserMessage[]];
  signal?: AbortSignal;
}

export interface StartedAgentToolRun {
  runId: AgentRunId;
  handle: AgentRunHandle;
}

export interface AgentToolTranscriptRead {
  data: string;
  next_offset: number;
  eof: boolean;
}

/** Narrow bound runtime calls - never a service object (§5). */
export interface AgentRunCalls {
  /** `startRun` recursion with the caller stamped as parent (§2.6). */
  startRun(params: StartAgentToolRunParams): StartedAgentToolRun;
  /** Registry lookup over runs this runtime started. */
  transcriptPathOf(runId: AgentRunId): string | undefined;
  /** Byte-offset read over a write-quiesced transcript file. */
  readTranscriptFile(
    path: string,
    offset: number,
    maxBytes: number,
  ): Promise<AgentToolTranscriptRead>;
  /** Advisory prompt for the target tool, if that tool is advisory-gated. */
  advisorPromptFor(toolName: string): string | undefined;
}

/** The agent family, one bound definition per `AGENT_TOOL_NAMES` entry. */
export function agentTools(
  calls: AgentRunCalls,
  supervisor: BackgroundSessionSupervisor,
): ToolDefinition[] {
  return [
    runSubagentTool(calls, supervisor),
    askAdvisorTool(calls),
    readAgentRunTranscriptTool(calls),
  ];
}
