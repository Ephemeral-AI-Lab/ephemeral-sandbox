import type { ToolDefinition } from "../../contract.js";
import { ADVISOR_PROMPT } from "../advisory_prompts/submit_main_outcome_prompt.js";
import { DESCRIPTION } from "../description_prompts/submit_main_outcome_prompt.js";
import { defineSubmissionTool } from "./shared.js";

export function submitMainOutcomeTool(): ToolDefinition {
  return defineSubmissionTool({
    name: "submit_main_outcome",
    description: DESCRIPTION,
    isAdvisoryRequired: true,
    advisorPrompt: ADVISOR_PROMPT,
  });
}
