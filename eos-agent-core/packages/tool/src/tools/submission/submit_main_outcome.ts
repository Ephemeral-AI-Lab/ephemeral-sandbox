import type { ToolDefinition } from "../../contract.js";
import { descriptionPrompt } from "../description_prompts/index.js";
import { defineSubmissionTool } from "./shared.js";

export function submitMainOutcomeTool(): ToolDefinition {
  return defineSubmissionTool({
    name: "submit_main_outcome",
    description: descriptionPrompt("submit_main_outcome"),
    isAdvisoryRequired: true,
    advisorPrompt:
      "Review whether the main agent's terminal submission faithfully completes the user's goal.",
  });
}
