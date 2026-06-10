import { inspect } from "node:util";

import { describe, expect, it } from "vitest";

import {
  AnthropicApiConfigSchema,
  OpenAiApiConfigSchema,
  RetryConfigSchema,
  StreamGuardConfigSchema,
} from "../src/config.js";
import { SecretString } from "../src/secret.js";

describe("secret string", () => {
  const secret = new SecretString("sk-super-secret");

  it("redacts string conversion, json, and inspect", () => {
    expect(String(secret)).toBe("[redacted]");
    expect(JSON.stringify({ api_key: secret })).toBe(
      '{"api_key":"[redacted]"}',
    );
    expect(inspect(secret)).toBe("[redacted]");
    expect(inspect({ nested: secret })).not.toContain("sk-super-secret");
  });

  it("exposes the raw value only explicitly", () => {
    expect(secret.expose()).toBe("sk-super-secret");
  });
});

describe("retry config", () => {
  it("applies the documented defaults", () => {
    expect(RetryConfigSchema.parse({})).toEqual({
      max_retries: 3,
      base_delay_s: 1,
      max_delay_s: 30,
      status_codes: [429, 500, 502, 503, 529],
    });
  });

  it("rejects negative delays", () => {
    expect(RetryConfigSchema.safeParse({ base_delay_s: -1 }).success).toBe(
      false,
    );
    expect(RetryConfigSchema.safeParse({ max_delay_s: -0.5 }).success).toBe(
      false,
    );
  });
});

describe("stream guard config", () => {
  it("defaults the idle timeout to 90s and rejects negatives", () => {
    expect(StreamGuardConfigSchema.parse({})).toEqual({ idle_timeout_s: 90 });
    expect(
      StreamGuardConfigSchema.safeParse({ idle_timeout_s: -1 }).success,
    ).toBe(false);
  });
});

describe("provider configs", () => {
  it("defaults base urls and wraps api keys as secrets", () => {
    const anthropic = AnthropicApiConfigSchema.parse({ api_key: "a-key" });
    expect(anthropic.base_url).toBe("https://api.anthropic.com");
    expect(anthropic.api_key).toBeInstanceOf(SecretString);
    expect(anthropic.api_key.expose()).toBe("a-key");
    expect(JSON.stringify(anthropic)).not.toContain("a-key");

    const openai = OpenAiApiConfigSchema.parse({ api_key: "o-key" });
    expect(openai.base_url).toBe("https://api.openai.com/v1");
    expect(openai.api_key.expose()).toBe("o-key");
  });

  it("accepts an already-wrapped secret and requires the key", () => {
    const wrapped = new SecretString("pre-wrapped");
    expect(
      AnthropicApiConfigSchema.parse({ api_key: wrapped }).api_key.expose(),
    ).toBe("pre-wrapped");
    expect(AnthropicApiConfigSchema.safeParse({}).success).toBe(false);
  });
});
