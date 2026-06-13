import { sep } from "node:path";
import { resolve } from "node:path";

import { executeJsonCommand } from "@eos/scripts";
import { ContextScriptOutputSchema, type ComposeLaunchContext } from "@eos/pursuit";

import { zodIssues } from "../config/config-file.js";
import { configBaseDir } from "../config/config-root.js";
import type { AgentProfileRegistry } from "../config/profiles.js";

interface ResolvedScript {
  command: string;
  cwd: string;
}

/**
 * Resolve every profile's `pursuit_context_script` into a per-agent-name map,
 * validated to live under `.eos-agents/pursuit/scripts/`. Pursuit never spawns
 * subprocesses; the app owns script selection and wrapping (spec §11).
 */
export function resolvePursuitContextScripts(
  profiles: AgentProfileRegistry,
): Map<string, ResolvedScript> {
  const base = configBaseDir();
  const scriptsRoot = resolve(base, ".eos-agents", "pursuit", "scripts");
  const scripts = new Map<string, ResolvedScript>();
  for (const profile of profiles.list()) {
    if (profile.pursuit_context_script === undefined) continue;
    const scriptPath = resolve(base, profile.pursuit_context_script);
    if (scriptPath !== scriptsRoot && !scriptPath.startsWith(scriptsRoot + sep)) {
      throw new Error(
        `profile "${profile.name}" pursuit_context_script must resolve under ${scriptsRoot}`,
      );
    }
    if (!scriptPath.endsWith(".cjs") && !scriptPath.endsWith(".mjs")) {
      throw new Error(`profile "${profile.name}" pursuit_context_script must be a .cjs or .mjs file`);
    }
    scripts.set(profile.name, { command: `node ${JSON.stringify(scriptPath)}`, cwd: base });
  }
  return scripts;
}

/**
 * Wrap the resolved scripts into the pursuit compose seam: hook-parity
 * subprocess semantics — JSON snapshot on stdin, `initial_messages` JSON on
 * stdout, replace-never-merge. A start failure, timeout, non-zero exit, or
 * invalid output rejects, which pursuit turns into a context-composition failure.
 */
export function pursuitContextScriptComposer(
  scripts: Map<string, ResolvedScript>,
): ComposeLaunchContext {
  return async (agentName, input, signal) => {
    const script = scripts.get(agentName);
    if (!script) {
      throw new Error(`no pursuit context script for agent "${agentName}"`);
    }
    const result = await executeJsonCommand(
      { command: script.command, cwd: script.cwd },
      input,
      signal,
    );
    if (result.kind !== "exited" || result.code !== 0) {
      const detail =
        result.kind === "exited" ? `exit ${String(result.code)} ${result.stderr}` : result.kind;
      throw new Error(`context script for "${agentName}" failed: ${detail}`);
    }
    let json: unknown;
    try {
      json = JSON.parse(result.stdout);
    } catch {
      throw new Error(`context script for "${agentName}" produced non-JSON output`);
    }
    const parsed = ContextScriptOutputSchema.safeParse(json);
    if (!parsed.success) {
      throw new Error(`context script for "${agentName}" output is invalid: ${zodIssues(parsed.error)}`);
    }
    return parsed.data.initial_messages;
  };
}
