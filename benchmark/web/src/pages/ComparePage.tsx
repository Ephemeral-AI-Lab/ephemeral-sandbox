import {
  Alert,
  Badge,
  Button,
  Card,
  Code,
  Group,
  Select,
  SimpleGrid,
  Stack,
  Table,
  Text,
  Title,
} from "@mantine/core";
import { useMutation, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useSearchParams } from "react-router";
import { benchmarkApi } from "@/api/client";
import type { ComparisonRequest, NormalizedComparisonPlan, RunState } from "@/api/types";
import { ErrorState, LoadingState } from "@/components/AsyncState";
import { errorMessage, formatInteger, formatMetricValue, formatNumber, labelIdentifier } from "@/lib/format";

function hasTerminalReport(state: RunState): boolean {
  switch (state) {
    case "completed":
    case "failed":
    case "cancelled": return true;
    case "queued":
    case "planned":
    case "preparing":
    case "running":
    case "verifying":
    case "tearing_down":
    case "cancelling": return false;
  }
}

function ProtocolIdentity({ label, plan }: { label: string; plan: NormalizedComparisonPlan }) {
  return (
    <Card withBorder padding="md">
      <Text size="xs" c="dimmed" tt="uppercase" fw={600}>{label}</Text>
      <Text fw={700}>{plan.protocol_id} v{plan.protocol_version}</Text>
      <Text size="sm">Declaration {plan.source}</Text>
      <Text size="sm">Treatment allowlist: {plan.treatment_fields.length === 0 ? "none" : plan.treatment_fields.map(labelIdentifier).join(", ")}</Text>
    </Card>
  );
}

export function ComparePage() {
  const [searchParams] = useSearchParams();
  const [referenceRunId, setReferenceRunId] = useState<string | null>(() => searchParams.get("reference"));
  const [candidateRunId, setCandidateRunId] = useState<string | null>(() => searchParams.get("candidate"));
  const runs = useQuery({ queryKey: ["runs"], queryFn: () => benchmarkApi.listRuns() });
  const comparison = useMutation({ mutationFn: (request: ComparisonRequest) => benchmarkApi.compare(request) });

  if (runs.isPending) return <LoadingState label="Loading runs for comparison" />;
  if (runs.error) return <ErrorState error={runs.error} retry={() => void runs.refetch()} />;
  if (!runs.data) return null;

  const options = runs.data.runs
    .filter(({ state }) => hasTerminalReport(state))
    .map((run) => ({ value: run.run_id, label: `${run.name} · ${run.run_id} · ${run.state}` }));
  const canCompare = referenceRunId !== null && candidateRunId !== null && referenceRunId !== candidateRunId;
  const submit = (descriptiveOverride: boolean) => {
    if (!referenceRunId || !candidateRunId || referenceRunId === candidateRunId) return;
    comparison.mutate({
      reference_run_id: referenceRunId,
      candidate_run_id: candidateRunId,
      descriptive_override: descriptiveOverride,
    });
  };

  return (
    <Stack gap="xl">
      <header>
        <Text size="sm" c="dimmed">Run comparison</Text>
        <Title>Compatibility before delta</Title>
        <Text maw={820}>The runner decides declared treatment, scientific compatibility, matched typed cells, correctness differences, and metric deltas before the browser renders them.</Text>
      </header>

      <Card withBorder padding="lg">
        <Stack>
          <SimpleGrid cols={{ base: 1, md: 2 }}>
            <Select
              label="Reference run"
              placeholder="Select reference"
              data={options}
              value={referenceRunId}
              onChange={(value) => { setReferenceRunId(value); comparison.reset(); }}
              searchable
              nothingFoundMessage="No matching terminal run"
            />
            <Select
              label="Candidate run"
              placeholder="Select candidate"
              data={options}
              value={candidateRunId}
              onChange={(value) => { setCandidateRunId(value); comparison.reset(); }}
              searchable
              nothingFoundMessage="No matching terminal run"
            />
          </SimpleGrid>
          {referenceRunId && referenceRunId === candidateRunId ? <Text c="red" role="alert">Choose two different runs.</Text> : null}
          <Button onClick={() => submit(false)} disabled={!canCompare} loading={comparison.isPending}>Check compatibility</Button>
          {options.length === 0 ? <Text c="dimmed">No terminal runs are available to compare.</Text> : null}
        </Stack>
      </Card>

      {comparison.error ? <Alert color="red" title="Comparison failed">{errorMessage(comparison.error)}</Alert> : null}
      {comparison.data ? (
        <Stack>
          <Alert
            color={comparison.data.compatible ? "green" : "yellow"}
            title={comparison.data.compatible ? "Runs are scientifically compatible" : "Aggregate comparison is blocked"}
          >
            {comparison.data.descriptive_only
              ? "Descriptive-only override is active. Every mismatch remains visible and no aggregate performance claim is available."
              : comparison.data.compatible
                ? "The versioned protocol, treatment decision, correctness gates, metric identities, phases, and matched cell projections passed."
                : "Review every aggregate-blocking mismatch before choosing a descriptive side-by-side view."}
          </Alert>

          <Card withBorder padding="lg">
            <Stack>
              <Group justify="space-between" wrap="wrap">
                <Title order={2} size="h3">Protocol & treatment decision</Title>
                <Badge color={comparison.data.protocol.declarations_compatible ? "green" : "yellow"}>
                  Declarations {comparison.data.protocol.declarations_compatible ? "match" : "differ"}
                </Badge>
              </Group>
              <SimpleGrid cols={{ base: 1, md: 2 }}>
                <ProtocolIdentity label="Reference protocol" plan={comparison.data.protocol.reference} />
                <ProtocolIdentity label="Candidate protocol" plan={comparison.data.protocol.candidate} />
              </SimpleGrid>
              {comparison.data.typed_treatment_differences.length === 0 ? (
                <Text>Treatment identity: no differing components.</Text>
              ) : (
                <Table.ScrollContainer
                  minWidth={720}
                  scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Typed treatment differences" } }}
                >
                  <Table striped>
                    <Table.Thead><Table.Tr><Table.Th>Field</Table.Th><Table.Th>Identity component</Table.Th><Table.Th>Reference</Table.Th><Table.Th>Candidate</Table.Th><Table.Th>Declared treatment</Table.Th></Table.Tr></Table.Thead>
                    <Table.Tbody>{comparison.data.typed_treatment_differences.map((difference) => (
                      <Table.Tr key={`${difference.field}:${difference.identity_component}`}>
                        <Table.Td>{labelIdentifier(difference.field)}</Table.Td>
                        <Table.Td>{difference.identity_component}</Table.Td>
                        <Table.Td><Code>{difference.reference ?? "missing"}</Code></Table.Td>
                        <Table.Td><Code>{difference.candidate ?? "missing"}</Code></Table.Td>
                        <Table.Td><Badge color={difference.declared ? "blue" : "yellow"}>{difference.declared ? "Yes" : "No"}</Badge></Table.Td>
                      </Table.Tr>
                    ))}</Table.Tbody>
                  </Table>
                </Table.ScrollContainer>
              )}
            </Stack>
          </Card>

          <Card withBorder padding="lg">
            <Stack>
              <Title order={2} size="h3">Compatibility checks</Title>
              <Table.ScrollContainer
                minWidth={820}
                scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Compatibility checks" } }}
              >
                <Table striped highlightOnHover>
                  <Table.Thead><Table.Tr><Table.Th>Result</Table.Th><Table.Th>Scope</Table.Th><Table.Th>Check</Table.Th><Table.Th>Consequence</Table.Th><Table.Th>Aggregate gate</Table.Th></Table.Tr></Table.Thead>
                  <Table.Tbody>{comparison.data.checks.map((check) => (
                    <Table.Tr key={check.check_id}>
                      <Table.Td><Badge color={check.compatible ? "green" : "yellow"}>{check.compatible ? "Passed" : "Mismatch"}</Badge></Table.Td>
                      <Table.Td>{labelIdentifier(check.scope)}</Table.Td>
                      <Table.Td><Text fw={600}>{check.label}</Text><Text size="xs" ff="monospace">{check.check_id}</Text></Table.Td>
                      <Table.Td>{check.consequence}</Table.Td>
                      <Table.Td>{check.blocks_aggregate ? "Required" : "Informational"}</Table.Td>
                    </Table.Tr>
                  ))}</Table.Tbody>
                </Table>
              </Table.ScrollContainer>
              {!comparison.data.compatible && !comparison.data.descriptive_only ? (
                <Button variant="light" color="yellow" onClick={() => submit(true)}>
                  Show matched evidence anyway — descriptive only
                </Button>
              ) : null}
            </Stack>
          </Card>

          {(comparison.data.compatible || comparison.data.descriptive_only) ? (
            <>
              <Card withBorder padding="lg">
                <Stack>
                  <Group justify="space-between" wrap="wrap">
                    <Title order={2} size="h3">Matched typed cells</Title>
                    <Badge variant="light">{formatInteger(comparison.data.matched_cells.length)} pair(s)</Badge>
                  </Group>
                  {comparison.data.matched_cells.length === 0 ? <Text c="dimmed">No persisted operation comparison keys matched.</Text> : (
                    <Table.ScrollContainer
                      minWidth={760}
                      scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Matched benchmark cells" } }}
                    >
                      <Table striped>
                        <Table.Thead><Table.Tr><Table.Th>Operation</Table.Th><Table.Th>Reference cell</Table.Th><Table.Th>Candidate cell</Table.Th><Table.Th>Effective protocol</Table.Th></Table.Tr></Table.Thead>
                        <Table.Tbody>{comparison.data.matched_cells.map((cell) => (
                          <Table.Tr key={cell.match_id}>
                            <Table.Td>{labelIdentifier(cell.operation_id)}</Table.Td>
                            <Table.Td><Code>{cell.reference_cell_id}</Code></Table.Td>
                            <Table.Td><Code>{cell.candidate_cell_id}</Code></Table.Td>
                            <Table.Td><Badge color={cell.effective_protocol_compatible ? "green" : "yellow"}>{cell.effective_protocol_compatible ? "Matched" : "Different"}</Badge></Table.Td>
                          </Table.Tr>
                        ))}</Table.Tbody>
                      </Table>
                    </Table.ScrollContainer>
                  )}
                </Stack>
              </Card>

              <Card withBorder padding="lg">
                <Stack>
                  <Group justify="space-between" wrap="wrap">
                    <Title order={2} size="h3">Backend-authored metric deltas</Title>
                    {comparison.data.descriptive_only ? <Badge color="yellow">Descriptive only</Badge> : <Badge color="green">Compatible evidence</Badge>}
                  </Group>
                  {comparison.data.deltas.length === 0 ? <Text c="dimmed">The runner returned no matched metric deltas.</Text> : (
                    <Table.ScrollContainer
                      minWidth={1280}
                      scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Comparison deltas" } }}
                    >
                      <Table striped highlightOnHover>
                        <Table.Thead>
                          <Table.Tr><Table.Th>Metric</Table.Th><Table.Th>Reference</Table.Th><Table.Th>Candidate</Table.Th><Table.Th>Absolute</Table.Th><Table.Th>Percent</Table.Th><Table.Th>Difference interval</Table.Th><Table.Th>Correctness / cleanup</Table.Th></Table.Tr>
                        </Table.Thead>
                        <Table.Tbody>{comparison.data.deltas.map((delta) => (
                          <Table.Tr key={delta.comparison_id}>
                            <Table.Td>{labelIdentifier(delta.metric_id)}<br /><Text span size="xs">{delta.direction.replaceAll("_", " ")}</Text></Table.Td>
                            <Table.Td>{formatMetricValue(delta.reference_value, delta.reference_unit ?? delta.unit)}<br /><Text size="xs">n={formatInteger(delta.reference_n)} · unavailable={formatInteger(delta.reference_unavailable_n)}</Text></Table.Td>
                            <Table.Td>{formatMetricValue(delta.candidate_value, delta.candidate_unit ?? delta.unit)}<br /><Text size="xs">n={formatInteger(delta.candidate_n)} · unavailable={formatInteger(delta.candidate_unavailable_n)}</Text></Table.Td>
                            <Table.Td>{delta.absolute_change === null ? delta.unavailable_reason ?? "Unavailable" : formatMetricValue(delta.absolute_change, delta.unit)}</Table.Td>
                            <Table.Td>{delta.percent_change === null ? delta.unavailable_reason ?? "Unavailable" : `${formatNumber(delta.percent_change, 2)}%`}</Table.Td>
                            <Table.Td>{delta.median_difference_confidence_interval
                              ? `${formatMetricValue(delta.median_difference_confidence_interval.lower, delta.unit)} – ${formatMetricValue(delta.median_difference_confidence_interval.upper, delta.unit)}`
                              : delta.confidence_interval_omission_reason ?? "Unavailable"}</Table.Td>
                            <Table.Td>
                              correctness {formatInteger(delta.correctness.reference_correctness_failed)} → {formatInteger(delta.correctness.candidate_correctness_failed)}<br />
                              cleanup {formatInteger(delta.correctness.reference_cleanup_invalid)} → {formatInteger(delta.correctness.candidate_cleanup_invalid)}
                            </Table.Td>
                          </Table.Tr>
                        ))}</Table.Tbody>
                      </Table>
                    </Table.ScrollContainer>
                  )}
                  <Text size="sm" c="dimmed">No browser-side performance verdict is inferred. Runner verdict: {comparison.data.performance_verdict ?? "not provided"}.</Text>
                </Stack>
              </Card>
            </>
          ) : null}
        </Stack>
      ) : null}
    </Stack>
  );
}
