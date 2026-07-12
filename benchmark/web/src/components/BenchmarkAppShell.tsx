import {
  Alert,
  AppShell,
  Badge,
  Box,
  Burger,
  Button,
  Divider,
  Drawer,
  Group,
  ScrollArea,
  Stack,
  Text,
  TextInput,
  Title,
} from "@mantine/core";
import { useDisclosure } from "@mantine/hooks";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Activity,
  Boxes,
  Files,
  FlaskConical,
  GitCompare,
  History,
  Layers3,
  Settings,
  TerminalSquare,
} from "lucide-react";
import { useEffect, useState } from "react";
import { NavLink, Outlet } from "react-router";
import { benchmarkApi } from "@/api/client";
import { ErrorState, LoadingState } from "@/components/AsyncState";
import { errorMessage } from "@/lib/format";
import { FAMILY_ROUTES, type FamilyRouteId } from "@/routes";

const familyIcons: Record<FamilyRouteId, typeof TerminalSquare> = {
  command: TerminalSquare,
  files: Files,
  workspace: Boxes,
  layerstack: Layers3,
};

function Navigation({ close }: { close: () => void }) {
  const definitions = useQuery({ queryKey: ["definitions"], queryFn: benchmarkApi.definitions });

  return (
    <nav aria-label="Benchmark navigation" className="benchmark-nav">
      <NavLink to="/benchmark" end onClick={close}>
        <Activity size={20} aria-hidden="true" /> <span>Overview</span>
      </NavLink>
      {(Object.keys(FAMILY_ROUTES) as FamilyRouteId[]).map((familyRouteId) => {
        const route = FAMILY_ROUTES[familyRouteId];
        const Icon = familyIcons[familyRouteId];
        const label = definitions.data?.catalog.families.find(({ id }) => id === route.familyId)?.label;
        return (
          <NavLink key={familyRouteId} to={route.path} onClick={close}>
            <Icon size={20} aria-hidden={true} /> <span>{label ?? route.fallbackLabel}</span>
          </NavLink>
        );
      })}
      <NavLink to="/benchmark#recent-runs" onClick={close}>
        <History size={20} aria-hidden="true" /> <span>Runs</span>
      </NavLink>
      <NavLink to="/benchmark/compare" onClick={close}>
        <GitCompare size={20} aria-hidden="true" /> <span>Compare</span>
      </NavLink>
    </nav>
  );
}

function EnvironmentStrip({ openDetails }: { openDetails: () => void }) {
  const health = useQuery({ queryKey: ["health"], queryFn: benchmarkApi.health, refetchInterval: 10_000 });
  const settings = useQuery({ queryKey: ["settings"], queryFn: benchmarkApi.settings });

  return (
    <Box className="environment-strip" aria-label="Runner environment">
      <Group justify="space-between" wrap="nowrap" gap="md">
        <Group gap="xs" wrap="wrap" className="environment-summary">
          <Badge color={health.data?.execution_ready ? "green" : health.isError ? "red" : "gray"}>
            Runner {health.data?.status ?? (health.isPending ? "checking" : "unavailable")}
          </Badge>
          <Text size="sm" className="path-value" title={settings.data?.test_workspace_root}>
            {settings.data?.test_workspace_root ?? "Workspace root unavailable"}
          </Text>
          <Text size="sm" c={settings.data?.writable ? "green" : "dimmed"}>
            {settings.data ? (settings.data.writable ? "Writable" : "Not writable") : "Checking path"}
          </Text>
        </Group>
        <Button variant="subtle" size="compact-sm" onClick={openDetails}>
          Details
        </Button>
      </Group>
    </Box>
  );
}

function EnvironmentDrawer({ opened, close }: { opened: boolean; close: () => void }) {
  const queryClient = useQueryClient();
  const health = useQuery({ queryKey: ["health"], queryFn: benchmarkApi.health });
  const settings = useQuery({ queryKey: ["settings"], queryFn: benchmarkApi.settings });
  const [workspaceRoot, setWorkspaceRoot] = useState("");
  const updateSettings = useMutation({
    mutationFn: () => benchmarkApi.updateSettings({ test_workspace_root: workspaceRoot.trim() }),
    onSuccess: async (updated) => {
      queryClient.setQueryData(["settings"], updated);
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["health"] }),
        queryClient.invalidateQueries({ queryKey: ["plan-validation"] }),
      ]);
    },
  });
  const serverWorkspaceRoot = settings.data?.test_workspace_root;
  useEffect(() => {
    if (opened && serverWorkspaceRoot) setWorkspaceRoot(serverWorkspaceRoot);
  }, [opened, serverWorkspaceRoot]);
  const pending = health.isPending || settings.isPending;
  const error = health.error ?? settings.error;

  return (
    <Drawer opened={opened} onClose={close} title="Runner environment" position="right" size="md">
      {pending ? <LoadingState /> : null}
      {error ? <ErrorState error={error} retry={() => void Promise.all([health.refetch(), settings.refetch()])} /> : null}
      {health.data && settings.data ? (
        <Stack>
          <Group justify="space-between">
            <Text fw={600}>Execution readiness</Text>
            <Badge color={health.data.execution_ready ? "green" : "red"}>{health.data.status}</Badge>
          </Group>
          <Divider />
          <form onSubmit={(event) => { event.preventDefault(); updateSettings.mutate(); }}>
            <Stack gap="sm">
              <TextInput
                label="Test workspace root"
                description="The only editable host path. The runner validates containment, ownership, and writability before accepting it."
                value={workspaceRoot}
                onChange={(event) => { updateSettings.reset(); setWorkspaceRoot(event.currentTarget.value); }}
                disabled={updateSettings.isPending}
                styles={{ input: { fontFamily: "var(--mantine-font-family-monospace)" } }}
              />
              {updateSettings.error ? (
                <Alert color="red" title="Workspace root was not changed" role="alert">
                  {errorMessage(updateSettings.error)}
                </Alert>
              ) : null}
              {updateSettings.isSuccess && workspaceRoot === settings.data.test_workspace_root ? (
                <Alert color="green" title="Workspace root saved" role="status">
                  New plans and validations use this runner-local binding.
                </Alert>
              ) : null}
              <Group justify="flex-end">
                <Button
                  variant="default"
                  type="button"
                  onClick={() => { updateSettings.reset(); setWorkspaceRoot(settings.data.test_workspace_root); }}
                  disabled={workspaceRoot === settings.data.test_workspace_root || updateSettings.isPending}
                >
                  Reset edit
                </Button>
                <Button
                  type="submit"
                  loading={updateSettings.isPending}
                  disabled={workspaceRoot.trim().length === 0 || workspaceRoot.trim() === settings.data.test_workspace_root}
                >
                  Save workspace root
                </Button>
              </Group>
            </Stack>
          </form>
          <Group grow align="flex-start">
            <div>
              <Text size="sm" c="dimmed">Source</Text>
              <Text>{settings.data.source}</Text>
            </div>
            <div>
              <Text size="sm" c="dimmed">Writable</Text>
              <Text>{settings.data.writable ? "Yes" : "No"}</Text>
            </div>
          </Group>
          <Group grow align="flex-start">
            <div>
              <Text size="sm" c="dimmed">Canonical path</Text>
              <Text>{settings.data.path_health.canonical ? "Verified" : "Not verified"}</Text>
            </div>
            <div>
              <Text size="sm" c="dimmed">Ownership marker</Text>
              <Text>{settings.data.path_health.root_marker ? "Present" : "Missing"}</Text>
            </div>
          </Group>
          <Text>
            {settings.data.path_health.outside_repository
              ? "Workspace root is outside the repository."
              : "Workspace root must be outside the repository."}
          </Text>
          <Text size="sm" c="dimmed">Runner version {health.data.version}</Text>
          {health.data.checks.map((check) => (
            <Box key={check.id} className={`check-row check-${check.status}`}>
              <Text fw={600}>{check.id}: {check.status}</Text>
              <Text size="sm">{check.message}</Text>
            </Box>
          ))}
        </Stack>
      ) : null}
    </Drawer>
  );
}

export function BenchmarkAppShell() {
  const [navOpened, nav] = useDisclosure(false);
  const [detailsOpened, details] = useDisclosure(false);
  const health = useQuery({ queryKey: ["health"], queryFn: benchmarkApi.health, refetchInterval: 10_000 });

  return (
    <>
      <a href="#main-content" className="skip-link">Skip to benchmark content</a>
      <AppShell
        header={{ height: 56 }}
        navbar={{ width: 224, breakpoint: "md", collapsed: { mobile: true } }}
        padding={0}
      >
        <AppShell.Header>
          <Group h="100%" px={{ base: "sm", sm: "md" }} justify="space-between" wrap="nowrap">
            <Group gap="sm" wrap="nowrap" miw={0}>
              <Burger
                opened={navOpened}
                onClick={nav.toggle}
                hiddenFrom="md"
                aria-label={navOpened ? "Close navigation" : "Open navigation"}
              />
              <FlaskConical size={24} aria-hidden="true" />
              <Title order={1} size="h3" lineClamp={1}>EphemeralOS Benchmark Laboratory</Title>
            </Group>
            <Group gap="sm" wrap="nowrap">
              <Badge color={health.data?.execution_ready ? "green" : health.isError ? "red" : "gray"} visibleFrom="sm">
                {health.data?.execution_ready ? "Ready" : health.isPending ? "Checking" : "Not ready"}
              </Badge>
              <Button aria-label="Settings" variant="subtle" leftSection={<Settings size={18} aria-hidden="true" />} onClick={details.open}>
                <span className="settings-label">Settings</span>
              </Button>
            </Group>
          </Group>
        </AppShell.Header>
        <AppShell.Navbar p="sm">
          <AppShell.Section grow component={ScrollArea}>
            <Navigation close={nav.close} />
          </AppShell.Section>
        </AppShell.Navbar>
        <AppShell.Main>
          <EnvironmentStrip openDetails={details.open} />
          {health.data?.active_run ? (
            <Box className="active-run-banner" role="status">
              <Group justify="space-between" wrap="wrap">
                <Text fw={600}>Campaign {health.data.active_run.run_id} is {health.data.active_run.state}.</Text>
                <Button
                  component={NavLink}
                  to={`/benchmark/runs/${encodeURIComponent(health.data.active_run.run_id)}`}
                  variant="light"
                  size="compact-sm"
                >
                  Follow active run
                </Button>
              </Group>
            </Box>
          ) : null}
          <Box id="main-content" tabIndex={-1} className="content-canvas">
            <Outlet />
          </Box>
        </AppShell.Main>
      </AppShell>
      <Drawer
        opened={navOpened}
        onClose={nav.close}
        title="Benchmark navigation"
        size={280}
        hiddenFrom="md"
      >
        <Navigation close={nav.close} />
      </Drawer>
      <EnvironmentDrawer opened={detailsOpened} close={details.close} />
    </>
  );
}
