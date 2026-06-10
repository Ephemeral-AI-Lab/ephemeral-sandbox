import { setTimeout as sleep } from "node:timers/promises";

import type { RetryConfig } from "./config.js";
import { ProviderError } from "./errors.js";
import type { LlmStreamEvent } from "./events.js";

function isVisible(event: LlmStreamEvent): boolean {
  return (
    event.type === "assistant_text_delta" ||
    event.type === "reasoning_delta" ||
    event.type === "tool_use_delta"
  );
}

/**
 * Retryable iff `rate_limit`/`server` with a status in `cfg.status_codes`, a
 * `transport` failure, or a truncated-stream `decode`. Parse-failure
 * `decode`, `authentication`, and `request` are never retried.
 */
function isRetryable(cfg: RetryConfig, error: ProviderError): boolean {
  switch (error.kind) {
    case "rate_limit":
    case "server":
      return (
        error.status_code !== undefined &&
        cfg.status_codes.includes(error.status_code)
      );
    case "transport":
      return true;
    case "decode":
      return error.truncated;
    default:
      return false;
  }
}

/**
 * Sleep `min(retry_after_s ?? base_delay_s * 2^attempt, max_delay_s)`. A
 * non-finite or non-positive delay skips the sleep. The sleep races the
 * abort signal: an interrupt during backoff must not wait out the delay.
 */
async function backoff(
  cfg: RetryConfig,
  error: ProviderError,
  attempt: number,
  signal: AbortSignal | undefined,
): Promise<void> {
  const delayMs =
    Math.min(
      error.retry_after_s ?? cfg.base_delay_s * 2 ** attempt,
      cfg.max_delay_s,
    ) * 1000;
  if (Number.isFinite(delayMs) && delayMs > 0) {
    try {
      await sleep(delayMs, undefined, { signal });
    } catch {
      // aborted mid-backoff; surfaced by throwIfAborted below
    }
  }
  signal?.throwIfAborted();
}

/**
 * Wrap a per-attempt stream factory in the visible-output retry gate.
 *
 * One `emittedVisible` flag spans all attempts: once any delta variant has
 * been forwarded, a later failure fails fast - re-running the request would
 * duplicate text and double-dispatch tool_use ids. No new attempt starts
 * after `signal` aborts, and abort errors are rethrown as-is.
 */
export async function* retryStream(
  cfg: RetryConfig,
  attempt: () => AsyncIterable<LlmStreamEvent>,
  signal?: AbortSignal,
): AsyncGenerator<LlmStreamEvent, void, undefined> {
  let emittedVisible = false;
  for (let attemptIndex = 0; ; attemptIndex += 1) {
    signal?.throwIfAborted();
    try {
      for await (const event of attempt()) {
        if (isVisible(event)) emittedVisible = true;
        yield event;
      }
      return;
    } catch (error) {
      if (signal?.aborted) throw error;
      if (
        emittedVisible ||
        attemptIndex >= cfg.max_retries ||
        !(error instanceof ProviderError) ||
        !isRetryable(cfg, error)
      ) {
        throw error;
      }
      await backoff(cfg, error, attemptIndex, signal);
    }
  }
}
