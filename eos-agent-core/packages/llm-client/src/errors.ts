import {
  APIConnectionError as AnthropicAPIConnectionError,
  APIError as AnthropicAPIError,
} from "@anthropic-ai/sdk";
import {
  APIConnectionError as OpenAiAPIConnectionError,
  APIError as OpenAiAPIError,
} from "openai";

/** The category of a provider failure. */
export type ProviderErrorKind =
  | "authentication"
  | "rate_limit"
  | "server"
  | "request"
  | "transport"
  | "decode";

export interface ProviderErrorOptions {
  status_code?: number;
  request_id?: string;
  retry_after_s?: number;
  truncated?: boolean;
}

/**
 * A normalized upstream provider failure. Callers branch on `kind`, never on
 * message text. `request_id` is the provider's opaque http
 * `request-id`/`x-request-id` header.
 */
export class ProviderError extends Error {
  readonly kind: ProviderErrorKind;
  /** The http status code, if the failure was http-shaped. */
  readonly status_code?: number;
  /** The provider http request-id header, if present. */
  readonly request_id?: string;
  /** The provider `retry-after` hint in seconds, if present. */
  readonly retry_after_s?: number;
  /**
   * True only for a stream that ended without the provider terminal event -
   * the one `decode` flavor the retry gate may retry. Parse failures stay
   * false and are never retried.
   */
  readonly truncated: boolean;

  constructor(
    kind: ProviderErrorKind,
    message: string,
    options: ProviderErrorOptions = {},
  ) {
    super(message);
    this.name = "ProviderError";
    this.kind = kind;
    this.status_code = options.status_code;
    this.request_id = options.request_id;
    this.retry_after_s = options.retry_after_s;
    this.truncated = options.truncated ?? false;
  }

  /**
   * Map an http status to a kind: 401/403 authentication, 429 rate_limit,
   * 500/502/503/529 server, anything else request.
   */
  static fromStatus(
    status: number,
    message: string,
    options: Omit<ProviderErrorOptions, "status_code" | "truncated"> = {},
  ): ProviderError {
    const kind: ProviderErrorKind =
      status === 401 || status === 403
        ? "authentication"
        : status === 429
          ? "rate_limit"
          : status === 500 || status === 502 || status === 503 || status === 529
            ? "server"
            : "request";
    return new ProviderError(kind, message, { ...options, status_code: status });
  }

  /** A connect/timeout/idle transport failure with no http status. */
  static transport(message: string): ProviderError {
    return new ProviderError("transport", message);
  }

  /** A stream that ended without the provider terminal event. */
  static truncatedStream(requestId?: string): ProviderError {
    return new ProviderError(
      "decode",
      "stream ended without provider terminal event",
      { request_id: requestId, truncated: true },
    );
  }
}

function requestIdOf(headers: Headers | undefined): string | undefined {
  return (
    headers?.get("request-id") ?? headers?.get("x-request-id") ?? undefined
  );
}

function retryAfterSeconds(headers: Headers | undefined): number | undefined {
  const raw = headers?.get("retry-after");
  if (raw === null || raw === undefined) return undefined;
  const seconds = Number(raw);
  if (Number.isFinite(seconds)) return seconds >= 0 ? seconds : undefined;
  const dateMs = Date.parse(raw);
  if (Number.isNaN(dateMs)) return undefined;
  return Math.max(0, (dateMs - Date.now()) / 1000);
}

/**
 * Map an error thrown by an sdk call to the owned taxonomy. `phase` selects
 * the fallback for errors with no http status: request construction failures
 * at `open`, stream parse failures while iterating a `stream`. Abort errors
 * never reach this mapping - providers rethrow them as-is when the caller's
 * signal is aborted.
 */
export function toProviderError(
  error: unknown,
  phase: "open" | "stream",
  requestIdFallback?: string,
): ProviderError {
  if (error instanceof ProviderError) return error;
  if (
    error instanceof AnthropicAPIConnectionError ||
    error instanceof OpenAiAPIConnectionError
  ) {
    return new ProviderError("transport", error.message, {
      request_id: requestIdFallback,
    });
  }
  if (
    error instanceof AnthropicAPIError ||
    error instanceof OpenAiAPIError
  ) {
    // instanceof narrows the generic sdk class to <any> type arguments;
    // restate the declared field shapes.
    const { status, headers, message } = error as {
      status?: number;
      headers?: Headers;
      message: string;
    };
    const requestId = requestIdOf(headers) ?? requestIdFallback;
    if (typeof status === "number") {
      return ProviderError.fromStatus(status, message, {
        request_id: requestId,
        retry_after_s: retryAfterSeconds(headers),
      });
    }
    // A status-less api error is an in-stream provider error payload.
    return new ProviderError("decode", message, {
      request_id: requestId,
    });
  }
  const message = error instanceof Error ? error.message : String(error);
  return new ProviderError(phase === "open" ? "request" : "decode", message, {
    request_id: requestIdFallback,
  });
}
