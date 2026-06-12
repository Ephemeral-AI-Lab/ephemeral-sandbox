import {
  isPursuitEntityTerminal,
  mintPursuitId,
  type AgentRunId,
  type DelegatePursuitInput,
  DelegatePursuitInputSchema,
  type InitialUserMessage,
  type PlannerOutcomePayload,
  type PursuitHandle,
  type PursuitId,
  type PursuitSettlement,
  type PursuitAgentSubmissionBinding,
  type SubmissionResult,
  type WorkerOutcomePayload,
} from "@eos/contracts";
import type { PursuitDb, PursuitTransaction } from "@eos/db";

import {
  claimLaunchable,
  stampAgentRunId,
  verifyClaimLaunchable,
  type AgentLaunchPort,
  type ClaimedLaunch,
  type LaunchSettlement,
} from "./agent-launcher.js";
import { formatAttemptFailureReason } from "./attempt/context.js";
import type { ComposeLaunchContext } from "./context-engine/composer.js";
import {
  buildPlannerContextInput,
  buildWorkerContextInput,
} from "./context-engine/input.js";
import { buildPursuitContext } from "./context-engine/projection/paths.js";
import { projectPursuitContextMirror } from "./context-engine/projection/mirror.js";
import { applyPlannerSettlement } from "./plan/transition.js";
import { cancelPursuit, createPursuitRows } from "./pursuit/transition.js";
import { loadPursuitTree, type PursuitTree } from "./pursuit-tree.js";
import { applyWorkItemSettlement } from "./work-item/transition.js";

const DEFAULT_MAX_ATTEMPTS = 2;

export interface PursuitServiceDependencies {
  db: PursuitDb;
  port: AgentLaunchPort;
  compose: ComposeLaunchContext;
  contextRoot: string;
  plannerAgentName: string;
  isRegisteredWorkerAgent: (agentName: string) => boolean;
  defaultMaxAttempts?: number;
  logMirrorFailure?: (pursuitId: PursuitId, error: unknown) => void;
}

interface TerminalResolver {
  promise: Promise<PursuitSettlement>;
  resolve(terminal: PursuitSettlement): void;
}

function terminalResolver(): TerminalResolver {
  let resolve!: (terminal: PursuitSettlement) => void;
  const promise = new Promise<PursuitSettlement>((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

interface ActivePursuit {
  controller: AbortController;
  terminal: TerminalResolver;
  cancelReason?: string;
}

export class PursuitService {
  readonly #deps: PursuitServiceDependencies;
  readonly #active = new Map<PursuitId, ActivePursuit>();

  constructor(deps: PursuitServiceDependencies) {
    this.#deps = deps;
  }

  async createPursuit(
    input: DelegatePursuitInput,
    parentRunId?: AgentRunId | null,
  ): Promise<PursuitHandle> {
    const parsedInput = DelegatePursuitInputSchema.parse(input);
    const pursuitId = mintPursuitId();
    const active: ActivePursuit = {
      controller: new AbortController(),
      terminal: terminalResolver(),
    };
    this.#active.set(pursuitId, active);
    await this.#mutate(pursuitId, (trx) =>
      createPursuitRows(trx, {
        pursuitId,
        parentRunId,
        input: parsedInput,
        maxAttempts:
          parsedInput.max_attempts ??
          this.#deps.defaultMaxAttempts ??
          DEFAULT_MAX_ATTEMPTS,
      }),
    );
    return {
      pursuit_id: pursuitId,
      cancel: (reason = "pursuit_cancelled") => this.cancel(pursuitId, reason),
      settle: () => active.terminal.promise,
    };
  }

  async cancel(pursuitId: PursuitId, reason: string): Promise<void> {
    const active = this.#active.get(pursuitId);
    if (active) {
      active.cancelReason = reason;
      active.controller.abort("pursuit_cancelled");
    }
    await this.#mutate(pursuitId, async (trx, tree) => {
      if (tree) await cancelPursuit(trx, tree.pursuit.id);
    });
  }

  async #mutate(
    pursuitId: PursuitId,
    mutator: (
      trx: PursuitTransaction,
      tree: PursuitTree | null,
    ) => Promise<void> | void,
  ): Promise<void> {
    const { claims, before } = await this.#deps.db.transaction().execute(async (trx) => {
      const tree = await loadPursuitTree(trx, pursuitId);
      await mutator(trx, tree);
      return {
        claims: await claimLaunchable(trx, pursuitId, this.#deps.plannerAgentName),
        before: tree,
      };
    });
    const after = await loadPursuitTree(this.#deps.db, pursuitId);
    if (!after) return;
    this.#advanceAbortGenerationOnAttemptFailure(pursuitId, before, after);
    await this.#mirror(pursuitId, after);
    for (const claim of claims) {
      await this.#launchClaim(pursuitId, claim, after);
    }
    this.#resolveTerminal(pursuitId, after);
  }

  #advanceAbortGenerationOnAttemptFailure(
    pursuitId: PursuitId,
    before: PursuitTree | null,
    after: PursuitTree,
  ): void {
    const active = this.#active.get(pursuitId);
    if (!active || isPursuitEntityTerminal(after.pursuit.status)) return;
    const failedBefore = new Set(
      (before?.legs ?? [])
        .flatMap((leg) => leg.attempts)
        .filter((attempt) => attempt.status === "Failed")
        .map((attempt) => attempt.id),
    );
    const newlyFailed = after.legs
      .flatMap((leg) => leg.attempts)
      .some((attempt) => attempt.status === "Failed" && !failedBefore.has(attempt.id));
    if (!newlyFailed) return;
    active.controller.abort("attempt_failed");
    active.controller = new AbortController();
  }

  async #mirror(pursuitId: PursuitId, tree: PursuitTree): Promise<void> {
    try {
      await projectPursuitContextMirror(
        this.#deps.contextRoot,
        buildPursuitContext(tree),
      );
    } catch (error) {
      const log =
        this.#deps.logMirrorFailure ??
        ((id: PursuitId, cause: unknown): void => {
          console.warn(`pursuit ${id} context mirror write failed`, cause);
        });
      log(pursuitId, error);
    }
  }

  async #launchClaim(
    pursuitId: PursuitId,
    claim: ClaimedLaunch,
    tree: PursuitTree,
  ): Promise<void> {
    const active = this.#active.get(pursuitId);
    let messages: InitialUserMessage[];
    try {
      const input =
        claim.kind === "plan"
          ? buildPlannerContextInput(tree, claim)
          : buildWorkerContextInput(tree, claim);
      messages = await this.#deps.compose(
        claim.agentName,
        input,
        active?.controller.signal,
      );
      if (messages.length === 0) throw new Error("composer returned no initial messages");
    } catch (error) {
      await this.#synthesizeFailure(
        pursuitId,
        claim,
        `context_script_error: ${describeError(error)}`,
      );
      return;
    }

    const permitted = await verifyClaimLaunchable(this.#deps.db, claim);
    if (!permitted) return;

    const launched = this.#deps.port.launch(claim.agentName, messages, {
      submission: this.#buildBinding(pursuitId, claim),
      ...(active && { signal: active.controller.signal }),
      ...(tree.pursuit.parentRunId !== null && { parent: tree.pursuit.parentRunId }),
    });
    await stampAgentRunId(this.#deps.db, claim, launched.runId);
    void launched.outcome
      .catch((): LaunchSettlement => ({ status: "failed" }))
      .then((settlement) => this.#onSettlement(pursuitId, claim, settlement))
      .catch(() => undefined);
  }

  #buildBinding(
    pursuitId: PursuitId,
    claim: ClaimedLaunch,
  ): PursuitAgentSubmissionBinding {
    if (claim.kind === "plan") {
      return {
        kind: "planner",
        submit: (payload) => this.#submitPlanner(pursuitId, claim, payload),
      };
    }
    return {
      kind: "worker",
      submit: (payload) => this.#submitWorker(pursuitId, claim, payload),
    };
  }

  async #submitPlanner(
    pursuitId: PursuitId,
    claim: Extract<ClaimedLaunch, { kind: "plan" }>,
    payload: PlannerOutcomePayload,
  ): Promise<SubmissionResult> {
    let error: string | undefined;
    await this.#mutate(pursuitId, async (trx, tree) => {
      if (!tree) {
        error = "unknown pursuit";
        return;
      }
      error = plannerSubmissionError(tree, claim, payload, {
        isRegisteredWorkerAgent: this.#deps.isRegisteredWorkerAgent,
      });
      if (error !== undefined) return;
      await applyPlannerSettlement(trx, tree, claim.planId, {
        kind: "submitted",
        payload,
      });
    });
    return error === undefined ? { ok: true } : { ok: false, error };
  }

  async #submitWorker(
    pursuitId: PursuitId,
    claim: Extract<ClaimedLaunch, { kind: "work_item" }>,
    payload: WorkerOutcomePayload,
  ): Promise<SubmissionResult> {
    await this.#mutate(pursuitId, async (trx, tree) => {
      if (!tree) return;
      await applyWorkItemSettlement(trx, tree, claim, {
        isPass: payload.is_pass,
        summary: payload.summary,
        outcome: payload.outcome,
      });
    });
    return { ok: true };
  }

  async #onSettlement(
    pursuitId: PursuitId,
    claim: ClaimedLaunch,
    settlement: LaunchSettlement,
  ): Promise<void> {
    await this.#synthesizeFailure(
      pursuitId,
      claim,
      `run settled '${settlement.status}' without a submission`,
    );
  }

  async #synthesizeFailure(
    pursuitId: PursuitId,
    claim: ClaimedLaunch,
    reason: string,
  ): Promise<void> {
    await this.#mutate(pursuitId, async (trx, tree) => {
      if (!tree) return;
      if (claim.kind === "plan") {
        await applyPlannerSettlement(trx, tree, claim.planId, {
          kind: "failed",
          reason,
        });
        return;
      }
      await applyWorkItemSettlement(trx, tree, claim, {
        isPass: false,
        summary: reason,
        outcome: reason,
      });
    });
  }

  #resolveTerminal(pursuitId: PursuitId, tree: PursuitTree): void {
    const status = tree.pursuit.status;
    if (!isPursuitEntityTerminal(status)) return;
    const active = this.#active.get(pursuitId);
    if (!active) return;
    this.#active.delete(pursuitId);
    active.terminal.resolve({
      status,
      summary: terminalSummary(tree, active.cancelReason),
    });
  }
}

function plannerSubmissionError(
  tree: PursuitTree,
  claim: Extract<ClaimedLaunch, { kind: "plan" }>,
  payload: PlannerOutcomePayload,
  deps: { isRegisteredWorkerAgent(agentName: string): boolean },
): string | undefined {
  const leg = tree.legs.find((candidate) => candidate.id === claim.legId);
  const attempt = leg?.attempts.find((candidate) => candidate.id === claim.attemptId);
  if (!leg || !attempt) return "unknown leg attempt";

  if (
    tree.pursuit.legGoalMode === "predefined" &&
    (payload.leg_goal !== undefined || payload.next_leg_goal !== undefined)
  ) {
    return "predefined leg goals cannot be refocused or declare next_leg_goal";
  }

  const currentIds = new Set<string>();
  for (const item of payload.work_items) {
    if (currentIds.has(item.id)) return `duplicate work item id "${item.id}"`;
    currentIds.add(item.id);
    if (!deps.isRegisteredWorkerAgent(item.agent_name)) {
      return `work item "${item.id}" names unknown worker agent "${item.agent_name}"`;
    }
  }

  const allExisting = tree.legs.flatMap((candidateLeg) =>
    candidateLeg.attempts.flatMap((candidateAttempt) =>
      candidateAttempt.workItems.map((item) => ({
        leg: candidateLeg,
        attempt: candidateAttempt,
        item,
      })),
    ),
  );
	  const existingInVersion = allExisting.filter(
	    (entry) =>
	      entry.leg.id === leg.id &&
	      entry.attempt.isConsistentWithLegGoal &&
	      entry.item.legGoalVersion === leg.legGoalVersion,
	  );
	  if (payload.leg_goal === undefined) {
	    for (const item of payload.work_items) {
	      if (existingInVersion.some((entry) => String(entry.item.id) === item.id)) {
	        return `duplicate work item id "${item.id}" in current leg goal version`;
	      }
	    }
	  }

	  for (const item of payload.work_items) {
	    for (const dependency of item.depends_on) {
	      if (currentIds.has(dependency)) continue;
	      if (payload.leg_goal !== undefined) {
	        return "replacement leg_goal submissions cannot depend_on prior work items";
	      }
	      const matching = allExisting.filter(
	        (entry) => String(entry.item.id) === dependency,
	      );
	      if (matching.length === 0) {
	        return `work item "${item.id}" depends_on unknown id "${dependency}"`;
	      }
	      const existing = matching.find(
	        (entry) =>
	          entry.leg.id === leg.id &&
	          entry.attempt.sequence < attempt.sequence &&
	          entry.attempt.isConsistentWithLegGoal &&
	          entry.item.legGoalVersion === leg.legGoalVersion,
	      );
	      if (existing) continue;
	      const first = matching[0];
	      if (first.leg.id !== leg.id) {
	        return `work item "${item.id}" depends_on item from another leg`;
	      }
	      if (first.attempt.sequence >= attempt.sequence) {
	        return `work item "${item.id}" depends_on future attempt item "${dependency}"`;
	      }
	      if (
	        !first.attempt.isConsistentWithLegGoal ||
	        first.item.legGoalVersion !== leg.legGoalVersion
	      ) {
	        return `work item "${item.id}" depends_on superseded leg-goal version item "${dependency}"`;
	      }
    }
  }

  return currentGraphCycle(payload);
}

function currentGraphCycle(payload: PlannerOutcomePayload): string | undefined {
  const graph = new Map(
    payload.work_items.map((item) => [
      item.id,
      item.depends_on.filter((dependency) =>
        payload.work_items.some((candidate) => candidate.id === dependency),
      ),
    ]),
  );
  const done = new Set<string>();
  const visiting = new Set<string>();
  const hasCycle = (id: string): boolean => {
    if (done.has(id)) return false;
    if (visiting.has(id)) return true;
    visiting.add(id);
    for (const dependency of graph.get(id) ?? []) {
      if (hasCycle(dependency)) return true;
    }
    visiting.delete(id);
    done.add(id);
    return false;
  };
  for (const id of graph.keys()) {
    if (hasCycle(id)) return "work item depends_on contains a dependency cycle";
  }
  return undefined;
}

function terminalSummary(tree: PursuitTree, cancelReason?: string): string {
  switch (tree.pursuit.status) {
    case "Success": {
      const closing = tree.legs.at(-1)?.attempts.at(-1);
      return closing?.plan.summary ?? "pursuit completed";
    }
    case "Failed": {
      const reasons = [...tree.legs]
        .reverse()
        .flatMap((leg) => [...leg.attempts].reverse())
        .find((attempt) => attempt.failureReasons.length > 0)?.failureReasons;
      return reasons?.[0] ? formatAttemptFailureReason(reasons[0]) : "pursuit failed";
    }
    default:
      return cancelReason ?? "pursuit cancelled";
  }
}

function describeError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
