import { mkdirSync } from "node:fs";
import { dirname, join } from "node:path";

import { describe, expect, it } from "vitest";

import type { JsonObject } from "@eos/contracts";
import type { LlmClient } from "@eos/llm-client";
import { eosAgentsPath } from "@eos/testkit";

import { createAgentRuntime, type AgentRuntime } from "../src/runtime.js";
import { runTranscriptPath } from "../src/transcript.js";
import {
  MockLlmClient,
  assistantMessage,
  complete,
  hangingTurn,
  lastToolResultJson,
  llmRegistry,
  must,
  readResultLines,
  readTranscriptLines,
  scriptedTurn,
  tempDir,
  textBlock,
  toolUseBlock,
  userMessage,
  writeProfile,
  type ScriptedTurn,
} from "./support.js";

// The §10 reference scripts live as checked-in fixtures under
// `.eos-agents/tests/pursuit/scripts`, referenced by the checked-in
// `.eos-agents/tests/profile/pursuit*` profile directories.
const SCRIPTS_DIR = eosAgentsPath("tests/pursuit/scripts");

interface PursuitFixtureOptions {
  clients: Record<string, LlmClient>;
  /** `pursuit-broken` points the planner at `broken-planner.cjs`. */
  profileGroup?: "pursuit" | "pursuit-broken";
}

function pursuitRuntimeFixture(options: PursuitFixtureOptions): {
  runtime: AgentRuntime;
  dataDir: string;
  contextRoot: string;
} {
  const root = tempDir("eos-pursuit-runtime-");
  const dataDir = join(root, "data");
  const contextRoot = join(root, "pursuit-context");
  const runtime = createAgentRuntime({
    agentProfilesDir: eosAgentsPath("tests/profile", options.profileGroup ?? "pursuit"),
    llmClients: llmRegistry(options.clients),
    hookConfigPath: eosAgentsPath("tests/hooks/none.json"),
    notificationRulesPath: eosAgentsPath("tests/notification-rules/none.json"),
    dataDir,
    pursuitDb: ":memory:",
    pursuitContextRoot: contextRoot,
    pursuitScriptsDir: SCRIPTS_DIR,
  });
  return { runtime, dataDir, contextRoot };
}

function delegateTurn(goal: string): ScriptedTurn {
  return scriptedTurn([
    complete(
      assistantMessage(
        toolUseBlock("tu_d", "delegate_pursuit", { pursuit_goal: goal }),
      ),
      "tool_use",
    ),
  ]);
}

const submitMainTurn = scriptedTurn([
  complete(
    assistantMessage(toolUseBlock("tu_m", "submit_main_outcome", { summary: "done" })),
    "tool_use",
  ),
]);

function plannerSubmissionTurn(workItems: JsonObject[]): ScriptedTurn {
  return scriptedTurn([
    complete(
      assistantMessage(
        toolUseBlock("tu_p", "submit_planner_outcome", {
          summary: "planned both items",
          leg_goal: "the whole goal",
          work_items: workItems,
        }),
      ),
      "tool_use",
    ),
  ]);
}

function workerSubmissionTurn(id: string, summary: string): ScriptedTurn {
  return scriptedTurn([
    complete(
      assistantMessage(
        toolUseBlock(`tu_w_${id}`, "submit_worker_outcome", {
          summary,
          is_pass: true,
          outcome: `${summary} in detail`,
        }),
      ),
      "tool_use",
    ),
  ]);
}

describe("pursuit runtime end-to-end (§16 case 12)", () => {
  it("delegates, runs scripted planner and workers through real engine loops, auto-waits, and submits", async () => {
    const mainClient = new MockLlmClient([
      delegateTurn("build the thing"),
      scriptedTurn([complete(assistantMessage(textBlock("waiting")))]),
      submitMainTurn,
    ]);
    const plannerClient = new MockLlmClient([
      plannerSubmissionTurn([
        {
          id: "a",
          agent_name: "worker",
          title: "first item",
          spec: "do the first item",
          depends_on: [],
        },
        {
          id: "b",
          agent_name: "worker",
          title: "second item",
          spec: "do the second item",
          depends_on: ["a"],
        },
      ]),
    ]);
    const workerClient = new MockLlmClient([
      workerSubmissionTurn("a", "first item shipped"),
      workerSubmissionTurn("b", "second item shipped"),
    ]);
    const { runtime } = pursuitRuntimeFixture({
      clients: {
        main_llm: mainClient,
        planner_llm: plannerClient,
        worker_llm: workerClient,
      },
    });

    const run = runtime.startRun({
      agentName: "orchestrator",
      initialMessages: [userMessage("orchestrate the thing")],
    });
    const outcome = await run.handle.outcome;
    expect(outcome.status).toBe("completed");

    // The planner's initial messages are EXACTLY the script's output -
    // nothing merged around them (§2.12).
    const plannerRequest = must(plannerClient.requests.at(0));
    expect(plannerRequest.messages).toEqual([
      userMessage("# Pursuit goal\nbuild the thing"),
      userMessage("# Current leg goal\nbuild the thing"),
      userMessage("Submit planner outcome with work items for this leg goal."),
    ]);

    // Worker A: title + spec from the snapshot, no dependencies.
    const workerARequest = must(workerClient.requests.at(0));
    expect(workerARequest.messages).toEqual([
      userMessage("# Pursuit goal\nbuild the thing"),
      userMessage("# Current leg goal\nthe whole goal"),
      userMessage("# Work item title\nfirst item"),
      userMessage("# Work item spec\ndo the first item"),
      userMessage("Submit worker outcome for this work item."),
    ]);

    // Worker B sees its dependency outcomes, fully expanded by the script.
    const workerBRequest = must(workerClient.requests.at(1));
    const texts = workerBRequest.messages.flatMap((message) =>
      message.content.flatMap((block) => (block.type === "text" ? [block.text] : [])),
    );
    expect(texts).toContain("# Work item title\nsecond item");
    expect(
      texts.some(
        (text) =>
          text.startsWith("# Dependencies\n") &&
          text.includes("first item shipped"),
      ),
      "dependency outcomes ride the worker's initial messages",
    ).toBe(true);

    // The settlement notification reached the caller's conversation.
    const settled = mainClient.requests
      .flatMap((request) => request.messages)
      .flatMap((message) => message.content)
      .filter((block) => block.type === "text")
      .map((block) => block.text)
      .find((text) => text.includes("session_settled"));
    expect(settled, "session_settled drained into the caller").toBeDefined();
    expect(settled).toContain('"pursuit"');
    expect(settled).toContain('"completed"');
    expect(settled).toContain("planned both items");
  });

  it("cancel_background_session mid-pursuit cascades pursuit_cancelled into child transcripts", async () => {
    let plannerStarted!: () => void;
    const started = new Promise<void>((resolve) => (plannerStarted = resolve));
    // The second turn gates on the planner having started, then cancels
    // the pursuit session by the id the delegate result returned.
    const gatedCancelTurn: ScriptedTurn = async function* (request) {
      await started;
      const result = lastToolResultJson(request);
      yield complete(
        assistantMessage(
          toolUseBlock("tu_c", "cancel_background_session", {
            type: "pursuit",
            id: result.pursuit_id,
            reason: "wrong direction",
          }),
        ),
        "tool_use",
      );
    };
    const mainClient = new MockLlmClient([
      delegateTurn("never finishes"),
      gatedCancelTurn,
      submitMainTurn,
    ]);
    const plannerClient = new MockLlmClient([hangingTurn(plannerStarted)]);
    const { runtime, dataDir } = pursuitRuntimeFixture({
      clients: {
        main_llm: mainClient,
        planner_llm: plannerClient,
        worker_llm: new MockLlmClient([]),
      },
    });

    const run = runtime.startRun({
      agentName: "orchestrator",
      initialMessages: [userMessage("orchestrate, then cancel")],
    });
    const outcome = await run.handle.outcome;
    expect(outcome.status).toBe("completed");

    const plannerRun = must(
      runtime.listRuns().find((entry) => entry.agent_name === "planner"),
    );
    const transcriptPath = runTranscriptPath(dataDir, plannerRun.run_id);
    const finished = readTranscriptLines(transcriptPath).find(
      (line) => line.kind === "run_finished",
    );
    expect(finished).toMatchObject({
      outcome_status: "cancelled",
      interrupt_reason: "pursuit_cancelled",
    });
    const result = readResultLines(join(dirname(transcriptPath), "result.jsonl"));
    expect(result.at(0)).toMatchObject({ interrupt_reason: "pursuit_cancelled" });
  });

  it("a broken context script drives the case-9 synthesis path live and the session settles failed", async () => {
    const mainClient = new MockLlmClient([
      delegateTurn("doomed goal"),
      scriptedTurn([complete(assistantMessage(textBlock("waiting")))]),
      submitMainTurn,
    ]);
    const { runtime } = pursuitRuntimeFixture({
      clients: {
        main_llm: mainClient,
        planner_llm: new MockLlmClient([]),
        worker_llm: new MockLlmClient([]),
      },
      profileGroup: "pursuit-broken",
    });

    const run = runtime.startRun({
      agentName: "orchestrator",
      initialMessages: [userMessage("orchestrate the doomed thing")],
    });
    const outcome = await run.handle.outcome;
    expect(outcome.status).toBe("completed");

    const settled = mainClient.requests
      .flatMap((request) => request.messages)
      .flatMap((message) => message.content)
      .filter((block) => block.type === "text")
      .map((block) => block.text)
      .find((text) => text.includes("session_settled"));
    expect(settled).toBeDefined();
    expect(settled).toContain('"failed"');
    expect(settled).toContain("context_script_error");
  });
});

describe("pursuit runtime startup validation (§16 case 12)", () => {
  function startupFixture(mutate: {
    plannerScriptPath?: (scriptsDir: string) => string;
    skipPlannerProfile?: boolean;
    secondPlanner?: boolean;
    pursuitDb?: string;
    allowDelegateWithoutDb?: boolean;
  }): () => void {
    const root = tempDir("eos-pursuit-startup-");
    const profilesDir = join(root, "profiles");
    mkdirSync(profilesDir, { recursive: true });

    writeProfile(profilesDir, {
      name: "orchestrator",
      kind: "main",
      llmClientId: "main_llm",
      allowed: mutate.allowDelegateWithoutDb ? ["delegate_pursuit"] : [],
    });
    if (!mutate.skipPlannerProfile) {
      writeProfile(profilesDir, {
        name: "planner",
        kind: "planner",
        llmClientId: "planner_llm",
        allowed: ["ask_advisor"],
        pursuitContextScript:
          mutate.plannerScriptPath?.(SCRIPTS_DIR) ?? join(SCRIPTS_DIR, "planner.cjs"),
      });
    }
    if (mutate.secondPlanner) {
      writeProfile(profilesDir, {
        name: "planner-b",
        kind: "planner",
        llmClientId: "planner_llm",
        allowed: ["ask_advisor"],
        pursuitContextScript: join(SCRIPTS_DIR, "planner.cjs"),
      });
    }
    return () =>
      createAgentRuntime({
        agentProfilesDir: profilesDir,
        llmClients: llmRegistry({
          main_llm: new MockLlmClient([]),
          planner_llm: new MockLlmClient([]),
        }),
        hookConfigPath: eosAgentsPath("tests/hooks/none.json"),
        notificationRulesPath: eosAgentsPath("tests/notification-rules/none.json"),
        dataDir: join(root, "data"),
        pursuitScriptsDir: SCRIPTS_DIR,
        ...(mutate.pursuitDb !== undefined && { pursuitDb: mutate.pursuitDb }),
      });
  }

  it("fails startup on a missing profile script path", () => {
    expect(
      startupFixture({
        plannerScriptPath: (dir) => join(dir, "absent.cjs"),
        pursuitDb: ":memory:",
      }),
    ).toThrow(/is not readable/);
  });

  it("fails startup on a script path escaping the script root", () => {
    expect(
      startupFixture({
        plannerScriptPath: (dir) => join(dir, "..", "outside.cjs"),
        pursuitDb: ":memory:",
      }),
    ).toThrow(/escapes the script root/);
  });

  it("fails startup on a non-script extension", () => {
    expect(
      startupFixture({
        plannerScriptPath: (dir) => join(dir, "not-a-script.js"),
        pursuitDb: ":memory:",
      }),
    ).toThrow(/must be a \.cjs or \.mjs file/);
  });

  it("requires exactly one planner profile when pursuitDb is configured", () => {
    expect(
      startupFixture({ secondPlanner: true, pursuitDb: ":memory:" }),
    ).toThrow(/exactly one planner profile; found 2/);
    expect(
      startupFixture({ skipPlannerProfile: true, pursuitDb: ":memory:" }),
    ).toThrow(/exactly one planner profile; found 0/);
  });

  it("rejects a profile listing delegate_pursuit when no pursuitDb is configured", () => {
    expect(startupFixture({ allowDelegateWithoutDb: true })).toThrow(
      /allows "delegate_pursuit", which is not a known non-terminal tool/,
    );
    expect(
      startupFixture({ allowDelegateWithoutDb: true, pursuitDb: ":memory:" }),
    ).not.toThrow();
  });
});
