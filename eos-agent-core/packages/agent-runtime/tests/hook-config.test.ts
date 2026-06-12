import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { describe, expect, it } from "vitest";
import { REPO_ROOT, eosAgentsPath } from "@eos/testkit";

import { loadHookConfig } from "../src/hook-config.js";
import { tempDir } from "./support.js";

describe("hook config loading", () => {
  it("treats a missing file as no hooks (§7)", () => {
    expect(loadHookConfig(join(tempDir("eos-hooks-"), "absent.json"))).toEqual([]);
  });

  it("loads a checked-in HookConfigEntry array, filling command cwd from the config dir", () => {
    const path = eosAgentsPath("tests/hooks/sample.json");
    expect(loadHookConfig(path)).toEqual([
      {
        event: "PreToolUse",
        matcher: "write_file",
        hooks: [
          { type: "command", command: "node check.js", timeout_ms: 1000, cwd: dirname(path) },
        ],
      },
      {
        event: "PostToolUse",
        hooks: [{ type: "command", command: "node audit.js", cwd: dirname(path) }],
      },
    ]);
  });

  it("runs the REAL .eos-agents/hooks.json commands from the repo root", () => {
    const entries = loadHookConfig(eosAgentsPath("hooks.json"));
    expect(entries.length, "the repo baseline registers hooks").toBeGreaterThan(0);
    for (const entry of entries) {
      for (const hook of entry.hooks) {
        expect(hook, "a .eos-agents config owns the repo root as cwd").toMatchObject({
          cwd: REPO_ROOT,
        });
      }
    }
  });

  it("fails loudly on a file that is not JSON (§7)", () => {
    const path = join(tempDir("eos-hooks-"), "hooks.json");
    writeFileSync(path, "{not json");
    expect(() => loadHookConfig(path)).toThrow(/is not valid JSON/);
  });

  it("fails loudly naming the Zod issues on a malformed entry (§7)", () => {
    const path = join(tempDir("eos-hooks-"), "hooks.json");
    writeFileSync(
      path,
      JSON.stringify([{ event: "OnBoot", hooks: [{ type: "command", command: "x" }] }]),
    );
    expect(() => loadHookConfig(path)).toThrow(/is invalid: .*event/);
  });

  it("rejects a top-level object: the config is an entry array (§7)", () => {
    const path = join(tempDir("eos-hooks-"), "hooks.json");
    writeFileSync(path, JSON.stringify({ hooks: [] }));
    expect(() => loadHookConfig(path)).toThrow(/is invalid/);
  });

  it("rejects a trigger event: notification rules live in their own file (04.9 §5)", () => {
    const path = join(tempDir("eos-hooks-"), "hooks.json");
    writeFileSync(
      path,
      JSON.stringify([
        { event: "TurnCompleted", hooks: [{ type: "command", command: "node x.cjs" }] },
      ]),
    );
    expect(() => loadHookConfig(path)).toThrow(/is invalid: .*event/);
  });
});
