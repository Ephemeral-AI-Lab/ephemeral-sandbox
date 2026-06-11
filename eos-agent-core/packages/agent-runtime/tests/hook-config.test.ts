import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { describe, expect, it } from "vitest";

import { loadHookConfig } from "../src/hook-config.js";
import { tempDir } from "./support.js";

describe("hook config loading", () => {
  it("treats a missing file as no hooks (§7)", () => {
    expect(loadHookConfig(join(tempDir("eos-hooks-"), "absent.json"))).toEqual([]);
  });

  it("loads a valid HookConfigEntry array", () => {
    const path = join(tempDir("eos-hooks-"), "hooks.json");
    const entries = [
      {
        event: "PreToolUse",
        matcher: "write_file",
        hooks: [{ type: "command", command: "node check.js", timeout_ms: 1000 }],
      },
      {
        event: "PostToolUse",
        hooks: [{ type: "command", command: "node audit.js" }],
      },
    ];
    writeFileSync(path, JSON.stringify(entries));
    expect(loadHookConfig(path)).toEqual(
      entries.map((entry) => ({
        ...entry,
        hooks: entry.hooks.map((hook) => ({ ...hook, cwd: dirname(path) })),
      })),
    );
  });

  it("runs .eos-agents hook commands from the repo root", () => {
    const root = tempDir("eos-hooks-root-");
    const agentsDir = join(root, ".eos-agents");
    mkdirSync(agentsDir);
    const path = join(agentsDir, "hooks.json");
    writeFileSync(
      path,
      JSON.stringify([
        {
          event: "PreToolUse",
          hooks: [{ type: "command", command: "node .eos-agents/hooks/check.cjs" }],
        },
      ]),
    );
    expect(loadHookConfig(path)[0]?.hooks[0]).toMatchObject({ cwd: root });
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
