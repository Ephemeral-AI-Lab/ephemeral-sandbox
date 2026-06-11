import { describe, expect, it } from "vitest";

import type { CommandScript, TriggerPayload } from "@eos/notifications";
import { scriptedRunState } from "@eos/testkit";

import { runTriggerCommand } from "../src/trigger-runner.js";
import { snapshotRunState } from "../src/run-state.js";

/** A trigger command running an inline node script (double quotes only). */
function nodeCommand(js: string, timeoutMs?: number): CommandScript {
  return {
    type: "command",
    command: `"${process.execPath}" -e '${js}'`,
    ...(timeoutMs !== undefined && { timeout_ms: timeoutMs }),
  };
}

const PAYLOAD: TriggerPayload = {
  event: "TurnCompleted",
  facts: {
    turn: 1,
    max_turns: 4,
    tool_calls: 0,
    live_sessions: 0,
    has_pending_steers: false,
  },
  run: snapshotRunState(scriptedRunState("main")),
  terminal_tool: "submit_main_outcome",
  background_sessions: [],
};

describe("trigger command runner", () => {
  it("answers the notification from valid stdout, reading the payload as JSON on stdin", async () => {
    const echo =
      'let d="";process.stdin.on("data",(c)=>d+=c);process.stdin.on("end",()=>{const p=JSON.parse(d);console.log(JSON.stringify({notification:p.event+":"+p.terminal_tool}));});';
    await expect(runTriggerCommand(nodeCommand(echo), PAYLOAD)).resolves.toEqual({
      notification: "TurnCompleted:submit_main_outcome",
    });
  });

  it.each`
    js                       | label
    ${"process.exit(0);"}    | ${"empty stdout"}
    ${'console.log("{}");'}  | ${"an empty JSON object"}
  `("answers a skip (no notification, no warning) for $label", async ({ js }) => {
    await expect(runTriggerCommand(nodeCommand(js as string), PAYLOAD)).resolves.toEqual({});
  });

  it("maps a nonzero exit to a warning carrying stderr - exit 2 has no deny semantics here", async () => {
    const run = await runTriggerCommand(
      nodeCommand('process.stderr.write("blocked"); process.exit(2);'),
      PAYLOAD,
    );
    expect(run.notification).toBeUndefined();
    expect(run.warning).toContain("exited 2");
    expect(run.warning).toContain("blocked");
  });

  it("maps non-JSON stdout to a warning", async () => {
    const run = await runTriggerCommand(
      nodeCommand('process.stdout.write("not json at all");'),
      PAYLOAD,
    );
    expect(run.notification).toBeUndefined();
    expect(run.warning).toContain("not JSON");
  });

  it.each`
    js                                                       | label
    ${'console.log(JSON.stringify({decision:"deny"}));'}     | ${"a decision field"}
    ${'console.log(JSON.stringify({updatedInput:{n:1}}));'}  | ${"an updatedInput field"}
    ${'console.log(JSON.stringify({notification:""}));'}     | ${"an empty notification"}
  `("rejects $label as a schema mismatch, never accepting or stripping it", async ({ js }) => {
    const run = await runTriggerCommand(nodeCommand(js as string), PAYLOAD);
    expect(run.notification).toBeUndefined();
    expect(run.warning).toContain("did not match TriggerOutput");
  });

  it("kills a command on its timeout and maps it to a warning", { timeout: 10_000 }, async () => {
    const run = await runTriggerCommand(
      nodeCommand("setInterval(() => {}, 1000);", 250),
      PAYLOAD,
    );
    expect(run.notification).toBeUndefined();
    expect(run.warning).toBe("trigger command timed out");
  });
});
