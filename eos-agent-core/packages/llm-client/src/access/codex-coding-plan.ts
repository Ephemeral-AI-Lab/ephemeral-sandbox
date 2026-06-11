import { z } from "zod";

import { ProviderError } from "../errors.js";
import type { SecretString } from "../secret.js";
import type { Access } from "./access.js";

/** The namespaced claim where ChatGPT account metadata is stored. */
const OPENAI_AUTH_CLAIM = "https://api.openai.com/auth";

const CodexAccessClaimsSchema = z.object({
  [OPENAI_AUTH_CLAIM]: z
    .object({
      chatgpt_account_id: z.string().optional(),
      chatgpt_account_is_fedramp: z.boolean().default(false),
    })
    .optional(),
});

export interface CodexAccessClaims {
  /** The ChatGPT workspace/account id sent as `chatgpt-account-id`. */
  accountId: string;
  /** Whether the account must route through the FedRAMP edge. */
  isFedrampAccount: boolean;
}

/**
 * Claim parsing for a Codex access token: the payload is the second
 * `.`-separated segment, base64url without padding, json, carrying a
 * non-blank account id under the namespaced auth claim.
 *
 * Failures throw `ProviderError` kind `request` with lowercase messages.
 * This function reads only the token value supplied by the caller; it
 * never loads a credential cache file.
 */
export function codexAccessClaimsFromJwt(token: string): CodexAccessClaims {
  const payload = token.split(".").at(1);
  if (payload === undefined || payload === "") {
    throw new ProviderError("request", "codex access token is not a jwt");
  }
  if (!/^[A-Za-z0-9_-]+$/.test(payload)) {
    throw new ProviderError(
      "request",
      "codex access token payload is not base64url",
    );
  }
  let claims: z.infer<typeof CodexAccessClaimsSchema>;
  try {
    const decoded: unknown = JSON.parse(
      Buffer.from(payload, "base64url").toString("utf8"),
    );
    claims = CodexAccessClaimsSchema.parse(decoded);
  } catch {
    throw new ProviderError(
      "request",
      "codex access token payload is not json",
    );
  }
  const auth = claims[OPENAI_AUTH_CLAIM];
  if (auth === undefined) {
    throw new ProviderError(
      "request",
      `codex access token missing ${OPENAI_AUTH_CLAIM} claim`,
    );
  }
  if (auth.chatgpt_account_id === undefined || auth.chatgpt_account_id.trim() === "") {
    throw new ProviderError(
      "request",
      "codex access token missing chatgpt_account_id claim",
    );
  }
  return {
    accountId: auth.chatgpt_account_id,
    isFedrampAccount: auth.chatgpt_account_is_fedramp,
  };
}

/**
 * Codex coding-plan access: ChatGPT-managed bearer plus account routing
 * headers derived from the token's JWT claims at construction time.
 */
export function codexCodingPlanAccess(
  baseUrl: string,
  accessToken: SecretString,
): Access {
  const claims = codexAccessClaimsFromJwt(accessToken.expose());
  const headers: Record<string, string> = {
    "chatgpt-account-id": claims.accountId,
  };
  if (claims.isFedrampAccount) {
    headers["x-openai-fedramp"] = "true";
  }
  return {
    baseUrl,
    credential: { kind: "bearer", secret: accessToken },
    headers: () => Promise.resolve({ ...headers }),
  };
}
