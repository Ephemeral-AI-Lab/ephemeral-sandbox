import { dirname, join } from "node:path";

import { assistantText, toolUses } from "@eos/contracts";

import {
  CODEX_CLIENT_ID,
  loadConfiguredCodexRuntime,
} from "./e2e/support/codex-runtime.js";
import {
  TERSE_BODY,
  lookupCodewordTool,
  rootHookConfigPath,
  runtimeFixture,
} from "./e2e/support/fixtures.js";

const codex = loadConfiguredCodexRuntime();
if (!codex.available) throw new Error(codex.reason);

const repoRoot = dirname(dirname(rootHookConfigPath()));
const command = `node ${JSON.stringify(join(repoRoot, ".eos-agents", "hooks", "budget-reminder.cjs"))}`;

const { runtime } = runtimeFixture({
  llmClientsPath: codex.llmClientsPath,
  profiles: [
    {
      name: "counter",
      kind: "subagent",
      llmClientId: CODEX_CLIENT_ID,
      allowed: [],
      maxTurns: 10,
      body: TERSE_BODY,
    },
  ],
  hookEntries: [{ event: "TurnCompleted", hooks: [{ type: "command", command }] }],
});
const run = runtime.startRun({
  agentName: "counter",
  initialMessages: [
    {
      role: "user",
      content: [
        {
          type: "text",
          text: [
            '1. Reply with the plain text "standing by" and make no tool calls.',
            "2. Repeat that on every turn until a system notification reports the turn budget.",
            '3. When it arrives, call submit_subagent_outcome with summary "budget heeded".',
          ].join("\n"),
        },
      ],
    },
  ],
});
const outcome = await run.handle.outcome;
console.log("status:", outcome.status, "turns:", outcome.turns);
if (outcome.status === "failed") console.log("failure:", JSON.stringify(outcome.failure));
for (const [i, message] of outcome.llm.entries()) {
  const uses = toolUses(message).map((u) => u.name).join(",");
  console.log(
    `${String(i)} [${message.role}]${uses ? ` tools=${uses}` : ""} ${assistantText(message).slice(0, 160)}`,
  );
}
