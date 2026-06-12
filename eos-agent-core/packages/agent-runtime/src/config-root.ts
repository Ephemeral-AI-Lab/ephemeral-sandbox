import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

const EOS_AGENTS_DIR_NAME = ".eos-agents";

/**
 * The operator config root: the nearest `.eos-agents` directory walking up
 * from the working directory, so a process started anywhere inside the
 * checkout loads the repo-root config. Falls back to `<cwd>/.eos-agents`
 * when no ancestor owns one.
 */
export function eosAgentsRoot(): string {
  let dir = resolve(process.cwd());
  for (;;) {
    const candidate = join(dir, EOS_AGENTS_DIR_NAME);
    if (existsSync(candidate)) return candidate;
    const parent = dirname(dir);
    if (parent === dir) return resolve(EOS_AGENTS_DIR_NAME);
    dir = parent;
  }
}

/** The directory owning `.eos-agents`; config-relative paths resolve here. */
export function configBaseDir(): string {
  return dirname(eosAgentsRoot());
}
