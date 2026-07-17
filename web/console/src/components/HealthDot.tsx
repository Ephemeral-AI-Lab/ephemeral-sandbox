import { Box, Text, Tooltip } from "@mantine/core";
import type { Endpoint } from "@/api/types";

/**
 * Manager-owned daemon endpoint readiness. This is derived from the sandbox
 * record and never probes the daemon from an idle console page.
 */
export function HealthDot({
  endpoint,
  showLabel = false,
}: {
  endpoint: Endpoint | null;
  showLabel?: boolean;
}) {
  const ready = endpoint !== null;
  const label = ready
    ? "daemon_http endpoint registered"
    : "daemon_http endpoint unavailable";
  const color = ready
    ? "var(--mantine-color-success-6)"
    : "var(--mantine-color-danger-6)";

  return (
    <Tooltip label={label} openDelay={300}>
      <Box component="span" style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
        <Box
          component="span"
          style={{ display: "inline-block", width: 8, height: 8, borderRadius: "50%", background: color }}
          role="img"
          aria-label={label}
        />
        {showLabel ? (
          <Text component="span" size="xs" c="dimmed">
            {ready ? "http ready" : "http unavailable"}
          </Text>
        ) : null}
      </Box>
    </Tooltip>
  );
}
