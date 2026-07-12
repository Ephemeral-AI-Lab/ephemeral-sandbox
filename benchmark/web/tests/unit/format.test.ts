import { describe, expect, it } from "vitest";
import { formatBytes, formatDurationNs } from "@/lib/format";

describe("explicit unit boundaries", () => {
  it.each([
    [0, "0 ns"],
    [999, "999 ns"],
    [1_000, "1 µs"],
    [999_999, "1,000 µs"],
    [1_000_000, "1 ms"],
    [999_999_999, "1,000 ms"],
    [1_000_000_000, "1 s"],
  ] as const)("formats %i nanoseconds as %s", (value, expected) => {
    expect(formatDurationNs(value)).toBe(expected);
  });

  it.each([
    [0, "0 B"],
    [1_023, "1,023 B"],
    [1_024, "1 KiB"],
    [1_048_576, "1 MiB"],
    [1_073_741_824, "1 GiB"],
  ] as const)("formats %i bytes as %s", (value, expected) => {
    expect(formatBytes(value)).toBe(expected);
  });
});
