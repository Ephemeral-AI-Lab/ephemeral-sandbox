import { cn } from "@/lib/cn";
import { Tooltip } from "@/components/ui/tooltip";
import { useHealth } from "@/api/health";

/**
 * The daemon_http endpoint health dot, always paired with a tooltip label
 * (state colors never stand alone).
 */
export function HealthDot({
  sandboxId,
  enabled = true,
  showLabel = false,
}: {
  sandboxId: string;
  enabled?: boolean;
  showLabel?: boolean;
}) {
  const health = useHealth(sandboxId, enabled);
  const status = health.data?.status;
  const label =
    status === "ok"
      ? "daemon_http reachable"
      : status === "unreachable"
        ? `daemon_http unreachable: ${health.data?.detail ?? ""}`
        : "probing daemon_http…";
  return (
    <Tooltip content={label}>
      <span className="inline-flex items-center gap-1">
        <span
          className={cn(
            "inline-block size-2 rounded-full",
            status === "ok" && "bg-ok",
            status === "unreachable" && "bg-danger",
            !status && "bg-idle/40",
          )}
          role="img"
          aria-label={label}
        />
        {showLabel ? (
          <span className="text-xs text-ink-mid">
            {status === "ok" ? "http ok" : status === "unreachable" ? "http down" : "http ?"}
          </span>
        ) : null}
      </span>
    </Tooltip>
  );
}
