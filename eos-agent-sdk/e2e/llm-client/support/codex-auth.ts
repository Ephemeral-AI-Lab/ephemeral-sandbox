import { readFileSync } from "node:fs";

import { z } from "zod";

import { SecretString } from "../../../src/llm-client/secret.js";

/** The Codex CLI cache shape; only `tokens.access_token` is consumed. */
const AuthFileSchema = z.object({
  tokens: z.object({ access_token: z.string() }),
});

const JwtPayloadSchema = z.object({
  exp: z.number().optional(),
  "https://api.openai.com/auth": z
    .object({ chatgpt_account_id: z.string().optional() })
    .optional(),
});

export type CodexAuth =
  | { available: true; accessToken: SecretString }
  | { available: false; reason: string };

function decodeJwtPayload(
  token: string,
): z.infer<typeof JwtPayloadSchema> | undefined {
  const segment = token.split(".").at(1);
  if (segment === undefined || segment === "") return undefined;
  try {
    return JwtPayloadSchema.parse(
      JSON.parse(Buffer.from(segment, "base64url").toString("utf8")),
    );
  } catch {
    return undefined;
  }
}

/**
 * Load one Codex CLI credential cache. Missing or stale credentials are a
 * skip, never a failure. The raw token goes straight into `SecretString`;
 * no token material is logged and nothing is written. Refreshing tokens is
 * the Codex CLI's job.
 */
export function loadCodexAuthFromPath(path: string): CodexAuth {
  let raw: string;
  try {
    raw = readFileSync(path, "utf8");
  } catch {
    return { available: false, reason: `${path} not found (run "codex login")` };
  }

  let accessToken: string;
  try {
    accessToken = AuthFileSchema.parse(JSON.parse(raw)).tokens.access_token;
  } catch {
    return {
      available: false,
      reason: `${path} has no tokens.access_token (run "codex login")`,
    };
  }

  const payload = decodeJwtPayload(accessToken);
  if (payload?.["https://api.openai.com/auth"]?.chatgpt_account_id === undefined) {
    return {
      available: false,
      reason: "access token has no chatgpt account claim (run codex to refresh)",
    };
  }
  if (payload.exp === undefined || payload.exp * 1000 <= Date.now() + 60_000) {
    return {
      available: false,
      reason: "access token expired (run codex to refresh)",
    };
  }
  return { available: true, accessToken: new SecretString(accessToken) };
}
