import { Alert, Badge, Card, Code, Group, SimpleGrid, Stack, Table, Text, Title } from "@mantine/core";
import type { MetricSummary } from "@/api/types";
import { formatInteger, formatMetricValue, formatNumber } from "@/lib/format";
import { DistributionPlot } from "@/plots/DistributionPlot";

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <Text size="xs" c="dimmed" tt="uppercase" fw={600}>{label}</Text>
      <Text fw={600}>{value}</Text>
    </div>
  );
}

export function DistributionEvidence({ metric }: { metric: MetricSummary }) {
  const { identity, statistics, unavailable } = metric;
  const unavailableReasons = Object.entries(unavailable.reasons);

  return (
    <Card withBorder padding="lg">
      <Stack>
        <Group justify="space-between" align="flex-start" wrap="wrap">
          <div>
            <Title order={3} size="h4">{identity.label}</Title>
            <Text size="sm">{identity.help}</Text>
            <Text size="sm" c="dimmed">
              <Code>{identity.id}</Code> · {identity.scope} · {identity.unit} · {identity.aggregation} · semantic revision {identity.semantic_revision} · report derivation {identity.report_derivation_revision}
            </Text>
          </div>
          <Group>
            <Badge variant="light">{identity.direction.replaceAll("_", " ")}</Badge>
            {statistics.p95_exploratory ? <Badge color="yellow" variant="light">p95 exploratory</Badge> : null}
          </Group>
        </Group>

        <SimpleGrid cols={{ base: 2, sm: 3, lg: 7 }}>
          <Stat label="Attempted n" value={formatInteger(metric.attempted_n)} />
          <Stat label="Available n" value={formatInteger(metric.available_n)} />
          <Stat label="Failed n" value={formatInteger(metric.failed_n)} />
          <Stat label="Unavailable n" value={formatInteger(unavailable.count)} />
          <Stat label="Median" value={formatMetricValue(statistics.median, identity.unit)} />
          <Stat label="p25 / p75" value={`${formatMetricValue(statistics.p25, identity.unit)} / ${formatMetricValue(statistics.p75, identity.unit)}`} />
          <Stat label="p95" value={formatMetricValue(statistics.p95, identity.unit)} />
          <Stat label="Mean" value={formatMetricValue(statistics.mean, identity.unit)} />
          <Stat label="Std. deviation" value={formatMetricValue(statistics.sample_standard_deviation, identity.unit)} />
          <Stat label="MAD" value={formatMetricValue(statistics.median_absolute_deviation, identity.unit)} />
          <Stat label="Coefficient variation" value={formatNumber(statistics.coefficient_of_variation, 4)} />
          <Stat label="Minimum" value={formatMetricValue(statistics.minimum, identity.unit)} />
          <Stat label="Maximum" value={formatMetricValue(statistics.maximum, identity.unit)} />
        </SimpleGrid>

        {statistics.median_confidence_interval ? (
          <Alert color="blue" title={`${formatNumber(statistics.median_confidence_interval.level * 100)}% median confidence interval`}>
            {formatMetricValue(statistics.median_confidence_interval.lower, identity.unit)} to {formatMetricValue(statistics.median_confidence_interval.upper, identity.unit)} · {statistics.median_confidence_interval.method.replaceAll("_", " ")} · {formatInteger(statistics.median_confidence_interval.resamples)} resamples
          </Alert>
        ) : (
          <Alert color="gray" title="Median confidence interval unavailable">
            {statistics.confidence_interval_omission?.replaceAll("_", " ") ?? "The runner did not provide an interval or omission code."}
          </Alert>
        )}

        {unavailableReasons.length > 0 ? (
          <Alert color="yellow" title="Explicitly unavailable observations">
            {unavailableReasons.map(([reason, count]) => `${reason}: ${formatInteger(count)}`).join(" · ")}
          </Alert>
        ) : null}

        {statistics.distribution.kind === "empty" ? (
          <Text c="dimmed">No distribution projection is available because there are no available samples.</Text>
        ) : null}
        <DistributionPlot metric={metric} />
        {metric.raw_points.length > 0 ? (
          <div>
            <Text fw={600} mb="xs">Authored trial/request data rows</Text>
            <Table.ScrollContainer minWidth={860} mah={420} type="native" tabIndex={0} aria-label={`${identity.label} raw trial and request data`}>
              <Table striped stickyHeader>
                <Table.Thead><Table.Tr><Table.Th>Trial id</Table.Th><Table.Th>Request id</Table.Th><Table.Th>Display value</Table.Th><Table.Th>Exact raw integer</Table.Th><Table.Th>Outlier policy result</Table.Th></Table.Tr></Table.Thead>
                <Table.Tbody>
                  {metric.raw_points.map((point, index) => (
                    <Table.Tr key={`${point.trial_id}:${point.request_id ?? "trial"}:${index}`}>
                      <Table.Td><Code>{point.trial_id}</Code></Table.Td>
                      <Table.Td><Code>{point.request_id ?? "Trial-scoped"}</Code></Table.Td>
                      <Table.Td>{formatMetricValue(point.value, identity.unit)}</Table.Td>
                      <Table.Td>{point.raw_integer_value === null ? "Not integer-backed" : <Code>{formatInteger(point.raw_integer_value)}</Code>}</Table.Td>
                      <Table.Td>{point.outlier ? <Badge color="yellow">Flagged and retained</Badge> : "Included"}</Table.Td>
                    </Table.Tr>
                  ))}
                </Table.Tbody>
              </Table>
            </Table.ScrollContainer>
          </div>
        ) : <Text c="dimmed">No eligible raw trial/request rows were authored for this metric.</Text>}
        {statistics.distribution.kind === "histogram_ecdf" ? (
          <SimpleGrid cols={{ base: 1, md: 2 }}>
            <div>
              <Text fw={600} mb="xs">Backend-projected histogram · {statistics.distribution.histogram.method.replaceAll("_", " ")}</Text>
              <Table.ScrollContainer minWidth={420} mah={360} type="native" tabIndex={0} aria-label="Histogram bins">
                <Table striped stickyHeader>
                  <Table.Thead><Table.Tr><Table.Th>Lower edge</Table.Th><Table.Th>Upper edge</Table.Th><Table.Th>Count</Table.Th></Table.Tr></Table.Thead>
                  <Table.Tbody>
                    {statistics.distribution.histogram.counts.map((count, index) => (
                      <Table.Tr key={index}>
                        <Table.Td>{formatMetricValue(statistics.distribution.kind === "histogram_ecdf" ? statistics.distribution.histogram.edges[index] ?? null : null, identity.unit)}</Table.Td>
                        <Table.Td>{formatMetricValue(statistics.distribution.kind === "histogram_ecdf" ? statistics.distribution.histogram.edges[index + 1] ?? null : null, identity.unit)}</Table.Td>
                        <Table.Td>{formatInteger(count)}</Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </Table.ScrollContainer>
            </div>
            <div>
              <Text fw={600} mb="xs">Backend-projected ECDF</Text>
              <Table.ScrollContainer minWidth={420} mah={360} type="native" tabIndex={0} aria-label="Empirical cumulative distribution">
                <Table striped stickyHeader>
                  <Table.Thead><Table.Tr><Table.Th>Value</Table.Th><Table.Th>Cumulative probability</Table.Th></Table.Tr></Table.Thead>
                  <Table.Tbody>
                    {statistics.distribution.ecdf.map((point, index) => (
                      <Table.Tr key={index}>
                        <Table.Td>{formatMetricValue(point.value, identity.unit)}</Table.Td>
                        <Table.Td>{formatNumber(point.cumulative_probability, 4)}</Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </Table.ScrollContainer>
            </div>
          </SimpleGrid>
        ) : null}
      </Stack>
    </Card>
  );
}
