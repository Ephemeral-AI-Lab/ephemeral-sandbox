import { Alert, Card, List, Stack, Text, Title } from "@mantine/core";
import { useQuery } from "@tanstack/react-query";
import { benchmarkApi } from "@/api/client";
import { DefaultPlanLauncher } from "@/components/DefaultPlanLauncher";
import { ErrorState, LoadingState } from "@/components/AsyncState";
import { FAMILY_ROUTES, type FamilyRouteId } from "@/routes";

export function FamilyPage({ familyRouteId }: { familyRouteId: FamilyRouteId }) {
  const definitions = useQuery({ queryKey: ["definitions"], queryFn: benchmarkApi.definitions });
  if (definitions.isPending) return <LoadingState label="Loading family definition" />;
  if (definitions.error) return <ErrorState error={definitions.error} retry={() => void definitions.refetch()} />;

  const route = FAMILY_ROUTES[familyRouteId];
  const definition = definitions.data.catalog.families.find(({ id }) => id === route.familyId);
  if (!definition) {
    return (
      <Alert color="red" title="Unsupported family">
        The runner definition catalog does not contain <Text span ff="monospace">{route.familyId}</Text>.
      </Alert>
    );
  }
  const operations = definitions.data.catalog.operations.filter(
    ({ family }) => family === route.familyId,
  );

  return (
    <Stack gap="xl">
      <header>
        <Text size="sm" c="dimmed">Benchmark / {definition.label}</Text>
        <Title>{definition.label}</Title>
        <Text maw={880}>{definition.help}</Text>
      </header>
      <Card withBorder padding="lg">
        <Stack gap="xs">
          <Title order={2} size="h3">Research question</Title>
          <Text>{definition.research_question}</Text>
          <Text fw={600}>Measured boundary</Text>
          <Text>{definition.measured_boundary}</Text>
          <Title order={3} size="h4" mt="sm">Registered operations</Title>
          {operations.length > 0 ? (
            <List spacing="sm">
              {operations.map((operation) => (
                <List.Item key={operation.id}>
                  <Text span fw={600}>{operation.label}</Text> — {operation.count_semantics_help}
                </List.Item>
              ))}
            </List>
          ) : <Text c="dimmed">No registered operation was returned for this family.</Text>}
          {familyRouteId === "layerstack" ? (
            <Alert color="blue" title="Server-authored count semantics">
              The registered squash operation above defines live sessions as prepared load and one product request per measured trial.
            </Alert>
          ) : null}
        </Stack>
      </Card>
      <DefaultPlanLauncher scope={route.scope} />
      <Text size="sm" c="dimmed">
        Route: <Text span ff="monospace">{route.path}</Text>
      </Text>
    </Stack>
  );
}
