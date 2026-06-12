import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";
import { eosAgentsPath, scriptedTool } from "@eos/testkit";

import { loadAgentProfile } from "../src/agent-profile-loader.js";
import {
  loadAgentProfileRegistry,
  selectProfileDefinitions,
  type KnownToolNames,
} from "../src/agent-profile-registry.js";
import { tempDir, writeProfile, type ProfileSpec } from "./support.js";

/** The REAL repo profile directory: the §4 worker example lives there. */
const REAL_PROFILE_DIR = eosAgentsPath("profile");

/** The repo worker profile, the base every mutation case breaks one way. */
const WORKER_PROFILE = readFileSync(eosAgentsPath("profile/worker.md"), "utf8");

const SANDBOX_NAMES = [
  "read",
  "multi_read",
  "write",
  "edit",
  "exec_command",
  "command_stdin",
  "read_command_transcript",
] as const;

const KNOWN: KnownToolNames = {
  ordinary: new Set([
    ...SANDBOX_NAMES,
    "list_background_sessions",
    "cancel_background_session",
    "run_subagent",
    "ask_advisor",
    "read_agent_run_transcript",
  ]),
  terminal: new Set([
    "submit_main_outcome",
    "submit_planner_outcome",
    "submit_worker_outcome",
    "submit_advisor_outcome",
    "submit_subagent_outcome",
  ]),
};

function workerDir(): string {
  const dir = join(tempDir("eos-profiles-"), "profiles");
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "worker.md"), WORKER_PROFILE);
  return dir;
}

describe("agent profile loader and registry", () => {
  it("loads the REAL repo worker profile by agent name (§13.1)", () => {
    const registry = loadAgentProfileRegistry(REAL_PROFILE_DIR, KNOWN);
    const profile = registry.require("worker");
    expect(profile).toMatchObject({
      name: "worker",
      description: "Worker",
      llm_client_id: "codex_coding_plan",
      max_turns: 100,
      agent_kind: "worker",
      terminal_tool: "submit_worker_outcome",
      pursuit_context_script: ".eos-agents/pursuit/scripts/worker.cjs",
      source_path: join(REAL_PROFILE_DIR, "worker.md"),
    });
    expect(profile.allowed_tools).toEqual([
      ...SANDBOX_NAMES,
      "list_background_sessions",
      "cancel_background_session",
      "ask_advisor",
    ]);
    expect(profile.system_prompt.startsWith("You are the worker")).toBe(true);
    expect(
      profile.system_prompt.includes('tool_name="submit_worker_outcome"'),
      "the body after frontmatter is the system prompt",
    ).toBe(true);
  });

  it("throws on an unknown agent name, naming the known ones", () => {
    const registry = loadAgentProfileRegistry(REAL_PROFILE_DIR, KNOWN);
    expect(() => registry.require("nobody")).toThrow(
      'unknown agent profile "nobody" (known: planner, worker)',
    );
  });

  it("rejects duplicate profile names across files at startup (§13.1)", () => {
    const dir = workerDir();
    writeFileSync(
      join(dir, "worker-copy.md"),
      WORKER_PROFILE, // same `name: worker` under a second file name
    );
    expect(() => loadAgentProfileRegistry(dir, KNOWN)).toThrow(
      /duplicate agent profile name "worker"/,
    );
  });

  it("rejects a missing profiles directory at startup", () => {
    expect(() =>
      loadAgentProfileRegistry(join(tempDir("eos-none-"), "absent"), KNOWN),
    ).toThrow(/is not readable/);
  });

  it.each`
    breakage                                | mutate                                                          | expected
    ${"missing llm_client_id"}              | ${(raw: string) => raw.replace(/^llm_client_id:.*\n/m, "")}     | ${/llm_client_id/}
    ${"zero max_turns"}                     | ${(raw: string) => raw.replace("max_turns: 100", "max_turns: 0")} | ${/max_turns/}
    ${"non-numeric max_turns"}              | ${(raw: string) => raw.replace("max_turns: 100", "max_turns: many")} | ${/max_turns/}
    ${"unknown allowed_tools entry"}        | ${(raw: string) => raw.replace("  - read\n", "  - teleport\n")} | ${/allows "teleport", which is not a known non-terminal tool/}
    ${"unknown terminal_tool"}              | ${(raw: string) => raw.replace("terminal_tool: submit_worker_outcome", "terminal_tool: submit_nothing")} | ${/selects "submit_nothing", which is not a known terminal tool/}
    ${"non-terminal terminal_tool"}         | ${(raw: string) => raw.replace("terminal_tool: submit_worker_outcome", "terminal_tool: run_subagent")} | ${/selects "run_subagent", which is not a known terminal tool/}
    ${"terminal_tool inside allowed_tools"} | ${(raw: string) => raw.replace("  - ask_advisor\n", "  - ask_advisor\n  - submit_worker_outcome\n")} | ${/lists its terminal_tool "submit_worker_outcome" under allowed_tools/}
    ${"no frontmatter block"}               | ${() => "just prose, no frontmatter\n"}                         | ${/must open with a --- YAML frontmatter block/}
    ${"worker without pursuit_context_script"} | ${(raw: string) => raw.replace(/^pursuit_context_script:.*\n/m, "")} | ${/requires pursuit_context_script/}
  `(
    "fails at startup on $breakage (§13.1)",
    ({ mutate, expected }: { mutate: (raw: string) => string; expected: RegExp }) => {
      const dir = join(tempDir("eos-profiles-"), "profiles");
      mkdirSync(dir, { recursive: true });
      writeFileSync(join(dir, "worker.md"), mutate(WORKER_PROFILE));
      expect(() => loadAgentProfileRegistry(dir, KNOWN)).toThrow(expected);
    },
  );

  it("rejects pursuit_context_script on non-pursuit agent kinds", () => {
    const dir = join(tempDir("eos-profiles-"), "profiles");
    mkdirSync(dir, { recursive: true });
    writeFileSync(
      join(dir, "advisor.md"),
      WORKER_PROFILE.replace("agent_kind: worker", "agent_kind: advisor").replace(
        "terminal_tool: submit_worker_outcome",
        "terminal_tool: submit_advisor_outcome",
      ),
    );
    expect(() => loadAgentProfileRegistry(dir, KNOWN)).toThrow(
      /must omit pursuit_context_script/,
    );
  });

  it("parses a profile whose body is empty into an empty system prompt", () => {
    const dir = join(tempDir("eos-profiles-"), "profiles");
    mkdirSync(dir, { recursive: true });
    const path = writeProfile(dir, {
      name: "advisor",
      kind: "advisor",
      llmClientId: "advisor_llm",
      allowed: [],
      body: "",
    });
    expect(loadAgentProfile(path).system_prompt).toBe("");
  });

  it.each`
    kind
    ${"main"}
    ${"advisor"}
    ${"subagent"}
  `("loads a $kind profile that omits terminal_tool (U10)", ({ kind }: { kind: ProfileSpec["kind"] }) => {
    const dir = join(tempDir("eos-profiles-"), "profiles");
    mkdirSync(dir, { recursive: true });
    const path = writeProfile(dir, {
      name: "textish",
      kind,
      llmClientId: "any_llm",
      allowed: [],
      terminal: null,
    });
    expect(loadAgentProfile(path).terminal_tool).toBeUndefined();
  });

  it.each`
    kind
    ${"planner"}
    ${"worker"}
  `("rejects a $kind profile without a terminal tool (U11)", ({ kind }: { kind: ProfileSpec["kind"] }) => {
    const dir = join(tempDir("eos-profiles-"), "profiles");
    mkdirSync(dir, { recursive: true });
    const path = writeProfile(dir, {
      name: "wf",
      kind,
      llmClientId: "any_llm",
      allowed: [],
      terminal: null,
      pursuitContextScript: ".eos-agents/pursuit/scripts/x.cjs",
    });
    expect(() => loadAgentProfile(path)).toThrow(
      `agent profile ${path} (agent_kind ${kind}) requires terminal_tool`,
    );
  });

  it("selects no submission definition for a no-terminal profile while allowed_tools rules hold (U12)", () => {
    const dir = join(tempDir("eos-profiles-"), "profiles");
    mkdirSync(dir, { recursive: true });
    writeProfile(dir, {
      name: "texty",
      kind: "subagent",
      llmClientId: "any_llm",
      allowed: ["read", "read_agent_run_transcript"],
      terminal: null,
    });
    const registry = loadAgentProfileRegistry(dir, KNOWN);
    const profile = registry.require("texty");
    const define = (name: string): ReturnType<typeof scriptedTool> =>
      scriptedTool({ name, execute: () => Promise.resolve({ content: name }) });
    const available = [
      define("read"),
      define("read_agent_run_transcript"),
      define("submit_subagent_outcome"),
      define("submit_main_outcome"),
    ];
    const selected = selectProfileDefinitions(profile, available).map(
      (definition) => definition.name as string,
    );
    expect(selected, "no submit_* spec is exposed at all").toEqual([
      "read",
      "read_agent_run_transcript",
    ]);

    const badDir = join(tempDir("eos-profiles-"), "profiles");
    mkdirSync(badDir, { recursive: true });
    writeProfile(badDir, {
      name: "texty",
      kind: "subagent",
      llmClientId: "any_llm",
      allowed: ["teleport"],
      terminal: null,
    });
    expect(
      () => loadAgentProfileRegistry(badDir, KNOWN),
      "allowed_tools validation is unchanged for text-mode profiles",
    ).toThrow(/allows "teleport", which is not a known non-terminal tool/);
  });

  it("selects exactly allowed_tools + terminal_tool from the available definitions (§2.8)", () => {
    const registry = loadAgentProfileRegistry(REAL_PROFILE_DIR, KNOWN);
    const profile = registry.require("worker");
    const define = (name: string): ReturnType<typeof scriptedTool> =>
      scriptedTool({ name, execute: () => Promise.resolve({ content: name }) });
    const available = [
      ...SANDBOX_NAMES.map(define),
      define("list_background_sessions"),
      define("cancel_background_session"),
      define("ask_advisor"),
      define("run_subagent"), // known, but not allowed by this profile
      define("submit_worker_outcome"),
      define("submit_main_outcome"), // terminal inventory entry not selected
    ];
    const selected = selectProfileDefinitions(profile, available).map(
      (definition) => definition.name as string,
    );
    expect(selected).toEqual([
      ...SANDBOX_NAMES,
      "list_background_sessions",
      "cancel_background_session",
      "ask_advisor",
      "submit_worker_outcome",
    ]);
  });
});
