import type { ToolDefinition } from "../../contract.js";
import { descriptionPrompt } from "../description_prompts/index.js";
import { defineSubmissionTool } from "./shared.js";

export function submitAdvisorOutcomeTool(): ToolDefinition {
  return defineSubmissionTool({
    name: "submit_advisor_outcome",
    description: descriptionPrompt("submit_advisor_outcome"),
  });
}
