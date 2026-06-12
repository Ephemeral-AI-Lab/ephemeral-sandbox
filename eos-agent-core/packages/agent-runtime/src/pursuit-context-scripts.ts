import { statSync } from "node:fs";
import { resolve, sep } from "node:path";

import {
  ContextScriptOutputSchema,
  zodIssueSummary,
  type InitialUserMessage,
} from "@eos/contracts";
import { executeJsonCommand } from "@eos/scripts";
import type { ComposeLaunchContext } from "@eos/pursuit";

import type { AgentProfile } from "./agent-profile-loader.js";
import { configBaseDir } from "./config-root.js";

export interface PursuitContextScript {
  /** Absolute, validated script path. */
  scriptPath: string;
  /** Set only when `pursuitScriptsDir` was explicitly overridden (§10). */
  cwd?: string;
}

/**
 * Startup validation of every planner/worker profile's
 * `pursuit_context_script`: the path must resolve inside the scripts
 * root, name a readable `.cjs`/`.mjs` file, and never a directory. Helper
 * files in the same directory are allowed and simply never registered -
 * only profile-referenced files are spawned. Relative paths resolve from
 * the directory owning `.eos-agents`, never the process cwd.
 */
export function resolvePursuitContextScripts(
  profiles: readonly AgentProfile[],
  scriptsDir: string,
  scriptsDirOverridden: boolean,
): Map<string, PursuitContextScript> {
  const scripts = new Map<string, PursuitContextScript>();
  const base = configBaseDir();
  const root = resolve(base, scriptsDir);
  for (const profile of profiles) {
    if (profile.agent_kind !== "planner" && profile.agent_kind !== "worker") continue;
    const raw = profile.pursuit_context_script;
    if (raw === undefined) {
      throw new Error(
        `agent profile "${profile.name}" requires pursuit_context_script`,
      );
    }
    const scriptPath = resolve(base, raw);
    if (scriptPath !== root && !scriptPath.startsWith(`${root}${sep}`)) {
      throw new Error(
        `agent profile "${profile.name}" pursuit_context_script "${raw}" escapes the script root ${scriptsDir}`,
      );
    }
    if (!scriptPath.endsWith(".cjs") && !scriptPath.endsWith(".mjs")) {
      throw new Error(
        `agent profile "${profile.name}" pursuit_context_script "${raw}" must be a .cjs or .mjs file`,
      );
    }
    let isDirectory: boolean;
    try {
      isDirectory = statSync(scriptPath).isDirectory();
    } catch (error) {
      throw new Error(
        `agent profile "${profile.name}" pursuit_context_script "${raw}" is not readable`,
        { cause: error },
      );
    }
    if (isDirectory) {
      throw new Error(
        `agent profile "${profile.name}" pursuit_context_script "${raw}" is a directory`,
      );
    }
    scripts.set(profile.name, {
      scriptPath,
      ...(scriptsDirOverridden && { cwd: root }),
    });
  }
  return scripts;
}

/**
 * The script-runner composer adapter: hook-parity subprocess semantics
 * (spawned, never imported; JSON snapshot on stdin; bounded by the command
 * default timeout and the launch signal). The parsed `initial_messages`
 * ARE the launch's complete initial messages - replace, never merge. Any
 * non-zero exit, timeout, or parse failure rejects, which the pursuit
 * service records as a `context_script_error` compose failure (§2.14).
 */
export function pursuitContextScriptComposer(
  scripts: ReadonlyMap<string, PursuitContextScript>,
): ComposeLaunchContext {
  return async (agentName, input, signal): Promise<InitialUserMessage[]> => {
    const script = scripts.get(agentName);
    if (!script) {
      throw new Error(`agent profile "${agentName}" has no pursuit_context_script`);
    }
    const result = await executeJsonCommand(
      {
        command: `"${process.execPath}" "${script.scriptPath}"`,
        ...(script.cwd !== undefined && { cwd: script.cwd }),
      },
      input,
      signal,
    );
    if (result.kind === "execute_error") {
      throw new Error(`context script failed to start: ${result.message}`);
    }
    if (result.kind === "aborted") {
      throw new Error("context script timed out or was aborted");
    }
    if (result.code !== 0) {
      throw new Error(
        `context script exited ${String(result.code)}: ${truncate(result.stderr)}`,
      );
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(result.stdout);
    } catch {
      throw new Error(`context script printed invalid JSON: ${truncate(result.stdout)}`);
    }
    const output = ContextScriptOutputSchema.safeParse(parsed);
    if (!output.success) {
      throw new Error(
        `context script output is invalid: ${zodIssueSummary(output.error)}`,
      );
    }
    return output.data.initial_messages;
  };
}

function truncate(text: string): string {
  const trimmed = text.trim();
  return trimmed.length > 200 ? `${trimmed.slice(0, 200)}…` : trimmed;
}
