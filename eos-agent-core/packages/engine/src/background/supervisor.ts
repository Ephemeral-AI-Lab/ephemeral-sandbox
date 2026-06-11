import {
  systemNotificationMessage,
  type NotificationInbox,
} from "@eos/notifications";
import type {
  SessionHandle,
  SessionOutcome,
  SessionRef,
  SessionRow,
  SessionStatus,
} from "./session.js";

interface SessionEntry {
  ref: SessionRef;
  handle: SessionHandle;
  status: Exclude<SessionStatus, "delivered">;
  started_at: string;
  summary?: string;
}

function sessionKey(ref: SessionRef): string {
  return `${ref.type}:${ref.id}`;
}

function isSessionRef(tag: unknown): tag is SessionRef {
  return (
    typeof tag === "object" &&
    tag !== null &&
    typeof (tag as SessionRef).type === "string" &&
    typeof (tag as SessionRef).id === "string"
  );
}

/**
 * Generic session lifecycle, keyed by the native ids the model already
 * holds. Sessions are loop lifecycle: the loop's exit path calls
 * `dispose()` on every finish, and every terminal transition publishes one
 * `session_settled` notification whose drain marks the session delivered -
 * delivery bookkeeping never leaves this class.
 */
export class BackgroundSupervisor {
  readonly #inbox: NotificationInbox;
  readonly #sessions = new Map<string, SessionEntry>();
  #disposedReason: string | undefined;

  constructor(inbox: NotificationInbox) {
    this.#inbox = inbox;
    inbox.onDrained((tags) => {
      this.#markDelivered(tags);
    });
  }

  /**
   * Adopt a spawn site's capability handle. The supervisor owns rejection
   * mapping: a `settled` that rejects settles the session as `failed`, so
   * spawn sites hand over raw promise chains and no rejection escapes
   * unhandled. After `dispose()` the supervisor is latched: a late
   * registration (an abandoned `execute()` continuation finishing after an
   * abort) is immediately cancelled and nothing is registered or published.
   */
  register(ref: SessionRef, handle: SessionHandle): void {
    if (this.#disposedReason !== undefined) {
      handle.settled.catch(() => undefined);
      void handle.cancel(this.#disposedReason).catch(() => undefined);
      return;
    }
    const key = sessionKey(ref);
    this.#sessions.set(key, {
      ref,
      handle,
      status: "running",
      started_at: new Date().toISOString(),
    });
    handle.settled.then(
      (outcome) => {
        this.#settle(key, outcome);
      },
      (error: unknown) => {
        this.#settle(key, {
          status: "failed",
          summary: error instanceof Error ? error.message : String(error),
        });
      },
    );
  }

  /**
   * Transition to `cancelled` immediately and publish, then call the
   * handle's teardown; the late natural settle is dropped by the
   * status-machine guard. Returns false when the ref is unknown or the
   * session is no longer running.
   */
  async cancel(ref: SessionRef, reason: string): Promise<boolean> {
    const entry = this.#sessions.get(sessionKey(ref));
    if (entry?.status !== "running") return false;
    this.#transition(entry, { status: "cancelled", summary: reason });
    try {
      await entry.handle.cancel(reason);
    } catch {
      // Teardown failures never undo the recorded cancellation.
    }
    return true;
  }

  /** Running plus settled-but-undelivered sessions. */
  list(): SessionRow[] {
    return [...this.#sessions.values()].map((entry) => {
      const row: SessionRow = {
        type: entry.ref.type,
        id: entry.ref.id,
        status: entry.status,
        started_at: entry.started_at,
      };
      if (entry.summary !== undefined) row.summary = entry.summary;
      const description = entry.handle.describe?.();
      if (description !== undefined) row.description = description;
      return row;
    });
  }

  /** Running only - the loop's auto-wait gate. */
  liveCount(): number {
    let count = 0;
    for (const entry of this.#sessions.values()) {
      if (entry.status === "running") count += 1;
    }
    return count;
  }

  /**
   * Running plus undelivered-terminal - the submission guard. Guarding on
   * `liveCount` alone would make submit-vs-settle a race that silently
   * drops the pending notification on the allowed side.
   */
  openCount(): number {
    return this.#sessions.size;
  }

  /**
   * Cancel all running sessions and latch the supervisor. Fire-and-forget
   * from the loop: `run_finished` never waits on teardown. Nothing is
   * published - the run is over, so there is no drain left to deliver to.
   */
  async dispose(reason: string): Promise<void> {
    this.#disposedReason ??= reason;
    const running = [...this.#sessions.values()].filter(
      (entry) => entry.status === "running",
    );
    for (const entry of running) {
      entry.status = "cancelled";
      entry.summary = reason;
    }
    await Promise.allSettled(running.map((entry) => entry.handle.cancel(reason)));
  }

  /** A settle against a non-running session is dropped (cancel race). */
  #settle(key: string, outcome: SessionOutcome): void {
    const entry = this.#sessions.get(key);
    if (entry?.status !== "running") return;
    this.#transition(entry, outcome);
  }

  #transition(entry: SessionEntry, outcome: SessionOutcome): void {
    entry.status = outcome.status;
    entry.summary = outcome.summary;
    this.#inbox.publish(
      systemNotificationMessage({
        type: "session_settled",
        session: { type: entry.ref.type, id: entry.ref.id },
        status: outcome.status,
        summary: outcome.summary,
      }),
      { key: sessionKey(entry.ref), tag: entry.ref },
    );
  }

  /** Drained settlement tags: mark delivered, then evict. */
  #markDelivered(tags: unknown[]): void {
    for (const tag of tags) {
      if (!isSessionRef(tag)) continue;
      const key = sessionKey(tag);
      const entry = this.#sessions.get(key);
      if (!entry) continue;
      if (entry.status !== "running") this.#sessions.delete(key);
    }
  }
}
