import { describe, expect, it } from "vitest";

import {
  triggerRuleAppliesTo,
  TriggerRuleEntrySchema,
  type TriggerRuleEntry,
} from "../src/triggers.js";

const RUN = { agent_name: "researcher", agent_kind: "main" } as const;

function rule(matchers: Partial<Pick<TriggerRuleEntry, "agent_name" | "agent_kind">>): TriggerRuleEntry {
  return TriggerRuleEntrySchema.parse({
    event: "TurnCompleted",
    ...matchers,
    rules: [{ type: "command", command: "node remind.cjs" }],
  });
}

describe("trigger rule matchers", () => {
  it.each`
    agent_name      | agent_kind     | applies  | reason
    ${undefined}    | ${undefined}   | ${true}  | ${"absent matchers match every run"}
    ${"researcher"} | ${undefined}   | ${true}  | ${"agent_name match"}
    ${"writer"}     | ${undefined}   | ${false} | ${"agent_name mismatch"}
    ${undefined}    | ${"main"}      | ${true}  | ${"agent_kind match"}
    ${undefined}    | ${"subagent"}  | ${false} | ${"agent_kind mismatch"}
    ${"researcher"} | ${"main"}      | ${true}  | ${"both match (AND)"}
    ${"researcher"} | ${"subagent"}  | ${false} | ${"name matches but kind does not (AND)"}
    ${"writer"}     | ${"main"}      | ${false} | ${"kind matches but name does not (AND)"}
  `("applies=$applies when $reason", ({ agent_name, agent_kind, applies }) => {
    expect(
      triggerRuleAppliesTo(
        rule({
          ...(agent_name !== undefined && { agent_name: agent_name as string }),
          ...(agent_kind !== undefined && {
            agent_kind: agent_kind as TriggerRuleEntry["agent_kind"],
          }),
        }),
        RUN,
      ),
    ).toBe(applies as boolean);
  });
});
