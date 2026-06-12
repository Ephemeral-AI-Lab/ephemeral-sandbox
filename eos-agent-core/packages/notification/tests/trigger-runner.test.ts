import { afterEach, describe, expect, it, vi } from "vitest";

import {
  agentRunIdFrom,
  sandboxIdFrom,
  type AgentRunSnapshot,
  type BackgroundSessionSnapshot,
} from "@eos/contracts";
import { eosAgentsPath } from "@eos/testkit";

import { NotificationInbox, systemNotificationMessage } from "../src/inbox.js";
import type { TurnFacts } from "../src/loop-observer.js";
import { NotificationTriggerEngine, runTriggerCommand } from "../src/trigger-runner.js";
import type {
  CommandScript,
  TriggerCommandRun,
  TriggerCommandRunner,
  TriggerPayload,
  TriggerRuleEntry,
} from "../src/triggers.js";

const SNAPSHOT: AgentRunSnapshot = {
  run_id: agentRunIdFrom("run-fixture"),
  kind: "main",
  agent_name: "main",
  sandbox_id: sandboxIdFrom("sb-fixture"),
  transcript_path: "/dev/null",
  workspace: { is_isolated: false },
};
const FACTS: TurnFacts = {
  turn: 3,
  maxTurns: 10,
  toolCalls: 0,
  backgroundSessionCount: 0,
  hasPendingSteers: false,
};

function command(name: string): CommandScript {
  return { type: "command", command: name };
}

function session(type: string, id: string): BackgroundSessionSnapshot {
  return { type, id, status: "running", started_at: "2026-06-11T00:00:00Z" };
}

function turnRule(...rules: CommandScript[]): TriggerRuleEntry {
  return { event: "TurnCompleted", rules };
}

function idleRule(timeoutMs: number, ...rules: CommandScript[]): TriggerRuleEntry {
  return { event: "IdleParked", timeout_ms: timeoutMs, rules };
}

function reminder(source: "TurnCompleted" | "IdleTimeout", text: string) {
  return systemNotificationMessage({ type: "reminder", source, text });
}

interface Fixture {
  engine: NotificationTriggerEngine;
  inbox: NotificationInbox;
  /** Mutable: what `listBackgroundSessions` answers at fire time. */
  sessions: BackgroundSessionSnapshot[];
  /** One entry per runCommand call: the command string and its payload. */
  ran: { command: string; payload: TriggerPayload }[];
}

/** An engine over a scripted runner: each command name maps to its answer. */
function fixture(
  rules: TriggerRuleEntry[],
  answers: Partial<Record<string, TriggerCommandRun | (() => Promise<TriggerCommandRun>)>> = {},
  terminalTool: string | null = "finish_task",
): Fixture {
  const inbox = new NotificationInbox();
  const sessions: BackgroundSessionSnapshot[] = [];
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
    listBackgroundSessions: () => [...sessions],
    runSnapshot: () => SNAPSHOT,
    terminalTool,
  });
  return { engine, inbox, sessions, ran };
}

describe("notification trigger engine", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("resolves immediately and executes nothing when no TurnCompleted rule is configured", async () => {
    const { engine, inbox, ran } = fixture([idleRule(1_000, command("idle"))]);
    await engine.turnCompleted(FACTS);
    expect(ran).toEqual([]);
    expect(inbox.drain()).toEqual([]);
  });

  it("runs each TurnCompleted command with the serialized payload and publishes the answer as a reminder", async () => {
    const { engine, inbox, sessions, ran } = fixture(
      [turnRule(command("remind"))],
      { remind: { notification: "wrap it up" } },
    );
    sessions.push(session("command", "c1"));
    await engine.turnCompleted(FACTS);
    expect(ran).toHaveLength(1);
    expect(ran[0].payload).toEqual({
      event: "TurnCompleted",
      facts: {
        turn: 3,
        max_turns: 10,
        tool_calls: 0,
        background_session_count: 0,
        has_pending_steers: false,
      },
      run: SNAPSHOT,
      terminal_tool: "finish_task",
      background_sessions: [session("command", "c1")],
    });
    expect(inbox.drain()).toEqual([reminder("TurnCompleted", "wrap it up")]);
  });

  it("carries terminal_tool null for a text-mode run, surviving JSON serialization (U15)", async () => {
    const { engine, ran } = fixture([turnRule(command("observe"))], {}, null);
    await engine.turnCompleted(FACTS);
    expect(ran).toHaveLength(1);
    expect(ran[0].payload.terminal_tool).toBeNull();
    const wire = JSON.parse(JSON.stringify(ran[0].payload)) as TriggerPayload;
    expect(
      wire.terminal_tool,
      "the field crosses the process boundary as an explicit null, never absent",
    ).toBeNull();
    expect("terminal_tool" in wire).toBe(true);
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
      explode: () => Promise.reject(new Error("execute machinery died")),
    });
    await expect(engine.turnCompleted(FACTS)).resolves.toBeUndefined();
    expect(warn).toHaveBeenCalledWith(
      "notification trigger (TurnCompleted): execute machinery died",
    );
    expect(inbox.drain()).toEqual([]);
  });

  it("fires an IdleParked rule only once the park outlives timeout_ms, with fire-time facts (T6)", async () => {
    vi.useFakeTimers();
    const { engine, inbox, sessions, ran } = fixture(
      [idleRule(1_000, command("idle"))],
      { idle: { notification: "still waiting" } },
    );
    engine.idleStarted();
    // The session registers after the park starts: fire time, not park time.
    sessions.push(session("subagent", "r1"));
    await vi.advanceTimersByTimeAsync(999);
    expect(ran, "no fire before the timeout").toEqual([]);
    await vi.advanceTimersByTimeAsync(1);
    expect(ran).toHaveLength(1);
    expect(ran[0].payload).toMatchObject({
      event: "IdleTimeout",
      facts: { idle_elapsed_ms: 1_000, timeout_ms: 1_000 },
      background_sessions: [session("subagent", "r1")],
    });
    expect(inbox.drain(), "the publish is the wake").toEqual([
      reminder("IdleTimeout", "still waiting"),
    ]);
  });

  it("never executes when a wake lands before the timeout (T6)", async () => {
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
    expect(ran, "the script is mid-execution").toHaveLength(1);
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

/** A trigger command spawning a shared `.eos-agents/tests/scripts` file. */
function scriptCommand(name: string, timeoutMs?: number): CommandScript {
  return {
    type: "command",
    command: `"${process.execPath}" "${eosAgentsPath("tests/scripts", name)}"`,
    ...(timeoutMs !== undefined && { timeout_ms: timeoutMs }),
  };
}

const PAYLOAD: TriggerPayload = {
  event: "TurnCompleted",
  facts: {
    turn: 1,
    max_turns: 4,
    tool_calls: 0,
    background_session_count: 0,
    has_pending_steers: false,
  },
  run: SNAPSHOT,
  terminal_tool: "submit_main_outcome",
  background_sessions: [],
};

describe("trigger command runner", () => {
  it("answers the notification from valid stdout, reading the payload as JSON on stdin", async () => {
    await expect(
      runTriggerCommand(scriptCommand("echo-trigger-event.cjs"), PAYLOAD),
    ).resolves.toEqual({
      notification: "TurnCompleted:submit_main_outcome",
    });
  });

  it.each`
    script                      | label
    ${"noop.cjs"}               | ${"empty stdout"}
    ${"print-empty-object.cjs"} | ${"an empty JSON object"}
  `("answers a skip (no notification, no warning) for $label", async ({ script }) => {
    await expect(
      runTriggerCommand(scriptCommand(script as string), PAYLOAD),
    ).resolves.toEqual({});
  });

  it("maps a nonzero exit to a warning carrying stderr - exit 2 has no deny semantics here", async () => {
    const run = await runTriggerCommand(scriptCommand("deny-with-stderr.cjs"), PAYLOAD);
    expect(run.notification).toBeUndefined();
    expect(run.warning).toContain("exited 2");
    expect(run.warning).toContain("blocked");
  });

  it("maps non-JSON stdout to a warning", async () => {
    const run = await runTriggerCommand(scriptCommand("garbage-stdout.cjs"), PAYLOAD);
    expect(run.notification).toBeUndefined();
    expect(run.warning).toContain("not JSON");
  });

  it.each`
    script                      | label
    ${"decision-deny.cjs"}      | ${"a decision field"}
    ${"update-input.cjs"}       | ${"an updatedInput field"}
    ${"notification-empty.cjs"} | ${"an empty notification"}
  `("rejects $label as a schema mismatch, never accepting or stripping it", async ({ script }) => {
    const run = await runTriggerCommand(scriptCommand(script as string), PAYLOAD);
    expect(run.notification).toBeUndefined();
    expect(run.warning).toContain("did not match TriggerOutput");
  });

  it("kills a command on its timeout and maps it to a warning", { timeout: 10_000 }, async () => {
    const run = await runTriggerCommand(scriptCommand("hang.cjs", 250), PAYLOAD);
    expect(run.notification).toBeUndefined();
    expect(run.warning).toBe("trigger command timed out");
  });
});
