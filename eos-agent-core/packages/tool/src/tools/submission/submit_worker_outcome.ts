import type { ToolDefinition } from "../../contract.js";
import { descriptionPrompt } from "../description_prompts/index.js";
import { defineSubmissionTool } from "./shared.js";

export function submitWorkerOutcomeTool(): ToolDefinition {
  return defineSubmissionTool({
    name: "submit_worker_outcome",
    description: descriptionPrompt("submit_worker_outcome"),
    isAdvisoryRequired: true,
    advisorPrompt:
      "Review whether the worker's terminal submission accurately reports the completed work and remaining risk.",
  });
}
