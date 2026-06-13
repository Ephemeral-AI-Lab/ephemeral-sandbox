import { createAgentSdk, type Agent } from "eos-agent-sdk";
import { z } from "zod";

import {
  buildAgentFactory,
  createAgentOutcomeFnWithAdvisory,
  type AgentFactory,
} from "./agents/agent-factory.js";
import { loadEosConfig } from "./config/config-file.js";
import { eosAgentsRoot } from "./config/config-root.js";
import { WorkflowHub } from "./workflows/hub.js";
import {
  pursuitContextScriptComposer,
  resolvePursuitContextScripts,
} from "./workflows/pursuit-context-scripts.js";
import { pursuitWorkflowProvider } from "./workflows/pursuit-provider.js";

const MainOutcomeSchema = z.object({ summary: z.string().min(1) });
type MainOutcome = z.infer<typeof MainOutcomeSchema>;

const SUBMIT_MAIN_DESCRIPTION =
  "Finish the operator run by submitting its final outcome summary.";
const MAIN_ADVISOR_PROMPT =
  "Review whether the operator's terminal submission faithfully completes the user's goal.";

export interface CodingAgent {
  agents: AgentFactory;
  operator: Agent<MainOutcome>;
}

/**
 * The composition root: build each value once and wire only public SDK values.
 * `WorkflowHub.open` completes before the factory so profiles can bind workflow
 * tools; advisory prompts ride the terminal binding (no registry). Every
 * configured path resolves from the directory owning `.eos-agents`.
 */
export function bootstrap(configRoot: string = eosAgentsRoot()): CodingAgent {
  const cfg = loadEosConfig(configRoot);

  const sdk = createAgentSdk({
    llmClients: cfg.llmClients,
    hooks: cfg.hooks,
    recordsDir: cfg.recordsDir,
  });

  const compose = pursuitContextScriptComposer(resolvePursuitContextScripts(cfg.profiles));

  const hub = WorkflowHub.open({
    workflows: cfg.workflows,
    providers: [pursuitWorkflowProvider({ profiles: cfg.profiles, compose })],
  });

  const agents = buildAgentFactory(sdk, cfg.profiles, cfg.recordsDir, hub);

  const mainOutcomeFn = createAgentOutcomeFnWithAdvisory({
    name: "submit_main_outcome",
    description: SUBMIT_MAIN_DESCRIPTION,
    schema: MainOutcomeSchema,
    advisoryPrompt: MAIN_ADVISOR_PROMPT,
  });

  const operator = agents.create("operator", mainOutcomeFn);
  return { agents, operator };
}
