import { readdirSync } from "node:fs";
import { dirname, join } from "node:path";

import { describe, expect, it, vi } from "vitest";

import { toolUseIdFrom } from "@eos/contracts";
import { systemNotificationMessage } from "@eos/notification";
import type { LlmClient } from "@eos/llm-client";
import { terminalToolDefinitions, type ToolDefinition } from "@eos/tool";
import { eosAgentsPath, scriptedTool } from "@eos/testkit";

import { createAgentRuntime, type AgentRuntime } from "../src/runtime.js";
import { runTranscriptPath } from "../src/transcript.js";
import {
  MockLlmClient,
  asString,
  assistantMessage,
  complete,
  dynamicTurn,
  gatedTurn,
  hangingTurn,
  lastToolResult,
  lastToolResultJson,
  llmRegistry,
  must,
  readTranscriptLines,
  readEventLines,
  readResultLines,
  scriptedTurn,
  tempDir,
  textBlock,
  toolUseBlock,
  userMessage,
} from "./support.js";

/** Every llm_client_id a `.eos-agents/tests/profile/<group>` directory names. */
const PROFILE_GROUP_CLIENT_IDS = {
  core: [
    "root_llm",
    "root2_llm",
    "helper_llm",
    "advisor_llm",
    "asker_llm",
    "unsafe_llm",
    "researcher_llm",
    "scout_llm",
  ],
  sandbox: ["worker_llm", "helper_llm"],
  scribe: ["scribe_llm"],
  "missing-client": [],
} as const;

interface FixtureOptions {
  /** A checked-in `.eos-agents/tests/profile/<group>` directory. */
  profileGroup?: keyof typeof PROFILE_GROUP_CLIENT_IDS;
  /** Scripted clients; the group's other ids get an empty mock. */
  clients?: Record<string, LlmClient>;
  baseTools?: ToolDefinition[];
  hookConfigPath?: string;
  notificationRulesPath?: string;
}

function runtimeFixture(options: FixtureOptions): {
  runtime: AgentRuntime;
  dataDir: string;
} {
  const group = options.profileGroup ?? "core";
  const clients: Record<string, LlmClient> = {};
  for (const id of PROFILE_GROUP_CLIENT_IDS[group]) clients[id] = new MockLlmClient([]);
  Object.assign(clients, options.clients);
  const dataDir = join(tempDir("eos-runtime-"), "data");
  const runtime = createAgentRuntime({
    agentProfilesDir: eosAgentsPath("tests/profile", group),
    llmClients: llmRegistry(clients),
    baseTools: options.baseTools,
    hookConfigPath: options.hookConfigPath ?? eosAgentsPath("tests/hooks/none.json"),
    notificationRulesPath:
      options.notificationRulesPath ??
      eosAgentsPath("tests/notification-rules/none.json"),
    dataDir,
    pursuitScriptsDir: eosAgentsPath("tests/pursuit/scripts"),
  });
  return { runtime, dataDir };
}

async function waitForFinished(runtime: AgentRuntime, agentName: string): Promise<void> {
  await vi.waitFor(() => {
    const entry = runtime.listRuns().find((run) => run.agent_name === agentName);
    expect(entry?.status).toBe("finished");
  });
}

describe("agent runtime", () => {
  it("wires §4 in order: profile-selected tools (the worker's ask_advisor included) reach one engine run that drains a notification (§13.3)", async () => {
    const sandboxNames = [
      "read",
      "multi_read",
      "write",
      "edit",
      "exec_command",
      "command_stdin",
      "read_command_transcript",
    ];
    const baseTools = sandboxNames.map((name) =>
      scriptedTool({ name, execute: () => Promise.resolve({ content: name }) }),
    );
    const helperClient = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_hs", "submit_subagent_outcome", {
              summary: "smoke helper done",
            }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const client = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_spawn", "run_subagent", {
              agent_name: "helper",
              prompt: "smoke",
            }),
          ),
          "tool_use",
        ),
      ]),
      scriptedTurn([complete(assistantMessage(textBlock("waiting")))]),
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_s", "submit_worker_outcome", {
              summary: "done",
              is_pass: true,
              outcome: "done",
            }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const { runtime } = runtimeFixture({
      profileGroup: "sandbox",
      clients: { worker_llm: client, helper_llm: helperClient },
      baseTools,
    });

    const run = runtime.startRun({
      agentName: "worker",
      initialMessages: [userMessage("work the item")],
    });
    expect(runtime.listRuns(), "registration is atomic and immediate").toEqual([
      expect.objectContaining({
        run_id: run.runId,
        agent_name: "worker",
        agent_kind: "worker",
        status: "running",
      }),
    ]);
    expect(
      () => run.handle.events[Symbol.asyncIterator](),
      "no second event surface this phase (§2.5)",
    ).toThrow(/single consumer/);

    const outcome = await run.handle.outcome;
    expect(outcome.status).toBe("completed");
    const request = must(client.requests.at(0));
    expect(
      request.tools.map((tool) => tool.name),
      "exactly allowed_tools + terminal_tool, sorted",
    ).toEqual([
      "ask_advisor",
      "cancel_background_session",
      "command_stdin",
      "edit",
      "exec_command",
      "list_background_sessions",
      "multi_read",
      "read",
      "read_command_transcript",
      "run_subagent",
      "submit_worker_outcome",
      "write",
    ]);
    expect(request.model).toBe("model-worker_llm");
    expect(request.reasoning_effort).toBe("low");
    expect(request.system_prompt).toContain("You are worker");
    expect(request.messages).toEqual([userMessage("work the item")]);

    const helper = must(runtime.listRuns().find((r) => r.agent_name === "helper"));
    expect(
      client.requests.flatMap((r) => r.messages),
      "the per-run inbox drains the settlement into the conversation",
    ).toContainEqual(
      systemNotificationMessage({
        type: "session_settled",
        session: { type: "subagent", id: helper.run_id },
        status: "completed",
        summary: "smoke helper done",
      }),
    );

    await waitForFinished(runtime, "worker");
    expect(
      readTranscriptLines(run.transcriptPath).map((line) => line.kind),
      "the smoke run leaves an ordered transcript",
    ).toEqual([
      "user",
      "assistant",
      "tool_result",
      "assistant",
      "assistant",
      "tool_result",
      "run_finished",
    ]);
  });

  it("rejects baseTools whose names collide with a runtime tool family (§4)", () => {
    expect(() =>
      runtimeFixture({
        baseTools: [
          scriptedTool({
            name: "run_subagent",
            execute: () => Promise.resolve({ content: "shadow" }),
          }),
        ],
      }),
    ).toThrow('baseTools name "run_subagent" collides');
  });

  it("fails createAgentRuntime when a profile references a missing llm client id (§13.2)", () => {
    expect(() => runtimeFixture({ profileGroup: "missing-client" })).toThrow(
      'unknown llm client id "ghost_llm"',
    );
  });

  it("rejects starting a non-advisor profile that selects advisory tools without ask_advisor", () => {
    const { runtime } = runtimeFixture({});
    expect(() =>
      runtime.startRun({
        agentName: "unsafe_worker",
        initialMessages: [userMessage("finish")],
      }),
    ).toThrow(
      'profile "unsafe_worker" selects advisory-required tools but cannot call ask_advisor',
    );
  });

  it("carries the terminal submission onto the outcome and the run_finished line (§13.4)", async () => {
    const client = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(
            textBlock("submitting"),
            toolUseBlock("tu_s", "submit_main_outcome", {
              summary: "shipped",
              payload: { n: 1 },
            }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const { runtime } = runtimeFixture({ clients: { root_llm: client } });
    const run = runtime.startRun({
      agentName: "root",
      initialMessages: [userMessage("go")],
    });
    const outcome = await run.handle.outcome;
    if (outcome.status !== "completed") throw new Error("expected completion");
    expect(outcome.submission).toEqual({ summary: "shipped", payload: { n: 1 } });

    await waitForFinished(runtime, "root");
    expect(must(readTranscriptLines(run.transcriptPath).at(-1))).toMatchObject({
      kind: "run_finished",
      outcome_status: "completed",
      submission: { summary: "shipped", payload: { n: 1 } },
    });
  });

  it("completes a no-terminal run by text over the mock client (U13)", async () => {
    const client = new MockLlmClient([
      scriptedTurn([complete(assistantMessage(textBlock("research summary: 42")))]),
    ]);
    const { runtime } = runtimeFixture({ clients: { researcher_llm: client } });
    const run = runtime.startRun({
      agentName: "researcher",
      initialMessages: [userMessage("find the answer")],
    });
    const outcome = await run.handle.outcome;
    if (outcome.status !== "completed") throw new Error("expected completion");
    expect(outcome.submission, "the final text rides the outcome").toBe(
      "research summary: 42",
    );
    expect(
      must(client.requests.at(0)).tools.map((tool) => tool.name),
      "no submit_* spec is exposed to the model",
    ).toEqual(["read_agent_run_transcript"]);

    await waitForFinished(runtime, "researcher");
    expect(
      readResultLines(join(dirname(run.transcriptPath), "result.jsonl")),
      "the rollup records the text completion",
    ).toEqual([expect.objectContaining({ run_id: run.runId, status: "completed" })]);
  });

  it("delivers a text child's settlement summary verbatim to the parent (U14)", async () => {
    let releaseScout!: () => void;
    const gate = new Promise<void>((resolve) => (releaseScout = resolve));
    const scoutClient = new MockLlmClient([
      gatedTurn(gate, [
        complete(assistantMessage(textBlock("the codeword is zebra-7"))),
      ]),
    ]);
    const rootClient = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_spawn", "run_subagent", {
              agent_name: "scout",
              prompt: "find the codeword",
            }),
          ),
          "tool_use",
        ),
      ]),
      scriptedTurn([complete(assistantMessage(textBlock("waiting on the scout")))]),
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_s", "submit_main_outcome", { summary: "done" }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const { runtime } = runtimeFixture({
      clients: { root_llm: rootClient, scout_llm: scoutClient },
    });

    const root = runtime.startRun({
      agentName: "root",
      initialMessages: [userMessage("delegate the lookup")],
    });
    await vi.waitFor(() => {
      expect(rootClient.requests, "the parent parked on the scout").toHaveLength(2);
    });
    releaseScout();
    const outcome = await root.handle.outcome;
    expect(outcome.status).toBe("completed");

    const scout = must(runtime.listRuns().find((run) => run.agent_name === "scout"));
    expect(
      must(rootClient.requests.at(2)).messages.at(-1),
      "the settlement summary is the child's text verbatim, unquoted",
    ).toEqual(
      systemNotificationMessage({
        type: "session_settled",
        session: { type: "subagent", id: scout.run_id },
        status: "completed",
        summary: "the codeword is zebra-7",
      }),
    );
  });

  it("runs the subagent round-trip: start by name, park, settle notification, transcript read, submit (§13.5)", async () => {
    let releaseHelper!: () => void;
    const gate = new Promise<void>((resolve) => (releaseHelper = resolve));
    const helperClient = new MockLlmClient([
      gatedTurn(gate, [
        complete(
          assistantMessage(
            toolUseBlock("tu_hs", "submit_subagent_outcome", {
              summary: "helper done",
              payload: { n: 42 },
            }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const rootClient = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_spawn", "run_subagent", {
              agent_name: "helper",
              prompt: "compute the answer",
            }),
          ),
          "tool_use",
        ),
      ]),
      scriptedTurn([complete(assistantMessage(textBlock("waiting on the helper")))]),
      dynamicTurn((request) => [
        complete(
          assistantMessage(
            toolUseBlock("tu_read", "read_agent_run_transcript", {
              run_id: asString(lastToolResultJson(request).run_id),
            }),
          ),
          "tool_use",
        ),
      ]),
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_s", "submit_main_outcome", { summary: "all done" }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const { runtime } = runtimeFixture({
      clients: { root_llm: rootClient, helper_llm: helperClient },
    });

    const root = runtime.startRun({
      agentName: "root",
      initialMessages: [userMessage("delegate the work")],
    });
    await vi.waitFor(() => {
      expect(rootClient.requests, "bare text turn reached").toHaveLength(2);
    });
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(
      rootClient.requests,
      "the caller parks on the background session instead of spinning",
    ).toHaveLength(2);

    releaseHelper();
    const outcome = await root.handle.outcome;
    expect(outcome.status).toBe("completed");

    const helper = must(
      runtime.listRuns().find((run) => run.agent_name === "helper"),
    );
    expect(helper.parent, "the caller is the parent link").toBe(root.runId);
    expect(helper.agent_kind).toBe("subagent");

    expect(
      must(rootClient.requests.at(2)).messages.at(-1),
      "the settlement notification wakes and reaches the next request",
    ).toEqual(
      systemNotificationMessage({
        type: "session_settled",
        session: { type: "subagent", id: helper.run_id },
        status: "completed",
        summary: "helper done",
      }),
    );

    const read = lastToolResultJson(must(rootClient.requests.at(3)));
    expect(read.eof, "the subagent transcript is fully flushed at read").toBe(true);
    const transcript = asString(read.transcript);
    expect(transcript).toContain('"run_finished"');
    expect(transcript).toContain("helper done");
  });

  it("answers ask_advisor with the advisor submission over transcript evidence (§13.6)", async () => {
    const advisorClient = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_as", "submit_advisor_outcome", {
              summary: "approve",
              payload: {
                verdict: "pass",
                tool_name: "submit_worker_outcome",
                payload: { summary: "done", is_pass: true, outcome: "done" },
                reason: "matches the transcript",
              },
            }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const askerClient = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_ask", "ask_advisor", {
              tool_name: "submit_worker_outcome",
              payload: { summary: "done", is_pass: true, outcome: "done" },
            }),
          ),
          "tool_use",
        ),
      ]),
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_s", "submit_worker_outcome", {
              summary: "done",
              is_pass: true,
              outcome: "done",
            }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const { runtime } = runtimeFixture({
      clients: { asker_llm: askerClient, advisor_llm: advisorClient },
    });

    const asker = runtime.startRun({
      agentName: "asker",
      initialMessages: [userMessage("finish the item")],
    });
    const outcome = await asker.handle.outcome;
    expect(outcome.status).toBe("completed");

    expect(
      lastToolResultJson(must(askerClient.requests.at(1))),
      "the advisor submission is the tool result",
    ).toEqual({
      summary: "approve",
      payload: {
        verdict: "pass",
        tool_name: "submit_worker_outcome",
        payload: { summary: "done", is_pass: true, outcome: "done" },
        reason: "matches the transcript",
      },
    });

    const advisorRequest = must(advisorClient.requests.at(0));
    expect(
      advisorRequest.messages.map((message) => message.role),
      "transcript evidence and instruction stay separable user messages (§2.9)",
    ).toEqual(["user", "user"]);
    const [evidence, instruction] = advisorRequest.messages;
    const evidenceText = evidence.content[0];
    if (evidenceText.type !== "text") throw new Error("expected text evidence");
    expect(
      evidenceText.text,
      "the caller transcript includes the in-flight ask_advisor call",
    ).toContain('"ask_advisor"');
    const workerSubmission = must(
      terminalToolDefinitions().find(
        (definition) => definition.name === "submit_worker_outcome",
      ),
    );
    const advisorPrompt = must(workerSubmission.advisorPrompt);
    expect(instruction).toEqual(
      userMessage(
        `${advisorPrompt} Please verify against the below tool name + payload\n{"payload":{"is_pass":true,"outcome":"done","summary":"done"},"tool_name":"submit_worker_outcome"}`,
      ),
    );

    const advisorRun = must(
      runtime.listRuns().find((run) => run.agent_name === "advisor"),
    );
    expect(advisorRun.parent).toBe(asker.runId);
  });

  it("cancels the advisor run when the caller aborts mid-ask (§13.6)", async () => {
    let advisorStarted!: () => void;
    const started = new Promise<void>((resolve) => (advisorStarted = resolve));
    const advisorClient = new MockLlmClient([hangingTurn(advisorStarted)]);
    const askerClient = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_ask", "ask_advisor", {
              tool_name: "submit_worker_outcome",
              payload: { summary: "done", is_pass: true, outcome: "done" },
            }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const { runtime, dataDir } = runtimeFixture({
      clients: { asker_llm: askerClient, advisor_llm: advisorClient },
    });

    const asker = runtime.startRun({
      agentName: "asker",
      initialMessages: [userMessage("finish the item")],
    });
    await started;
    asker.handle.interrupt("stop");
    const outcome = await asker.handle.outcome;
    if (outcome.status !== "cancelled") throw new Error("expected cancellation");
    expect(outcome.reason).toBe("stop");

    await waitForFinished(runtime, "advisor");
    const advisorRun = must(
      runtime.listRuns().find((run) => run.agent_name === "advisor"),
    );
    expect(
      must(readTranscriptLines(runTranscriptPath(dataDir, advisorRun.run_id)).at(-1)),
      "the advisor run dies with the caller's abort",
    ).toMatchObject({
      kind: "run_finished",
      outcome_status: "cancelled",
      interrupt_reason: "interrupted",
    });
  });

  it("cascades disposal: interrupting the caller cancels the background subagent as caller_disposed (§13.7)", async () => {
    let helperStarted!: () => void;
    const started = new Promise<void>((resolve) => (helperStarted = resolve));
    const helperClient = new MockLlmClient([hangingTurn(helperStarted)]);
    const rootClient = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_spawn", "run_subagent", {
              agent_name: "helper",
              prompt: "never finishes",
            }),
          ),
          "tool_use",
        ),
      ]),
      scriptedTurn([complete(assistantMessage(textBlock("waiting")))]),
    ]);
    const { runtime, dataDir } = runtimeFixture({
      clients: { root_llm: rootClient, helper_llm: helperClient },
    });

    const root = runtime.startRun({
      agentName: "root",
      initialMessages: [userMessage("delegate")],
    });
    await started;
    root.handle.interrupt("user stop");
    const outcome = await root.handle.outcome;
    if (outcome.status !== "cancelled") throw new Error("expected cancellation");
    expect(outcome.reason).toBe("user stop");

    await waitForFinished(runtime, "root");
    await waitForFinished(runtime, "helper");
    const helper = must(runtime.listRuns().find((run) => run.agent_name === "helper"));
    expect(
      must(readTranscriptLines(runTranscriptPath(dataDir, helper.run_id)).at(-1)),
      "the engine-triggered dispose cascades through the session handle",
    ).toMatchObject({
      kind: "run_finished",
      outcome_status: "cancelled",
      interrupt_reason: "caller_disposed",
    });
  });

  it("records model_cancelled when cancel_background_session stops a subagent (§8)", async () => {
    const helperClient = new MockLlmClient([hangingTurn()]);
    const rootClient = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_spawn", "run_subagent", {
              agent_name: "helper",
              prompt: "obsolete work",
            }),
          ),
          "tool_use",
        ),
      ]),
      dynamicTurn((request) => [
        complete(
          assistantMessage(
            toolUseBlock("tu_cancel", "cancel_background_session", {
              type: "subagent",
              id: asString(lastToolResultJson(request).run_id),
              reason: "no longer needed",
            }),
          ),
          "tool_use",
        ),
      ]),
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_s", "submit_main_outcome", { summary: "done" }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const { runtime, dataDir } = runtimeFixture({
      clients: { root_llm: rootClient, helper_llm: helperClient },
    });

    const root = runtime.startRun({
      agentName: "root",
      initialMessages: [userMessage("delegate, then change course")],
    });
    const outcome = await root.handle.outcome;
    expect(outcome.status, "the cancelled session unblocks the submission").toBe(
      "completed",
    );

    await waitForFinished(runtime, "helper");
    const helper = must(runtime.listRuns().find((run) => run.agent_name === "helper"));
    expect(
      must(readTranscriptLines(runTranscriptPath(dataDir, helper.run_id)).at(-1)),
      "a model-initiated cancel is distinguishable from the disposal cascade",
    ).toMatchObject({
      kind: "run_finished",
      outcome_status: "cancelled",
      interrupt_reason: "model_cancelled",
    });
  });

  it("rejects starting a main profile from inside a run (§4)", async () => {
    const rootClient = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_spawn", "run_subagent", {
              agent_name: "root2",
              prompt: "be primary",
            }),
          ),
          "tool_use",
        ),
      ]),
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_s", "submit_main_outcome", { summary: "done" }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const { runtime } = runtimeFixture({ clients: { root_llm: rootClient } });
    const root = runtime.startRun({
      agentName: "root",
      initialMessages: [userMessage("go")],
    });
    const outcome = await root.handle.outcome;
    expect(outcome.status).toBe("completed");
    const result = lastToolResult(must(rootClient.requests.at(1)));
    expect(result.is_error).toBe(true);
    expect(result.content).toContain("main profiles can only be started externally");
  });

  it("lets a real spawned hook gate a tool on transcript contents and republishes its context (§13.8)", async () => {
    const baseTools = [
      scriptedTool({
        name: "read_note",
        execute: () => Promise.resolve({ content: "the note says 42" }),
      }),
      scriptedTool({
        name: "write_note",
        execute: () => Promise.resolve({ content: "wrote" }),
      }),
    ];
    const client = new MockLlmClient([
      scriptedTurn([
        complete(
          assistantMessage(toolUseBlock("tu_w1", "write_note", { text: "first" })),
          "tool_use",
        ),
      ]),
      scriptedTurn([
        complete(assistantMessage(toolUseBlock("tu_r", "read_note", {})), "tool_use"),
      ]),
      scriptedTurn([
        complete(
          assistantMessage(toolUseBlock("tu_w2", "write_note", { text: "second" })),
          "tool_use",
        ),
      ]),
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_s", "submit_worker_outcome", {
              summary: "noted",
              is_pass: true,
              outcome: "noted",
            }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const { runtime } = runtimeFixture({
      profileGroup: "scribe",
      clients: { scribe_llm: client },
      baseTools,
      hookConfigPath: eosAgentsPath("tests/hooks/write-note-gate.json"),
    });

    const run = runtime.startRun({
      agentName: "scribe",
      initialMessages: [userMessage("update the note")],
    });
    const outcome = await run.handle.outcome;
    expect(outcome.status).toBe("completed");

    const denied = lastToolResult(must(client.requests.at(1)));
    expect(denied.is_error, "read-before-write: the first write is denied").toBe(true);
    expect(denied.content).toContain("write_note requires reading the note first");

    const allowed = lastToolResult(must(client.requests.at(3)));
    expect(allowed.is_error, "the write after the read passes the hook").toBe(false);

    expect(
      must(client.requests.at(3)).messages.at(-1),
      "the hook's additionalContext reaches the conversation as a hook_context notification at the next boundary (decision 11)",
    ).toEqual(
      systemNotificationMessage({
        type: "hook_context",
        tool_use_id: toolUseIdFrom("tu_w2"),
        text: "note was read before writing",
      }),
    );
  });

  it("reminds once per consecutive bare-text turn and drains same-boundary rule answers as separate messages (04.9)", async () => {
    // The REAL reference scripts over the checked-in test rules file:
    // remind-terminal speaks on every bare-text/no-session turn, and the
    // 50% budget rung (ceil(4 * 0.5) = 2) collides with the second one -
    // two notifications at one boundary.
    const client = new MockLlmClient([
      scriptedTurn([complete(assistantMessage(textBlock("a")))]),
      scriptedTurn([complete(assistantMessage(textBlock("b")))]),
      scriptedTurn([complete(assistantMessage(textBlock("c")))]),
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_s", "submit_subagent_outcome", { summary: "finally" }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const { runtime } = runtimeFixture({
      clients: { helper_llm: client },
      notificationRulesPath: eosAgentsPath("tests/notification-rules/turn-reminders.json"),
    });
    const run = runtime.startRun({
      agentName: "drifty",
      initialMessages: [userMessage("go")],
    });
    const outcome = await run.handle.outcome;
    expect(outcome.status).toBe("completed");

    const remind = systemNotificationMessage({
      type: "reminder",
      source: "TurnCompleted",
      text:
        "You produced no tool call and have no background work. " +
        "To finish this run you must call your terminal tool submit_subagent_outcome.",
    });
    const budget = systemNotificationMessage({
      type: "reminder",
      source: "TurnCompleted",
      text: "Turn 2 of 4 (50% of budget). Wrap up and submit via submit_subagent_outcome.",
    });
    expect(
      must(client.requests.at(1)).messages.slice(-1),
      "turn 1 bare text: one reminder drained before the next call",
    ).toEqual([remind]);
    expect(
      must(client.requests.at(2)).messages.slice(-2),
      "turn 2 bare text on the 50% threshold: two notifications at one boundary arrive as two separate user messages, in rule-config order",
    ).toEqual([remind, budget]);
    expect(
      must(client.requests.at(3)).messages.slice(-1),
      "turn 3 bare text: reminded again - once per offending turn, no more",
    ).toEqual([remind]);
    expect(
      outcome.llm.filter(
        (message) =>
          message.role === "user" &&
          message.content.some(
            (block) => block.type === "text" && block.text.includes('"reminder"'),
          ),
      ),
      "three spin turns earned exactly four reminder messages: one per turn plus the budget rung",
    ).toHaveLength(4);
  });

  it("narrows notification rules per run by the agent matchers (04.9 §5)", async () => {
    const helperClient = new MockLlmClient([
      scriptedTurn([complete(assistantMessage(textBlock("thinking")))]),
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_hs", "submit_subagent_outcome", { summary: "done" }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const rootClient = new MockLlmClient([
      scriptedTurn([complete(assistantMessage(textBlock("thinking")))]),
      scriptedTurn([
        complete(
          assistantMessage(toolUseBlock("tu_rs", "submit_main_outcome", { summary: "done" })),
          "tool_use",
        ),
      ]),
    ]);
    const { runtime } = runtimeFixture({
      clients: { root_llm: rootClient, helper_llm: helperClient },
      notificationRulesPath: eosAgentsPath("tests/notification-rules/kind-scoped.json"),
    });
    const helper = runtime.startRun({
      agentName: "helper",
      initialMessages: [userMessage("go")],
    });
    const root = runtime.startRun({ agentName: "root", initialMessages: [userMessage("go")] });
    await Promise.all([helper.handle.outcome, root.handle.outcome]);
    expect(
      must(helperClient.requests.at(1)).messages.at(-1),
      "the matching subagent-kind run drained the reminder before its next provider call",
    ).toEqual(
      systemNotificationMessage({
        type: "reminder",
        source: "TurnCompleted",
        text: "kind scoped",
      }),
    );
    expect(
      JSON.stringify(must(rootClient.requests.at(1)).messages),
      "the main-kind run never matched the rule, so its script never ran",
    ).not.toContain('"reminder"');
  });

  it("keeps every conversation-shaping event readable through the tool once the outcome settles (§13.9)", async () => {
    const client = new MockLlmClient([
      scriptedTurn([complete(assistantMessage(textBlock("thinking")))]),
      scriptedTurn([
        complete(
          assistantMessage(
            toolUseBlock("tu_s", "submit_main_outcome", { summary: "fin" }),
          ),
          "tool_use",
        ),
      ]),
    ]);
    const { runtime } = runtimeFixture({ clients: { root_llm: client } });
    const run = runtime.startRun({
      agentName: "root",
      initialMessages: [userMessage("first"), userMessage("second")],
    });
    const outcome = await run.handle.outcome;
    expect(outcome.status).toBe("completed");
    await waitForFinished(runtime, "root");

    const lines = readTranscriptLines(run.transcriptPath);
    const events = readEventLines(join(dirname(run.transcriptPath), "events.jsonl"));
    const result = readResultLines(join(dirname(run.transcriptPath), "result.jsonl"));
    expect(readdirSync(dirname(run.transcriptPath)).sort()).toEqual([
      "events.jsonl",
      "result.jsonl",
      "transcript.jsonl",
    ]);
    expect(lines.map((line) => line.kind)).toEqual([
      "user",
      "user",
      "assistant",
      "assistant",
      "tool_result",
      "run_finished",
    ]);
    const transcriptSeqs = lines.map((line) => line.seq);
    expect(
      transcriptSeqs,
      "transcript seq is ordered but sparse because run audit files share the counter",
    ).toEqual([...transcriptSeqs].sort((a, b) => a - b));
    expect(transcriptSeqs[0]).toBeGreaterThan(0);
    expect(lines[0]).toMatchObject({
      kind: "user",
      origin: "initial",
      message: userMessage("first"),
    });
    expect(
      events.filter((line) => line.type === "turn_completed"),
      "each completed turn records usage in events.jsonl",
    ).toHaveLength(2);
    expect(result).toEqual([
      expect.objectContaining({
        run_id: run.runId,
        status: "completed",
        usage: outcome.usage,
      }),
    ]);
  });
});
