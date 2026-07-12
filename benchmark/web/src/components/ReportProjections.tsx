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
import type {
  CheckEvidenceReport,
  FactorId,
  FactorStudyCell,
  FactorStudyProjection,
  MetricUnit,
  ReportFactor,
  ReportFactorValue,
  ResourceTimelineProjection,
} from "@/api/types";
import { formatBytes, formatDurationNs, formatInteger, formatMetricValue, formatNumber, labelIdentifier } from "@/lib/format";

function assertNever(value: never): never {
  throw new Error(`Unhandled report projection variant: ${JSON.stringify(value)}`);
}

function formatFactorValue(value: ReportFactorValue, unit: ReportFactor["unit"]): string {
  switch (value.kind) {
    case "choice":
      return labelIdentifier(value.value);
    case "ratio":
      return formatNumber(value.value, 4);
    case "unsigned_integer":
      switch (unit) {
        case "bytes": return formatBytes(value.value);
        case "ratio": return formatNumber(value.value, 4);
        case "count": return formatInteger(value.value);
        case null: return formatInteger(value.value);
      }
      return assertNever(unit);
  }
  return assertNever(value);
}

function formatFactor(factor: ReportFactor): string {
  return formatFactorValue(factor.value, factor.unit);
}

function factorFor(cell: FactorStudyCell, factorId: FactorId): ReportFactor | undefined {
  return cell.factors.find(({ id }) => id === factorId);
}

function FactorRoleList({ study, ids, empty }: { study: FactorStudyProjection; ids: FactorId[]; empty: string }) {
  if (ids.length === 0) return <Text c="dimmed">{empty}</Text>;
  return (
    <Stack gap="xs">
      {ids.map((factorId) => {
        const projected = study.cells.flatMap((cell) => cell.factors.filter(({ id }) => id === factorId));
        const example = projected[0];
        const values = [...new Set(projected.map(formatFactor))];
        return (
          <div className="report-factor-row" key={factorId}>
            <div>
              <Text fw={650}>{example?.label ?? labelIdentifier(factorId)}</Text>
              <Text size="xs" c="dimmed">{example?.help ?? "No definition help was projected."}</Text>
            </div>
            <Group gap="xs" justify="flex-end" wrap="wrap">
              <Badge variant="light" color={example?.role === "varied" ? "blue" : "gray"}>{example?.role ?? "unknown"}</Badge>
              {example?.control ? <Badge className="report-control-badge" variant="outline">Control {formatFactorValue(example.control, example.unit)}</Badge> : null}
              {values.length > 0 ? values.map((value) => <Code key={value}>{value}</Code>) : <Text size="sm" c="dimmed">No projected value</Text>}
            </Group>
          </div>
        );
      })}
    </Stack>
  );
}

export function TreatmentEvidence({ studies }: { studies: FactorStudyProjection[] }) {
  return (
    <Card withBorder padding="lg">
      <Stack>
        <div>
          <Text size="xs" c="dimmed" tt="uppercase" fw={700}>Treatment identity</Text>
          <Title order={2} size="h3">Changed & held factors</Title>
          <Text c="dimmed">Canonical values, roles, and labels come from the immutable report projection.</Text>
        </div>
        {studies.length === 0 ? (
          <Alert color="gray" title="No factor-study projection">This report did not project treatment factors.</Alert>
        ) : studies.map((study) => {
          const studyKey = `${study.operation_id}:${study.metric.id}:${study.metric.semantic_revision}:${study.metric.source}`;
          return (
          <Card key={studyKey} withBorder padding="md">
            <Stack>
              <Group justify="space-between" align="flex-start" wrap="wrap">
                <div>
                  <Text fw={700}>{study.operation_label}</Text>
                  <Text size="xs" ff="monospace">{study.operation_id}</Text>
                </div>
                <Badge variant="light">{study.metric.label}</Badge>
              </Group>
              <SimpleGrid cols={{ base: 1, md: 2 }}>
                <div>
                  <Title id={`${study.operation_id}-${study.metric.id}-changed-factors`} order={3} size="h4" mb="xs">Changed across test combinations</Title>
                  <FactorRoleList study={study} ids={study.varied_factor_ids} empty="No changed factors were projected." />
                </div>
                <div>
                  <Title id={`${study.operation_id}-${study.metric.id}-held-factors`} order={3} size="h4" mb="xs">Held constant</Title>
                  <FactorRoleList study={study} ids={study.controlled_factor_ids} empty="No held factors were projected." />
                </div>
              </SimpleGrid>
            </Stack>
          </Card>
          );
        })}
      </Stack>
    </Card>
  );
}

function layoutLabel(study: FactorStudyProjection): string {
  switch (study.layout.kind) {
    case "single_cell": return "Single cell";
    case "trend": return `Trend · ${study.cells[0] ? factorFor(study.cells[0], study.layout.factor_id)?.label ?? labelIdentifier(study.layout.factor_id) : labelIdentifier(study.layout.factor_id)}`;
    case "matrix": return `Matrix · ${labelIdentifier(study.layout.row_factor_id)} × ${labelIdentifier(study.layout.column_factor_id)}`;
    case "small_multiples": return `Small multiples · ${study.layout.factor_ids.map(labelIdentifier).join(", ")}`;
  }
  return assertNever(study.layout);
}

function studyFactorSummary(cell: FactorStudyCell): string {
  return cell.factors
    .filter(({ role }) => role === "varied")
    .map((factor) => `${factor.label}: ${formatFactor(factor)}`)
    .join(" · ") || "Single test combination";
}

function intervalText(cell: FactorStudyCell, unit: MetricUnit): string {
  if (cell.confidence_interval) {
    const interval = cell.confidence_interval;
    return `${formatMetricValue(interval.lower, unit)} – ${formatMetricValue(interval.upper, unit)} · ${formatNumber(interval.level * 100)}% · ${interval.method.replaceAll("_", " ")} · ${formatInteger(interval.resamples)} resamples`;
  }
  return cell.interval_omission_reason ?? "Unavailable";
}

function ControlComparisonTable({ study }: { study: FactorStudyProjection }) {
  if (study.control_comparisons.length === 0) {
    return <Alert color="gray" title="No control-value comparisons">No varied factor in this projection declared a control value.</Alert>;
  }
  return (
    <div>
      <Title order={4} size="h5" mb="xs">Control-value comparisons</Title>
      <Table.ScrollContainer minWidth={1180} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": `${study.operation_label} control-value comparisons` } }}>
        <Table striped>
          <Table.Thead><Table.Tr><Table.Th>Comparison</Table.Th><Table.Th>Control cell</Table.Th><Table.Th>Candidate cell</Table.Th><Table.Th>Changed factors</Table.Th><Table.Th>Control median</Table.Th><Table.Th>Candidate median</Table.Th><Table.Th>Absolute difference</Table.Th><Table.Th>Percentage difference</Table.Th><Table.Th>Median-difference interval</Table.Th></Table.Tr></Table.Thead>
          <Table.Tbody>{study.control_comparisons.map((comparison) => {
            const interval = comparison.median_difference_confidence_interval;
            return (
              <Table.Tr key={comparison.comparison_id}>
                <Table.Td><Code>{comparison.comparison_id}</Code></Table.Td>
                <Table.Td><Code>{comparison.control_cell_id}</Code></Table.Td>
                <Table.Td><Code>{comparison.candidate_cell_id}</Code></Table.Td>
                <Table.Td>{comparison.changed_factor_ids.map(labelIdentifier).join(", ")}</Table.Td>
                <Table.Td>{formatMetricValue(comparison.control_median, study.metric.unit)}</Table.Td>
                <Table.Td>{formatMetricValue(comparison.candidate_median, study.metric.unit)}</Table.Td>
                <Table.Td>{formatMetricValue(comparison.absolute_difference, study.metric.unit)}</Table.Td>
                <Table.Td>{comparison.percentage_difference === null ? "Unavailable" : `${formatNumber(comparison.percentage_difference, 2)}%`}</Table.Td>
                <Table.Td>{interval
                  ? `${formatMetricValue(interval.lower, study.metric.unit)} – ${formatMetricValue(interval.upper, study.metric.unit)} · ${formatNumber(interval.level * 100)}% · ${interval.method.replaceAll("_", " ")} · ${formatInteger(interval.resamples)} resamples`
                  : comparison.interval_omission_reason ?? "Unavailable"}</Table.Td>
              </Table.Tr>
            );
          })}</Table.Tbody>
        </Table>
      </Table.ScrollContainer>
    </div>
  );
}

function StudyPointPlot({ study }: { study: FactorStudyProjection }) {
  const values = study.cells.flatMap((cell) => [
    ...cell.raw_points.map(({ value }) => value),
    ...(cell.median === null ? [] : [cell.median]),
    ...(cell.confidence_interval ? [cell.confidence_interval.lower, cell.confidence_interval.upper] : []),
  ]);
  if (values.length === 0) return <Alert color="gray" title="No numeric points">The backend projected no available observations for this study.</Alert>;
  const minimum = Math.min(...values);
  const maximum = Math.max(...values);
  const span = maximum - minimum || Math.max(Math.abs(maximum), 1);
  const low = minimum - span * 0.08;
  const high = maximum + span * 0.08;
  const x = (cellIndex: number) => 86 + (study.cells.length === 1 ? 390 : cellIndex * (780 / (study.cells.length - 1)));
  const y = (value: number) => 220 - ((value - low) / (high - low)) * 172;
  return (
    <div className="factor-study-plot" tabIndex={0} role="region" aria-label={`${study.operation_label} factor-study plot`}>
      <svg viewBox="0 0 960 270" role="img" aria-label={`${study.operation_label} ${study.metric.label} server-authored observations, medians, and confidence intervals`}>
        <title>{study.operation_label}: raw observations, backend medians, and backend confidence intervals</title>
        <line className="study-axis" x1="70" x2="900" y1="220" y2="220" />
        <line className="study-axis" x1="70" x2="70" y1="40" y2="220" />
        <text className="study-axis-label" x="64" y="48" textAnchor="end">{formatMetricValue(high, study.metric.unit)}</text>
        <text className="study-axis-label" x="64" y="220" textAnchor="end">{formatMetricValue(low, study.metric.unit)}</text>
        {study.cells.map((cell, cellIndex) => {
          const center = x(cellIndex);
          return (
            <g key={cell.cell_id}>
              {cell.confidence_interval ? <line className="study-interval" x1={center} x2={center} y1={y(cell.confidence_interval.lower)} y2={y(cell.confidence_interval.upper)} /> : null}
              {cell.raw_points.map((point, pointIndex) => (
                <circle
                  className={point.outlier ? "study-point study-point-outlier" : "study-point"}
                  cx={center + ((pointIndex % 5) - 2) * 5}
                  cy={y(point.value)}
                  key={point.trial_id}
                  r={point.outlier ? 5 : 4}
                />
              ))}
              {cell.median === null ? null : <rect className="study-median" x={center - 6} y={y(cell.median) - 6} width="12" height="12" />}
              <text className="study-axis-label" x={center} y="245" textAnchor="middle">{cell.cell_id.length > 22 ? `${cell.cell_id.slice(0, 19)}…` : cell.cell_id}</text>
            </g>
          );
        })}
      </svg>
      <Text size="xs" c="dimmed">Circles are server-authored raw points; squares and vertical bars are server-authored medians and intervals. Plot positions are display-only.</Text>
    </div>
  );
}

function MatrixProjection({ study }: { study: FactorStudyProjection & { layout: Extract<FactorStudyProjection["layout"], { kind: "matrix" }> } }) {
  const rowId = study.layout.row_factor_id;
  const columnId = study.layout.column_factor_id;
  const rows = [...new Set(study.cells.map((cell) => factorFor(cell, rowId)).filter((factor): factor is ReportFactor => factor !== undefined).map(formatFactor))];
  const columns = [...new Set(study.cells.map((cell) => factorFor(cell, columnId)).filter((factor): factor is ReportFactor => factor !== undefined).map(formatFactor))];
  return (
    <Table.ScrollContainer minWidth={640} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": `${study.operation_label} factor matrix` } }}>
      <Table withTableBorder withColumnBorders>
        <Table.Thead>
          <Table.Tr><Table.Th>{labelIdentifier(rowId)} ↓ / {labelIdentifier(columnId)} →</Table.Th>{columns.map((value) => <Table.Th key={value}>{value}</Table.Th>)}</Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {rows.map((row) => (
            <Table.Tr key={row}>
              <Table.Th scope="row">{row}</Table.Th>
              {columns.map((column) => {
                const cell = study.cells.find((candidate) => {
                  const rowFactor = factorFor(candidate, rowId);
                  const columnFactor = factorFor(candidate, columnId);
                  return rowFactor !== undefined && columnFactor !== undefined && formatFactor(rowFactor) === row && formatFactor(columnFactor) === column;
                });
                return <Table.Td key={column} className={cell ? "factor-matrix-cell" : undefined}>{cell ? <><Text fw={700}>{formatMetricValue(cell.median, study.metric.unit)}</Text><Text size="xs">n {formatInteger(cell.successful_n)} · {intervalText(cell, study.metric.unit)}</Text></> : <Text c="dimmed">Not projected</Text>}</Table.Td>;
              })}
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
    </Table.ScrollContainer>
  );
}

function SmallMultiples({ study }: { study: FactorStudyProjection }) {
  return (
    <SimpleGrid cols={{ base: 1, sm: 2, lg: 3 }}>
      {study.cells.map((cell) => (
        <Card key={cell.cell_id} withBorder padding="sm">
          <Text fw={700} ff="monospace" size="sm">{cell.cell_id}</Text>
          <Text size="xs" c="dimmed">{studyFactorSummary(cell)}</Text>
          <Text mt="xs"><strong>{formatMetricValue(cell.median, study.metric.unit)}</strong> median</Text>
          <Text size="xs">n {formatInteger(cell.successful_n)} · interval {intervalText(cell, study.metric.unit)}</Text>
        </Card>
      ))}
    </SimpleGrid>
  );
}

function LayoutProjection({ study }: { study: FactorStudyProjection }) {
  switch (study.layout.kind) {
    case "single_cell": return <StudyPointPlot study={study} />;
    case "trend": return <StudyPointPlot study={study} />;
    case "matrix": return <MatrixProjection study={{ ...study, layout: study.layout }} />;
    case "small_multiples": return <><SmallMultiples study={study} /><StudyPointPlot study={study} /></>;
  }
  return assertNever(study.layout);
}

function FactorStudyTables({ study }: { study: FactorStudyProjection }) {
  return (
    <Stack>
      <Table.ScrollContainer minWidth={860} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": `${study.operation_label} factor study summary` } }}>
        <Table striped>
          <Table.Thead><Table.Tr><Table.Th>Cell</Table.Th><Table.Th>Changed factors</Table.Th><Table.Th>Successful / failed</Table.Th><Table.Th>Median</Table.Th><Table.Th>Median interval</Table.Th></Table.Tr></Table.Thead>
          <Table.Tbody>{study.cells.map((cell) => (
            <Table.Tr key={cell.cell_id}>
              <Table.Td><Code>{cell.cell_id}</Code></Table.Td>
              <Table.Td>{studyFactorSummary(cell)}</Table.Td>
              <Table.Td>{formatInteger(cell.successful_n)} / {formatInteger(cell.failed_n)}</Table.Td>
              <Table.Td>{formatMetricValue(cell.median, study.metric.unit)}</Table.Td>
              <Table.Td>{intervalText(cell, study.metric.unit)}</Table.Td>
            </Table.Tr>
          ))}</Table.Tbody>
        </Table>
      </Table.ScrollContainer>
      <ControlComparisonTable study={study} />
      <details>
        <summary>Raw observations ({formatInteger(study.cells.reduce((sum, cell) => sum + cell.raw_points.length, 0))})</summary>
        <Table.ScrollContainer minWidth={640} mah={420} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": `${study.operation_label} raw observations` } }}>
          <Table striped stickyHeader>
            <Table.Thead><Table.Tr><Table.Th>Cell</Table.Th><Table.Th>Trial</Table.Th><Table.Th>Request</Table.Th><Table.Th>Display value</Table.Th><Table.Th>Exact integer</Table.Th><Table.Th>Outlier label</Table.Th></Table.Tr></Table.Thead>
            <Table.Tbody>{study.cells.flatMap((cell) => cell.raw_points.map((point) => (
              <Table.Tr key={`${cell.cell_id}:${point.trial_id}`}>
                <Table.Td><Code>{cell.cell_id}</Code></Table.Td>
                <Table.Td><Code>{point.trial_id}</Code></Table.Td>
                <Table.Td><Code>{point.request_id ?? "Trial-scoped"}</Code></Table.Td>
                <Table.Td>{formatMetricValue(point.value, study.metric.unit)}</Table.Td>
                <Table.Td>{point.raw_integer_value === null ? "Not integer-backed" : <Code>{formatInteger(point.raw_integer_value)}</Code>}</Table.Td>
                <Table.Td><Badge color={point.outlier ? "yellow" : "gray"} variant="light">{point.outlier ? "Labeled" : "No"}</Badge></Table.Td>
              </Table.Tr>
            )))}</Table.Tbody>
          </Table>
        </Table.ScrollContainer>
      </details>
    </Stack>
  );
}

export function FactorStudyEvidence({ studies }: { studies: FactorStudyProjection[] }) {
  if (studies.length === 0) return <Alert color="gray" title="No factor-study evidence">The report contains no backend-authored factor projections.</Alert>;
  return (
    <Stack>
      <div>
        <Title order={2} size="h3">Factor-study projections</Title>
        <Text c="dimmed">Layouts, statistics, intervals, raw points, and outlier labels are supplied by the report.</Text>
      </div>
      {studies.map((study) => {
        const studyKey = `${study.operation_id}:${study.metric.id}:${study.metric.semantic_revision}:${study.metric.source}`;
        return (
        <Card key={studyKey} withBorder padding="lg">
          <Stack>
            <Group justify="space-between" align="flex-start" wrap="wrap">
              <div>
                <Title order={3} size="h4">{study.operation_label}</Title>
                <Text>{study.metric.label} · {study.metric.unit}</Text>
                <Text size="xs" c="dimmed">{study.metric.help} · semantic revision {study.metric.semantic_revision}</Text>
              </div>
              <Badge variant="light">{layoutLabel(study)}</Badge>
            </Group>
            <LayoutProjection study={study} />
            <FactorStudyTables study={study} />
          </Stack>
        </Card>
        );
      })}
    </Stack>
  );
}

function timelinePosition(value: number, timeline: ResourceTimelineProjection): number {
  const span = timeline.domain_end_ns - timeline.domain_start_ns;
  if (span <= 0) return 0;
  return Math.max(0, Math.min(100, ((value - timeline.domain_start_ns) / span) * 100));
}

function timelineWidth(start: number, duration: number, timeline: ResourceTimelineProjection): number {
  const startPosition = timelinePosition(start, timeline);
  const endPosition = timelinePosition(start + duration, timeline);
  return Math.max(0, endPosition - startPosition);
}

function resourceValue(value: ResourceTimelineProjection["series"][number]["points"][number]["value"], unit: MetricUnit): string {
  switch (value.availability) {
    case "available": return formatMetricValue(value.value, unit);
    case "unavailable": return `${value.source}: ${value.reason}`;
  }
  return assertNever(value);
}

function ResourceTimelineGraphic({ timeline }: { timeline: ResourceTimelineProjection }) {
  return (
    <div className="resource-timeline-scroll" tabIndex={0} role="region" aria-label={`Resource timeline graphic for ${timeline.trial_id}`}>
      <div className="resource-timeline" role="img" aria-label={`Exact request and phase spans with resource sample offsets from ${formatDurationNs(timeline.domain_start_ns)} to ${formatDurationNs(timeline.domain_end_ns)}`}>
        <div className="resource-timeline-axis"><span>{formatDurationNs(timeline.domain_start_ns)}</span><span>Monotonic offset</span><span>{formatDurationNs(timeline.domain_end_ns)}</span></div>
        {timeline.operation_window ? (
          <div className="resource-timeline-row">
            <Text size="sm" fw={650}>Operation window</Text>
            <div className="resource-timeline-track" aria-hidden="true"><span className="resource-timeline-span timeline-operation-window" style={{ left: `${timelinePosition(timeline.operation_window.start_offset_ns, timeline)}%`, width: `${timelineWidth(timeline.operation_window.start_offset_ns, timeline.operation_window.duration_ns, timeline)}%` }} title={`${formatDurationNs(timeline.operation_window.start_offset_ns)} + ${formatDurationNs(timeline.operation_window.duration_ns)}`} /></div>
          </div>
        ) : null}
        {timeline.request_spans.map((span) => (
          <div className="resource-timeline-row" key={`request:${span.request_id}`}>
            <Text size="sm" ff="monospace" className="wrap-anywhere">Request · {span.request_id}</Text>
            <div className="resource-timeline-track" aria-hidden="true"><span className={`resource-timeline-span timeline-status-${span.succeeded ? "succeeded" : "failed"}`} style={{ left: `${timelinePosition(span.start_offset_ns, timeline)}%`, width: `${timelineWidth(span.start_offset_ns, span.duration_ns, timeline)}%` }} title={`${formatDurationNs(span.start_offset_ns)} + ${formatDurationNs(span.duration_ns)}`} /></div>
          </div>
        ))}
        {timeline.phase_spans.map((span, index) => (
          <div className="resource-timeline-row" key={`phase:${span.id}:${span.start_offset_ns}:${index}`}>
            <Text size="sm">Phase · {span.label}</Text>
            <div className="resource-timeline-track" aria-hidden="true"><span className={`resource-timeline-span timeline-status-${span.status}`} style={{ left: `${timelinePosition(span.start_offset_ns, timeline)}%`, width: `${timelineWidth(span.start_offset_ns, span.duration_ns, timeline)}%` }} title={`${formatDurationNs(span.start_offset_ns)} + ${formatDurationNs(span.duration_ns)}`} /></div>
          </div>
        ))}
        {timeline.series.map((series, seriesIndex) => (
          <div className="resource-timeline-row" key={`series:${series.identity.id}:${series.request_id ?? "trial"}:${seriesIndex}`}>
            <Text size="sm">Resource · {series.identity.label}</Text>
            <div className="resource-timeline-track" aria-hidden="true">{series.points.map((point, pointIndex) => <span className={`resource-timeline-marker ${point.value.availability === "unavailable" ? "resource-marker-unavailable" : "resource-marker-available"}`} key={`${point.monotonic_offset_ns}:${pointIndex}`} style={{ left: `${timelinePosition(point.monotonic_offset_ns, timeline)}%` }} title={`${formatDurationNs(point.monotonic_offset_ns)} · ${resourceValue(point.value, series.identity.unit)}`} />)}</div>
          </div>
        ))}
      </div>
    </div>
  );
}

function ResourceTimelineTables({ timeline }: { timeline: ResourceTimelineProjection }) {
  return (
    <Stack>
      {timeline.operation_window ? (
        <SimpleGrid cols={{ base: 1, sm: 3 }} className="operation-window-summary">
          <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Operation start</Text><Text>{formatDurationNs(timeline.operation_window.start_offset_ns)}</Text></div>
          <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Operation duration</Text><Text>{formatDurationNs(timeline.operation_window.duration_ns)}</Text></div>
          <div><Text size="xs" c="dimmed" tt="uppercase" fw={650}>Operation end</Text><Text>{formatDurationNs(timeline.operation_window.start_offset_ns + timeline.operation_window.duration_ns)}</Text></div>
        </SimpleGrid>
      ) : <Alert color="gray" title="Operation window unavailable">This timeline did not project an eligible operation window.</Alert>}
      <Table.ScrollContainer minWidth={760} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": `${timeline.trial_id} exact request spans` } }}>
        <Table striped>
          <Table.Thead><Table.Tr><Table.Th>Request</Table.Th><Table.Th>Start offset</Table.Th><Table.Th>Duration</Table.Th><Table.Th>End offset</Table.Th><Table.Th>Status</Table.Th></Table.Tr></Table.Thead>
          <Table.Tbody>{timeline.request_spans.map((span) => <Table.Tr key={span.request_id}><Table.Td><Code>{span.request_id}</Code></Table.Td><Table.Td>{formatDurationNs(span.start_offset_ns)}</Table.Td><Table.Td>{formatDurationNs(span.duration_ns)}</Table.Td><Table.Td>{formatDurationNs(span.start_offset_ns + span.duration_ns)}</Table.Td><Table.Td><Badge color={span.succeeded ? "green" : "red"}>{span.status}</Badge></Table.Td></Table.Tr>)}</Table.Tbody>
        </Table>
      </Table.ScrollContainer>
      {timeline.phase_spans.length > 0 ? (
        <Table.ScrollContainer minWidth={960} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": `${timeline.trial_id} exact phase spans` } }}>
          <Table striped>
            <Table.Thead><Table.Tr><Table.Th>Phase</Table.Th><Table.Th>Request</Table.Th><Table.Th>Revision</Table.Th><Table.Th>Start offset</Table.Th><Table.Th>Duration</Table.Th><Table.Th>Status</Table.Th></Table.Tr></Table.Thead>
            <Table.Tbody>{timeline.phase_spans.map((span, index) => <Table.Tr key={`${span.id}:${index}`}><Table.Td><Text fw={600}>{span.label}</Text><Text size="xs" c="dimmed">{span.help}</Text></Table.Td><Table.Td><Code>{span.request_id ?? "Trial-scoped"}</Code></Table.Td><Table.Td>{span.semantic_revision}</Table.Td><Table.Td>{formatDurationNs(span.start_offset_ns)}</Table.Td><Table.Td>{formatDurationNs(span.duration_ns)}</Table.Td><Table.Td><Badge color={span.status === "succeeded" ? "green" : span.status === "failed" ? "red" : "yellow"}>{span.status}</Badge></Table.Td></Table.Tr>)}</Table.Tbody>
          </Table>
        </Table.ScrollContainer>
      ) : null}
      <Table.ScrollContainer minWidth={1040} mah={440} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": `${timeline.trial_id} resource samples` } }}>
        <Table striped stickyHeader>
          <Table.Thead><Table.Tr><Table.Th>Metric / scope</Table.Th><Table.Th>Request</Table.Th><Table.Th>Offset</Table.Th><Table.Th>Sampled</Table.Th><Table.Th>Value or explicit unavailability</Table.Th></Table.Tr></Table.Thead>
          <Table.Tbody>{timeline.series.flatMap((series, seriesIndex) => series.points.map((point, pointIndex) => (
            <Table.Tr key={`${seriesIndex}:${pointIndex}:${point.monotonic_offset_ns}`}>
              <Table.Td>{series.identity.label}<br /><Text span size="xs">{series.identity.help} · {series.identity.scope} · {series.identity.source}</Text></Table.Td>
              <Table.Td><Code>{series.request_id ?? "Trial-scoped"}</Code></Table.Td>
              <Table.Td>{formatDurationNs(point.monotonic_offset_ns)}</Table.Td>
              <Table.Td><Badge variant="light" color={point.sampled ? "blue" : "gray"}>{point.sampled ? "Yes" : "No"}</Badge></Table.Td>
              <Table.Td>{point.value.availability === "available" ? resourceValue(point.value, series.identity.unit) : <Alert color="yellow" className="resource-unavailable-cell" title="Unavailable">{resourceValue(point.value, series.identity.unit)}</Alert>}</Table.Td>
            </Table.Tr>
          )))}</Table.Tbody>
        </Table>
      </Table.ScrollContainer>
    </Stack>
  );
}

export function ResourceTimelineEvidence({ timelines }: { timelines: ResourceTimelineProjection[] }) {
  if (timelines.length === 0) return <Alert color="gray" title="No resource timelines">No trial-level span or sampling projection was persisted for this cell.</Alert>;
  return (
    <Stack>
      <Title order={3} size="h4">Resource timelines</Title>
      <Text c="dimmed">Request spans, phase spans, samples, and unavailability are aligned by persisted monotonic offsets. Exact values remain available in the tables.</Text>
      <Accordion variant="separated" multiple defaultValue={[timelines[0]?.trial_id ?? ""]}>
        {timelines.map((timeline) => (
          <Accordion.Item key={timeline.trial_id} value={timeline.trial_id}>
            <Accordion.Control><Text fw={650}>Trial <Code>{timeline.trial_id}</Code> · {formatDurationNs(timeline.domain_end_ns - timeline.domain_start_ns)} domain</Text></Accordion.Control>
            <Accordion.Panel><Stack><ResourceTimelineGraphic timeline={timeline} /><ResourceTimelineTables timeline={timeline} /></Stack></Accordion.Panel>
          </Accordion.Item>
        ))}
      </Accordion>
    </Stack>
  );
}

export function DetailedCheckEvidence({ evidence }: { evidence: CheckEvidenceReport[] }) {
  if (evidence.length === 0) return <Alert color="gray" title="No detailed check evidence">No bounded expected/actual items were projected for this cell.</Alert>;
  return (
    <Stack>
      <Title order={3} size="h4">Detailed check evidence</Title>
      <Accordion variant="separated" multiple defaultValue={[`${evidence[0]?.id}:${evidence[0]?.trial_id}`]}>
        {evidence.map((check) => {
          const value = `${check.id}:${check.trial_id}`;
          return (
            <Accordion.Item key={value} value={value}>
              <Accordion.Control>
                <Group justify="space-between" wrap="wrap" pr="sm">
                  <div><Text fw={650}>{check.label}</Text><Text size="xs">Trial <Code>{check.trial_id}</Code></Text></div>
                  <Badge color={check.verdict === "pass" ? "green" : "red"}>{check.verdict}</Badge>
                </Group>
              </Accordion.Control>
              <Accordion.Panel>
                <Stack>
                  <Text>{check.help}</Text>
                  <Group gap="xs" wrap="wrap">
                    <Badge variant="light">Revision {check.semantic_revision}</Badge>
                    <Badge variant="light">{formatDurationNs(check.duration_ns)}</Badge>
                    <Text size="xs">Request <Code>{check.request_id ?? "Trial-scoped"}</Code></Text>
                  </Group>
                  <Table.ScrollContainer minWidth={760} scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": `${check.label} expected and actual evidence` } }}>
                    <Table striped withTableBorder>
                      <Table.Thead><Table.Tr><Table.Th>Expected</Table.Th><Table.Th>Actual</Table.Th><Table.Th>Artifact label</Table.Th></Table.Tr></Table.Thead>
                      <Table.Tbody>{check.evidence.items.map((item, index) => (
                        <Table.Tr key={`${index}:${item.artifact_id ?? "inline"}`}>
                          <Table.Td className="wrap-anywhere">{item.expected}</Table.Td>
                          <Table.Td className="wrap-anywhere">{item.actual}</Table.Td>
                          <Table.Td>{item.artifact_id ? <><Text size="xs" c="dimmed">Artifact</Text><Code className="wrap-anywhere">{item.artifact_id}</Code></> : <Text c="dimmed">Inline evidence only</Text>}</Table.Td>
                        </Table.Tr>
                      ))}</Table.Tbody>
                    </Table>
                  </Table.ScrollContainer>
                  {check.evidence.truncated_count > 0 ? (
                    <Alert color="yellow" title={`${formatInteger(check.evidence.truncated_count)} evidence items omitted by the backend bound`}>
                      Omitted-items SHA-256 <Code className="wrap-anywhere">{check.evidence.truncated_sha256 ?? "Unavailable"}</Code>
                    </Alert>
                  ) : <Alert color="green" title="Complete bounded evidence">No evidence items were omitted.</Alert>}
                </Stack>
              </Accordion.Panel>
            </Accordion.Item>
          );
        })}
      </Accordion>
    </Stack>
  );
}
