import { readdirSync } from "node:fs";
import { join } from "node:path";

import { loadAgentProfile, type AgentProfile } from "./profile-loader.js";

export interface AgentProfileRegistry {
  /** Throws on an unknown name. */
  require(name: string): AgentProfile;
  list(): readonly AgentProfile[];
}

const RUN_SUBAGENT_TOOL = "run_subagent";

/**
 * Load every `<dir>/*.md` profile and apply the self-contained startup rules:
 * unique names, and `subagents` targets that name known, non-terminal profiles
 * (a subagent launch supplies no outcome function). Tool-name and workflow
 * resolution is the factory's job — it owns the host tool registry and the hub.
 */
export function loadAgentProfiles(dir: string): AgentProfileRegistry {
  const byName = new Map<string, AgentProfile>();
  for (const file of readdirSync(dir).filter((name) => name.endsWith(".md")).sort()) {
    const profile = loadAgentProfile(join(dir, file));
    if (byName.has(profile.name)) {
      throw new Error(
        `duplicate agent profile name "${profile.name}" (${profile.source_path})`,
      );
    }
    byName.set(profile.name, profile);
  }

  for (const profile of byName.values()) {
    if (profile.allowed_tools.includes(RUN_SUBAGENT_TOOL) && profile.subagents.length === 0) {
      throw new Error(
        `agent profile "${profile.name}" exposes ${RUN_SUBAGENT_TOOL} but lists no subagents`,
      );
    }
    for (const target of profile.subagents) {
      const sub = byName.get(target);
      if (!sub) {
        throw new Error(`agent profile "${profile.name}" names unknown subagent "${target}"`);
      }
      if (sub.terminal_tool !== undefined) {
        throw new Error(
          `agent profile "${profile.name}" names terminal profile "${target}" as a subagent`,
        );
      }
    }
  }

  return {
    require(name) {
      const profile = byName.get(name);
      if (!profile) {
        const known = [...byName.keys()].join(", ") || "none";
        throw new Error(`unknown agent profile "${name}" (configured: ${known})`);
      }
      return profile;
    },
    list: () => [...byName.values()],
  };
}
