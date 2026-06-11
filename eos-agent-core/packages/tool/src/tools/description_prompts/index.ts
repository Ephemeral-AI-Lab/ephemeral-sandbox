import { readFileSync } from "node:fs";

/**
 * Load a tool's description from the sibling `<toolName>_prompt.md` file.
 * Read synchronously at tool construction; a missing file throws immediately.
 */
export function descriptionPrompt(toolName: string): string {
  return readFileSync(new URL(`./${toolName}_prompt.md`, import.meta.url), "utf8").trim();
}
