import type { ToolDefinition } from "eos-agent-sdk";

import { zodIssues } from "../config/config-file.js";
import type { AgentFactory } from "../agents/agent-factory.js";
import type {
  BuildWorkflowTools,
  ProfileWorkflowView,
  WorkflowConfig,
  WorkflowHubInit,
} from "./contract.js";

interface OpenedWorkflow {
  config: WorkflowConfig;
  build: BuildWorkflowTools;
}

/**
 * The host-owned workflow registry. `open` is a fail-fast join of config files
 * and providers; every workflow a profile view exposes is delegatable (no
 * readiness/error rows). The hub mints no tools at open — the factory drives
 * `view.tools(agents)` per profile.
 */
export class WorkflowHub {
  readonly #workflows: Map<string, OpenedWorkflow>;

  private constructor(workflows: Map<string, OpenedWorkflow>) {
    this.#workflows = workflows;
  }

  static open(init: WorkflowHubInit): WorkflowHub {
    const byType = new Map(init.providers.map((provider) => [provider.type, provider]));
    const workflows = new Map<string, OpenedWorkflow>();
    for (const config of init.workflows) {
      const provider = byType.get(config.type);
      if (!provider) {
        throw new Error(`workflow "${config.name}" has unknown type "${config.type}"`);
      }
      const args = provider.args.safeParse(config.args);
      if (!args.success) {
        throw new Error(`workflow "${config.name}" args are invalid: ${zodIssues(args.error)}`);
      }
      let build: BuildWorkflowTools;
      try {
        build = provider.create(config.name, args.data);
      } catch (error) {
        throw new Error(
          `workflow "${config.name}" failed to open: ${error instanceof Error ? error.message : String(error)}`,
          { cause: error },
        );
      }
      workflows.set(config.name, { config, build });
    }
    return new WorkflowHub(workflows);
  }

  /** Union of every configured workflow's declared tool names; the factory's collision set. */
  declaredToolNames(): ReadonlySet<string> {
    const names = new Set<string>();
    for (const workflow of this.#workflows.values()) {
      for (const tool of workflow.config.tools) names.add(tool);
    }
    return names;
  }

  forProfile(names: readonly [string, ...string[]]): ProfileWorkflowView {
    const opened = names.map((name) => {
      const workflow = this.#workflows.get(name);
      if (!workflow) throw new Error(`profile lists unknown workflow "${name}"`);
      return workflow;
    });
    return {
      names: () => names,
      tools: (agents) => assembleTools(opened, agents),
      promptFragment: () => renderPromptFragment(opened),
      docs: (name) => {
        const workflow = this.#workflows.get(name);
        if (!workflow || !names.includes(name)) {
          throw new Error(`workflow "${name}" is not visible to this profile`);
        }
        return workflow.config.docs;
      },
    };
  }
}

function assembleTools(
  opened: readonly OpenedWorkflow[],
  agents: AgentFactory,
): ToolDefinition[] {
  const tools = opened.flatMap((workflow) => workflow.build(agents));
  const produced = new Set(tools.map((tool) => tool.name));
  const declared = new Set(opened.flatMap((workflow) => workflow.config.tools));
  for (const workflow of opened) {
    for (const tool of workflow.config.tools) {
      if (!produced.has(tool)) {
        throw new Error(`workflow "${workflow.config.name}" declares tool "${tool}" its provider did not produce`);
      }
    }
  }
  for (const tool of produced) {
    if (!declared.has(tool)) {
      throw new Error(`a workflow produced undeclared tool "${tool}"`);
    }
  }
  return tools;
}

function renderPromptFragment(opened: readonly OpenedWorkflow[]): string {
  const lines = [
    "## Available workflows",
    "Drive these configured workflows through their tools. Call read_workflow_docs(<name>) only when this summary is insufficient.",
  ];
  for (const { config } of opened) {
    lines.push(`### ${config.name} — ${config.description}`, `Tools: ${config.tools.join(", ")}`);
  }
  return lines.join("\n");
}
