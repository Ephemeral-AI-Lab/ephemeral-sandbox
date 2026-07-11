import { useEffect, useMemo, useState } from "react";
import {
  Alert,
  Badge,
  Box,
  Button,
  Flex,
  Group,
  Paper,
  ScrollArea,
  Select,
  Stack,
  Text,
  Tooltip,
  UnstyledButton,
} from "@mantine/core";
import {
  fetchLayerStack,
  fetchLayerStackLayer,
  fetchLayerStackWorkspace,
  type LayerStackLayerEntry,
  type LayerStackLayerEntryKind,
  type LayerStackLayerResult,
  type LayerStackWorkspaceResult,
  type StackLayer,
} from "@/api/observability";
import { usePoll } from "@/poll/usePoll";
import { useSandbox } from "@/pages/sandbox/SandboxContext";
import { SquashDialog } from "@/components/SquashDialog";
import { ResourceSparkline } from "@/components/ResourceSparkline";
import { formatBytes, shortHash } from "@/lib/format";

const ALL_WORKSPACES = "__all__";
const depthRings = new Map<string, number[]>();

function recordDepth(sandboxId: string, depth: number): number[] {
  const ring = depthRings.get(sandboxId) ?? [];
  if (ring.length === 0 || ring.at(-1) !== depth) {
    ring.push(depth);
    if (ring.length > 24) ring.shift();
    depthRings.set(sandboxId, ring);
  }
  return [...ring];
}

export function LayerStackView() {
  const { sandboxId, snapshot } = useSandbox();
  const [workspaceId, setWorkspaceId] = useState(ALL_WORKSPACES);
  const [selectedLayerId, setSelectedLayerId] = useState<string | null>(null);
  const workspaces = snapshot?.sandboxes[0]?.workspaces ?? [];
  const stack = usePoll({
    key: ["observability", sandboxId, "layerstack"],
    fn: () => fetchLayerStack(sandboxId),
    mode: "slow",
  });
  const workspaceDetail = usePoll({
    key: ["observability", sandboxId, "layerstack", "workspace", workspaceId],
    fn: () => fetchLayerStackWorkspace(sandboxId, workspaceId),
    mode: "slow",
    enabled: workspaceId !== ALL_WORKSPACES,
  });
  const layerDetail = usePoll({
    key: ["observability", sandboxId, "layerstack", "layer", selectedLayerId ?? ""],
    fn: () => fetchLayerStackLayer(sandboxId, selectedLayerId ?? ""),
    mode: "slow",
    enabled: selectedLayerId !== null,
  });

  const layers = stack.data?.layers ?? [];
  const squashableRuns = useMemo(() => squashRuns(layers), [layers]);
  const depthTrend = stack.data ? recordDepth(sandboxId, layers.length) : [];
  const maxBytes = Math.max(1, ...layers.map((layer) => layer.bytes));
  const selectedLayer = layers.find((layer) => layer.layer_id === selectedLayerId) ?? null;
  const workspaceData = workspaceDetail.data?.workspace === workspaceId ? workspaceDetail.data : null;
  const layerData = layerDetail.data?.layer_id === selectedLayerId ? layerDetail.data : null;

  useEffect(() => {
    if (workspaceId !== ALL_WORKSPACES && !workspaces.some((workspace) => workspace.workspace_id === workspaceId)) {
      setWorkspaceId(ALL_WORKSPACES);
    }
  }, [workspaceId, workspaces]);
  useEffect(() => {
    if (layers.length === 0) {
      setSelectedLayerId(null);
    } else if (!selectedLayerId || !layers.some((layer) => layer.layer_id === selectedLayerId)) {
      setSelectedLayerId(layers[0].layer_id);
    }
  }, [layers, selectedLayerId]);

  if (stack.isError && !stack.data) {
    return <Alert color="red" title="Layer stack unavailable" m="md">{(stack.error as Error).message}</Alert>;
  }

  return (
    <Stack gap="md" p="md" data-layer-stack-view>
      <Group gap="md" align="end" wrap="wrap">
        <Text size="xs">manifest v <Text component="span" ff="monospace">{stack.data?.manifest_version ?? "..."}</Text></Text>
        <Text ff="monospace" size="xs" title="root hash">{stack.data ? shortHash(stack.data.root_hash, 12) : "..."}</Text>
        <Text size="xs">{layers.length} layers · {formatBytes(stack.data?.total_bytes ?? 0)}</Text>
        <Text size="xs">{stack.data?.active_lease_count ?? 0} active leases</Text>
        <Select
          label="Workspace"
          size="xs"
          value={workspaceId}
          onChange={(value) => setWorkspaceId(value ?? ALL_WORKSPACES)}
          data={[
            { value: ALL_WORKSPACES, label: "all workspaces" },
            ...workspaces.map((workspace) => ({ value: workspace.workspace_id, label: `workspace · ${workspace.workspace_id}` })),
          ]}
          ml="auto"
          style={{ width: "16rem" }}
        />
        <SquashDialog
          sandboxId={sandboxId}
          layerCount={layers.length}
          trigger={(open) => <Button variant="filled" onClick={open}>Squash ({layers.length})</Button>}
        />
      </Group>

      {stack.isError ? <Alert color="yellow" title="Showing the last confirmed stack">{(stack.error as Error).message}</Alert> : null}
      <Flex gap="md" direction={{ base: "column", lg: "row" }} align="stretch">
        <Paper withBorder p="sm" style={{ flex: 1, minWidth: 0 }}>
          {layers.length === 0 ? (
            <Text size="sm" c="dimmed">no layers yet</Text>
          ) : (
            <Stack component="ul" gap={4} m={0} p={0} style={{ listStyle: "none" }}>
              {layers.map((layer, index) => (
                <LayerRow
                  key={layer.layer_id}
                  layer={layer}
                  index={index}
                  total={layers.length}
                  maxBytes={maxBytes}
                  squashable={squashableRuns.has(index)}
                  selected={layer.layer_id === selectedLayerId}
                  onSelect={setSelectedLayerId}
                />
              ))}
            </Stack>
          )}
        </Paper>
        <Stack gap="md" style={{ flex: "0 1 20rem", minWidth: 0 }}>
          <LayerDeltaPanel layer={selectedLayer} data={layerData} isError={layerDetail.isError && !layerData} error={layerDetail.error} isFetching={layerDetail.isFetching && !layerData} />
          <WorkspacePanel workspaceId={workspaceId} workspaceCount={workspaces.length} data={workspaceData} isError={workspaceDetail.isError && !workspaceData} error={workspaceDetail.error} isFetching={workspaceDetail.isFetching && !workspaceData} />
          <Paper withBorder p="md">
            <Text component="h3" size="sm" fw={600} c="dimmed" mb="xs">Stack depth</Text>
            <ResourceSparkline values={depthTrend} width={220} height={36} label="stack depth" />
            <Text size="xs" c="dimmed" mt="xs">{layers.length} now · trend accumulates while this view polls</Text>
          </Paper>
          <Alert color="blue" title="Squashable layers">
            Contiguous published layers with no live leases or bookings are marked. The base layer stays.
          </Alert>
        </Stack>
      </Flex>
    </Stack>
  );
}

function LayerRow({
  layer,
  index,
  total,
  maxBytes,
  squashable,
  selected,
  onSelect,
}: {
  layer: StackLayer;
  index: number;
  total: number;
  maxBytes: number;
  squashable: boolean;
  selected: boolean;
  onSelect: (layerId: string) => void;
}) {
  const isBase = index === total - 1;
  const level = total - index - 1;
  const leased = layer.leased_by_workspaces > 0 || layer.booked_by.length > 0;
  return (
    <Box component="li">
      <UnstyledButton
        aria-pressed={selected}
        data-layer-row={layer.layer_id}
        onClick={() => onSelect(layer.layer_id)}
        p="sm"
        style={{
          background: selected ? "var(--mantine-color-eyeBlue-0)" : squashable ? "var(--mantine-color-eyeBlue-0)" : undefined,
          border: `1px solid ${selected ? "var(--mantine-color-eyeBlue-5)" : "var(--mantine-color-neutral-3)"}`,
          borderRadius: "var(--mantine-radius-sm)",
          display: "block",
          width: "100%",
        }}
      >
        <Group gap="sm" wrap="nowrap">
          <Text ff="monospace" size="xs" c="dimmed" w={32}>{isBase ? "base" : `L${level}`}</Text>
          <Text ff="monospace" size="xs" truncate style={{ flex: "0 1 10rem" }} title={layer.layer_id}>{layer.layer_id}</Text>
          <Box visibleFrom="sm" style={{ flex: 1, minWidth: 0 }}>
            <Box title={`${layer.bytes} bytes`} style={{ background: "var(--mantine-color-eyeBlue-4)", borderRadius: "var(--mantine-radius-xs)", height: 8, width: `${Math.max((layer.bytes / maxBytes) * 100, 2)}%` }} />
          </Box>
          <Text ff="monospace" size="xs" c="dimmed" ta="right" w={64}>{formatBytes(layer.bytes)}</Text>
          <Tooltip label={`${layer.leased_by_workspaces} workspace lease(s)${layer.booked_by.length ? ` · booked by ${layer.booked_by.join(", ")}` : ""}`} openDelay={300}>
            <Text size="xs" c={leased ? "warning.8" : "dimmed"} ta="right" w={72}>
              {layer.leased_by_workspaces} leases{layer.booked_by.length ? ` +${layer.booked_by.length}b` : ""}
            </Text>
          </Tooltip>
          {squashable ? <Badge variant="light">squashable</Badge> : null}
        </Group>
      </UnstyledButton>
    </Box>
  );
}

function LayerDeltaPanel({ layer, data, isError, error, isFetching }: {
  layer: StackLayer | null;
  data: LayerStackLayerResult | null;
  isError: boolean;
  error: unknown;
  isFetching: boolean;
}) {
  return (
    <Paper withBorder p="md">
      <Group gap="xs" mb="xs">
        <Text component="h3" size="sm" fw={600} c="dimmed">Changed paths</Text>
        {layer ? <Text ff="monospace" size="xs" c="dimmed" truncate>{shortHash(layer.layer_id, 10)}</Text> : null}
      </Group>
      {layer ? <Text size="xs" c="dimmed" mb="sm">{formatBytes(layer.bytes)}</Text> : null}
      {isError ? <Alert color="red">{(error as Error).message}</Alert> : isFetching ? <Text size="xs" c="dimmed">loading…</Text> : data ? <LayerEntryList entries={data.entries} truncated={data.truncated} /> : <Text size="xs" c="dimmed">no layer selected</Text>}
    </Paper>
  );
}

function LayerEntryList({ entries, truncated }: { entries: LayerStackLayerEntry[]; truncated: boolean }) {
  if (entries.length === 0) return <Text size="xs" c="dimmed">no changed paths</Text>;
  return (
    <Stack gap="xs">
      <ScrollArea.Autosize mah={256} type="auto" viewportProps={{ "aria-label": "Layer entries", tabIndex: 0 }}>
        <Stack component="ul" gap={4} m={0} p={0} style={{ listStyle: "none" }}>
          {entries.map((entry) => (
            <Group component="li" key={`${entry.kind}:${entry.path}`} gap="xs" wrap="nowrap">
              <Badge color={entryKindColor(entry.kind)} variant="light" style={{ flexShrink: 0 }}>{entryKindLabel(entry.kind)}</Badge>
              <Text ff="monospace" size="xs" truncate title={entry.path}>{entry.path}</Text>
            </Group>
          ))}
        </Stack>
      </ScrollArea.Autosize>
      {truncated ? <Text size="xs" c="warning.8">Showing the first 500 entries returned by the backend.</Text> : null}
    </Stack>
  );
}

function WorkspacePanel({ workspaceId, workspaceCount, data, isError, error, isFetching }: {
  workspaceId: string;
  workspaceCount: number;
  data: LayerStackWorkspaceResult | null;
  isError: boolean;
  error: unknown;
  isFetching: boolean;
}) {
  return (
    <Paper withBorder p="md">
      <Text component="h3" size="sm" fw={600} c="dimmed" mb="sm">Workspace mounts</Text>
      {workspaceId === ALL_WORKSPACES ? (
        <Text size="xs" c="dimmed">{workspaceCount} live workspace{workspaceCount === 1 ? "" : "s"}</Text>
      ) : isError ? <Alert color="red">{(error as Error).message}</Alert> : isFetching ? <Text size="xs" c="dimmed">loading…</Text> : data ? (
        <Stack gap="xs">
          <Group justify="space-between">
            <Text ff="monospace" size="xs">{data.workspace}</Text>
            <Text size="xs" c="dimmed">{formatBytes(data.upper_bytes ?? 0)} upper</Text>
          </Group>
          <ScrollArea.Autosize mah={160} type="auto" viewportProps={{ "aria-label": "Workspace mount list", tabIndex: 0 }}>
            <Stack component="ul" gap={4} m={0} p={0} style={{ listStyle: "none" }}>
              {data.mounts.map((mount) => (
                <Paper component="li" withBorder key={mount.layer_id} p="xs">
                  <Text ff="monospace" size="xs" truncate title={mount.layer_id}>{mount.layer_id}</Text>
                  <Text size="xs" c="dimmed">shared with {mount.shared_with.length}</Text>
                </Paper>
              ))}
            </Stack>
          </ScrollArea.Autosize>
        </Stack>
      ) : <Text size="xs" c="dimmed">no workspace selected</Text>}
    </Paper>
  );
}

function entryKindLabel(kind: LayerStackLayerEntryKind): string {
  if (kind === "directory") return "dir";
  if (kind === "opaque_dir") return "opaque";
  if (kind === "symlink") return "link";
  return kind;
}

function entryKindColor(kind: LayerStackLayerEntryKind): string {
  if (kind === "delete") return "red";
  if (kind === "directory") return "blue";
  if (kind === "opaque_dir") return "yellow";
  return "gray";
}

function squashRuns(layers: StackLayer[]): Set<number> {
  const marks = new Set<number>();
  let runStart: number | null = null;
  const flush = (end: number) => {
    if (runStart !== null && end - runStart >= 2) {
      for (let index = runStart; index < end; index += 1) marks.add(index);
    }
    runStart = null;
  };
  for (let index = 0; index < layers.length; index += 1) {
    const layer = layers[index];
    const free = index !== layers.length - 1 && layer.leased_by_workspaces === 0 && layer.booked_by.length === 0;
    if (free && runStart === null) runStart = index;
    if (!free) flush(index);
  }
  flush(layers.length);
  return marks;
}
