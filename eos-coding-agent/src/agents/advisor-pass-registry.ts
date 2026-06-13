import type { AgentRunId, JsonObject } from "eos-agent-sdk";

/** The exact terminal submission an advisor pass is keyed to. */
export interface AdvisorSubmission {
  tool_name: string;
  payload: JsonObject;
}

/**
 * In-memory, per-run record of advisor passes. The gate consults it; the
 * `ask_advisor` tool records into it. Keyed by `runId`, matched by
 * canonical-JSON (sorted keys) deep equality of `{ tool_name, payload }`, so a
 * pass authorizes exactly the submission it reviewed and nothing else.
 */
export class AdvisorPassRegistry {
  readonly #passes = new Map<string, Set<string>>();

  recordPass(runId: AgentRunId, submission: AdvisorSubmission): void {
    const key = canonicalJson(submission);
    const existing = this.#passes.get(runId);
    if (existing) existing.add(key);
    else this.#passes.set(runId, new Set([key]));
  }

  hasPass(runId: AgentRunId, submission: AdvisorSubmission): boolean {
    return this.#passes.get(runId)?.has(canonicalJson(submission)) ?? false;
  }
}

/** Deterministic JSON with recursively sorted object keys. */
export function canonicalJson(value: unknown): string {
  return JSON.stringify(sortKeys(value));
}

function sortKeys(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortKeys);
  if (value !== null && typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>).sort(([a], [b]) =>
      a < b ? -1 : a > b ? 1 : 0,
    );
    return Object.fromEntries(entries.map(([key, child]) => [key, sortKeys(child)]));
  }
  return value;
}
