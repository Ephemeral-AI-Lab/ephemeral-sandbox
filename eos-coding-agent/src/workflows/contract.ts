import type { ToolDefinition } from "eos-agent-sdk";
import type { z } from "zod";

import type { AgentFactory } from "../agents/agent-factory.js";

export type WorkflowArgs = Record<string, unknown>;

/**
 * One parsed `.eos-agents/workflow/<name>.md`. The hub holds this for the
 * workflow's lifetime; the provider only sees `name`/`args`, everything else
 * (description, docs, declared tool names) lives here.
 */
export interface WorkflowConfig {
  /** Basename; also the tool-name stem. */
  name: string;
  /** Frontmatter; selects the provider. */
  type: string;
  /** Frontmatter; parsed by the provider's args schema at `open`. */
  args: unknown;
  /** Frontmatter; the prompt-fragment line. */
  description: string;
  /** Markdown body; the `read_workflow_docs` manual. */
  docs: string;
  /** Frontmatter; declared model-tool names this workflow exposes. */
  tools: string[];
}

/**
 * Build this workflow's model tools, closed over the per-profile agent factory.
 * `agents` is the only value that post-dates `WorkflowHub.open` (the
 * hub↔factory cycle); a non-agent workflow returns a nullary builder.
 */
export type BuildWorkflowTools = (agents: AgentFactory) => ToolDefinition[];

export interface WorkflowProvider<A extends WorkflowArgs = WorkflowArgs> {
  type: string;
  args: z.ZodType<A>;
  /** Open this configured workflow's service (sync, fail-fast) and return its tool builder. */
  create(name: string, args: A): BuildWorkflowTools;
}

export interface WorkflowHubInit {
  workflows: WorkflowConfig[];
  providers: readonly WorkflowProvider[];
}

/** The four-method profile-scoped view the factory consumes (spec §8.1). */
export interface ProfileWorkflowView {
  /** Profile-visible workflow names; scopes `read_workflow_docs` input. */
  names(): readonly [string, ...string[]];
  /** This profile's workflow tools — each workflow's `ToolDefinition[]`, concatenated. */
  tools(agents: AgentFactory): ToolDefinition[];
  /** The system-prompt block listing this profile's workflows and their tool names. */
  promptFragment(): string;
  /** The `read_workflow_docs` manual for one profile-visible workflow. */
  docs(name: string): string;
}
