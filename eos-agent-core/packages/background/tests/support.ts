import type { BackgroundSessionOutcome } from "../src/index.js";

export interface TestBackgroundSessionHandle {
  handle: {
    settled: Promise<BackgroundSessionOutcome>;
    cancel(reason: string): Promise<void>;
    describe?(): string;
  };
  settle(outcome: BackgroundSessionOutcome): void;
  fail(error: Error): void;
  /** Reasons passed to `cancel`, in call order. */
  cancelled: string[];
}

/** A push-settled capability handle for supervisor tests. */
export function backgroundSessionHandle(
  options: { describe?: string } = {},
): TestBackgroundSessionHandle {
  let settle!: (outcome: BackgroundSessionOutcome) => void;
  let fail!: (error: Error) => void;
  const settled = new Promise<BackgroundSessionOutcome>((resolve, reject) => {
    settle = resolve;
    fail = reject;
  });
  const cancelled: string[] = [];
  const description = options.describe;
  return {
    handle: {
      settled,
      cancel: (reason) => {
        cancelled.push(reason);
        return Promise.resolve();
      },
      ...(description !== undefined && { describe: () => description }),
    },
    settle,
    fail,
    cancelled,
  };
}

/** One macrotask: every already-queued microtask has run by then. */
export function tick(): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(() => {
      resolve();
    }, 0);
  });
}

export function must<T>(value: T | undefined | null): T {
  if (value === undefined || value === null) {
    throw new Error("expected a value to be present");
  }
  return value;
}
