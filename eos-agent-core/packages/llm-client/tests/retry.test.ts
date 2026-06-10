import { describe, expect, it } from "vitest";

import { toolUseIdFrom } from "@eos/contracts";

import type { RetryConfig } from "../src/config.js";
import { ProviderError } from "../src/errors.js";
import type { LlmStreamEvent } from "../src/events.js";
import { retryStream } from "../src/retry.js";
import { collect, collectUntilError } from "./support.js";

const FAST: RetryConfig = {
  max_retries: 3,
  base_delay_s: 0,
  max_delay_s: 0,
  status_codes: [429, 500, 502, 503, 529],
};

function text(value: string): LlmStreamEvent {
  return { type: "assistant_text_delta", text: value };
}

function complete(): LlmStreamEvent {
  return {
    type: "assistant_message_complete",
    message: { role: "assistant", content: [] },
    usage: { input_tokens: 0, output_tokens: 0 },
  };
}

type Scripted = (LlmStreamEvent | ProviderError)[][];

/** Replay scripted attempts in order; an error element throws mid-stream. */
function scripted(attempts: Scripted): {
  factory: () => AsyncIterable<LlmStreamEvent>;
  calls: () => number;
} {
  let calls = 0;
  const factory = (): AsyncIterable<LlmStreamEvent> => {
    const script =
      attempts[calls] ?? [ProviderError.transport("script exhausted")];
    calls += 1;
    return (async function* () {
      await Promise.resolve();
      for (const item of script) {
        if (item instanceof ProviderError) throw item;
        yield item;
      }
    })();
  };
  return { factory, calls: () => calls };
}

describe("retry gate", () => {
  it("fails fast after visible output", async () => {
    const { factory, calls } = scripted([
      [text("hello"), ProviderError.transport("dropped")],
    ]);
    const { events, error } = await collectUntilError(
      retryStream(FAST, factory),
    );
    expect(events).toEqual([text("hello")]);
    expect(error).toBeInstanceOf(ProviderError);
    expect((error as ProviderError).kind).toBe("transport");
    expect(calls()).toBe(1);
  });

  it("retries only before visible output", async () => {
    const { factory, calls } = scripted([
      [ProviderError.fromStatus(429, "slow down")],
      [ProviderError.fromStatus(503, "unavailable")],
      [text("a"), text("b"), complete()],
    ]);
    const events = await collect(retryStream(FAST, factory));
    expect(events).toEqual([text("a"), text("b"), complete()]);
    expect(calls()).toBe(3);
  });

  it("does not retry a non-retryable auth error", async () => {
    const { factory, calls } = scripted([
      [ProviderError.fromStatus(401, "bad key")],
    ]);
    const { error } = await collectUntilError(retryStream(FAST, factory));
    expect((error as ProviderError).kind).toBe("authentication");
    expect(calls()).toBe(1);
  });

  it("exhausts the retry budget at 1 + max_retries attempts", async () => {
    const attempts: Scripted = Array.from({ length: 6 }, () => [
      ProviderError.fromStatus(429, "rl"),
    ]);
    const { factory, calls } = scripted(attempts);
    const { error } = await collectUntilError(retryStream(FAST, factory));
    expect((error as ProviderError).kind).toBe("rate_limit");
    expect(calls()).toBe(4);
  });

  it("treats tool_use_delta as visible output", async () => {
    const tool: LlmStreamEvent = {
      type: "tool_use_delta",
      tool_use_id: toolUseIdFrom("toolu_1"),
      name: "read",
      input: {},
    };
    const { factory, calls } = scripted([
      [tool, ProviderError.fromStatus(503, "server")],
    ]);
    const { events, error } = await collectUntilError(
      retryStream(FAST, factory),
    );
    expect(events).toEqual([tool]);
    expect((error as ProviderError).kind).toBe("server");
    expect(calls()).toBe(1);
  });

  it("skips the sleep on a degenerate delay instead of hanging", async () => {
    const cfg: RetryConfig = {
      max_retries: 1,
      base_delay_s: Number.POSITIVE_INFINITY,
      max_delay_s: Number.POSITIVE_INFINITY,
      status_codes: [503],
    };
    const { factory, calls } = scripted([
      [ProviderError.fromStatus(503, "server")],
      [text("ok"), complete()],
    ]);
    const started = Date.now();
    const events = await collect(retryStream(cfg, factory));
    expect(events).toHaveLength(2);
    expect(calls()).toBe(2);
    expect(Date.now() - started).toBeLessThan(1000);
  });

  it("retries a truncated stream but never a parse-failure decode", async () => {
    const truncatedThenOk = scripted([
      [ProviderError.truncatedStream("req-1")],
      [complete()],
    ]);
    await expect(
      collect(retryStream(FAST, truncatedThenOk.factory)),
    ).resolves.toEqual([complete()]);
    expect(truncatedThenOk.calls()).toBe(2);

    const parseFailure = scripted([
      [new ProviderError("decode", "bad frame")],
    ]);
    const { error } = await collectUntilError(
      retryStream(FAST, parseFailure.factory),
    );
    expect((error as ProviderError).kind).toBe("decode");
    expect(parseFailure.calls()).toBe(1);
  });

  it("stops immediately when aborted during backoff", async () => {
    const cfg: RetryConfig = { ...FAST, base_delay_s: 5, max_delay_s: 30 };
    const { factory, calls } = scripted([
      [ProviderError.fromStatus(503, "server")],
      [complete()],
    ]);
    const controller = new AbortController();
    const started = Date.now();
    const pending = collectUntilError(
      retryStream(cfg, factory, controller.signal),
    );
    setTimeout(() => { controller.abort(); }, 25);
    const { error } = await pending;
    expect(controller.signal.aborted).toBe(true);
    expect(error).toBe(controller.signal.reason);
    expect(calls()).toBe(1);
    expect(Date.now() - started).toBeLessThan(1500);
  });

  it("honors retry-after over the exponential delay, capped by max_delay_s", async () => {
    // retry_after_s would wait 9999s; max_delay_s caps the sleep at 10ms.
    const capped = scripted([
      [
        ProviderError.fromStatus(429, "rl", { retry_after_s: 9999 }),
      ],
      [complete()],
    ]);
    const cappedCfg: RetryConfig = { ...FAST, base_delay_s: 5, max_delay_s: 0.01 };
    let started = Date.now();
    await collect(retryStream(cappedCfg, capped.factory));
    expect(capped.calls()).toBe(2);
    expect(Date.now() - started).toBeLessThan(1500);

    // base_delay_s would wait 5s; retry-after overrides it down to 20ms.
    const overridden = scripted([
      [ProviderError.fromStatus(429, "rl", { retry_after_s: 0.02 })],
      [complete()],
    ]);
    const overrideCfg: RetryConfig = { ...FAST, base_delay_s: 5, max_delay_s: 30 };
    started = Date.now();
    await collect(retryStream(overrideCfg, overridden.factory));
    expect(overridden.calls()).toBe(2);
    expect(Date.now() - started).toBeLessThan(1500);
  });
});
