import { join, resolve } from "node:path";

/** The repo checkout root; the testkit sources always live inside it. */
export const REPO_ROOT = resolve(import.meta.dirname, "../../../..");

/** An absolute path into the repo `.eos-agents` config tree shared by tests. */
export function eosAgentsPath(...segments: string[]): string {
  return join(REPO_ROOT, ".eos-agents", ...segments);
}
