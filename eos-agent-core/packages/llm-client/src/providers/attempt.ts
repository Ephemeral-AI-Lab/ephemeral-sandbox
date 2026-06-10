import type { JsonObject } from "@eos/contracts";

import { ProviderError, toProviderError } from "../errors.js";
import type { LlmStreamEvent } from "../events.js";

/**
 * Per-provider decoder state machine: sdk stream events in, normalized
 * events out. Decoders accumulate per-block strings linearly and parse tool
 * arguments once at block close.
 */
export interface StreamDecoder<TEvent> {
  /** Set once the provider terminal event has been decoded. */
  readonly completed: boolean;
  handle(event: TEvent): Iterable<LlmStreamEvent>;
}

export interface ProviderAttempt<TEvent> {
  /** Open one sdk streaming call under the attempt's abort signal. */
  open(
    signal: AbortSignal,
  ): Promise<{ stream: AsyncIterable<TEvent>; requestId?: string }>;
  decoder(requestId: string | undefined): StreamDecoder<TEvent>;
}

/** Parse accumulated tool-argument json; malformed provider json yields `{}`. */
export function parseToolArgs(raw: string): JsonObject {
  if (raw === "") return {};
  try {
    const parsed: unknown = JSON.parse(raw);
    if (parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as JsonObject;
    }
  } catch {
    // fall through to the empty object
  }
  return {};
}

class IdleTimeoutSignal extends Error {}

/**
 * Race each chunk against the idle watchdog: a stream that goes quiet is a
 * first-class `transport` failure, not a hang. On timeout the in-flight
 * request is aborted.
 */
async function* withIdleGuard<T>(
  source: AsyncIterable<T>,
  idleTimeoutMs: number,
  abort: AbortController,
): AsyncGenerator<T> {
  const iterator = source[Symbol.asyncIterator]();
  try {
    for (;;) {
      const next = iterator.next();
      let timer: NodeJS.Timeout | undefined;
      try {
        const result = await Promise.race([
          next,
          new Promise<never>((_, reject) => {
            timer = setTimeout(
              () => { reject(new IdleTimeoutSignal()); },
              idleTimeoutMs,
            );
          }),
        ]);
        if (result.done) return;
        yield result.value;
      } catch (error) {
        if (error instanceof IdleTimeoutSignal) {
          // The pending read settles after the abort; observe its rejection.
          void next.catch(() => undefined);
          abort.abort();
          throw ProviderError.transport(
            `provider stream idle for ${String(idleTimeoutMs / 1000)}s`,
          );
        }
        throw error;
      } finally {
        clearTimeout(timer);
      }
    }
  } finally {
    try {
      await iterator.return?.();
    } catch {
      // the stream already failed; nothing left to release
    }
  }
}

/**
 * Run one provider attempt: open the sdk stream, guard it with the idle
 * watchdog, feed events through the decoder, and enforce the iteration
 * contract (exactly one terminal event; a clean end without it is a
 * truncated stream). Caller aborts are rethrown as-is.
 */
export async function* runAttempt<TEvent>(
  attempt: ProviderAttempt<TEvent>,
  idleTimeoutMs: number,
  signal: AbortSignal | undefined,
): AsyncGenerator<LlmStreamEvent> {
  const attemptAbort = new AbortController();
  const attemptSignal = signal
    ? AbortSignal.any([signal, attemptAbort.signal])
    : attemptAbort.signal;
  let stream: AsyncIterable<TEvent>;
  let requestId: string | undefined;
  try {
    ({ stream, requestId } = await attempt.open(attemptSignal));
  } catch (error) {
    if (signal?.aborted) throw error;
    throw toProviderError(error, "open");
  }
  const decoder = attempt.decoder(requestId);
  try {
    for await (const event of withIdleGuard(
      stream,
      idleTimeoutMs,
      attemptAbort,
    )) {
      yield* decoder.handle(event);
      if (decoder.completed) break;
    }
  } catch (error) {
    if (signal?.aborted) throw error;
    throw toProviderError(error, "stream", requestId);
  } finally {
    attemptAbort.abort();
  }
  signal?.throwIfAborted();
  if (!decoder.completed) {
    throw ProviderError.truncatedStream(requestId);
  }
}
