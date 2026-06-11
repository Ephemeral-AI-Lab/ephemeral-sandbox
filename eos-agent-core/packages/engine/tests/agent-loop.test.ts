import { getEventListeners } from "node:events";

import { describe, expect, it } from "vitest";

import { toolUseIdFrom, type ToolCallResult } from "@eos/contracts";
import { ProviderError } from "@eos/llm-client";
import { RunHandle } from "@eos/agent-runtime/agent-run-handle";
import { BackgroundSessionSupervisor } from "@eos/background";

import {
  NotificationInbox,
  systemNotificationMessage,
} from "@eos/notification";
import { startAgentRun, type ToolExecutor } from "../src/index.js";
import {
  MockLlmClient,
  USAGE,
  asCancelled,
  asCompleted,
  asFailed,
  assistantMessage,
  collectEvents,
  complete,
  deferred,
  emptyExecutor,
  expectProviderValid,
  failingTurn,
  gatedTurn,
  hangingTurn,
  must,
  reasoningDelta,
  recordingObserver,
  scriptedExecutor,
  scriptedTurn,
  backgroundSessionHandle,
  startMockRun,
  submitHandler,
  textBlock,
  textDelta,
  tick,
  toolResultBlock,
  toolUseBlock,
  toolUseDelta,
  userText,
} from "./support.js";

const SUBMISSION = { summary: "done", payload: { answer: 42 } };

describe("agent loop", () => {
  it("finishes only through a terminal tool result and carries the submission (§15.2)", async () => {
    const tools = scriptedExecutor(["submit", submitHandler(SUBMISSION)]);
    const call = assistantMessage(
      textBlock("submitting"),
      toolUseBlock("tu_s", "submit", { summary: "done" }),
    );
    const { client, handle } = startMockRun(
      [scriptedTurn([complete(call, "tool_use")])],
      { tools },
    );
    const outcome = asCompleted(await handle.outcome);
    expect(outcome.turns).toBe(1);
    expect(outcome.submission).toEqual(SUBMISSION);
    expect(outcome.final_message).toEqual(call);
    expect(outcome.stop_reason).toBe("tool_use");
    expect(outcome.usage).toEqual(USAGE);
    expect(must(outcome.llm.at(-1))).toEqual({
      role: "user",
      content: [toolResultBlock("tu_s", JSON.stringify(SUBMISSION))],
    });
    expect(client.requests).toHaveLength(1);
    expectProviderValid(outcome.llm);
  });

  it("feeds tool results back into the next provider request (P03 §14.2)", async () => {
    const tools = scriptedExecutor(
      ["calc", () => Promise.resolve({ content: "42" })],
      ["submit", submitHandler(SUBMISSION)],
    );
    const call = assistantMessage(toolUseBlock("tu_1", "calc", { expr: "6*7" }));
    const { client, handle } = startMockRun(
      [
        scriptedTurn([complete(call, "tool_use")]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_2", "submit")), "tool_use"),
        ]),
      ],
      { tools },
    );
    const outcome = asCompleted(await handle.outcome);
    expect(outcome.turns).toBe(2);
    const secondRequest = must(client.requests.at(1));
    expect(secondRequest.messages).toEqual([
      userText("hi"),
      call,
      { role: "user", content: [toolResultBlock("tu_1", "42")] },
    ]);
    expect(secondRequest.tools).toEqual([
      { name: "calc", description: "calc", input_schema: {} },
      { name: "submit", description: "submit", input_schema: {} },
    ]);
    expectProviderValid(outcome.llm);
  });

  it("continues past bare text turns appending nothing — text never terminates (§15.4)", async () => {
    const tools = scriptedExecutor(["submit", submitHandler(SUBMISSION)]);
    const { client, handle } = startMockRun(
      [
        scriptedTurn([complete(assistantMessage(textBlock("thinking")))]),
        scriptedTurn([complete(assistantMessage(textBlock("still thinking")), "max_tokens")]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools },
    );
    const outcome = asCompleted(await handle.outcome);
    expect(outcome.turns).toBe(3);
    expect(outcome.stop_reason, "stop_reason comes from the submitting turn").toBe(
      "tool_use",
    );
    expect(
      must(client.requests.at(1)).messages,
      "a bare text turn appends nothing before the next call",
    ).toEqual([userText("hi"), assistantMessage(textBlock("thinking"))]);
    expect(
      must(client.requests.at(2)).messages,
      "a max_tokens truncation does not terminate either",
    ).toEqual([
      userText("hi"),
      assistantMessage(textBlock("thinking")),
      assistantMessage(textBlock("still thinking")),
    ]);
    expectProviderValid(outcome.llm);
  });

  it("fails with max_turns and no submission when the model never submits (§15.4)", async () => {
    const { client, handle } = startMockRun(
      [
        scriptedTurn([complete(assistantMessage(textBlock("a")))]),
        scriptedTurn([complete(assistantMessage(textBlock("b")))]),
      ],
      { maxTurns: 2 },
    );
    const outcome = asFailed(await handle.outcome);
    expect(outcome.failure.kind).toBe("max_turns");
    expect(outcome.turns).toBe(2);
    expect(client.requests).toHaveLength(2);
    expect("submission" in outcome).toBe(false);
    expectProviderValid(outcome.llm);
  });

  it("parks on background sessions after a bare text turn and wakes on the settlement (§15.3)", async () => {
    const inbox = new NotificationInbox();
    const supervisor = new BackgroundSessionSupervisor(inbox);
    const session = backgroundSessionHandle();
    supervisor.registerBackgroundSession(
      { type: "command", id: "c1" },
      session.handle,
    );
    const tools = scriptedExecutor(["submit", submitHandler(SUBMISSION)]);
    const { client, handle } = startMockRun(
      [
        scriptedTurn([complete(assistantMessage(textBlock("waiting on the build")))]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools, notifications: inbox, background: supervisor },
    );
    await tick();
    await tick();
    expect(client.requests, "the loop parks instead of burning a provider call").toHaveLength(1);
    session.settle({ status: "completed", summary: "build ok" });
    const outcome = asCompleted(await handle.outcome);
    expect(outcome.turns, "waiting consumed no turn").toBe(2);
    expect(must(client.requests.at(1)).messages).toEqual([
      userText("hi"),
      assistantMessage(textBlock("waiting on the build")),
      systemNotificationMessage({
        type: "session_settled",
        session: { type: "command", id: "c1" },
        status: "completed",
        summary: "build ok",
      }),
    ]);
    expect(supervisor.openBackgroundSessionCount(), "the drain marked the session delivered").toBe(0);
    expectProviderValid(outcome.llm);
  });

  it("wakes the parked loop on a steer, draining steers above notifications (§15.3, §15.5)", async () => {
    const inbox = new NotificationInbox();
    const supervisor = new BackgroundSessionSupervisor(inbox);
    const session = backgroundSessionHandle();
    supervisor.registerBackgroundSession(
      { type: "command", id: "c9" },
      session.handle,
    );
    const tools = scriptedExecutor(["submit", submitHandler(SUBMISSION)]);
    const { client, handle } = startMockRun(
      [
        scriptedTurn([complete(assistantMessage(textBlock("waiting")))]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools, notifications: inbox, background: supervisor },
    );
    await tick();
    await tick();
    expect(client.requests).toHaveLength(1);
    session.settle({ status: "completed", summary: "finished" });
    expect(handle.steer(userText("change of plans"))).toBe(true);
    const outcome = asCompleted(await handle.outcome);
    expect(
      must(client.requests.at(1)).messages,
      "steers drain before notifications at the same boundary",
    ).toEqual([
      userText("hi"),
      assistantMessage(textBlock("waiting")),
      userText("change of plans"),
      systemNotificationMessage({
        type: "session_settled",
        session: { type: "command", id: "c9" },
        status: "completed",
        summary: "finished",
      }),
    ]);
    expectProviderValid(outcome.llm);
  });

  it("publishes metadata.hook_contexts as hook_context notifications drained at the next boundary (04.5 §11)", async () => {
    const inbox = new NotificationInbox();
    const tools = scriptedExecutor(
      [
        "probe",
        () =>
          Promise.resolve({
            content: "ran",
            metadata: { hook_contexts: ["lint config changed recently"] },
          }),
      ],
      ["submit", submitHandler(SUBMISSION)],
    );
    const { client, handle } = startMockRun(
      [
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_p", "probe")), "tool_use"),
        ]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools, notifications: inbox },
    );
    const outcome = asCompleted(await handle.outcome);
    expect(must(client.requests.at(0)).messages, "no early delivery").toEqual([
      userText("hi"),
    ]);
    expect(
      must(client.requests.at(1)).messages.at(-1),
      "the context lands after the tool result, at the next boundary",
    ).toEqual(
      systemNotificationMessage({
        type: "hook_context",
        tool_use_id: toolUseIdFrom("tu_p"),
        text: "lint config changed recently",
      }),
    );
    expectProviderValid(outcome.llm);
  });

  it("salvages an interrupted stream to displayed only and cancels with the reason (P03 §14.6)", async () => {
    const streamed = deferred();
    const { handle } = startMockRun([
      hangingTurn([textDelta("Hello, wo")], streamed),
    ]);
    const { events, done } = collectEvents(handle);
    await streamed.promise;
    handle.interrupt("user clicked stop");
    handle.interrupt("second call ignored");
    const outcome = asCancelled(await handle.outcome);
    expect(outcome.reason).toBe("user clicked stop");
    expect(outcome.turns).toBe(0);
    const partial = must(outcome.displayed.at(-1));
    expect(partial.partial).toBe("interrupted");
    expect(partial.message).toEqual(assistantMessage(textBlock("Hello, wo")));
    expect(outcome.llm).toEqual([userText("hi")]);
    await done;
    expect(must(events.at(-1)).type).toBe("run_finished");
    expectProviderValid(outcome.llm);
  });

  it("closes a cancelled batch with settled plus synthetic results in both lists (P03 §14.7)", async () => {
    const fastDone = deferred();
    const tools = scriptedExecutor(
      [
        "fast",
        () => {
          fastDone.resolve();
          return Promise.resolve({ content: "fast ok" });
        },
      ],
      ["slow", () => new Promise(() => undefined)],
    );
    const { handle } = startMockRun(
      [
        scriptedTurn([
          complete(
            assistantMessage(
              toolUseBlock("tu_fast", "fast"),
              toolUseBlock("tu_slow", "slow"),
            ),
            "tool_use",
          ),
        ]),
      ],
      { tools },
    );
    await fastDone.promise;
    await tick();
    handle.interrupt();
    const outcome = asCancelled(await handle.outcome);
    expect(outcome.reason).toBe("interrupted");
    const closing = {
      role: "user",
      content: [
        toolResultBlock("tu_fast", "fast ok"),
        toolResultBlock("tu_slow", "interrupted", true),
      ],
    };
    expect(must(outcome.llm.at(-1))).toEqual(closing);
    expect(must(outcome.displayed.at(-1)).message).toEqual(closing);
    expectProviderValid(outcome.llm);
  });

  it("fills results the executor dropped so history stays provider-valid (§15.21)", async () => {
    const dropping: ToolExecutor = {
      specs: () => [],
      executeBatch: (calls) =>
        Promise.resolve(
          calls
            .filter((call) => call.name === "submit")
            .map(
              (call): ToolCallResult => ({
                tool_use_id: call.tool_use_id,
                content: SUBMISSION,
                is_error: false,
                is_terminal: true,
                tool_start_time: Date.now(),
                tool_end_time: Date.now(),
              }),
            ),
        ),
    };
    const { client, handle } = startMockRun(
      [
        scriptedTurn([
          complete(
            assistantMessage(
              toolUseBlock("tu_a", "ghost_a"),
              toolUseBlock("tu_b", "ghost_b"),
            ),
            "tool_use",
          ),
        ]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools: dropping },
    );
    const outcome = asCompleted(await handle.outcome);
    expect(must(client.requests.at(1)).messages.at(-1)).toEqual({
      role: "user",
      content: [
        toolResultBlock("tu_a", "interrupted", true),
        toolResultBlock("tu_b", "interrupted", true),
      ],
    });
    expect(outcome.submission).toEqual(SUBMISSION);
    expectProviderValid(outcome.llm);
  });

  it("delivers a steer queued mid-run with the next provider request (P03 §14.8)", async () => {
    const batchStarted = deferred();
    const releaseBatch = deferred();
    const tools = scriptedExecutor(
      [
        "wait",
        () => {
          batchStarted.resolve();
          return releaseBatch.promise.then(() => ({ content: "done" }));
        },
      ],
      ["submit", submitHandler(SUBMISSION)],
    );
    const { client, handle } = startMockRun(
      [
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_w", "wait")), "tool_use"),
        ]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools },
    );
    await batchStarted.promise;
    expect(handle.steer(userText("also check Y"))).toBe(true);
    releaseBatch.resolve();
    const outcome = asCompleted(await handle.outcome);
    expect(must(client.requests.at(1)).messages).toEqual([
      userText("hi"),
      assistantMessage(toolUseBlock("tu_w", "wait")),
      { role: "user", content: [toolResultBlock("tu_w", "done")] },
      userText("also check Y"),
    ]);
    expect(outcome.llm).toContainEqual(userText("also check Y"));
    expectProviderValid(outcome.llm);
  });

  it("extends the run when a steer lands during a bare text turn (P03 §14.9)", async () => {
    const started = deferred();
    const release = deferred();
    const tools = scriptedExecutor(["submit", submitHandler(SUBMISSION)]);
    const { client, handle } = startMockRun(
      [
        gatedTurn(started, release.promise, [
          complete(assistantMessage(textBlock("first"))),
        ]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools },
    );
    await started.promise;
    expect(handle.steer(userText("one more"))).toBe(true);
    release.resolve();
    const outcome = asCompleted(await handle.outcome);
    expect(outcome.turns).toBe(2);
    expect(must(client.requests.at(1)).messages).toEqual([
      userText("hi"),
      assistantMessage(textBlock("first")),
      userText("one more"),
    ]);
    expectProviderValid(outcome.llm);
  });

  it("rejects a steer once finishing has begun and validates the role (P03 §14.10)", async () => {
    const tools = scriptedExecutor(["submit", submitHandler(SUBMISSION)]);
    const { handle } = startMockRun(
      [
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools },
    );
    const outcome = asCompleted(await handle.outcome);
    expect(handle.steer(userText("too late"))).toBe(false);
    expect(outcome.displayed).toHaveLength(3);
    expect(() => handle.steer(assistantMessage(textBlock("wrong role")))).toThrow(
      TypeError,
    );
  });

  it("fails with max_turns and drops a steer queued after the budget is spent (P03 §14.11)", async () => {
    const tools = scriptedExecutor(["echo", () => Promise.resolve({ content: "ok" })]);
    const started = deferred();
    const release = deferred();
    const { client, handle } = startMockRun(
      [
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_1", "echo")), "tool_use"),
        ]),
        gatedTurn(started, release.promise, [
          complete(assistantMessage(toolUseBlock("tu_2", "echo")), "tool_use"),
        ]),
      ],
      { tools, maxTurns: 2 },
    );
    await started.promise;
    expect(handle.steer(userText("late steer"))).toBe(true);
    release.resolve();
    const outcome = asFailed(await handle.outcome);
    expect(outcome.failure.kind).toBe("max_turns");
    expect(outcome.turns).toBe(2);
    expect(client.requests).toHaveLength(2);
    expect(outcome.llm).not.toContainEqual(userText("late steer"));
    expect(handle.steer(userText("post-finish"))).toBe(false);
    expectProviderValid(outcome.llm);
  });

  it("fails with provider_error and salvages pre-error deltas (P03 §14.12)", async () => {
    const { handle } = startMockRun([
      failingTurn(
        [textDelta("partial out")],
        new ProviderError("server", "upstream died", { status_code: 500 }),
      ),
    ]);
    const outcome = asFailed(await handle.outcome);
    expect(outcome.failure).toEqual({
      kind: "provider_error",
      message: "upstream died",
    });
    const partial = must(outcome.displayed.at(-1));
    expect(partial.partial).toBe("provider_error");
    expect(outcome.llm).toEqual([userText("hi")]);
    expectProviderValid(outcome.llm);
  });

  it("classifies an engine invariant violation as internal and still finishes (P03 §14.13)", async () => {
    const { handle } = startMockRun([scriptedTurn([textDelta("oops")])]);
    const { events, done } = collectEvents(handle);
    const outcome = asFailed(await handle.outcome);
    expect(outcome.failure.kind).toBe("internal");
    expect(outcome.failure.message).toContain(
      "provider stream ended without assistant completion",
    );
    await done;
    expect(must(events.at(-1)).type).toBe("run_finished");
    expectProviderValid(outcome.llm);
  });

  it("treats an external signal abort exactly like interrupt() (P03 §14.15)", async () => {
    const controller = new AbortController();
    const streamed = deferred();
    const { handle } = startMockRun(
      [hangingTurn([textDelta("Hi the")], streamed)],
      { signal: controller.signal },
    );
    await streamed.promise;
    controller.abort();
    const outcome = asCancelled(await handle.outcome);
    expect(outcome.reason).toBe("interrupted");
    expect(outcome.llm).toEqual([userText("hi")]);
    expectProviderValid(outcome.llm);
  });

  it("keeps running to completion after the consumer breaks early (P03 §14.16)", async () => {
    const tools = scriptedExecutor(["submit", submitHandler(SUBMISSION)]);
    const { handle } = startMockRun(
      [
        scriptedTurn([
          textDelta("a"),
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools },
    );
    const iterator = handle.events[Symbol.asyncIterator]();
    const first = await iterator.next();
    expect(first.done).toBe(false);
    await iterator.return?.();
    const outcome = asCompleted(await handle.outcome);
    expect(outcome.submission).toEqual(SUBMISSION);
    expectProviderValid(outcome.llm);
  });

  it("emits the golden event sequence with execution facts on completions (P03 §14.18)", async () => {
    const tools = scriptedExecutor(
      ["echo", () => Promise.resolve({ content: "echoed" })],
      ["submit", submitHandler(SUBMISSION)],
    );
    const { handle } = startMockRun(
      [
        scriptedTurn([
          textDelta("calling"),
          toolUseDelta("tu_1", "echo", { v: 1 }),
          complete(
            assistantMessage(textBlock("calling"), toolUseBlock("tu_1", "echo", { v: 1 })),
            "tool_use",
          ),
        ]),
        scriptedTurn([
          reasoningDelta("hmm"),
          textDelta("done"),
          complete(
            assistantMessage(textBlock("done"), toolUseBlock("tu_s", "submit")),
            "tool_use",
            { input_tokens: 7, output_tokens: 3 },
          ),
        ]),
      ],
      { tools },
    );
    const { events, done } = collectEvents(handle);
    const outcome = asCompleted(await handle.outcome);
    await done;
    expect(events.map((event) => event.type)).toEqual([
      "turn_started",
      "assistant_text_delta",
      "tool_use_delta",
      "assistant_message_complete",
      "tool_execution_started",
      "tool_execution_completed",
      "turn_started",
      "reasoning_delta",
      "assistant_text_delta",
      "assistant_message_complete",
      "tool_execution_started",
      "tool_execution_completed",
      "run_finished",
    ]);
    const completions = events.filter(
      (event) => event.type === "tool_execution_completed",
    );
    expect(must(completions.at(0))).toMatchObject({
      tool_use_id: "tu_1",
      is_error: false,
      is_terminal: false,
    });
    expect(must(completions.at(1))).toMatchObject({
      tool_use_id: "tu_s",
      is_terminal: true,
    });
    for (const [index, completion] of completions.entries()) {
      expect(
        completion.tool_end_time,
        `completion ${String(index)} carries the execute clock`,
      ).toBeGreaterThanOrEqual(completion.tool_start_time);
    }
    const last = must(events.at(-1));
    if (last.type === "run_finished") expect(last.outcome).toBe(outcome);
    expect(outcome.usage).toEqual({ input_tokens: 17, output_tokens: 8 });
    expectProviderValid(outcome.llm);
  });

  it("stringifies structured content exactly once at the tool_result projection (§15.20)", async () => {
    const structured = { files: ["a.ts", "b.ts"], count: 2 };
    const tools = scriptedExecutor(
      ["scan", () => Promise.resolve({ content: structured })],
      ["submit", submitHandler(SUBMISSION)],
    );
    const { client, handle } = startMockRun(
      [
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_1", "scan")), "tool_use"),
        ]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools },
    );
    const { events, done } = collectEvents(handle);
    const outcome = asCompleted(await handle.outcome);
    await done;
    const block = must(must(client.requests.at(1)).messages.at(-1)).content[0];
    if (block.type !== "tool_result") throw new Error("expected a tool_result");
    expect(block.content).toBe(JSON.stringify(structured));
    expect(
      JSON.parse(block.content),
      "single-encoded: parsing recovers the object, not a string",
    ).toEqual(structured);
    const completion = must(
      events.find((event) => event.type === "tool_execution_completed"),
    );
    expect(completion.output).toBe(JSON.stringify(structured));
    expect(outcome.submission, "the submission survives structured").toEqual(
      SUBMISSION,
    );
    expectProviderValid(outcome.llm);
  });

  it("leaves no abort listener on the run signal across repeated park/wake cycles", async () => {
    const inbox = new NotificationInbox();
    const supervisor = new BackgroundSessionSupervisor(inbox);
    const session = backgroundSessionHandle();
    supervisor.registerBackgroundSession(
      { type: "command", id: "c1" },
      session.handle,
    );
    const tools = scriptedExecutor(["submit", submitHandler(SUBMISSION)]);
    const { client, handle } = startMockRun(
      [
        scriptedTurn([complete(assistantMessage(textBlock("waiting")))]),
        scriptedTurn([complete(assistantMessage(textBlock("still waiting")))]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools, notifications: inbox, background: supervisor },
    );
    if (!(handle instanceof RunHandle)) throw new Error("expected a RunHandle");
    await tick();
    await tick();
    expect(client.requests, "first park").toHaveLength(1);
    expect(
      getEventListeners(handle.signal, "abort"),
      "parked waits register on a race-scoped signal, not the run signal",
    ).toHaveLength(0);
    handle.steer(userText("first wake"));
    await tick();
    await tick();
    expect(client.requests, "second park after the steer turn").toHaveLength(2);
    handle.steer(userText("second wake"));
    const outcome = asCompleted(await handle.outcome);
    expect(outcome.turns).toBe(3);
    expect(
      getEventListeners(handle.signal, "abort"),
      "no race loser survives the parks",
    ).toHaveLength(0);
  });

  it("tears down running sessions on finish without awaiting teardown (§15.22)", async () => {
    const inbox = new NotificationInbox();
    const supervisor = new BackgroundSessionSupervisor(inbox);
    const first = backgroundSessionHandle({ cancelMode: "hang" });
    const second = backgroundSessionHandle({ cancelMode: "hang" });
    supervisor.registerBackgroundSession(
      { type: "command", id: "c1" },
      first.handle,
    );
    supervisor.registerBackgroundSession(
      { type: "subagent", id: "r2" },
      second.handle,
    );
    const { client, handle } = startMockRun(
      [scriptedTurn([complete(assistantMessage(textBlock("waiting")))])],
      { notifications: inbox, background: supervisor },
    );
    await tick();
    await tick();
    expect(client.requests, "parked on the background sessions").toHaveLength(1);
    handle.interrupt("user stop");
    const outcome = asCancelled(await handle.outcome);
    expect(outcome.reason).toBe("user stop");
    await tick();
    expect(first.cancelled, "first session torn down").toEqual(["run finished"]);
    expect(second.cancelled, "second session torn down").toEqual(["run finished"]);
    const late = backgroundSessionHandle();
    supervisor.registerBackgroundSession(
      { type: "command", id: "late" },
      late.handle,
    );
    expect(late.cancelled, "the dispose latch cancels late registrations").toEqual([
      "run finished",
    ]);
  });

  it("rejects empty initialMessages with a TypeError", () => {
    const client = new MockLlmClient([]);
    expect(() =>
      startAgentRun({
        llmClient: client,
        tools: emptyExecutor(),
        model: "mock-model",
        initialMessages: [],
      }),
    ).toThrow(TypeError);
  });
});

describe("agent loop observer announcements (04.9 §4)", () => {
  it("awaits turnCompleted after every committed turn — text, single call, and batch — with exact facts", async () => {
    const tools = scriptedExecutor(
      ["echo", () => Promise.resolve({ content: "ok" })],
      ["submit", submitHandler(SUBMISSION)],
    );
    const { observer, calls } = recordingObserver();
    const { handle } = startMockRun(
      [
        scriptedTurn([complete(assistantMessage(textBlock("thinking")))]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_1", "echo")), "tool_use"),
        ]),
        scriptedTurn([
          complete(
            assistantMessage(toolUseBlock("tu_2", "echo"), toolUseBlock("tu_s", "submit")),
            "tool_use",
          ),
        ]),
      ],
      { tools, observer, maxTurns: 5 },
    );
    const outcome = asCompleted(await handle.outcome);
    expect(outcome.turns).toBe(3);
    expect(calls, "every turn shape announces, including the submitting batch").toEqual([
      {
        kind: "turnCompleted",
        facts: {
          turn: 1,
          maxTurns: 5,
          toolCalls: 0,
          backgroundSessionCount: 0,
          hasPendingSteers: false,
        },
      },
      {
        kind: "turnCompleted",
        facts: {
          turn: 2,
          maxTurns: 5,
          toolCalls: 1,
          backgroundSessionCount: 0,
          hasPendingSteers: false,
        },
      },
      {
        kind: "turnCompleted",
        facts: {
          turn: 3,
          maxTurns: 5,
          toolCalls: 2,
          backgroundSessionCount: 0,
          hasPendingSteers: false,
        },
      },
    ]);
  });

  it("holds the next provider call until turnCompleted resolves — awaited, not fire-and-forget", async () => {
    const release = deferred();
    const { observer } = recordingObserver((facts) =>
      facts.turn === 1 ? release.promise : undefined,
    );
    const tools = scriptedExecutor(["submit", submitHandler(SUBMISSION)]);
    const { client, handle } = startMockRun(
      [
        scriptedTurn([complete(assistantMessage(textBlock("first")))]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools, observer },
    );
    await tick();
    await tick();
    expect(
      client.requests,
      "the loop is parked inside the awaited turnCompleted",
    ).toHaveLength(1);
    release.resolve();
    const outcome = asCompleted(await handle.outcome);
    expect(outcome.turns).toBe(2);
  });

  it("drains a reminder published during turnCompleted before the next provider call (the spin case)", async () => {
    const inbox = new NotificationInbox();
    const reminder = systemNotificationMessage({
      type: "reminder",
      source: "TurnCompleted",
      text: "call your terminal tool",
    });
    const { observer } = recordingObserver((facts) => {
      if (facts.toolCalls === 0) inbox.publish(reminder);
    });
    const tools = scriptedExecutor(["submit", submitHandler(SUBMISSION)]);
    const { client, handle } = startMockRun(
      [
        scriptedTurn([complete(assistantMessage(textBlock("drifting")))]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools, notifications: inbox, observer },
    );
    const outcome = asCompleted(await handle.outcome);
    expect(
      must(client.requests.at(1)).messages,
      "the reminder is in the very next provider call",
    ).toEqual([userText("hi"), assistantMessage(textBlock("drifting")), reminder]);
    expectProviderValid(outcome.llm);
  });

  it("brackets the park with idleStarted/idleEnded and reports background sessions in the facts", async () => {
    const inbox = new NotificationInbox();
    const supervisor = new BackgroundSessionSupervisor(inbox);
    const session = backgroundSessionHandle();
    supervisor.registerBackgroundSession({ type: "command", id: "c1" }, session.handle);
    const tools = scriptedExecutor(["submit", submitHandler(SUBMISSION)]);
    const { observer, calls } = recordingObserver();
    const { handle } = startMockRun(
      [
        scriptedTurn([complete(assistantMessage(textBlock("waiting")))]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools, notifications: inbox, background: supervisor, observer, maxTurns: 4 },
    );
    await tick();
    await tick();
    expect(calls, "parked: announced and idle, not yet ended").toEqual([
      {
        kind: "turnCompleted",
        facts: {
          turn: 1,
          maxTurns: 4,
          toolCalls: 0,
          backgroundSessionCount: 1,
          hasPendingSteers: false,
        },
      },
      { kind: "idleStarted" },
    ]);
    session.settle({ status: "completed", summary: "done" });
    asCompleted(await handle.outcome);
    expect(
      calls.map((call) => call.kind),
      "the settlement wake ends the idle bracket before the next turn",
    ).toEqual(["turnCompleted", "idleStarted", "idleEnded", "turnCompleted"]);
  });

  it("calls idleEnded when an abort wakes the park", async () => {
    const inbox = new NotificationInbox();
    const supervisor = new BackgroundSessionSupervisor(inbox);
    const session = backgroundSessionHandle();
    supervisor.registerBackgroundSession({ type: "command", id: "c1" }, session.handle);
    const { observer, calls } = recordingObserver();
    const { handle } = startMockRun(
      [scriptedTurn([complete(assistantMessage(textBlock("waiting")))])],
      { notifications: inbox, background: supervisor, observer },
    );
    await tick();
    await tick();
    handle.interrupt("user stop");
    const outcome = asCancelled(await handle.outcome);
    expect(outcome.reason).toBe("user stop");
    expect(calls.map((call) => call.kind)).toEqual([
      "turnCompleted",
      "idleStarted",
      "idleEnded",
    ]);
  });

  it("skips the idle bracket when a steer is already pending at the boundary", async () => {
    const inbox = new NotificationInbox();
    const supervisor = new BackgroundSessionSupervisor(inbox);
    const session = backgroundSessionHandle();
    supervisor.registerBackgroundSession({ type: "command", id: "c1" }, session.handle);
    const started = deferred();
    const release = deferred();
    const tools = scriptedExecutor(["submit", submitHandler(SUBMISSION)]);
    const { observer, calls } = recordingObserver();
    const { handle } = startMockRun(
      [
        gatedTurn(started, release.promise, [
          complete(assistantMessage(textBlock("busy"))),
        ]),
        scriptedTurn([
          complete(assistantMessage(toolUseBlock("tu_s", "submit")), "tool_use"),
        ]),
      ],
      { tools, notifications: inbox, background: supervisor, observer },
    );
    await started.promise;
    expect(handle.steer(userText("redirect"))).toBe(true);
    release.resolve();
    asCompleted(await handle.outcome);
    expect(must(calls.at(0))).toMatchObject({
      kind: "turnCompleted",
      facts: { toolCalls: 0, backgroundSessionCount: 1, hasPendingSteers: true },
    });
    expect(
      calls.map((call) => call.kind),
      "no park, so no idle announcements",
    ).toEqual(["turnCompleted", "turnCompleted"]);
  });
});
