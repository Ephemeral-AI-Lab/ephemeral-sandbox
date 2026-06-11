import type { ToolDefinition } from "../../contract.js";
import { ADVISOR_PROMPT } from "../advisory_prompts/submit_planner_outcome_prompt.js";
import { DESCRIPTION } from "../description_prompts/submit_planner_outcome_prompt.js";
import { defineSubmissionTool } from "./shared.js";

export function submitPlannerOutcomeTool(): ToolDefinition {
  return defineSubmissionTool({
    name: "submit_planner_outcome",
    description: DESCRIPTION,
    isAdvisoryRequired: true,
    advisorPrompt: ADVISOR_PROMPT,
  });
}
