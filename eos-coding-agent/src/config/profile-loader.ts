import { readFileSync } from "node:fs";

import { parse as parseYaml } from "yaml";
import { z } from "zod";

import { zodIssues } from "./config-file.js";

/**
 * One host profile: Markdown frontmatter plus the body as the system prompt.
 * Profiles carry no role/kind discriminator (spec §1.1); planner/worker
 * membership is validated by the pursuit provider at registration, not here.
 */
export interface AgentProfile {
  name: string;
  llm_client_id: string;
  description?: string;
  /** Feeds SDK `AgentSpec.maxTurns`; the SDK default applies when absent. */
  max_turns?: number;
  /** Ordinary model-visible tools; `ask_advisor` and workflow tools are never listed. */
  allowed_tools: readonly string[];
  /** Present → terminal-tool mode; absent → SDK text termination. */
  terminal_tool?: string;
  /** Configured workflow names this profile may use; injects their tools + a prompt fragment. */
  workflows: readonly string[];
  /** Profile names this profile may launch through `run_subagent`. */
  subagents: readonly string[];
  /** Config-base-relative initial-message script; required for pursuit planner/worker. */
  pursuit_context_script?: string;
  /** Markdown body after the frontmatter. */
  system_prompt: string;
  /** Diagnostics only. */
  source_path: string;
}

// `.strict()` rejects every unrecognized key, so the dropped role/kind and
// workflow-script fields fail to parse without this source naming them.
const FrontmatterSchema = z
  .object({
    name: z.string().min(1),
    llm_client_id: z.string().min(1),
    description: z.string().min(1).optional(),
    max_turns: z.number().int().positive().optional(),
    allowed_tools: z.array(z.string().min(1)),
    terminal_tool: z.string().min(1).optional(),
    workflows: z.array(z.string().min(1)).default([]),
    subagents: z.array(z.string().min(1)).default([]),
    pursuit_context_script: z.string().min(1).optional(),
  })
  .strict();

/** `---\n<yaml>\n---\n<body>`; the first `---` line must open the file. */
const FRONTMATTER_SHAPE = /^---\r?\n([\s\S]*?)\r?\n---(\r?\n[\s\S]*|)$/;

/** Parse one profile file; every failure names the offending path. */
export function loadAgentProfile(path: string): AgentProfile {
  let raw: string;
  try {
    raw = readFileSync(path, "utf8");
  } catch (error) {
    throw new Error(`agent profile ${path} is not readable`, { cause: error });
  }
  const match = FRONTMATTER_SHAPE.exec(raw);
  if (!match) {
    throw new Error(`agent profile ${path} must open with a --- YAML frontmatter block`);
  }
  let data: unknown;
  try {
    data = parseYaml(match[1]);
  } catch (error) {
    throw new Error(`agent profile ${path} has invalid YAML frontmatter`, { cause: error });
  }
  const parsed = FrontmatterSchema.safeParse(data);
  if (!parsed.success) {
    throw new Error(`agent profile ${path} is invalid: ${zodIssues(parsed.error)}`);
  }
  if (parsed.data.terminal_tool !== undefined &&
      parsed.data.allowed_tools.includes(parsed.data.terminal_tool)) {
    throw new Error(
      `agent profile ${path} lists its terminal_tool "${parsed.data.terminal_tool}" in allowed_tools`,
    );
  }
  return { ...parsed.data, system_prompt: match[2].trim(), source_path: path };
}
