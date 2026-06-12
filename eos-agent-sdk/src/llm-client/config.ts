import { z } from "zod";

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
type RetryConfigInput = z.input<typeof RetryConfigSchema>;

/**
 * Per-chunk stream idle watchdog: a provider stream with no event inside the
 * window is aborted and surfaced as a `transport` failure.
 */
export const StreamGuardConfigSchema = z.object({
  idle_timeout_s: z.number().min(0).default(90),
});
type StreamGuardConfigInput = z.input<typeof StreamGuardConfigSchema>;

/** Construction options shared by every provider client (never serialized). */
export interface ProviderClientOptions {
  retry?: RetryConfigInput;
  streamGuard?: StreamGuardConfigInput;
  /** Injectable transport for tests; defaults to the global fetch. */
  fetch?: typeof globalThis.fetch;
}
