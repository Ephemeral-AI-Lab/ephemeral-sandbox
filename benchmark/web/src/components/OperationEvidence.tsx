import {
  Accordion,
  Alert,
  Badge,
  Card,
  Code,
  Group,
  SimpleGrid,
  Stack,
  Table,
  Text,
  Title,
} from "@mantine/core";
import type { ReactNode } from "react";
import type {
  Availability,
  OperationEvidence as OperationEvidenceValue,
  OperationEvidenceReport,
  SquashLayerstackEvidence,
  StorageSnapshot,
} from "@/api/types";
import { formatBytes, formatDurationNs, formatInteger, labelIdentifier } from "@/lib/format";

interface Fact {
  label: string;
  value: ReactNode;
}

function Facts({ facts }: { facts: Fact[] }) {
  return (
    <SimpleGrid cols={{ base: 2, sm: 3, lg: 5 }}>
      {facts.map(({ label, value }) => (
        <div key={label}>
          <Text component="div" fw={700}>{value}</Text>
          <Text size="xs" c="dimmed">{label}</Text>
        </div>
      ))}
    </SimpleGrid>
  );
}

function BooleanBadge({ value, positive = true }: { value: boolean; positive?: boolean }) {
  return <Badge color={value === positive ? "green" : "red"}>{value ? "Yes" : "No"}</Badge>;
}

function Hash({ value }: { value: string }) {
  return <Code className="wrap-anywhere">{value}</Code>;
}

function availabilityValue<T>(
  availability: Availability<T>,
  format: (value: T) => ReactNode,
): ReactNode {
  if (availability.availability === "available") return format(availability.value);
  return (
    <Stack gap={2}>
      <Badge color="gray" variant="light">Unavailable</Badge>
      <Text size="xs" c="dimmed" className="wrap-anywhere">
        {availability.source}: {availability.reason}
      </Text>
    </Stack>
  );
}

function idSet(ids: string[]) {
  if (ids.length === 0) return <Text c="dimmed">∅ — empty set</Text>;
  return (
    <Group gap="xs">
      {ids.map((id) => <Code key={id} className="wrap-anywhere">{id}</Code>)}
    </Group>
  );
}

function SnapshotTable({ evidence }: { evidence: SquashLayerstackEvidence }) {
  const snapshots: { id: string; label: string; value: StorageSnapshot }[] = [
    { id: "S0", label: "Baseline", value: evidence.s0_baseline },
    { id: "S1", label: "Sampled peak", value: evidence.s1_sampled_peak },
    { id: "S2", label: "Post-commit", value: evidence.s2_post_commit },
    { id: "S3", label: "Settled", value: evidence.s3_settled },
  ];
  const integerAvailability = (value: Availability<number>) => availabilityValue(value, formatInteger);
  const byteAvailability = (value: Availability<number>) => availabilityValue(value, formatBytes);

  return (
    <Table.ScrollContainer
      minWidth={1280}
      scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "LayerStack storage snapshots" } }}
    >
      <Table striped withTableBorder>
        <Table.Thead>
          <Table.Tr>
            <Table.Th>Snapshot</Table.Th>
            <Table.Th>Sampled / offset</Table.Th>
            <Table.Th>Manifest / root</Table.Th>
            <Table.Th>Active layers / leases</Table.Th>
            <Table.Th>Active logical / allocated</Table.Th>
            <Table.Th>Storage logical / allocated</Table.Th>
            <Table.Th>Staging entries</Table.Th>
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {snapshots.map(({ id, label, value }) => (
            <Table.Tr key={id}>
              <Table.Td><Text fw={700}>{id}</Text><Text size="xs" c="dimmed">{label}</Text></Table.Td>
              <Table.Td>
                <Badge color={value.sampled ? "blue" : "gray"} variant="light">{value.sampled ? "Sampled" : "Point observation"}</Badge>
                <Text component="div" size="xs" mt={4}>{availabilityValue(value.monotonic_offset_ns, formatDurationNs)}</Text>
              </Table.Td>
              <Table.Td>
                <Text component="div" size="sm">v{integerAvailability(value.manifest_version)}</Text>
                <Text component="div" size="xs" className="wrap-anywhere">{availabilityValue(value.root_hash, (hash) => <Code>{hash}</Code>)}</Text>
              </Table.Td>
              <Table.Td>{integerAvailability(value.active_layer_count)} / {integerAvailability(value.active_lease_count)}</Table.Td>
              <Table.Td>{byteAvailability(value.active_logical_bytes)} / {byteAvailability(value.active_allocated_bytes)}</Table.Td>
              <Table.Td>{byteAvailability(value.storage_logical_bytes)} / {byteAvailability(value.storage_allocated_bytes)}</Table.Td>
              <Table.Td>{integerAvailability(value.staging_entry_count)}</Table.Td>
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
    </Table.ScrollContainer>
  );
}

function LayerStackEvidence({ evidence }: { evidence: SquashLayerstackEvidence }) {
  return (
    <Stack gap="lg">
      <Alert color="blue" title="One product request after prepared live-session load">
        N is live-session load and W is the effective bounded remount parallelism observed by the server; neither value is browser-derived request concurrency.
      </Alert>
      <Facts facts={[
        { label: "N — requested live sessions", value: formatInteger(evidence.requested_live_sessions) },
        { label: "M — observed migrated", value: formatInteger(evidence.observed_migrated_sessions) },
        { label: "I — observed non-migrated", value: formatInteger(evidence.observed_non_migrated_sessions) },
        { label: "W — effective remount parallelism", value: formatInteger(evidence.effective_remount_parallelism) },
        { label: "B — observed squashed blocks", value: formatInteger(evidence.observed_squashed_block_count) },
      ]} />
      <Facts facts={[
        { label: "Observed replaced layers", value: formatInteger(evidence.observed_replaced_layer_count) },
        { label: "Usable sessions", value: formatInteger(evidence.usable_session_count) },
        { label: "Reclaimed bytes", value: availabilityValue(evidence.reclaimed_bytes, formatBytes) },
        { label: "Manifest reduced", value: <BooleanBadge value={evidence.manifest_reduced} /> },
        { label: "Content equivalent", value: <BooleanBadge value={evidence.content_equivalent} /> },
      ]} />

      <div>
        <Title order={4} size="h5" mb="xs">Session disposition evidence</Title>
        <Table.ScrollContainer
          minWidth={620}
          scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "LayerStack session dispositions" } }}
        >
          <Table withTableBorder>
            <Table.Thead><Table.Tr><Table.Th>Migrated</Table.Th><Table.Th>Identity</Table.Th><Table.Th>Leased</Table.Th><Table.Th>Faulty</Table.Th><Table.Th>Session gone</Table.Th></Table.Tr></Table.Thead>
            <Table.Tbody><Table.Tr>
              <Table.Td>{formatInteger(evidence.dispositions.migrated)}</Table.Td>
              <Table.Td>{formatInteger(evidence.dispositions.identity)}</Table.Td>
              <Table.Td>{formatInteger(evidence.dispositions.leased)}</Table.Td>
              <Table.Td>{formatInteger(evidence.dispositions.faulty)}</Table.Td>
              <Table.Td>{formatInteger(evidence.dispositions.session_gone)}</Table.Td>
            </Table.Tr></Table.Tbody>
          </Table>
        </Table.ScrollContainer>
      </div>

      <SimpleGrid cols={{ base: 1, md: 2 }}>
        <Card withBorder padding="md">
          <Text fw={700} mb="xs">Source layer set</Text>
          {idSet(evidence.source_layer_ids)}
        </Card>
        <Card withBorder padding="md">
          <Text fw={700} mb="xs">Retained source layer set</Text>
          {idSet(evidence.retained_source_layer_ids)}
        </Card>
      </SimpleGrid>

      <div>
        <Title order={4} size="h5" mb="xs">Source layer allocation evidence</Title>
        {evidence.source_layer_allocations.length === 0 ? <Text c="dimmed">No source-layer allocation rows were observed.</Text> : (
          <Table.ScrollContainer
            minWidth={620}
            scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "LayerStack source layer allocations" } }}
          >
            <Table striped withTableBorder>
              <Table.Thead><Table.Tr><Table.Th>Layer</Table.Th><Table.Th>Logical bytes</Table.Th><Table.Th>Allocated bytes</Table.Th></Table.Tr></Table.Thead>
              <Table.Tbody>{evidence.source_layer_allocations.map((allocation) => (
                <Table.Tr key={allocation.layer_id}>
                  <Table.Td><Code className="wrap-anywhere">{allocation.layer_id}</Code></Table.Td>
                  <Table.Td>{availabilityValue(allocation.logical_bytes, formatBytes)}</Table.Td>
                  <Table.Td>{availabilityValue(allocation.allocated_bytes, formatBytes)}</Table.Td>
                </Table.Tr>
              ))}</Table.Tbody>
            </Table>
          </Table.ScrollContainer>
        )}
      </div>

      <div>
        <Title order={4} size="h5" mb="xs">S0 / S1 / S2 / S3 storage observations</Title>
        <SnapshotTable evidence={evidence} />
      </div>
    </Stack>
  );
}

function EvidenceBody({ value }: { value: OperationEvidenceValue }) {
  switch (value.operation) {
    case "exec_command": {
      const evidence = value.evidence;
      return (
        <Stack>
          <Facts facts={[
            { label: "Allowlisted command case", value: labelIdentifier(evidence.command_case) },
            { label: "Template revision", value: formatInteger(evidence.template_revision) },
            { label: "Exit code", value: evidence.exit_code === null ? "Not observed" : formatInteger(evidence.exit_code) },
          ]} />
          <Text size="sm">Command SHA-256 <Hash value={evidence.command_sha256} /></Text>
          <Table.ScrollContainer
            minWidth={620}
            scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Command output evidence" } }}
          >
            <Table withTableBorder>
              <Table.Thead><Table.Tr><Table.Th>Stream</Table.Th><Table.Th>Bytes</Table.Th><Table.Th>Truncated</Table.Th><Table.Th>SHA-256</Table.Th></Table.Tr></Table.Thead>
              <Table.Tbody>{(["stdout", "stderr"] as const).map((stream) => (
                <Table.Tr key={stream}>
                  <Table.Td>{stream}</Table.Td>
                  <Table.Td>{formatBytes(evidence[stream].byte_count)}</Table.Td>
                  <Table.Td><BooleanBadge value={evidence[stream].truncated} positive={false} /></Table.Td>
                  <Table.Td><Hash value={evidence[stream].sha256} /></Table.Td>
                </Table.Tr>
              ))}</Table.Tbody>
            </Table>
          </Table.ScrollContainer>
        </Stack>
      );
    }
    case "file_read":
      return <Stack><Facts facts={[
        { label: "Requested bytes", value: formatBytes(value.evidence.requested_bytes) },
        { label: "Returned bytes", value: formatBytes(value.evidence.returned_bytes) },
        { label: "Returned lines", value: formatInteger(value.evidence.returned_lines) },
      ]} /><Text size="sm">Content SHA-256 <Hash value={value.evidence.content_sha256} /></Text></Stack>;
    case "file_write":
      return <Stack><Facts facts={[
        { label: "Requested bytes", value: formatBytes(value.evidence.requested_bytes) },
        { label: "Observed bytes", value: formatBytes(value.evidence.observed_bytes) },
        { label: "Attribution", value: labelIdentifier(value.evidence.attribution) },
        { label: "Attributed layers", value: formatInteger(value.evidence.attributed_layer_count) },
      ]} /><Text size="sm">Expected SHA-256 <Hash value={value.evidence.expected_sha256} /></Text><Text size="sm">Observed SHA-256 <Hash value={value.evidence.observed_sha256} /></Text></Stack>;
    case "file_edit":
      return <Stack><Facts facts={[
        { label: "Requested replacements", value: formatInteger(value.evidence.requested_replacements) },
        { label: "Applied replacements", value: formatInteger(value.evidence.applied_replacements) },
        { label: "Attribution", value: labelIdentifier(value.evidence.attribution) },
        { label: "Attributed layers", value: formatInteger(value.evidence.attributed_layer_count) },
      ]} /><Text size="sm">Before SHA-256 <Hash value={value.evidence.before_sha256} /></Text><Text size="sm">Expected SHA-256 <Hash value={value.evidence.expected_sha256} /></Text><Text size="sm">Observed SHA-256 <Hash value={value.evidence.observed_sha256} /></Text></Stack>;
    case "file_blame":
      return <Facts facts={[
        { label: "Requested lines", value: formatInteger(value.evidence.requested_lines) },
        { label: "Returned ranges", value: formatInteger(value.evidence.returned_ranges) },
        { label: "Covered lines", value: formatInteger(value.evidence.covered_lines) },
        { label: "Expected ownership segments", value: formatInteger(value.evidence.expected_ownership_segments) },
        { label: "Matched ownership segments", value: formatInteger(value.evidence.matched_ownership_segments) },
        { label: "Observed auditability events", value: formatInteger(value.evidence.observed_auditability_events) },
      ]} />;
    case "create_workspace":
      return <Facts facts={[
        { label: "Requested", value: formatInteger(value.evidence.requested_count) },
        { label: "Created", value: formatInteger(value.evidence.created_count) },
        { label: "Ready", value: formatInteger(value.evidence.ready_count) },
        { label: "Destroyed", value: formatInteger(value.evidence.destroyed_count) },
        { label: "Network profile matches", value: formatInteger(value.evidence.network_profile_matches) },
        { label: "Registry baseline restored", value: <BooleanBadge value={value.evidence.registry_baseline_restored} /> },
      ]} />;
    case "squash_layerstack":
      return <LayerStackEvidence evidence={value.evidence} />;
    default:
      return assertNever(value);
  }
}

function assertNever(value: never): never {
  throw new Error(`Unhandled operation evidence: ${JSON.stringify(value)}`);
}

export function OperationEvidence({ evidence }: { evidence: OperationEvidenceReport[] }) {
  if (evidence.length === 0) return null;
  return (
    <div>
      <Title order={3} size="h4" mb="xs">Operation evidence</Title>
      <Text size="sm" c="dimmed" mb="sm">
        Measured-trial evidence projected by the report service. Values and availability states are shown without browser-side aggregation.
      </Text>
      <Accordion variant="contained" multiple>
        {evidence.map((record, index) => {
          const itemId = `${record.trial_id}:${record.request_id ?? "no-request"}:${index}`;
          return (
            <Accordion.Item key={itemId} value={itemId}>
              <Accordion.Control>
                <Group justify="space-between" wrap="wrap" pr="sm">
                  <Text fw={600}>{labelIdentifier(record.evidence.operation)}</Text>
                  <Group gap="xs">
                    <Code>{record.trial_id}</Code>
                    <Badge variant="light">{record.request_id ?? "No request id"}</Badge>
                  </Group>
                </Group>
              </Accordion.Control>
              <Accordion.Panel><EvidenceBody value={record.evidence} /></Accordion.Panel>
            </Accordion.Item>
          );
        })}
      </Accordion>
    </div>
  );
}
