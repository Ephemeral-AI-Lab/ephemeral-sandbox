import { renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useRunEvents } from "@/api/useRunEvents";

const encoder = new TextEncoder();

function event(sequence: number): string {
  return [
    `id: ${sequence}`,
    "event: run_state",
    `data: ${JSON.stringify({ sequence, run_id: "run-1", monotonic_offset_ns: sequence, data: { kind: "run_state", state: "running" } })}`,
    "",
    "",
  ].join("\n");
}

afterEach(() => vi.unstubAllGlobals());

describe("useRunEvents", () => {
  it("waits for the initial run snapshot and counts the persisted gap from that connection boundary", async () => {
    const stream = { controller: null as ReadableStreamDefaultController<Uint8Array> | null };
    const fetchMock = vi.fn(async () => new Response(new ReadableStream({
      start(controller) {
        stream.controller = controller;
      },
    }), { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    window.sessionStorage.setItem("eos-benchmark:last-event:run-1", "10");

    const { result, rerender, unmount } = renderHook(
      ({ expectedSequence }) => useRunEvents("run-1", expectedSequence),
      { initialProps: { expectedSequence: null as number | null } },
    );

    expect(fetchMock).not.toHaveBeenCalled();
    rerender({ expectedSequence: 20 });
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    const controller = stream.controller;
    if (!controller) throw new Error("The SSE stream did not start.");
    controller.enqueue(encoder.encode(Array.from({ length: 10 }, (_, index) => event(index + 11)).join("")));

    await waitFor(() => {
      expect(result.current.lastEventId).toBe(20);
      expect(result.current.replayedEventCount).toBe(10);
      expect(result.current.connectionState).toBe("live");
    });
    unmount();
  });
});
