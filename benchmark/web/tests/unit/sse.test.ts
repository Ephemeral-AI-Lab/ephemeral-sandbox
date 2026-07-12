import { describe, expect, it } from "vitest";
import { decodeEventStream } from "@/api/sse";

const record = {
  sequence: 7,
  run_id: "run-1",
  monotonic_offset_ns: 1_250,
  data: { kind: "trial_phase", cell_id: "cell-1", trial_id: "trial-2", warmup: false, phase: "operation", state: "running" },
};

describe("benchmark SSE decoder", () => {
  it("decodes the persisted EventRecord envelope and keeps an incomplete remainder", () => {
    const stream = `id: 7\r\nevent: trial_phase\r\ndata: ${JSON.stringify(record)}\r\n\r\nid: 8`;
    const decoded = decodeEventStream(stream);

    expect(decoded.records).toEqual([record]);
    expect(decoded.remainder).toBe("id: 8");
  });

  it("rejects an event name that disagrees with the typed payload", () => {
    const stream = `id: 7\nevent: warning\ndata: ${JSON.stringify(record)}\n\n`;
    expect(() => decodeEventStream(stream)).toThrow(/event name/);
  });
});
