import { describe, expect, it } from "vitest";
import type { TraceNode } from "@/api/observability";
import { Waterfall } from "@/pages/sandbox/observability/TracesView";
import { renderWithAppProviders } from "../utils/renderWithAppProviders";

describe("Waterfall trace contract", () => {
  it("positions and labels attached EventNode payloads by their server offset", () => {
    const roots: TraceNode[] = [
      {
        offset_ms: 0,
        span: {
          ts: 1_100,
          trace: "trace-1",
          span: "span-1",
          name: "build",
          dur_ms: 200,
          status: "completed",
          attrs: {},
        },
        children: [],
        events: [
          {
            offset_ms: 100,
            event: {
              ts: 1_100,
              trace: "trace-1",
              parent: "span-1",
              name: "lease.acquired",
              attrs: {},
            },
          },
        ],
      },
    ] as unknown as TraceNode[];

    const { getByTitle } = renderWithAppProviders(<Waterfall traceId="trace-1" roots={roots} />);
    const marker = getByTitle("⚑ lease.acquired");

    expect((marker as HTMLElement).style.left).toBe("50%");
  });
});
