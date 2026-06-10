import { z } from "zod";

import { SecretString } from "./secret.js";

const secretString = z.union([
  z.instanceof(SecretString),
  z.string().transform((raw) => new SecretString(raw)),
]);

/** Provider retry policy consumed by the retry gate. */
export const RetryConfigSchema = z.object({
  max_retries: z.number().int().min(0).default(3),
  base_delay_s: z.number().min(0).default(1),
  max_delay_s: z.number().min(0).default(30),
  status_codes: z
    .array(z.number().int())
    .default([429, 500, 502, 503, 529]),
});
export type RetryConfig = z.infer<typeof RetryConfigSchema>;
export type RetryConfigInput = z.input<typeof RetryConfigSchema>;

/**
 * Per-chunk stream idle watchdog: a provider stream with no event inside the
 * window is aborted and surfaced as a `transport` failure.
 */
export const StreamGuardConfigSchema = z.object({
  idle_timeout_s: z.number().min(0).default(90),
});
export type StreamGuardConfig = z.infer<typeof StreamGuardConfigSchema>;
export type StreamGuardConfigInput = z.input<typeof StreamGuardConfigSchema>;

export const AnthropicApiConfigSchema = z.object({
  base_url: z.string().default("https://api.anthropic.com"),
  api_key: secretString,
});
export type AnthropicApiConfig = z.infer<typeof AnthropicApiConfigSchema>;
export type AnthropicApiConfigInput = z.input<typeof AnthropicApiConfigSchema>;

export const OpenAiApiConfigSchema = z.object({
  base_url: z.string().default("https://api.openai.com/v1"),
  api_key: secretString,
});
export type OpenAiApiConfig = z.infer<typeof OpenAiApiConfigSchema>;
export type OpenAiApiConfigInput = z.input<typeof OpenAiApiConfigSchema>;

/** Construction options shared by both provider clients (never serialized). */
export interface ProviderClientOptions {
  retry?: RetryConfigInput;
  streamGuard?: StreamGuardConfigInput;
  /** Injectable transport for tests; defaults to the global fetch. */
  fetch?: typeof globalThis.fetch;
}
