import { readFileSync } from "node:fs";

import type { LlmClientConfig, ProviderConnection } from "eos-agent-sdk";
import { z } from "zod";

import { zodIssues } from "./config-file.js";

const ReasoningEffortSchema = z.enum(["minimal", "low", "medium", "high", "max"]);

const AuthSchema = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("codex_cli_auth_file"), path: z.string().min(1) }),
  z.object({ kind: z.literal("inline"), credential: z.string().min(1) }),
]);

const LlmClientEntrySchema = z.object({
  id: z.string().min(1),
  provider: z.enum([
    "anthropic_api",
    "openai_api",
    "claude_coding_plan",
    "codex_coding_plan",
  ]),
  model_id: z.string().min(1),
  reasoning_effort: ReasoningEffortSchema.optional(),
  base_url: z.string().min(1).optional(),
  auth: AuthSchema,
});

const LlmClientsConfigSchema = z.object({ clients: z.array(LlmClientEntrySchema) });

type LlmClientEntry = z.infer<typeof LlmClientEntrySchema>;

/**
 * Load `.eos-agents/llm_clients.json` into the SDK's `LlmClientConfig`. The host
 * owns credential-file reading and validation (spec §4); the SDK builds the
 * actual provider clients from the returned connection objects.
 */
export function loadLlmClients(path: string): LlmClientConfig {
  let json: unknown;
  try {
    json = JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    throw new Error(`llm clients config ${path} is not readable JSON`, { cause: error });
  }
  const parsed = LlmClientsConfigSchema.safeParse(json);
  if (!parsed.success) {
    throw new Error(`llm clients config ${path} is invalid: ${zodIssues(parsed.error)}`);
  }
  const config: LlmClientConfig = {};
  for (const entry of parsed.data.clients) {
    if (entry.id in config) {
      throw new Error(`llm clients config ${path} has duplicate client id "${entry.id}"`);
    }
    config[entry.id] = {
      model: entry.model_id,
      ...(entry.reasoning_effort !== undefined && { reasoningEffort: entry.reasoning_effort }),
      connection: connectionFor(entry),
    };
  }
  return config;
}

function connectionFor(entry: LlmClientEntry): ProviderConnection {
  const credential = readCredential(entry.auth);
  const base = entry.base_url !== undefined ? { base_url: entry.base_url } : {};
  switch (entry.provider) {
    case "anthropic_api":
      return { provider: entry.provider, ...base, api_key: credential };
    case "openai_api":
      return { provider: entry.provider, ...base, api_key: credential };
    case "claude_coding_plan":
      return { provider: entry.provider, ...base, access_token: credential };
    case "codex_coding_plan":
      return { provider: entry.provider, ...base, access_token: credential };
  }
}

const CodexAuthFileSchema = z.object({ tokens: z.object({ access_token: z.string().min(1) }) });

function readCredential(auth: LlmClientEntry["auth"]): string {
  if (auth.kind === "inline") return auth.credential;
  let json: unknown;
  try {
    json = JSON.parse(readFileSync(auth.path, "utf8"));
  } catch (error) {
    throw new Error(`codex auth file ${auth.path} is not readable (run "codex login")`, {
      cause: error,
    });
  }
  const parsed = CodexAuthFileSchema.safeParse(json);
  if (!parsed.success) {
    throw new Error(`codex auth file ${auth.path} has no tokens.access_token`);
  }
  return parsed.data.tokens.access_token;
}
