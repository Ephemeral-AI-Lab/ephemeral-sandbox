import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

import { z } from "zod";

import {
  loadLlmClientRegistry,
  type LlmClientBinding,
} from "../../src/llm-client-registry.js";

/** The one live client id this suite runs against, as configured on disk. */
export const CODEX_CLIENT_ID = "codex_coding_plan";

export type ConfiguredCodexRuntime =
  | { available: true; llmClientsPath: string; binding: LlmClientBinding }
  | { available: false; reason: string };

function defaultConfigPath(env: NodeJS.ProcessEnv): string {
  if (env.EOS_LLM_CLIENTS_PATH !== undefined) return env.EOS_LLM_CLIENTS_PATH;

  const candidates = [
    resolve(process.cwd(), ".eos-agents", "llm_clients.json"),
    resolve(process.cwd(), "..", ".eos-agents", "llm_clients.json"),
  ];
  return candidates.find((path) => existsSync(path)) ?? candidates[1];
}

/**
 * Probe the codex coding-plan entry through the runtime's own loader
 * (`.eos-agents/llm_clients.json` + codex CLI auth file). Loading is local -
 * config parsing and JWT-claim validation, no network - so a missing or
 * stale credential becomes a suite skip carrying the loader's startup
 * error, never a test failure.
 */
export function loadConfiguredCodexRuntime(
  env: NodeJS.ProcessEnv = process.env,
): ConfiguredCodexRuntime {
  const llmClientsPath = defaultConfigPath(env);
  try {
    const binding = loadLlmClientRegistry(llmClientsPath).require(CODEX_CLIENT_ID);
    return { available: true, llmClientsPath, binding };
  } catch (error) {
    return {
      available: false,
      reason: error instanceof Error ? error.message : String(error),
    };
  }
}

const AuthFileSchema = z.object({
  tokens: z.object({ access_token: z.string() }),
});

const CodexEntrySchema = z.object({
  id: z.string().min(1),
  provider: z.literal("codex_coding_plan"),
  model_id: z.string().min(1),
  reasoning_effort: z.string().optional(),
  base_url: z.string().optional(),
  auth: z.object({
    kind: z.literal("codex_cli_auth_file"),
    path: z.string().min(1),
  }),
});

const ClientsConfigSchema = z.object({
  clients: z.array(CodexEntrySchema).min(1),
});

/**
 * Write a one-client copy of the configured codex entry into `dir` whose
 * JWT signature is tampered. The claims stay intact, so the runtime's local
 * startup validation passes and the bad credential surfaces live, inside
 * the run, as a provider authentication rejection.
 */
export function writeCorruptedCodexConfig(dir: string, llmClientsPath: string): string {
  const parsed = ClientsConfigSchema.parse(
    JSON.parse(readFileSync(llmClientsPath, "utf8")),
  );
  const entry = parsed.clients.find((client) => client.id === CODEX_CLIENT_ID);
  if (entry === undefined) {
    throw new Error(`${llmClientsPath} has no client id ${CODEX_CLIENT_ID}`);
  }
  const token = AuthFileSchema.parse(
    JSON.parse(readFileSync(entry.auth.path, "utf8")),
  ).tokens.access_token;
  const [header, payload] = token.split(".");
  const authPath = join(dir, "corrupted-auth.json");
  writeFileSync(
    authPath,
    JSON.stringify({
      tokens: { access_token: `${header}.${payload}.invalidsignature` },
    }),
  );
  const configPath = join(dir, "llm_clients.json");
  writeFileSync(
    configPath,
    JSON.stringify({
      clients: [{ ...entry, auth: { kind: "codex_cli_auth_file", path: authPath } }],
    }),
  );
  return configPath;
}
