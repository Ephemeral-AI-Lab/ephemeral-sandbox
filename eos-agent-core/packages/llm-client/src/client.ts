import type { LlmStreamEvent } from "./events.js";
import type { LlmRequest } from "./types.js";

export interface LlmStreamOptions {
  signal?: AbortSignal;
}

/**
 * A provider-neutral streaming model client.
 *
 * Iteration contract:
 * - single-pass: the returned iterable may be iterated once,
 * - success: zero or more deltas, then exactly one
 *   `assistant_message_complete`, then end (an empty assistant message is
 *   legal; absent usage fields default to zero),
 * - a stream that ends without the provider terminal event throws a
 *   truncated-stream `ProviderError` of kind `decode`,
 * - all other failures throw `ProviderError` from iteration - except
 *   cancellation: when `options.signal` aborts, the abort error is rethrown
 *   as-is and callers classify by `signal.aborted`, never by error type.
 */
export interface LlmClient {
  streamMessage(
    request: LlmRequest,
    options?: LlmStreamOptions,
  ): AsyncIterable<LlmStreamEvent>;
}
