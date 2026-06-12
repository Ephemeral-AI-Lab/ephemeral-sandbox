import { readFileSync } from "node:fs";

import { eosAgentsPath } from "@eos/testkit";
import { z } from "zod";

import {
  createLlmClient,
  SecretString,
  type LlmClient,
  type ProviderConnection,
  type ReasoningEffort,
} from "../../src/index.js";
import { loadCodexAuthFromPath } from "./codex-auth.js";

const ReasoningEffortSchema = z.enum([
  "minimal",
  "low",
  "medium",
  "high",
  "max",
]);

const CodexClientConfigSchema = z.object({
  id: z.string().min(1),
  provider: z.literal("codex_coding_plan"),
  model_id: z.string().min(1),
  reasoning_effort: ReasoningEffortSchema.default("medium"),
  base_url: z.url().optional(),
  auth: z.object({
    kind: z.literal("codex_cli_auth_file"),
    path: z.string().min(1),
  }),
});

const LlmClientsConfigSchema = z.object({
  clients: z.array(CodexClientConfigSchema).min(1),
});

type CodexClientConfig = z.infer<typeof CodexClientConfigSchema>;

export type ConfiguredCodexClient =
  | {
      available: true;
      id: string;
      model: string;
      reasoningEffort: ReasoningEffort;
      createClient(): LlmClient;
      createCorruptedClient(): LlmClient;
    }
  | { available: false; reason: string };

function defaultConfigPath(env: NodeJS.ProcessEnv): string {
  return env.EOS_LLM_CLIENTS_PATH ?? eosAgentsPath("llm_clients.json");
}

function corruptToken(token: SecretString): SecretString {
  const [header, payload] = token.expose().split(".");
  return new SecretString(`${header}.${payload}.invalidsignature`);
}

function connection(
  config: CodexClientConfig,
  accessToken: SecretString,
): ProviderConnection {
  return {
    provider: config.provider,
    ...(config.base_url !== undefined ? { base_url: config.base_url } : {}),
    access_token: accessToken,
  };
}

export function loadConfiguredCodexClient(
  id = "codex_coding_plan",
  env: NodeJS.ProcessEnv = process.env,
): ConfiguredCodexClient {
  const path = defaultConfigPath(env);
  let raw: string;
  try {
    raw = readFileSync(path, "utf8");
  } catch {
    throw new Error(`${path} not found`);
  }

  let config: CodexClientConfig;
  try {
    const parsed = LlmClientsConfigSchema.parse(JSON.parse(raw));
    const match = parsed.clients.find((client) => client.id === id);
    if (match === undefined) {
      throw new Error(`${path} has no client id ${id}`);
    }
    config = match;
  } catch (error) {
    throw new Error(`${path} is not a valid llm_clients.json`, {
      cause: error,
    });
  }

  const auth = loadCodexAuthFromPath(config.auth.path);
  if (!auth.available) return auth;

  return {
    available: true,
    id: config.id,
    model: config.model_id,
    reasoningEffort: config.reasoning_effort,
    createClient: () => createLlmClient(connection(config, auth.accessToken)),
    createCorruptedClient: () =>
      createLlmClient(connection(config, corruptToken(auth.accessToken))),
  };
}
