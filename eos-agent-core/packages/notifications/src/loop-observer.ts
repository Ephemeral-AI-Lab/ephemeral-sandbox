/**
 * Loop facts announced to the runtime after each committed assistant
 * turn; in-process camelCase. Two axes: the SHAPE of the turn that just
 * committed (`toolCalls`, `liveSessions`, `hasPendingSteers`) and the
 * run's BUDGET position (`turn`, `maxTurns`).
 */
export interface TurnFacts {
  /** Budget axis: 1-based number of the turn that just committed. */
  turn: number;
  /** Budget axis: the run's fixed turn budget (profile `max_turns`). */
  maxTurns: number;
  /** Shape axis: `tool_use` blocks in this turn; 0 means bare text. */
  toolCalls: number;
  /** Shape axis: running background sessions at this boundary. */
  liveSessions: number;
  /** Shape axis: a user steer is already queued at this boundary. */
  hasPendingSteers: boolean;
}

/**
 * The loop's announcement port. Implementations must never throw or
 * reject; the loop adds no defensive handling around these calls.
 */
export interface LoopObserver {
  /** Awaited after every committed assistant turn, before any branch. */
  turnCompleted(facts: TurnFacts): Promise<void>;
  /** The loop is entering auto-wait. */
  idleStarted(): void;
  /** The park woke (settlement, steer, abort) or the run is finishing. */
  idleEnded(): void;
}
