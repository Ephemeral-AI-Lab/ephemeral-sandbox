import {
  Alert,
  Badge,
  Box,
  Code,
  Group,
  Paper,
  SimpleGrid,
  Stack,
  Table,
  Text,
} from "@mantine/core";
import {
  fetchCgroup,
  type WorkspaceProcess,
  type WorkspaceProcesses,
  type WorkspaceProcessTopology,
} from "@/api/observability";
import { useSandbox } from "@/pages/sandbox/SandboxContext";
import { usePoll } from "@/poll/usePoll";

const TOPOLOGY_WINDOW_MS = 60_000;

export function CgroupView() {
  const { sandboxId } = useSandbox();
  const result = usePoll({
    key: ["observability", sandboxId, "cgroup", "topology"],
    fn: () => fetchCgroup(sandboxId, "sandbox", TOPOLOGY_WINDOW_MS),
    mode: "slow",
  });

  return (
    <Stack gap="md" p="md" data-process-topology>
      {result.isError ? (
        <Alert color="red" role="alert" title={result.data ? "Process topology refresh failed" : "Process topology unavailable"}>
          {result.error.message} — retrying automatically.
        </Alert>
      ) : null}

      <ProcessTopologyPanel
        topology={result.data?.topology}
        pending={result.data === undefined && !result.isError}
      />
    </Stack>
  );
}

function ProcessTopologyPanel({
  topology,
  pending,
}: {
  topology?: WorkspaceProcessTopology;
  pending: boolean;
}) {
  return (
    <Paper withBorder p="md" component="section" aria-labelledby="process-topology-title">
      <Group justify="space-between" align="flex-start" gap="md">
        <Box>
          <Text id="process-topology-title" component="h2" size="lg" fw={600}>
            Workspace process topology
          </Text>
          <Text size="sm" c="dimmed">
            Processes assigned by PID and mount namespace identity
          </Text>
        </Box>
        <Stack gap={2} align="flex-end">
          <Badge color={topology?.available ? "success" : pending ? "neutral" : "yellow"} variant="light">
            {topology?.available ? formatSource(topology.source) : pending ? "loading" : "unavailable"}
          </Badge>
          <Text size="xs" c="dimmed">auto-refresh</Text>
        </Stack>
      </Group>

      {pending ? (
        <Text size="sm" c="dimmed" mt="md" role="status">Loading process topology…</Text>
      ) : topology?.available ? (
        <AvailableTopology topology={topology} />
      ) : (
        <Alert color="red" mt="md" role="alert" title="Process topology unavailable">
          {topology?.error ?? "Topology was not reported by this daemon."} The view will retry automatically.
        </Alert>
      )}
    </Paper>
  );
}

function AvailableTopology({ topology }: { topology: WorkspaceProcessTopology }) {
  return (
    <Stack gap="md" mt="md">
      {topology.truncated ? (
        <Alert color="dark" role="status" title="Process list truncated">
          The backend returned the first 512 matching processes.
        </Alert>
      ) : null}
      {topology.warnings.length > 0 ? (
        <Alert color="dark" role="status" title="Partial collection warnings">
          <Stack gap={2}>
            {topology.warnings.map((warning) => <Text size="xs" key={warning}>{warning}</Text>)}
          </Stack>
        </Alert>
      ) : null}
      {topology.workspaces.length === 0 ? (
        <Text size="sm" c="dimmed" role="status" data-process-topology-empty>
          No active workspaces. Process topology is available.
        </Text>
      ) : (
        topology.workspaces.map((workspace) => (
          <WorkspaceCard key={workspace.workspace_id} workspace={workspace} />
        ))
      )}
    </Stack>
  );
}

function WorkspaceCard({ workspace }: { workspace: WorkspaceProcesses }) {
  const hasWorkload = workspace.processes.some((process) => process.kind === "process");
  return (
    <Paper withBorder p="sm" component="article" data-workspace-id={workspace.workspace_id}>
      <Group justify="space-between" align="flex-start" gap="xs">
        <Text component="h3" fw={600} ff="monospace" size="sm" style={{ overflowWrap: "anywhere" }}>
          {workspace.workspace_id}
        </Text>
        <Badge color={workspaceColor(workspace.state)} variant="light">
          {workspace.state}
        </Badge>
      </Group>

      <SimpleGrid cols={{ base: 1, sm: 3 }} spacing="xs" mt="sm">
        <TopologyField label="Holder PID" value={String(workspace.holder_pid)} />
        <TopologyField label="PID namespace" value={workspace.pid_namespace ?? "unavailable"} />
        <TopologyField label="Mount namespace" value={workspace.mount_namespace ?? "unavailable"} />
      </SimpleGrid>

      {workspace.state === "partial" ? (
        <Alert color="dark" mt="sm" role="status" title="Workspace topology is partial">
          Some namespace or process metadata could not be read during this refresh.
        </Alert>
      ) : null}
      {!hasWorkload ? (
        <Text size="sm" c="dimmed" mt="sm" role="status" data-workspace-idle>
          No workload processes. The namespace init process is idle.
        </Text>
      ) : null}

      {workspace.processes.length > 0 ? (
        <>
          <Box visibleFrom="sm" mt="sm">
            <Table.ScrollContainer minWidth={760}>
              <Table striped highlightOnHover aria-label={`Processes in ${workspace.workspace_id}`}>
                <Table.Thead>
                  <Table.Tr>
                    <Table.Th>Name</Table.Th>
                    <Table.Th>PID</Table.Th>
                    <Table.Th>Namespace PID</Table.Th>
                    <Table.Th>State</Table.Th>
                    <Table.Th>Kind</Table.Th>
                    <Table.Th>Cgroup membership</Table.Th>
                  </Table.Tr>
                </Table.Thead>
                <Table.Tbody>
                  {workspace.processes.map((process) => <ProcessRow key={process.pid} process={process} />)}
                </Table.Tbody>
              </Table>
            </Table.ScrollContainer>
          </Box>
          <Stack hiddenFrom="sm" gap="xs" mt="sm">
            {workspace.processes.map((process) => <ProcessCard key={process.pid} process={process} />)}
          </Stack>
        </>
      ) : null}
    </Paper>
  );
}

function ProcessRow({ process }: { process: WorkspaceProcess }) {
  return (
    <Table.Tr data-process-pid={process.pid}>
      <Table.Td>{process.name}</Table.Td>
      <Table.Td ff="monospace">{process.pid}</Table.Td>
      <Table.Td ff="monospace">{process.namespace_pid}</Table.Td>
      <Table.Td>{process.state}</Table.Td>
      <Table.Td>{formatKind(process.kind)}</Table.Td>
      <Table.Td><Memberships process={process} /></Table.Td>
    </Table.Tr>
  );
}

function ProcessCard({ process }: { process: WorkspaceProcess }) {
  return (
    <Paper withBorder p="xs" data-process-pid={process.pid}>
      <Text fw={600} size="sm" style={{ overflowWrap: "anywhere" }}>{process.name}</Text>
      <SimpleGrid cols={2} spacing={4} mt={4}>
        <TopologyField label="PID" value={String(process.pid)} />
        <TopologyField label="Namespace PID" value={String(process.namespace_pid)} />
        <TopologyField label="State" value={process.state} />
        <TopologyField label="Kind" value={formatKind(process.kind)} />
      </SimpleGrid>
      <Box mt="xs">
        <Text size="xs" c="dimmed">Cgroup membership</Text>
        <Memberships process={process} />
      </Box>
    </Paper>
  );
}

function Memberships({ process }: { process: WorkspaceProcess }) {
  return process.cgroup_memberships.length > 0 ? (
    <Stack gap={1}>
      {process.cgroup_memberships.map((membership) => (
        <Code key={membership} style={{ overflowWrap: "anywhere", whiteSpace: "normal" }}>{membership}</Code>
      ))}
    </Stack>
  ) : <Text size="xs" c="dimmed">Not reported</Text>;
}

function TopologyField({ label, value }: { label: string; value: string }) {
  return (
    <Box component="dl" m={0} style={{ minWidth: 0 }}>
      <Text component="dt" size="xs" c="dimmed">{label}</Text>
      <Text component="dd" m={0} mt={2} ff="monospace" size="xs" style={{ overflowWrap: "anywhere" }}>
        {value}
      </Text>
    </Box>
  );
}

function workspaceColor(state: WorkspaceProcesses["state"]) {
  if (state === "active") return "success";
  if (state === "partial") return "neutral";
  return "neutral";
}

function formatKind(kind: WorkspaceProcess["kind"]) {
  return kind === "namespace_init" ? "namespace init" : "process";
}

function formatSource(source: WorkspaceProcessTopology["source"]) {
  return source === "proc_namespaces" ? "proc namespaces" : "unknown source";
}
