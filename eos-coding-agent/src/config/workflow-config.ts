import { readFileSync, readdirSync } from "node:fs";
import { basename, join } from "node:path";

import { parse as parseYaml } from "yaml";
import { z } from "zod";

import type { WorkflowConfig } from "../workflows/contract.js";
import { zodIssues } from "./config-file.js";

/** A valid tool-name fragment: snake_case, so providers can name tools off it. */
const TOOL_NAME = /^[a-z][a-z0-9_]*$/;
const FRONTMATTER_SHAPE = /^---\r?\n([\s\S]*?)\r?\n---(\r?\n[\s\S]*|)$/;
const RESERVED_TOOL = "read_workflow_docs";

const FrontmatterSchema = z
  .object({
    name: z.string().min(1),
    type: z.string().min(1),
    description: z.string().min(1),
    tools: z.array(z.string().min(1)).min(1),
    args: z.unknown().optional(),
  })
  .strict();

/** Load every `<dir>/<name>.md` workflow. A missing directory means no workflows. */
export function loadWorkflowConfigs(dir: string): WorkflowConfig[] {
  let files: string[];
  try {
    files = readdirSync(dir).filter((name) => name.endsWith(".md"));
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw error;
  }
  const configs: WorkflowConfig[] = [];
  const seen = new Set<string>();
  for (const file of files.sort()) {
    const config = loadWorkflowConfig(join(dir, file));
    if (seen.has(config.name)) {
      throw new Error(`duplicate workflow name "${config.name}"`);
    }
    seen.add(config.name);
    configs.push(config);
  }
  return configs;
}

function loadWorkflowConfig(path: string): WorkflowConfig {
  let raw: string;
  try {
    raw = readFileSync(path, "utf8");
  } catch (error) {
    throw new Error(`workflow config ${path} is not readable`, { cause: error });
  }
  const match = FRONTMATTER_SHAPE.exec(raw);
  if (!match) {
    throw new Error(`workflow config ${path} must open with a --- YAML frontmatter block`);
  }
  let data: unknown;
  try {
    data = parseYaml(match[1]);
  } catch (error) {
    throw new Error(`workflow config ${path} has invalid YAML frontmatter`, { cause: error });
  }
  const parsed = FrontmatterSchema.safeParse(data);
  if (!parsed.success) {
    throw new Error(`workflow config ${path} is invalid: ${zodIssues(parsed.error)}`);
  }
  const base = basename(path).replace(/\.md$/, "");
  if (parsed.data.name !== base) {
    throw new Error(`workflow config ${path} name "${parsed.data.name}" must equal basename "${base}"`);
  }
  if (!TOOL_NAME.test(parsed.data.name)) {
    throw new Error(`workflow name "${parsed.data.name}" must be a snake_case tool-name fragment`);
  }
  for (const tool of parsed.data.tools) {
    if (!TOOL_NAME.test(tool)) {
      throw new Error(`workflow "${parsed.data.name}" declares invalid tool name "${tool}"`);
    }
    if (tool === RESERVED_TOOL) {
      throw new Error(`workflow "${parsed.data.name}" must not declare ${RESERVED_TOOL}`);
    }
  }
  if (new Set(parsed.data.tools).size !== parsed.data.tools.length) {
    throw new Error(`workflow "${parsed.data.name}" declares duplicate tool names`);
  }
  return {
    name: parsed.data.name,
    type: parsed.data.type,
    args: parsed.data.args,
    description: parsed.data.description,
    docs: match[2].trim(),
    tools: parsed.data.tools,
  };
}
