import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { loadNotificationRules } from "../src/notification-rules-config.js";
import { tempDir } from "./support.js";

describe("notification rules loading (04.9 §5)", () => {
  it("treats a missing file as no rules", () => {
    expect(loadNotificationRules(join(tempDir("eos-rules-"), "absent.json"))).toEqual([]);
  });

  it("loads both rule kinds with matchers and fills the command cwd from the config root", () => {
    const root = tempDir("eos-rules-root-");
    const agentsDir = join(root, ".eos-agents");
    mkdirSync(agentsDir);
    const path = join(agentsDir, "notification_rules.json");
    const entries = [
      {
        event: "TurnCompleted",
        agent_kind: "main",
        rules: [{ type: "command", command: "node .eos-agents/notification-rules/remind.cjs" }],
      },
      {
        event: "IdleParked",
        agent_name: "researcher",
        timeout_ms: 60_000,
        rules: [{ type: "command", command: "node .eos-agents/notification-rules/idle.cjs" }],
      },
    ];
    writeFileSync(path, JSON.stringify(entries));
    expect(
      loadNotificationRules(path),
      ".eos-agents rule commands run from the repo root",
    ).toEqual(
      entries.map((entry) => ({
        ...entry,
        rules: entry.rules.map((rule) => ({ ...rule, cwd: root })),
      })),
    );
  });

  it("rejects an IdleParked rule without timeout_ms", () => {
    const path = join(tempDir("eos-rules-"), "notification_rules.json");
    writeFileSync(
      path,
      JSON.stringify([
        { event: "IdleParked", rules: [{ type: "command", command: "node x.cjs" }] },
      ]),
    );
    expect(() => loadNotificationRules(path)).toThrow(/is invalid: .*timeout_ms/);
  });

  it("rejects a rule with an empty rules list", () => {
    const path = join(tempDir("eos-rules-"), "notification_rules.json");
    writeFileSync(path, JSON.stringify([{ event: "TurnCompleted", rules: [] }]));
    expect(() => loadNotificationRules(path)).toThrow(/is invalid: .*rules/);
  });

  it("rejects an unknown agent_kind matcher", () => {
    const path = join(tempDir("eos-rules-"), "notification_rules.json");
    writeFileSync(
      path,
      JSON.stringify([
        {
          event: "TurnCompleted",
          agent_kind: "supervisor",
          rules: [{ type: "command", command: "node x.cjs" }],
        },
      ]),
    );
    expect(() => loadNotificationRules(path)).toThrow(/is invalid: .*agent_kind/);
  });

  it("rejects a PreToolUse event: tool events live in hooks.json", () => {
    const path = join(tempDir("eos-rules-"), "notification_rules.json");
    writeFileSync(
      path,
      JSON.stringify([
        { event: "PreToolUse", rules: [{ type: "command", command: "node x.cjs" }] },
      ]),
    );
    expect(() => loadNotificationRules(path)).toThrow(/is invalid: .*event/);
  });

  it("fails loudly on a file that is not JSON", () => {
    const path = join(tempDir("eos-rules-"), "notification_rules.json");
    writeFileSync(path, "{not json");
    expect(() => loadNotificationRules(path)).toThrow(/is not valid JSON/);
  });
});
