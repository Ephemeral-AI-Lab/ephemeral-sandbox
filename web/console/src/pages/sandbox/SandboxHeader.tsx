import type { SandboxRecord } from "@/api/types";
import type { SnapshotResult } from "@/api/observability";
import { HealthDot } from "@/components/HealthDot";
import { DestroyAction } from "@/components/DestroyAction";
import { PortPreview } from "@/components/PortPreview";
import { SquashDialog } from "@/components/SquashDialog";
import { StateBadge } from "@/components/StateBadge";
import { Badge, Box, Button, Group, Text, Tooltip } from "@mantine/core";
import { shortHash } from "@/lib/format";

export function previewScopes(snapshot: SnapshotResult | undefined) {
  const workspaces = snapshot?.sandboxes[0]?.workspaces ?? [];
  return [
    { id: "shared", label: "shared network", isolated: false },
    ...workspaces
      .filter((workspace) => workspace.network_profile === "isolated")
      .map((workspace) => ({
        id: workspace.workspace_id,
        label: `isolated · ${workspace.workspace_id}`,
        isolated: true,
      })),
  ];
}

export function SandboxHeader({
  sandboxId,
  record,
  snapshot,
}: {
  sandboxId: string;
  record: SandboxRecord | null;
  snapshot: SnapshotResult | undefined;
}) {
  const layers = snapshot?.sandboxes[0]?.stack.layer_count;
  return (
    <Box data-sandbox-header px="md" pt="sm">
      <Group gap="sm" wrap="wrap">
        <Text ff="monospace" fw={700} size="sm">{sandboxId}</Text>
        {record ? <StateBadge state={record.state} /> : null}
        {record?.state === "ready" ? <HealthDot endpoint={record.daemon_http} showLabel /> : null}
        {record ? (
          <Text c="dimmed" ff="monospace" size="xs" title="workspace bind root" truncate maw={360}>
            {record.workspace_root}
          </Text>
        ) : null}
        {record?.daemon ? (
          <Text c="dimmed" ff="monospace" size="xs" title="daemon RPC endpoint">
            rpc {record.daemon.host}:{record.daemon.port}
          </Text>
        ) : null}
        {record?.daemon_http ? (
          <Text c="dimmed" ff="monospace" size="xs" title="daemon_http endpoint">
            http {record.daemon_http.host}:{record.daemon_http.port}
          </Text>
        ) : null}
        {record?.shared_base ? (
          <Tooltip label={`shared read-only base · root ${record.shared_base.root_hash}`} openDelay={300}>
            <Badge color="neutral" ff="monospace" size="sm" variant="light">
              base {shortHash(record.shared_base.root_hash)}
            </Badge>
          </Tooltip>
        ) : null}
        <Group gap="xs" ml="auto">
          {record?.state === "ready" ? (
            <>
              <PortPreview sandboxId={sandboxId} scopes={previewScopes(snapshot)} />
              <SquashDialog
                sandboxId={sandboxId}
                layerCount={layers}
                trigger={(open) => <Button size="compact-xs" onClick={open}>Squash</Button>}
              />
            </>
          ) : null}
          <DestroyAction sandboxId={sandboxId} />
        </Group>
      </Group>
    </Box>
  );
}
