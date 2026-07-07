import { useQuery } from "@tanstack/react-query";
import type { HealthProbe } from "@/api/types";

const HEALTH_POLL_MS = 10_000;

async function fetchHealth(sandboxId: string): Promise<HealthProbe> {
  const response = await fetch(
    `/api/sandboxes/${encodeURIComponent(sandboxId)}/health`,
  );
  const body = (await response.json().catch(() => null)) as
    | (HealthProbe & { error?: { message?: string } })
    | null;
  if (response.ok && body?.status) {
    return { status: body.status, detail: body.detail };
  }
  return {
    status: "unreachable",
    detail: body?.error?.message ?? `HTTP ${response.status}`,
  };
}

/**
 * The health-dot poll. Deliberately on its own slow cadence (10s per
 * sandbox) rather than fanning out a probe per fleet poll cycle — the
 * accepted fan-out posture from the plan's risk list.
 */
export function useHealth(sandboxId: string, enabled = true) {
  return useQuery({
    queryKey: ["health", sandboxId],
    queryFn: () => fetchHealth(sandboxId),
    enabled,
    retry: false,
    refetchInterval: HEALTH_POLL_MS,
    refetchIntervalInBackground: false,
  });
}
