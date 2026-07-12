import { Alert, Anchor, Badge, Card, Group, SimpleGrid, Stack, Table, Text, Title } from "@mantine/core";
import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router";
import { benchmarkApi } from "@/api/client";
import { DefaultPlanLauncher } from "@/components/DefaultPlanLauncher";
import { ErrorState, LoadingState } from "@/components/AsyncState";
import { formatTimestamp } from "@/lib/format";
import { FAMILY_ROUTES } from "@/routes";

function ReadinessPanel() {
  const health = useQuery({ queryKey: ["health"], queryFn: benchmarkApi.health });
  const settings = useQuery({ queryKey: ["settings"], queryFn: benchmarkApi.settings });

  if (health.isPending || settings.isPending) return <LoadingState label="Checking runner readiness" />;
  if (health.error || settings.error) {
    return <ErrorState error={health.error ?? settings.error} retry={() => void Promise.all([health.refetch(), settings.refetch()])} />;
  }
  if (!health.data || !settings.data) return null;

  return (
    <Alert
      color={health.data.execution_ready ? "green" : "red"}
      title={health.data.execution_ready ? "Runner and workspace are ready" : "Runner cannot execute this benchmark"}
    >
      <Stack gap="xs">
        <Text className="wrap-anywhere">{settings.data.test_workspace_root}</Text>
        <Group gap="lg" wrap="wrap">
          <Text>{settings.data.writable ? "Writable" : "Not writable"}</Text>
          <Text>{settings.data.path_health.root_marker ? "Ownership marker present" : "Ownership marker missing"}</Text>
          <Text>Runner {health.data.version}</Text>
        </Group>
        {!health.data.execution_ready ? (
          <Text>
            Canonical: {settings.data.path_health.canonical ? "yes" : "no"}; outside repository: {settings.data.path_health.outside_repository ? "yes" : "no"}.
          </Text>
        ) : null}
      </Stack>
    </Alert>
  );
}

function RecentRuns() {
  const runs = useQuery({ queryKey: ["runs"], queryFn: () => benchmarkApi.listRuns() });
  if (runs.isPending) return <LoadingState label="Loading recent runs" />;
  if (runs.error) return <ErrorState error={runs.error} retry={() => void runs.refetch()} />;
  if (!runs.data || runs.data.runs.length === 0) {
    return <Text c="dimmed">No benchmark runs have been recorded by this runner.</Text>;
  }

  return (
    <Table.ScrollContainer
      minWidth={720}
      scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Recent benchmark runs" } }}
    >
      <Table striped highlightOnHover>
        <Table.Thead>
          <Table.Tr>
            <Table.Th>Run</Table.Th>
            <Table.Th>Question</Table.Th>
            <Table.Th>State</Table.Th>
            <Table.Th>Correctness</Table.Th>
            <Table.Th>Started</Table.Th>
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {runs.data.runs.map((run) => (
            <Table.Tr key={run.run_id}>
              <Table.Td>
                <Anchor component={Link} to={`/benchmark/runs/${encodeURIComponent(run.run_id)}`} ff="monospace">
                  {run.run_id}
                </Anchor>
              </Table.Td>
              <Table.Td>{run.name}</Table.Td>
              <Table.Td><Badge variant="light">{run.state}</Badge></Table.Td>
              <Table.Td>{run.correctness}</Table.Td>
              <Table.Td>{formatTimestamp(run.started_at)}</Table.Td>
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
    </Table.ScrollContainer>
  );
}

export function OverviewPage() {
  return (
    <Stack gap="xl">
      <header>
        <Text size="sm" c="dimmed">Benchmark Laboratory</Text>
        <Title>Plan a local, reproducible benchmark</Title>
        <Text maw={760}>Readiness, exact defaults, execution evidence, and reports come from the loopback runner.</Text>
      </header>
      <ReadinessPanel />
      <DefaultPlanLauncher scope="all" />
      <Card withBorder padding="lg">
        <Stack>
          <Title order={2} size="h3">Open one family</Title>
          <SimpleGrid cols={{ base: 1, sm: 2, lg: 4 }}>
            {Object.entries(FAMILY_ROUTES).map(([familyId, route]) => (
              <Anchor key={familyId} component={Link} to={route.path} className="family-link">
                {route.fallbackLabel}
              </Anchor>
            ))}
          </SimpleGrid>
        </Stack>
      </Card>
      <Card withBorder padding="lg" id="recent-runs">
        <Stack>
          <Title order={2} size="h3">Recent runs</Title>
          <RecentRuns />
        </Stack>
      </Card>
    </Stack>
  );
}
