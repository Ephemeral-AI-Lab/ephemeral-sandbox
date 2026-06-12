import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { describe, expect, it } from "vitest";
import { REPO_ROOT, eosAgentsPath } from "@eos/testkit";

import { loadNotificationRules } from "../src/notification-rules-config.js";
import { tempDir } from "./support.js";

describe("notification rules loading (04.9 §5)", () => {
  it("treats a missing file as no rules", () => {
    expect(loadNotificationRules(join(tempDir("eos-rules-"), "absent.json"))).toEqual([]);
  });

  it("loads both checked-in rule kinds with matchers, filling command cwd from the config dir", () => {
    const path = eosAgentsPath("tests/notification-rules/sample.json");
    expect(loadNotificationRules(path)).toEqual([
      {
        event: "TurnCompleted",
        agent_kind: "main",
        rules: [{ type: "command", command: "node remind.cjs", cwd: dirname(path) }],
      },
      {
        event: "IdleParked",
        agent_name: "researcher",
        timeout_ms: 60_000,
        rules: [{ type: "command", command: "node idle.cjs", cwd: dirname(path) }],
      },
    ]);
  });

  it("runs the REAL .eos-agents/notification_rules.json commands from the repo root", () => {
    const entries = loadNotificationRules(eosAgentsPath("notification_rules.json"));
    expect(entries.length, "the repo baseline registers rules").toBeGreaterThan(0);
    for (const entry of entries) {
      for (const rule of entry.rules) {
        expect(rule, "a .eos-agents config owns the repo root as cwd").toMatchObject({
          cwd: REPO_ROOT,
        });
      }
    }
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
