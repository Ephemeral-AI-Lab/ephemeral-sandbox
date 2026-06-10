import {
  APIConnectionError as AnthropicAPIConnectionError,
  APIError as AnthropicAPIError,
} from "@anthropic-ai/sdk";
import {
  APIConnectionError as OpenAiAPIConnectionError,
  APIError as OpenAiAPIError,
} from "openai";
import { describe, expect, it } from "vitest";

import {
  ProviderError,
  toProviderError,
  type ProviderErrorKind,
} from "../src/errors.js";

describe("status mapping", () => {
  const cases: [number, ProviderErrorKind][] = [
    [401, "authentication"],
    [403, "authentication"],
    [429, "rate_limit"],
    [500, "server"],
    [502, "server"],
    [503, "server"],
    [529, "server"],
    [400, "request"],
    [404, "request"],
    [504, "request"],
  ];

  it.each(cases)("maps status %i to %s", (status, kind) => {
    const error = ProviderError.fromStatus(status, "boom", {
      request_id: "req-7",
    });
    expect(error.kind).toBe(kind);
    expect(error.status_code).toBe(status);
    expect(error.request_id).toBe("req-7");
  });

  it("keeps transport and truncated decode status-free", () => {
    const transport = ProviderError.transport("connection reset");
    expect(transport.kind).toBe("transport");
    expect(transport.status_code).toBeUndefined();
    expect(transport.truncated).toBe(false);

    const truncated = ProviderError.truncatedStream("req-9");
    expect(truncated.kind).toBe("decode");
    expect(truncated.status_code).toBeUndefined();
    expect(truncated.request_id).toBe("req-9");
    expect(truncated.truncated).toBe(true);
  });
});

describe("sdk error mapping", () => {
  it("maps both sdks' http errors through the status table", () => {
    const anthropic = new AnthropicAPIError(
      503,
      { error: { type: "api_error" } },
      "unavailable",
      new Headers({ "request-id": "req_a" }),
    );
    const mappedA = toProviderError(anthropic, "stream");
    expect(mappedA.kind).toBe("server");
    expect(mappedA.status_code).toBe(503);
    expect(mappedA.request_id).toBe("req_a");

    const openai = new OpenAiAPIError(
      401,
      { error: {} },
      "bad key",
      new Headers({ "x-request-id": "req_o" }),
    );
    const mappedO = toProviderError(openai, "open");
    expect(mappedO.kind).toBe("authentication");
    expect(mappedO.status_code).toBe(401);
    expect(mappedO.request_id).toBe("req_o");
  });

  it("captures retry-after seconds on 429", () => {
    const error = new AnthropicAPIError(
      429,
      { error: { type: "rate_limit_error" } },
      "slow down",
      new Headers({ "retry-after": "7", "request-id": "req_r" }),
    );
    const mapped = toProviderError(error, "stream");
    expect(mapped.kind).toBe("rate_limit");
    expect(mapped.retry_after_s).toBe(7);
    expect(mapped.request_id).toBe("req_r");
  });

  it("captures http-date retry-after as a non-negative delay", () => {
    const error = new OpenAiAPIError(
      429,
      { error: {} },
      "slow down",
      new Headers({ "retry-after": new Date(Date.now() + 5000).toUTCString() }),
    );
    const mapped = toProviderError(error, "stream");
    expect(mapped.retry_after_s).toBeGreaterThanOrEqual(0);
    expect(mapped.retry_after_s).toBeLessThanOrEqual(6);
  });

  it("maps connection errors to transport", () => {
    expect(
      toProviderError(
        new AnthropicAPIConnectionError({ message: "socket hang up" }),
        "stream",
      ).kind,
    ).toBe("transport");
    expect(
      toProviderError(
        new OpenAiAPIConnectionError({ message: "socket hang up" }),
        "open",
      ).kind,
    ).toBe("transport");
  });

  it("maps a status-less api error to decode (in-stream error payload)", () => {
    const error = new AnthropicAPIError(
      undefined,
      { error: { type: "overloaded_error", message: "overloaded" } },
      undefined,
      undefined,
    );
    const mapped = toProviderError(error, "stream", "req-fallback");
    expect(mapped.kind).toBe("decode");
    expect(mapped.truncated).toBe(false);
    expect(mapped.request_id).toBe("req-fallback");
  });

  it("falls back by phase for unrecognized errors", () => {
    const parse = toProviderError(new SyntaxError("bad json"), "stream");
    expect(parse.kind).toBe("decode");
    expect(parse.truncated).toBe(false);
    const build = toProviderError(new TypeError("bad url"), "open");
    expect(build.kind).toBe("request");
  });

  it("passes provider errors through unchanged", () => {
    const original = ProviderError.transport("idle");
    expect(toProviderError(original, "stream")).toBe(original);
  });
});
