import { afterEach, describe, expect, it, vi } from "vitest";

import {
  BackgroundSupervisor,
  NotificationInbox,
  systemNotificationMessage,
  type TurnFacts,
} from "@eos/engine";
import { scriptedRunState, scriptedSessionHandle } from "@eos/testkit";
import {
  ToolNameSchema,
  snapshotRunState,
  type TriggerCommand,
  type TriggerCommandRun,
  type TriggerCommandRunner,
  type TriggerPayload,
  type TriggerRuleEntry,
} from "@eos/tool";

import { NotificationTriggerEngine } from "../src/notification-trigger-engine.js";

const TERMINAL = ToolNameSchema.parse("finish_task");
const SNAPSHOT = snapshotRunState(scriptedRunState("main"));
const FACTS: TurnFacts = {
  turn: 3,
  maxTurns: 10,
  toolCalls: 0,
  liveSessions: 0,
  hasPendingSteers: false,
};

function command(name: string): TriggerCommand {
  return { type: "command", command: name };
}

function turnRule(...hooks: TriggerCommand[]): TriggerRuleEntry {
  return { event: "TurnCompleted", hooks };
}

function idleRule(timeoutMs: number, ...hooks: TriggerCommand[]): TriggerRuleEntry {
  return { event: "IdleParked", timeout_ms: timeoutMs, hooks };
}

function reminder(source: "TurnCompleted" | "IdleTimeout", text: string) {
  return systemNotificationMessage({ type: "reminder", source, text });
}

interface Fixture {
  engine: NotificationTriggerEngine;
  inbox: NotificationInbox;
  supervisor: BackgroundSupervisor;
  /** One entry per runCommand call: the command string and its payload. */
  ran: { command: string; payload: TriggerPayload }[];
}

/** An engine over a scripted runner: each command name maps to its answer. */
function fixture(
  rules: TriggerRuleEntry[],
  answers: Partial<Record<string, TriggerCommandRun | (() => Promise<TriggerCommandRun>)>> = {},
): Fixture {
  const inbox = new NotificationInbox();
  const supervisor = new BackgroundSupervisor(inbox);
  const ran: Fixture["ran"] = [];
  const runCommand: TriggerCommandRunner = (cmd, payload) => {
    ran.push({ command: cmd.command, payload });
    const answer = answers[cmd.command];
    if (answer === undefined) return Promise.resolve({});
    return typeof answer === "function" ? answer() : Promise.resolve(answer);
  };
  const engine = new NotificationTriggerEngine({
    rules,
    runCommand,
    inbox,
    supervisor,
    runSnapshot: () => SNAPSHOT,
    terminalTool: TERMINAL,
  });
  return { engine, inbox, supervisor, ran };
}

describe("notification trigger engine", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("resolves immediately and spawns nothing when no TurnCompleted rule is configured", async () => {
    const { engine, inbox, ran } = fixture([idleRule(1_000, command("idle"))]);
    await engine.turnCompleted(FACTS);
    expect(ran).toEqual([]);
    expect(inbox.drain()).toEqual([]);
  });

  it("runs each TurnCompleted command with the serialized payload and publishes the answer as a reminder", async () => {
    const { engine, inbox, supervisor, ran } = fixture(
      [turnRule(command("remind"))],
      { remind: { notification: "wrap it up" } },
    );
    supervisor.register({ type: "command", id: "c1" }, scriptedSessionHandle().handle);
    await engine.turnCompleted(FACTS);
    expect(ran).toHaveLength(1);
    expect(ran[0].payload).toEqual({
      event: "TurnCompleted",
      facts: {
        turn: 3,
        max_turns: 10,
        tool_calls: 0,
        live_sessions: 0,
        has_pending_steers: false,
      },
      run: SNAPSHOT,
      terminal_tool: "finish_task",
      background_sessions: [
        {
          type: "command",
          id: "c1",
          status: "running",
          started_at: expect.any(String) as string,
        },
      ],
    });
    expect(inbox.drain()).toEqual([reminder("TurnCompleted", "wrap it up")]);
  });

  it("publishes answers from multiple rules in config order", async () => {
    const { engine, inbox } = fixture(
      [turnRule(command("first")), turnRule(command("second"))],
      {
        first: { notification: "one" },
        second: { notification: "two" },
      },
    );
    await engine.turnCompleted(FACTS);
    expect(inbox.drain()).toEqual([
      reminder("TurnCompleted", "one"),
      reminder("TurnCompleted", "two"),
    ]);
  });

  it("skips publishing on an empty answer", async () => {
    const { engine, inbox } = fixture([turnRule(command("quiet"))], { quiet: {} });
    await engine.turnCompleted(FACTS);
    expect(inbox.drain()).toEqual([]);
  });

  it("logs and drops a warning answer while sibling answers still publish (T9)", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const { engine, inbox } = fixture(
      [turnRule(command("broken"), command("fine"))],
      {
        broken: { warning: "stdout was not JSON" },
        fine: { notification: "still speaking" },
      },
    );
    await engine.turnCompleted(FACTS);
    expect(warn).toHaveBeenCalledWith(
      "notification trigger (TurnCompleted): stdout was not JSON",
    );
    expect(inbox.drain(), "the failure dropped only its own firing").toEqual([
      reminder("TurnCompleted", "still speaking"),
    ]);
  });

  it("never rejects even when the injected runner does (T9)", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const { engine, inbox } = fixture([turnRule(command("explode"))], {
      explode: () => Promise.reject(new Error("spawn machinery died")),
    });
    await expect(engine.turnCompleted(FACTS)).resolves.toBeUndefined();
    expect(warn).toHaveBeenCalledWith(
      "notification trigger (TurnCompleted): spawn machinery died",
    );
    expect(inbox.drain()).toEqual([]);
  });

  it("fires an IdleParked rule only once the park outlives timeout_ms, with fire-time facts (T6)", async () => {
    vi.useFakeTimers();
    const { engine, inbox, supervisor, ran } = fixture(
      [idleRule(1_000, command("idle"))],
      { idle: { notification: "still waiting" } },
    );
    engine.idleStarted();
    // The session registers after the park starts: fire time, not park time.
    supervisor.register({ type: "subagent", id: "r1" }, scriptedSessionHandle().handle);
    await vi.advanceTimersByTimeAsync(999);
    expect(ran, "no fire before the timeout").toEqual([]);
    await vi.advanceTimersByTimeAsync(1);
    expect(ran).toHaveLength(1);
    expect(ran[0].payload).toMatchObject({
      event: "IdleTimeout",
      facts: { idle_elapsed_ms: 1_000, timeout_ms: 1_000 },
      background_sessions: [{ type: "subagent", id: "r1", status: "running" }],
    });
    expect(inbox.drain(), "the publish is the wake").toEqual([
      reminder("IdleTimeout", "still waiting"),
    ]);
  });

  it("never spawns when a wake lands before the timeout (T6)", async () => {
    vi.useFakeTimers();
    const { engine, ran } = fixture([idleRule(1_000, command("idle"))]);
    engine.idleStarted();
    await vi.advanceTimersByTimeAsync(500);
    engine.idleEnded();
    await vi.advanceTimersByTimeAsync(5_000);
    expect(ran).toEqual([]);
  });

  it("discards a fire that loses the race to a natural wake (generation guard, T7)", async () => {
    vi.useFakeTimers();
    let settle!: (run: TriggerCommandRun) => void;
    const { engine, inbox, ran } = fixture([idleRule(1_000, command("idle"))], {
      idle: () =>
        new Promise<TriggerCommandRun>((resolve) => {
          settle = resolve;
        }),
    });
    engine.idleStarted();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(ran, "the script is mid-spawn").toHaveLength(1);
    engine.idleEnded();
    settle({ notification: "stale answer" });
    await vi.advanceTimersByTimeAsync(0);
    expect(inbox.drain(), "the stale answer is discarded, never published").toEqual([]);
  });

  it("re-arms on re-park: one shot per park entry, repeated reminders across cycles (T8)", async () => {
    vi.useFakeTimers();
    const { engine, inbox, ran } = fixture([idleRule(1_000, command("idle"))], {
      idle: { notification: "still waiting" },
    });
    engine.idleStarted();
    await vi.advanceTimersByTimeAsync(6_000);
    expect(ran, "one shot per park: no repeat within the same park").toHaveLength(1);
    engine.idleEnded();
    engine.idleStarted();
    await vi.advanceTimersByTimeAsync(1_000);
    engine.idleEnded();
    expect(ran, "the re-park re-armed the timer").toHaveLength(2);
    expect(inbox.drain()).toEqual([
      reminder("IdleTimeout", "still waiting"),
      reminder("IdleTimeout", "still waiting"),
    ]);
  });
});
