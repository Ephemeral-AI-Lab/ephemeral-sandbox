import type { ToolDefinition } from "../../contract.js";
import { descriptionPrompt } from "../description_prompts/index.js";
import { defineSubmissionTool } from "./shared.js";

export function submitSubagentOutcomeTool(): ToolDefinition {
  return defineSubmissionTool({
    name: "submit_subagent_outcome",
    description: descriptionPrompt("submit_subagent_outcome"),
  });
}
