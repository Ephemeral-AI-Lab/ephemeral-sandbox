import { readFileSync } from "node:fs";

import type { LlmStreamEvent } from "../src/events.js";

export function fixture(relativePath: string): string {
  return readFileSync(new URL(relativePath, import.meta.url), "utf8");
}

export async function collect(
  stream: AsyncIterable<LlmStreamEvent>,
): Promise<LlmStreamEvent[]> {
  const events: LlmStreamEvent[] = [];
  for await (const event of stream) {
    events.push(event);
  }
  return events;
}

/** Collect events until iteration throws; the error is returned alongside. */
export async function collectUntilError(
  stream: AsyncIterable<LlmStreamEvent>,
): Promise<{ events: LlmStreamEvent[]; error: unknown }> {
  const events: LlmStreamEvent[] = [];
  try {
    for await (const event of stream) {
      events.push(event);
    }
  } catch (error) {
    return { events, error };
  }
  throw new Error("expected the stream to throw");
}

interface RecordedCall {
  url: string;
  init: RequestInit | undefined;
  body: unknown;
}

interface FetchStub {
  fetch: typeof globalThis.fetch;
  calls: RecordedCall[];
}

/**
 * A fetch double that replays one response factory per call (the last factory
 * repeats) and records each request body for encode assertions.
 */
export function fetchStub(
  factories: ((init: RequestInit | undefined) => Response)[],
): FetchStub {
  const calls: RecordedCall[] = [];
  const stub = (
    input: string | URL | Request,
    init?: RequestInit,
  ): Promise<Response> => {
    const factory =
      factories[Math.min(calls.length, factories.length - 1)] ??
      (() => new Response(null, { status: 500 }));
    calls.push({
      url:
        typeof input === "string"
          ? input
          : input instanceof URL
            ? input.href
            : input.url,
      init,
      body:
        typeof init?.body === "string"
          ? (JSON.parse(init.body) as unknown)
          : undefined,
    });
    return Promise.resolve(factory(init));
  };
  return { fetch: stub, calls };
}

/**
 * A complete SSE response with the given body and headers. The recorded
 * fixtures end without the final blank-line frame terminator; a live
 * provider always terminates frames, so the missing terminator is restored
 * at the transport double.
 */
export function sseResponse(
  body: string,
  headers: Record<string, string> = {},
): Response {
  const terminated = body.endsWith("\n\n") ? body : `${body}\n`;
  return new Response(terminated, {
    status: 200,
    headers: { "content-type": "text/event-stream", ...headers },
  });
}

/** A JSON error response, the shape both providers use for http failures. */
export function errorResponse(
  status: number,
  headers: Record<string, string> = {},
): Response {
  return new Response(
    JSON.stringify({
      error: { type: "api_error", message: "upstream says no" },
    }),
    { status, headers: { "content-type": "application/json", ...headers } },
  );
}

/**
 * An SSE response that emits a prefix and then hangs open. The stream errors
 * when the request signal aborts, like a real socket teardown.
 */
export function hangingSseResponse(
  prefix: string,
  init: RequestInit | undefined,
): Response {
  let controller: ReadableStreamDefaultController<Uint8Array> | undefined;
  const body = new ReadableStream<Uint8Array>({
    start(c) {
      controller = c;
      c.enqueue(new TextEncoder().encode(prefix));
    },
  });
  const signal = init?.signal;
  if (signal) {
    const fail = () =>
      controller?.error(new DOMException("aborted", "AbortError"));
    if (signal.aborted) fail();
    else signal.addEventListener("abort", fail, { once: true });
  }
  return new Response(body, {
    status: 200,
    headers: { "content-type": "text/event-stream" },
  });
}
