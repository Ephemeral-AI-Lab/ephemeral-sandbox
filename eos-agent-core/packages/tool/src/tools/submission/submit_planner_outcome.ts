import type { ToolDefinition } from "../../contract.js";
import { descriptionPrompt } from "../description_prompts/index.js";
import { defineSubmissionTool } from "./shared.js";

export function submitPlannerOutcomeTool(): ToolDefinition {
  return defineSubmissionTool({
    name: "submit_planner_outcome",
    description: descriptionPrompt("submit_planner_outcome"),
    isAdvisoryRequired: true,
    advisorPrompt:
      "Review whether the planner's terminal submission is coherent, complete, and safe to hand off.",
  });
}
