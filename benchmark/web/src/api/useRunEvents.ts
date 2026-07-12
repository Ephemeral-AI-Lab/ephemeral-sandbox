import { useEffect, useState } from "react";
import { benchmarkApi } from "./client";
import { decodeEventStream } from "./sse";
import type { EventRecord } from "./types";

export type EventConnectionState = "connecting" | "live" | "reconnecting" | "replaying" | "stale";

interface RunEventsState {
  connectionState: EventConnectionState;
  latestEvent: EventRecord | null;
  lastEventId: number | null;
  records: EventRecord[];
  replayedEventCount: number;
  error: Error | null;
}

const STORAGE_PREFIX = "eos-benchmark:last-event:";
const MAX_VISIBLE_EVENTS = 500;

function storedSequence(runId: string): number | null {
  if (!runId) return null;
  const value = window.sessionStorage.getItem(`${STORAGE_PREFIX}${runId}`);
  if (value === null) return null;
  const sequence = Number(value);
  return Number.isSafeInteger(sequence) && sequence >= 0 ? sequence : null;
}

function asError(value: unknown): Error {
  return value instanceof Error ? value : new Error("The benchmark event stream disconnected.");
}

export function useRunEvents(runId: string, expectedSequence: number | null): RunEventsState {
  const [state, setState] = useState<RunEventsState>(() => ({
    connectionState: "connecting",
    latestEvent: null,
    lastEventId: storedSequence(runId),
    records: [],
    replayedEventCount: 0,
    error: null,
  }));

  useEffect(() => {
    if (!runId || expectedSequence === null) return;
    const replayBoundary = expectedSequence;

    let disposed = false;
    let retryTimer: number | null = null;
    let resolveRetry: (() => void) | null = null;
    let failures = 0;
    let controller: AbortController | null = null;
    let lastSequence = storedSequence(runId);

    setState({
      connectionState: "connecting",
      latestEvent: null,
      lastEventId: lastSequence,
      records: [],
      replayedEventCount: 0,
      error: null,
    });

    const waitToRetry = (delay: number) =>
      new Promise<void>((resolve) => {
        resolveRetry = resolve;
        retryTimer = window.setTimeout(() => {
          retryTimer = null;
          resolveRetry = null;
          resolve();
        }, delay);
      });

    const connect = async () => {
      while (!disposed) {
        let receivedEvent = false;
        controller = new AbortController();
        setState((current) => ({
          ...current,
          connectionState: lastSequence === null && failures === 0 ? "connecting" : "reconnecting",
        }));

        try {
          const headers = new Headers({ Accept: "text/event-stream" });
          if (lastSequence !== null) headers.set("Last-Event-ID", String(lastSequence));
          const response = await fetch(benchmarkApi.eventsUrl(runId), {
            method: "GET",
            headers,
            credentials: "same-origin",
            cache: "no-store",
            signal: controller.signal,
          });
          if (!response.ok) throw new Error(`The benchmark event stream returned HTTP ${response.status}.`);
          if (!response.body) throw new Error("The benchmark event stream has no response body.");
          const decoder = new TextDecoder();

          // The run snapshot is the immutable boundary between persisted replay
          // and subsequent live records for this connection. Do not use a later
          // polling snapshot here: it can advance after the stream is open and
          // mislabel an exact replay gap as live traffic.
          const replaying = (lastSequence ?? 0) < replayBoundary;
          setState((current) => ({
            ...current,
            connectionState: replaying ? "replaying" : "live",
            error: null,
          }));

          const reader = response.body.getReader();
          let buffer = "";
          while (!disposed) {
            const chunk = await reader.read();
            if (chunk.done) throw new Error("The benchmark event stream closed.");
            buffer += decoder.decode(chunk.value, { stream: true });
            const decoded = decodeEventStream(buffer);
            buffer = decoded.remainder;
            for (const record of decoded.records) {
              if (record.run_id !== runId) throw new Error("The event stream returned a different run id.");
              if (lastSequence !== null && record.sequence <= lastSequence) continue;
              receivedEvent = true;
              lastSequence = record.sequence;
              window.sessionStorage.setItem(`${STORAGE_PREFIX}${runId}`, String(record.sequence));
              setState((current) => ({
                connectionState:
                  record.sequence >= replayBoundary ? "live" : "replaying",
                latestEvent: record,
                lastEventId: record.sequence,
                records: [...current.records, record].slice(-MAX_VISIBLE_EVENTS),
                replayedEventCount:
                  current.replayedEventCount +
                  (record.sequence <= replayBoundary ? 1 : 0),
                error: null,
              }));
            }
          }
        } catch (error) {
          if (disposed || controller.signal.aborted) return;
          failures = receivedEvent ? 1 : failures + 1;
          setState((current) => ({
            ...current,
            connectionState: failures >= 3 ? "stale" : "reconnecting",
            error: asError(error),
          }));
          await waitToRetry(Math.min(1_000 * 2 ** (failures - 1), 10_000));
        }
      }
    };

    void connect();
    return () => {
      disposed = true;
      controller?.abort();
      if (retryTimer !== null) window.clearTimeout(retryTimer);
      resolveRetry?.();
    };
  }, [runId, expectedSequence === null]);

  return state;
}
