import { describe, expect, it } from "vitest";

import { EventStream, type AgentEvent } from "../src/agent-runtime-handle.js";

const tick = (): Promise<void> => Promise.resolve();

function ping(text: string): AgentEvent {
  return { type: "assistant_text_delta", text };
}

async function drain(stream: EventStream): Promise<AgentEvent[]> {
  const seen: AgentEvent[] = [];
  for await (const event of stream) {
    seen.push(event);
  }
  return seen;
}

describe("EventStream", () => {
  it("yields pushed events in order and completes after close", async () => {
    const stream = new EventStream();
    stream.push(ping("a"));
    stream.push(ping("b"));
    const consuming = drain(stream);
    stream.push(ping("c"));
    stream.close();
    expect(await consuming).toEqual([ping("a"), ping("b"), ping("c")]);
  });

  it("wakes a consumer that is already waiting", async () => {
    const stream = new EventStream();
    const consuming = drain(stream);
    await tick();
    stream.push(ping("late"));
    stream.close();
    expect(await consuming).toEqual([ping("late")]);
  });

  it("throws on a second consumer", () => {
    const stream = new EventStream();
    stream[Symbol.asyncIterator]();
    expect(() => stream[Symbol.asyncIterator]()).toThrow("single consumer");
  });

  it("retains every event for a consumer that arrives after close", async () => {
    const stream = new EventStream();
    stream.push(ping("kept"));
    stream.push(ping("kept too"));
    stream.close();
    expect(await drain(stream)).toEqual([ping("kept"), ping("kept too")]);
  });

  it("detaches on early return and stays done while pushes continue", async () => {
    const stream = new EventStream();
    stream.push(ping("first"));
    const iterator = stream[Symbol.asyncIterator]();
    const first = await iterator.next();
    expect(first.done).toBe(false);
    expect(first.value).toEqual(ping("first"));
    await iterator.return?.();
    stream.push(ping("dropped"));
    stream.close();
    expect((await iterator.next()).done).toBe(true);
  });
});
