import type { ToolDefinition } from "../../contract.js";
import { submitAdvisorOutcomeTool } from "./submit_advisor_outcome.js";
import { submitMainOutcomeTool } from "./submit_main_outcome.js";
import { submitPlannerOutcomeTool } from "./submit_planner_outcome.js";
import { submitSubagentOutcomeTool } from "./submit_subagent_outcome.js";
import { submitWorkerOutcomeTool } from "./submit_worker_outcome.js";

export { submitAdvisorOutcomeTool } from "./submit_advisor_outcome.js";
export { submitMainOutcomeTool } from "./submit_main_outcome.js";
export { submitPlannerOutcomeTool } from "./submit_planner_outcome.js";
export { submitSubagentOutcomeTool } from "./submit_subagent_outcome.js";
export { submitWorkerOutcomeTool } from "./submit_worker_outcome.js";

/** The terminal name universe: static, so profile validation needs no supervisor. */
export const TERMINAL_TOOL_NAMES = [
  "submit_main_outcome",
  "submit_planner_outcome",
  "submit_worker_outcome",
  "submit_advisor_outcome",
  "submit_subagent_outcome",
] as const;

/**
 * The full terminal inventory, one definition per name. Not keyed by
 * `AgentKind`: the profile selects exactly one entry by `terminal_tool`.
 */
export function terminalToolDefinitions(): ToolDefinition[] {
  return [
    submitMainOutcomeTool(),
    submitPlannerOutcomeTool(),
    submitWorkerOutcomeTool(),
    submitAdvisorOutcomeTool(),
    submitSubagentOutcomeTool(),
  ];
}
