import { keepPreviousData, useQueries } from "@tanstack/react-query";
import { useEffect, useMemo, useRef } from "react";
import type { SandboxRecord } from "@/api/types";
import {
  fetchSandboxSnapshot,
  type SnapshotResult,
} from "@/api/observability";
import { FAST_POLL_MS } from "@/poll/usePoll";
import {
  shouldRequestSandboxSnapshot,
  snapshotHasActivity,
} from "@/poll/useSandboxSnapshot";

/**
 * Resolve fleet snapshots per sandbox so a revision or active execution in one
 * sandbox never wakes every other daemon through an aggregate request.
 */
export function useFleetSnapshots(records: SandboxRecord[]) {
  const requestedRevisions = useRef(new Map<string, number | null>());
  const recordIds = records.map((record) => record.id);

  useEffect(() => {
    const live = new Set(recordIds);
    for (const sandboxId of requestedRevisions.current.keys()) {
      if (!live.has(sandboxId)) requestedRevisions.current.delete(sandboxId);
    }
  }, [recordIds.join("\0")]);

  const queries = useQueries({
    queries: records.map((record) => ({
      queryKey: ["sandbox", record.id, "snapshot"],
      queryFn: async ({ signal }: { signal: AbortSignal }) => {
        const revision =
          typeof record.activity_revision === "number"
            ? record.activity_revision
            : null;
        // Record the attempt before I/O so a failed initial resolution cannot
        // turn into continuous idle daemon polling.
        requestedRevisions.current.set(record.id, revision);
        return fetchSandboxSnapshot(record.id, signal);
      },
      enabled: (query: { state: { data: unknown } }) => {
        const data = query.state.data as SnapshotResult | undefined;
        if (record.state !== "ready") return false;
        if (data === undefined) {
          const requested = requestedRevisions.current.get(record.id);
          const revision =
            typeof record.activity_revision === "number"
              ? record.activity_revision
              : null;
          return !requestedRevisions.current.has(record.id) || requested !== revision;
        }
        return shouldRequestSandboxSnapshot(
          record,
          data,
          requestedRevisions.current.get(record.id) ?? null,
        );
      },
      retry: false,
      placeholderData: keepPreviousData,
      refetchOnWindowFocus: false,
      refetchIntervalInBackground: false,
      refetchInterval: (query: { state: { data: unknown } }) => {
        const data = query.state.data as SnapshotResult | undefined;
        return snapshotHasActivity(record, data) ? FAST_POLL_MS : false;
      },
    })),
  });

  const data = useMemo<SnapshotResult>(
    () => ({
      sandboxes: queries.flatMap(
        (query) => query.data?.sandboxes ?? [],
      ),
    }),
    [queries],
  );

  return {
    data,
    isFetching: queries.some((query) => query.isFetching),
    error: queries.find((query) => query.error)?.error ?? null,
  };
}
