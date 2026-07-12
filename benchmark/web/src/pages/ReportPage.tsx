import {
  Accordion,
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
  Tabs,
  Text,
  Title,
} from "@mantine/core";
import { useMutation, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Link, useParams, useSearchParams } from "react-router";
import { benchmarkApi } from "@/api/client";
import type {
  CellSummary,
  CountSemantics,
  MetricSummary,
  ProductAccess,
  ReportDesignCounts,
  ReportResponse,
  StabilizationPolicy,
} from "@/api/types";
import { ArtifactBrowser, downloadArtifact } from "@/components/ArtifactBrowser";
import { DistributionEvidence } from "@/components/DistributionEvidence";
import { ErrorState, LoadingState } from "@/components/AsyncState";
import { OperationEvidence } from "@/components/OperationEvidence";
import {
  DetailedCheckEvidence,
  FactorStudyEvidence,
  ResourceTimelineEvidence,
  TreatmentEvidence,
} from "@/components/ReportProjections";
import { formatDurationNs, formatInteger, formatMetricValue, formatNumber, formatTimestamp, labelIdentifier } from "@/lib/format";

const reportViews = ["summary", "results", "resources", "correctness", "methods"] as const;
type ReportView = (typeof reportViews)[number];

function isReportView(value: string | null): value is ReportView {
  return reportViews.some((view) => view === value);
}

function assertNever(value: never): never {
  throw new Error(`Unhandled report identity variant: ${JSON.stringify(value)}`);
}

function CellIdentity({ cell }: { cell: CellSummary }) {
  return (
    <Group justify="space-between" align="flex-start" wrap="wrap">
      <div>
        <Text fw={700}>{cell.operation_label}</Text>
        <Text size="xs" ff="monospace" className="wrap-anywhere">{cell.cell_id}</Text>
      </div>
      <Badge variant="light">{cell.family_label}</Badge>
    </Group>
  );
}

function DesignCounts({ counts, compact = false }: { counts: ReportDesignCounts; compact?: boolean }) {
  return (
    <SimpleGrid cols={{ base: 1, xs: 3 }} className={compact ? "design-counts design-counts-compact" : "design-counts"}>
      <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Test combinations</Text><Text fw={700}>{formatInteger(counts.test_combinations)}</Text></div>
      <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Trial batches</Text><Text fw={700}>{formatInteger(counts.trial_batches)}</Text></div>
      <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Issued product requests</Text><Text fw={700}>{formatInteger(counts.issued_product_requests)}</Text></div>
    </SimpleGrid>
  );
}

function metricSelectorValue(metric: MetricSummary, index: number): string {
  const { identity } = metric;
  // A metric id names the metric definition, not a unique report row: resource
  // collectors can author multiple observations for that definition. This key is
  // only a stable UI selection identity; the report remains the source of facts.
  return JSON.stringify([
    identity.id,
    identity.semantic_revision,
    identity.source,
    identity.scope,
    identity.kind,
    identity.aggregation,
    identity.unit,
    index,
  ]);
}

function MetricSelectorEvidence({ metrics }: { metrics: MetricSummary[] }) {
  const [selectedMetricValue, setSelectedMetricValue] = useState<string | null>(
    () => metrics[0] ? metricSelectorValue(metrics[0], 0) : null,
  );
  if (metrics.length === 0) return <Alert color="gray" title="No metric evidence">This cell has no authored metric summaries.</Alert>;
  const selectedIndex = metrics.findIndex((metric, index) => metricSelectorValue(metric, index) === selectedMetricValue);
  const selected = metrics[selectedIndex === -1 ? 0 : selectedIndex];
  if (!selected) return null;
  const effectiveSelectedValue = metricSelectorValue(selected, selectedIndex === -1 ? 0 : selectedIndex);
  return (
    <Stack>
      <div>
        <Title order={3} size="h4">Metric selector & data rows</Title>
        <Text c="dimmed">Selection changes presentation only; identities, samples, and statistics are authored by the report.</Text>
      </div>
      <Select
        label="Metric selector"
        description="Choose one report-authored metric to inspect."
        value={effectiveSelectedValue}
        onChange={setSelectedMetricValue}
        data={metrics.map((metric, index) => ({
          value: metricSelectorValue(metric, index),
          label: `${metric.identity.label} · ${metric.identity.unit}`,
        }))}
        searchable
        allowDeselect={false}
        maw={520}
      />
      <Table.ScrollContainer minWidth={760} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Available report metric identities" } }}>
        <Table striped>
          <Table.Thead><Table.Tr><Table.Th>Metric</Table.Th><Table.Th>Stable id</Table.Th><Table.Th>Unit / scope</Table.Th><Table.Th>Available / failed / unavailable</Table.Th></Table.Tr></Table.Thead>
          <Table.Tbody>{metrics.map((metric, index) => {
            const value = metricSelectorValue(metric, index);
            return (
            <Table.Tr key={value} className={value === effectiveSelectedValue ? "metric-selector-selected-row" : undefined}>
              <Table.Td><Text fw={650}>{metric.identity.label}</Text><Text size="xs" c="dimmed">{metric.identity.help}</Text></Table.Td>
              <Table.Td><Code>{metric.identity.id}</Code></Table.Td>
              <Table.Td>{metric.identity.unit} · {metric.identity.scope}</Table.Td>
              <Table.Td>{formatInteger(metric.available_n)} / {formatInteger(metric.failed_n)} / {formatInteger(metric.unavailable.count)}</Table.Td>
            </Table.Tr>
            );
          })}</Table.Tbody>
        </Table>
      </Table.ScrollContainer>
      <DistributionEvidence metric={selected} />
    </Stack>
  );
}

function SummaryTable({ data }: { data: ReportResponse }) {
  if (data.summary.length === 0) return <Text c="dimmed">No completed measured samples are present.</Text>;
  return (
    <Table.ScrollContainer
      minWidth={920}
      scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Benchmark summary table" } }}
    >
      <Table striped highlightOnHover>
        <Table.Thead>
          <Table.Tr>
            <Table.Th>Operation / cell</Table.Th>
            <Table.Th>Metric</Table.Th>
            <Table.Th>Available / failed / unavailable</Table.Th>
            <Table.Th>Median</Table.Th>
            <Table.Th>Median interval</Table.Th>
            <Table.Th>Interpretation</Table.Th>
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {data.summary.map((row) => (
            <Table.Tr key={row.row_id}>
              <Table.Td>{labelIdentifier(row.operation_id)}<br /><Text span size="xs" ff="monospace">{row.cell_id}</Text></Table.Td>
              <Table.Td>{labelIdentifier(row.metric_id)}</Table.Td>
              <Table.Td>{formatInteger(row.successful_n)} / {formatInteger(row.failed_n)} / {formatInteger(row.unavailable_n)}</Table.Td>
              <Table.Td>{formatMetricValue(row.median, row.unit)}</Table.Td>
              <Table.Td>
                {row.confidence_interval
                  ? `${formatMetricValue(row.confidence_interval.lower, row.unit)} – ${formatMetricValue(row.confidence_interval.upper, row.unit)} · ${formatNumber(row.confidence_interval.level * 100)}% · ${row.confidence_interval.method.replaceAll("_", " ")} · ${formatInteger(row.confidence_interval.resamples)} resamples`
                  : row.interval_omission_reason ?? "Unavailable"}
              </Table.Td>
              <Table.Td>{row.direction.replaceAll("_", " ")}</Table.Td>
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
    </Table.ScrollContainer>
  );
}

function ResultsView({ cells, studies }: { cells: CellSummary[]; studies: ReportResponse["factor_studies"] }) {
  if (cells.length === 0) return <Text c="dimmed">The report contains no expanded cell evidence.</Text>;
  return (
    <Stack>
      <FactorStudyEvidence studies={studies} />
      <Title order={2} size="h3">Cell distributions & operation evidence</Title>
      <Accordion variant="separated" multiple defaultValue={[cells[0]?.cell_id ?? ""]}>
        {cells.map((cell) => (
          <Accordion.Item key={cell.cell_id} value={cell.cell_id}>
            <Accordion.Control><CellIdentity cell={cell} /></Accordion.Control>
            <Accordion.Panel>
              <Stack>
              <SimpleGrid cols={{ base: 2, sm: 3, lg: 5 }}>
                <Text><strong>{formatInteger(cell.counts.measured_attempted)}</strong><br /><Text span size="xs" c="dimmed">Measured batches</Text></Text>
                <Text><strong>{formatInteger(cell.counts.successful)}</strong><br /><Text span size="xs" c="dimmed">Successful</Text></Text>
                <Text><strong>{formatInteger(cell.counts.product_failed)}</strong><br /><Text span size="xs" c="dimmed">Product failed</Text></Text>
                <Text><strong>{formatInteger(cell.counts.infrastructure_failed)}</strong><br /><Text span size="xs" c="dimmed">Infrastructure failed</Text></Text>
                <Text><strong>{formatInteger(cell.counts.missing_primary_latency)}</strong><br /><Text span size="xs" c="dimmed">Missing latency</Text></Text>
              </SimpleGrid>
              <DesignCounts counts={cell.design_counts} compact />
              <MetricSelectorEvidence metrics={cell.metrics} />
              {cell.phases.length > 0 ? (
                <div>
                  <Title order={3} size="h4" mb="xs">Product phase timing</Title>
                  <Table.ScrollContainer
                    minWidth={1080}
                    scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Product phase timing" } }}
                  >
                    <Table>
                      <Table.Thead><Table.Tr><Table.Th>Phase</Table.Th><Table.Th>Attempted / failed</Table.Th><Table.Th>Median</Table.Th><Table.Th>p95</Table.Th><Table.Th>Unit</Table.Th><Table.Th>Source / correlation</Table.Th><Table.Th>Registered span</Table.Th></Table.Tr></Table.Thead>
                      <Table.Tbody>{cell.phases.map((phase) => (
                        <Table.Tr key={phase.id}>
                          <Table.Td><Text fw={600}>{phase.label} · rev {phase.semantic_revision}</Text><Text size="xs" c="dimmed">{phase.help}</Text></Table.Td>
                          <Table.Td>{formatInteger(phase.attempted)} / {formatInteger(phase.failed)}</Table.Td>
                          <Table.Td>{phase.duration.median === null ? "Unavailable" : formatDurationNs(phase.duration.median)}</Table.Td>
                          <Table.Td>{phase.duration.p95 === null ? "Unavailable" : formatDurationNs(phase.duration.p95)}</Table.Td>
                          <Table.Td>{phase.unit}</Table.Td>
                          <Table.Td>{phase.source.replaceAll("_", " ")}<br /><Text span size="xs">{phase.correlation.replaceAll("_", " ")}</Text></Table.Td>
                          <Table.Td><Code>{phase.trace_span_name}</Code></Table.Td>
                        </Table.Tr>
                      ))}</Table.Tbody>
                    </Table>
                  </Table.ScrollContainer>
                </div>
              ) : null}
                <OperationEvidence evidence={cell.operation_evidence} />
              </Stack>
            </Accordion.Panel>
          </Accordion.Item>
        ))}
      </Accordion>
    </Stack>
  );
}

function ResourcesView({ cells }: { cells: CellSummary[] }) {
  const cellsWithResources = cells
    .map((cell) => ({ ...cell, metrics: cell.metrics.filter((metric) => metric.identity.scope !== "operation") }))
    .filter((cell) => cell.metrics.length > 0 || cell.timelines.length > 0 || cell.cpu_latency_correlation.support_count > 0);
  if (cellsWithResources.length === 0) {
    return <Alert color="gray" title="No resource evidence">The report contains no available or explicitly unavailable resource metric summaries.</Alert>;
  }
  return (
    <Stack>
      <Alert color="blue" title="Server-authored resource evidence">
        Every value, unavailable count, distribution, and correlation is read from the versioned report. The browser only selects resource-scoped rows for this view.
      </Alert>
      {cellsWithResources.map((cell) => (
        <Card key={cell.cell_id} withBorder padding="lg">
          <Stack>
            <CellIdentity cell={cell} />
            {cell.metrics.map((metric) => <DistributionEvidence key={`${cell.cell_id}:${metric.identity.id}`} metric={metric} />)}
            <ResourceTimelineEvidence timelines={cell.timelines} />
            <Card withBorder padding="md">
              <Title order={3} size="h4">Sandbox CPU / operation latency correlation</Title>
              <SimpleGrid cols={{ base: 1, sm: 2, lg: 4 }} mt="sm">
                <Text><strong>{cell.cpu_latency_correlation.method.replaceAll("_", " ")}</strong><br /><Text span size="xs" c="dimmed">Method · semantic revision {cell.cpu_latency_correlation.semantic_revision}</Text></Text>
                <Text className="wrap-anywhere"><strong>{cell.cpu_latency_correlation.alignment.replaceAll("_", " ")}</strong><br /><Text span size="xs" c="dimmed">Alignment</Text></Text>
                <Text className="wrap-anywhere"><strong>{cell.cpu_latency_correlation.eligibility.replaceAll("_", " ")}</strong><br /><Text span size="xs" c="dimmed">Eligibility</Text></Text>
                <Text className="wrap-anywhere"><Code>{cell.cpu_latency_correlation.latency_metric_id}</Code> × <Code>{cell.cpu_latency_correlation.cpu_metric_id}</Code><br /><Text span size="xs" c="dimmed">Authored metric pair</Text></Text>
                <Text><strong>{formatInteger(cell.cpu_latency_correlation.support_count)}</strong><br /><Text span size="xs" c="dimmed">Support n</Text></Text>
                <Text><strong>{formatNumber(cell.cpu_latency_correlation.coefficient, 4)}</strong><br /><Text span size="xs" c="dimmed">Coefficient</Text></Text>
                <Text><strong>{formatInteger(cell.cpu_latency_correlation.exclusions.unavailable_cpu)}</strong><br /><Text span size="xs" c="dimmed">Unavailable CPU</Text></Text>
              </SimpleGrid>
              {cell.cpu_latency_correlation.confidence_interval ? (
                <Alert color="blue" title={`${formatNumber(cell.cpu_latency_correlation.confidence_interval.level * 100)}% Pearson confidence interval`} mt="md">
                  {formatNumber(cell.cpu_latency_correlation.confidence_interval.lower, 4)} to {formatNumber(cell.cpu_latency_correlation.confidence_interval.upper, 4)} · {cell.cpu_latency_correlation.confidence_interval.method.replaceAll("_", " ")} · {formatInteger(cell.cpu_latency_correlation.confidence_interval.valid_resamples)} valid of {formatInteger(cell.cpu_latency_correlation.confidence_interval.resamples)} resamples
                </Alert>
              ) : (
                <Alert color="gray" title="Pearson confidence interval unavailable" mt="md">
                  {cell.cpu_latency_correlation.interval_omission?.replaceAll("_", " ") ?? "No omission code was projected."}
                </Alert>
              )}
              <Table.ScrollContainer minWidth={720} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Correlation exclusion counts" } }} mt="md">
                <Table withTableBorder>
                  <Table.Thead><Table.Tr><Table.Th>Ineligible trial</Table.Th><Table.Th>Missing latency</Table.Th><Table.Th>Missing CPU</Table.Th><Table.Th>Unavailable CPU</Table.Th></Table.Tr></Table.Thead>
                  <Table.Tbody><Table.Tr><Table.Td>{formatInteger(cell.cpu_latency_correlation.exclusions.ineligible_trial)}</Table.Td><Table.Td>{formatInteger(cell.cpu_latency_correlation.exclusions.missing_latency)}</Table.Td><Table.Td>{formatInteger(cell.cpu_latency_correlation.exclusions.missing_cpu)}</Table.Td><Table.Td>{formatInteger(cell.cpu_latency_correlation.exclusions.unavailable_cpu)}</Table.Td></Table.Tr></Table.Tbody>
                </Table>
              </Table.ScrollContainer>
              {cell.cpu_latency_correlation.points.length > 0 ? (
                <Table.ScrollContainer minWidth={620} mah={360} type="native" tabIndex={0} aria-label="CPU and latency correlation points">
                  <Table striped stickyHeader mt="md">
                    <Table.Thead><Table.Tr><Table.Th>Trial</Table.Th><Table.Th>Operation latency</Table.Th><Table.Th>Sandbox CPU time</Table.Th></Table.Tr></Table.Thead>
                    <Table.Tbody>{cell.cpu_latency_correlation.points.map((point) => (
                      <Table.Tr key={point.trial_id}>
                        <Table.Td><Code>{point.trial_id}</Code></Table.Td>
                        <Table.Td>{formatDurationNs(point.operation_latency_ns)}</Table.Td>
                        <Table.Td>{formatDurationNs(point.sandbox_cpu_time_ns)}</Table.Td>
                      </Table.Tr>
                    ))}</Table.Tbody>
                  </Table>
                </Table.ScrollContainer>
              ) : <Text c="dimmed" mt="sm">No eligible paired points were projected.</Text>}
            </Card>
          </Stack>
        </Card>
      ))}
    </Stack>
  );
}

function CorrectnessView({ data }: { data: ReportResponse }) {
  if (data.cells.length === 0) return <Text c="dimmed">The report contains no cell correctness summaries.</Text>;
  return (
    <Stack>
      <Alert color={data.correctness_verdict === "pass" ? "green" : data.correctness_verdict === "fail" ? "red" : "yellow"} title={`Run correctness ${data.correctness_verdict}`}>
        Correctness and cleanup validity gate interpretation of timing evidence.
      </Alert>
      {data.cells.map((cell) => (
        <Card key={cell.cell_id} withBorder padding="lg">
          <Stack>
            <CellIdentity cell={cell} />
            <SimpleGrid cols={{ base: 2, sm: 4 }}>
              <Text><strong>{formatInteger(cell.counts.correctness_failed)}</strong><br /><Text span size="xs" c="dimmed">Correctness failures</Text></Text>
              <Text><strong>{formatInteger(cell.counts.cleanup_invalid)}</strong><br /><Text span size="xs" c="dimmed">Cleanup invalid</Text></Text>
              <Text><strong>{formatInteger(cell.counts.product_failed)}</strong><br /><Text span size="xs" c="dimmed">Product failures</Text></Text>
              <Text><strong>{formatInteger(cell.counts.infrastructure_failed)}</strong><br /><Text span size="xs" c="dimmed">Infrastructure failures</Text></Text>
            </SimpleGrid>
            {cell.checks.length === 0 ? <Text c="dimmed">No operation checks are registered for this cell.</Text> : (
              <Table.ScrollContainer
                minWidth={620}
                scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Correctness check summaries" } }}
              >
                <Table striped>
                  <Table.Thead><Table.Tr><Table.Th>Check</Table.Th><Table.Th>Semantic revision</Table.Th><Table.Th>Attempted</Table.Th><Table.Th>Passed</Table.Th><Table.Th>Failed</Table.Th></Table.Tr></Table.Thead>
                  <Table.Tbody>{cell.checks.map((check) => (
                    <Table.Tr key={check.id}>
                      <Table.Td><Text fw={600}>{check.label}</Text><Text size="xs" c="dimmed">{check.help}</Text></Table.Td>
                      <Table.Td>{check.semantic_revision}</Table.Td>
                      <Table.Td>{formatInteger(check.attempted)}</Table.Td>
                      <Table.Td>{formatInteger(check.passed)}</Table.Td>
                      <Table.Td><Badge color={check.failed === 0 ? "green" : "red"}>{formatInteger(check.failed)}</Badge></Table.Td>
                    </Table.Tr>
                  ))}</Table.Tbody>
                </Table>
              </Table.ScrollContainer>
            )}
            <DetailedCheckEvidence evidence={cell.check_evidence} />
          </Stack>
        </Card>
      ))}
    </Stack>
  );
}

function formatCountSemantics(value: CountSemantics): string {
  switch (value.kind) {
    case "concurrent_requests": return `concurrent requests · factor ${value.factor}`;
    case "concurrent_workspace_creates": return `concurrent workspace creates · factor ${value.factor}`;
    case "single_request_with_prepared_load": return `single request with prepared load · load factor ${value.load_factor}`;
  }
  return assertNever(value);
}

function formatProductAccess(value: ProductAccess): string {
  switch (value.kind) {
    case "public_gateway": return `public gateway · ${value.action}`;
    case "daemon_http": return `daemon HTTP · ${value.action}`;
    case "internal_workspace": return `internal workspace · ${value.action}`;
  }
  return assertNever(value);
}

function formatStabilization(value: StabilizationPolicy): string {
  switch (value.kind) {
    case "not_required": return `not required · semantic revision ${value.semantic_revision}`;
    case "exact_snapshot_quiet_window": return `exact snapshot quiet window · semantic revision ${value.semantic_revision} · ${formatInteger(value.quiet_window_matches)} matches · ${formatInteger(value.poll_interval_ms)} ms poll · ${formatInteger(value.timeout_ms)} ms timeout`;
  }
  return assertNever(value);
}

function MethodsView({ data }: { data: ReportResponse }) {
  const methods = data.methods;
  const artifactSchemas = [
    methods.artifact_schemas.run_manifest,
    methods.artifact_schemas.intent_plan,
    methods.artifact_schemas.expanded_plan,
    methods.artifact_schemas.definition_snapshot,
    methods.artifact_schemas.environment_metadata,
    methods.artifact_schemas.events,
    methods.artifact_schemas.observations,
    methods.artifact_schemas.bounded_evidence,
  ];
  const environment = methods.environment;
  const policies = [
    ["Raw time unit", methods.raw_time_unit],
    ["Monotonic clock", methods.monotonic_clock],
    ["Quantile interpolation", methods.quantile_interpolation],
    ["Confidence interval", methods.confidence_interval],
    ["Bootstrap resamples", formatInteger(methods.bootstrap_resamples)],
    ["Outlier policy", methods.outlier_policy],
    ["Warmup policy", methods.warmup_policy],
    ["Failure policy", methods.failure_policy],
    ["Resource policy", methods.resource_policy],
    ["Comparison policy", methods.comparison_policy],
  ] as const;
  return (
    <Stack>
      <Card withBorder padding="lg">
        <Stack>
          <div>
            <Title order={2} size="h3">Publication-ready Methods</Title>
            <Text c="dimmed">Every identity and revision below is persisted by the report builder; this view only formats it.</Text>
          </div>
          <SimpleGrid cols={{ base: 1, sm: 2, lg: 4 }}>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Methods / report schema</Text><Text>{methods.schema_version} / {data.schema_version}</Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Report derivation revision</Text><Text>{methods.report_derivation_revision}</Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Artifact reader revision</Text><Text>{methods.artifact_reader_revision}</Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Plan schema / seed</Text><Text>{methods.plan_schema_version} / {formatInteger(methods.plan_seed)}</Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Cell order</Text><Text>{methods.cell_order.replaceAll("_", " ")}</Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Resource sample interval</Text><Text>{formatInteger(methods.resource_sample_interval_ms)} ms</Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Fixture generator revision</Text><Text>{methods.fixture_generator_revision}</Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Producer</Text><Text><Code>{methods.producer.package}</Code> {methods.producer.version}</Text></div>
          </SimpleGrid>
          <DesignCounts counts={methods.design_counts} />
          <Text ff="monospace" className="wrap-anywhere">Definition snapshot v{data.definition_snapshot_version} · SHA-256 {data.definition_snapshot_sha256}</Text>
        </Stack>
      </Card>

      <Card withBorder padding="lg">
        <Stack>
          <Title order={3} size="h4">Statistical and evidence policies</Title>
          <SimpleGrid cols={{ base: 1, md: 2 }}>
            {policies.map(([label, value]) => <div key={label}><Text size="xs" c="dimmed" tt="uppercase" fw={650}>{label}</Text><Text className="wrap-anywhere">{value}</Text></div>)}
          </SimpleGrid>
        </Stack>
      </Card>

      <Card withBorder padding="lg">
        <Stack>
          <Title order={3} size="h4">Fixture identity</Title>
          {Object.entries(methods.fixture_hashes).length === 0 ? <Text c="dimmed">No fixture hashes were persisted.</Text> : (
            <Table.ScrollContainer minWidth={680} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Fixture hashes" } }}>
              <Table striped><Table.Thead><Table.Tr><Table.Th>Fixture</Table.Th><Table.Th>SHA-256</Table.Th></Table.Tr></Table.Thead><Table.Tbody>{Object.entries(methods.fixture_hashes).map(([fixture, hash]) => <Table.Tr key={fixture}><Table.Td><Code>{fixture}</Code></Table.Td><Table.Td><Code className="wrap-anywhere">{hash}</Code></Table.Td></Table.Tr>)}</Table.Tbody></Table>
            </Table.ScrollContainer>
          )}
        </Stack>
      </Card>

      <Card withBorder padding="lg">
        <Stack>
          <Title order={3} size="h4">Operation authorities</Title>
          <Table.ScrollContainer minWidth={1680} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Operation publication authorities" } }}>
            <Table striped>
              <Table.Thead><Table.Tr><Table.Th>Operation / family</Table.Th><Table.Th>Operation / factor / comparison revisions</Table.Th><Table.Th>Client cohort</Table.Th><Table.Th>Product access</Table.Th><Table.Th>Count semantics</Table.Th><Table.Th>Cleanup</Table.Th><Table.Th>Resolved isolation</Table.Th><Table.Th>Request timeouts</Table.Th><Table.Th>Stabilization</Table.Th></Table.Tr></Table.Thead>
              <Table.Tbody>{methods.operation_authorities.map((authority) => (
                <Table.Tr key={authority.operation_id}>
                  <Table.Td><Code>{authority.operation_id}</Code><br /><Text span size="xs">{authority.family_id}</Text></Table.Td>
                  <Table.Td>{authority.semantic_revision} / {authority.factor_schema_revision} / {authority.comparison_projection_revision}</Table.Td>
                  <Table.Td>{authority.client_cohort}</Table.Td>
                  <Table.Td>{formatProductAccess(authority.product_access)}</Table.Td>
                  <Table.Td>{formatCountSemantics(authority.count_semantics)}</Table.Td>
                  <Table.Td>{authority.cleanup_policy.replaceAll("_", " ")}</Table.Td>
                  <Table.Td>{authority.resolved_isolation_policies.map((value) => value.replaceAll("_", " ")).join(" · ")}</Table.Td>
                  <Table.Td>{authority.request_timeout_ms.map((value) => `${formatInteger(value)} ms`).join(" · ")}</Table.Td>
                  <Table.Td>{formatStabilization(authority.stabilization_policy)}</Table.Td>
                </Table.Tr>
              ))}</Table.Tbody>
            </Table>
          </Table.ScrollContainer>
        </Stack>
      </Card>

      <Card withBorder padding="lg">
        <Stack>
          <Title order={3} size="h4">Metric, check & phase revision identities</Title>
          <Table.ScrollContainer minWidth={760} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Metric check and phase revisions" } }}>
            <Table striped>
              <Table.Thead><Table.Tr><Table.Th>Identity class</Table.Th><Table.Th>Stable id</Table.Th><Table.Th>Semantic revision</Table.Th></Table.Tr></Table.Thead>
              <Table.Tbody>
                {methods.metric_revisions.map((item) => <Table.Tr key={`metric:${item.metric_id}`}><Table.Td>Collected metric</Table.Td><Table.Td><Code>{item.metric_id}</Code></Table.Td><Table.Td>{item.semantic_revision}</Table.Td></Table.Tr>)}
                {methods.derived_metric_revisions.map((item) => <Table.Tr key={`derived:${item.metric_id}`}><Table.Td>Derived metric</Table.Td><Table.Td><Code>{item.metric_id}</Code></Table.Td><Table.Td>{item.semantic_revision}</Table.Td></Table.Tr>)}
                {methods.check_revisions.map((item) => <Table.Tr key={`check:${item.check_id}`}><Table.Td>Check</Table.Td><Table.Td><Code>{item.check_id}</Code></Table.Td><Table.Td>{item.semantic_revision}</Table.Td></Table.Tr>)}
                {methods.phase_revisions.map((item) => <Table.Tr key={`phase:${item.phase_id}`}><Table.Td>Phase</Table.Td><Table.Td><Code>{item.phase_id}</Code></Table.Td><Table.Td>{item.semantic_revision}</Table.Td></Table.Tr>)}
              </Table.Tbody>
            </Table>
          </Table.ScrollContainer>
        </Stack>
      </Card>

      <Card withBorder padding="lg">
        <Stack>
          <Title order={3} size="h4">Artifact schema identities</Title>
          <Table.ScrollContainer minWidth={760} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Artifact schema identities" } }}>
            <Table striped><Table.Thead><Table.Tr><Table.Th>Schema name</Table.Th><Table.Th>Write version</Table.Th><Table.Th>Readable versions</Table.Th></Table.Tr></Table.Thead><Table.Tbody>{artifactSchemas.map((schema) => <Table.Tr key={schema.schema_name}><Table.Td><Code>{schema.schema_name}</Code></Table.Td><Table.Td>{schema.write_version}</Table.Td><Table.Td>{schema.read_versions.join(", ")}</Table.Td></Table.Tr>)}</Table.Tbody></Table>
          </Table.ScrollContainer>
        </Stack>
      </Card>

      <Card withBorder padding="lg">
        <Stack>
          <Title order={3} size="h4">Environment & treatment identity</Title>
          <SimpleGrid cols={{ base: 1, sm: 2, lg: 3 }}>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Environment schema / cohort</Text><Text>{environment.schema_version} · {environment.client_cohort}</Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Image</Text><Text className="wrap-anywhere">{environment.image_reference}<br /><Code>{environment.image_digest ?? "Digest unavailable"}</Code></Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Workspace root identity</Text><Text className="wrap-anywhere">{environment.workspace_root_identity}</Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Gateway endpoint identity</Text><Text className="wrap-anywhere">{environment.gateway_endpoint_identity}</Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Host</Text><Text>{environment.host.operating_system} · {environment.host.architecture}<br />kernel {environment.host.kernel_release ?? "Unavailable"} · Docker {environment.host.docker_engine_version ?? "Unavailable"} · filesystem {environment.host.filesystem ?? "Unavailable"}</Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Host monotonic clock</Text><Text>{environment.host.monotonic_clock}</Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Source treatment</Text><Text><Code>{environment.treatment.source_commit}</Code>{environment.treatment.source_dirty ? " · dirty" : " · clean"}<br />diff <Code>{environment.treatment.source_diff_hash ?? "None"}</Code></Text></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Daemon binary hash</Text><Code className="wrap-anywhere">{environment.treatment.daemon_binary_hash ?? "Unavailable"}</Code></div>
            <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Gateway binary hash</Text><Code className="wrap-anywhere">{environment.treatment.gateway_binary_hash ?? "Unavailable"}</Code></div>
          </SimpleGrid>
        </Stack>
      </Card>

      <Card withBorder padding="lg">
        <Stack>
          <Title order={2} size="h3">Allowlisted artifacts & export</Title>
          <ArtifactBrowser runId={data.run_id} />
        </Stack>
      </Card>
    </Stack>
  );
}

export function ReportPage() {
  const { runId = "" } = useParams();
  const [searchParams, setSearchParams] = useSearchParams();
  const [citationState, setCitationState] = useState<"idle" | "copied" | "failed">("idle");
  const viewParam = searchParams.get("view");
  const selectedView: ReportView = isReportView(viewParam) ? viewParam : "summary";
  const report = useQuery({
    queryKey: ["report", runId],
    queryFn: () => benchmarkApi.report(runId),
    enabled: runId.length > 0,
    retry: false,
  });
  const artifactExport = useMutation({
    mutationFn: (artifactId: "json_export" | "csv_export") => benchmarkApi.artifact(runId, artifactId),
    onSuccess: downloadArtifact,
  });

  if (!runId) return <Alert color="red" title="Run id is required" />;
  if (report.isPending) return <LoadingState label={`Loading report ${runId}`} />;
  if (report.error) return <ErrorState error={report.error} retry={() => void report.refetch()} />;
  if (!report.data) return null;

  const data = report.data;
  const copyCitation = async () => {
    const date = data.ended_at ?? data.started_at ?? "date unavailable";
    const citation = `EphemeralOS Benchmark Laboratory. ${data.research_question}. Run ${data.run_id}; source ${data.source_commit}; ${date}.`;
    try {
      await navigator.clipboard.writeText(citation);
      setCitationState("copied");
    } catch {
      setCitationState("failed");
    }
  };
  const changeView = (value: string | null) => {
    if (!isReportView(value)) return;
    setSearchParams(value === "summary" ? {} : { view: value }, { replace: true });
  };

  return (
    <Stack gap="xl">
      <header>
        <Text size="sm" c="dimmed">Scientific report</Text>
        <Group justify="space-between" align="flex-start" wrap="wrap">
          <div>
            <Title>{data.research_question}</Title>
            <Text ff="monospace" className="wrap-anywhere">Run {data.run_id}</Text>
          </div>
          <Stack gap="xs" align="flex-end">
            <Group>
              <Badge color={data.correctness_verdict === "pass" ? "green" : data.correctness_verdict === "fail" ? "red" : "gray"}>
                Correctness {data.correctness_verdict}
              </Badge>
              {data.provisional ? <Badge color="yellow">Provisional</Badge> : <Badge color="green" variant="light">Terminal evidence</Badge>}
            </Group>
            <Group gap="xs" justify="flex-end">
              <Button component={Link} to={`/benchmark/compare?reference=${encodeURIComponent(data.run_id)}`} variant="default">Compare</Button>
              <Button variant="default" onClick={() => void copyCitation()}>Copy citation</Button>
              <Button variant="default" loading={artifactExport.isPending && artifactExport.variables === "json_export"} onClick={() => artifactExport.mutate("json_export")}>Export JSON</Button>
              <Button variant="default" loading={artifactExport.isPending && artifactExport.variables === "csv_export"} onClick={() => artifactExport.mutate("csv_export")}>Export CSV</Button>
            </Group>
            <Text size="xs" c={citationState === "failed" ? "red" : "dimmed"} aria-live="polite">
              {citationState === "copied" ? "Citation copied." : citationState === "failed" ? "Citation could not be copied." : "Exports are immutable backend artifacts."}
            </Text>
          </Stack>
        </Group>
      </header>

      {artifactExport.error ? <Alert color="red" title="Export failed">{String(artifactExport.error)}</Alert> : null}

      {data.correctness_verdict === "fail" ? (
        <Alert color="red" title="Correctness gates failed">
          Timing evidence remains available, but this run does not support a passing performance claim.
        </Alert>
      ) : null}
      {data.provisional ? <Alert color="yellow" title="Provisional report">The run is not terminal; counts and evidence may grow.</Alert> : null}

      <Card withBorder padding="lg">
        <Stack>
          <SimpleGrid cols={{ base: 1, md: 2 }}>
            <Text>Source {data.source_commit}{data.source_dirty ? " (dirty)" : ""}</Text>
            <Text>Started {formatTimestamp(data.started_at)} · Ended {formatTimestamp(data.ended_at)}</Text>
            <Text ff="monospace" className="wrap-anywhere">Plan {data.plan_hash}</Text>
            <Text ff="monospace" className="wrap-anywhere">Environment {data.environment_fingerprint}</Text>
            <Text>Definition snapshot version {data.definition_snapshot_version}</Text>
            <Text>Cells {formatInteger(data.cells.length)} · Summary rows {formatInteger(data.summary.length)}</Text>
          </SimpleGrid>
          <DesignCounts counts={data.design_counts} />
        </Stack>
      </Card>

      <TreatmentEvidence studies={data.factor_studies} />

      <Tabs value={selectedView} onChange={changeView} keepMounted={false}>
        <div className="report-tab-scroll" role="region" aria-label="Report destinations" tabIndex={0}>
          <Tabs.List>
            <Tabs.Tab value="summary">Summary</Tabs.Tab>
            <Tabs.Tab value="results">Results & distributions</Tabs.Tab>
            <Tabs.Tab value="resources">Resources</Tabs.Tab>
            <Tabs.Tab value="correctness">Correctness</Tabs.Tab>
            <Tabs.Tab value="methods">Methods & data</Tabs.Tab>
          </Tabs.List>
        </div>

        <Tabs.Panel value="summary" pt="lg">
          <Card withBorder padding="lg">
            <Stack>
              <Title order={2} size="h3">Backend-authored summary</Title>
              <SummaryTable data={data} />
              {data.warnings.map((warning) => <Alert key={warning.code} color="yellow" title={warning.code}>{warning.message}</Alert>)}
              {data.limitations.map((limitation, index) => <Alert key={index} color="yellow" title="Limitation">{limitation}</Alert>)}
            </Stack>
          </Card>
        </Tabs.Panel>
        <Tabs.Panel value="results" pt="lg"><ResultsView cells={data.cells} studies={data.factor_studies} /></Tabs.Panel>
        <Tabs.Panel value="resources" pt="lg"><ResourcesView cells={data.cells} /></Tabs.Panel>
        <Tabs.Panel value="correctness" pt="lg"><CorrectnessView data={data} /></Tabs.Panel>
        <Tabs.Panel value="methods" pt="lg"><MethodsView data={data} /></Tabs.Panel>
      </Tabs>
    </Stack>
  );
}
