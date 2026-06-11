import { readFileSync } from "node:fs";

import { AgentKindSchema, zodIssueSummary, type AgentKind } from "@eos/contracts";
import { ToolNameSchema, type ToolName } from "@eos/tool";
import { parse as parseYaml } from "yaml";
import { z } from "zod";

/**
 * One agent definition: Markdown frontmatter plus the body as the system
 * prompt. snake_case throughout: every field but `source_path` is
 * config-derived, and the record is what transports will serialize.
 */
export interface AgentProfile {
  name: string;
  description: string;
  llm_client_id: string;
  max_turns: number;
  agent_kind: AgentKind;
  /** Ordinary non-terminal tools to expose; never inferred from prose. */
  allowed_tools: readonly ToolName[];
  /** Exactly one terminal tool, separate from the allowlist. */
  terminal_tool: ToolName;
  /** Markdown body after the frontmatter. */
  system_prompt: string;
  /** Diagnostics only, never API input. */
  source_path: string;
}

const FrontmatterSchema = z.object({
  name: z.string().min(1),
  description: z.string().min(1),
  llm_client_id: z.string().min(1),
  max_turns: z.number().int().positive(),
  agent_kind: AgentKindSchema,
  allowed_tools: z.array(ToolNameSchema),
  terminal_tool: ToolNameSchema,
});

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
    throw new Error(
      `agent profile ${path} must open with a --- YAML frontmatter block`,
    );
  }
  let data: unknown;
  try {
    data = parseYaml(match[1]);
  } catch (error) {
    throw new Error(`agent profile ${path} has invalid YAML frontmatter`, {
      cause: error,
    });
  }
  const parsed = FrontmatterSchema.safeParse(data);
  if (!parsed.success) {
    throw new Error(`agent profile ${path} is invalid: ${zodIssueSummary(parsed.error)}`);
  }
  return { ...parsed.data, system_prompt: match[2].trim(), source_path: path };
}
