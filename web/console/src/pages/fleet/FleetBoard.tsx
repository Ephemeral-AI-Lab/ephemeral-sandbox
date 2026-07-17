import { useEffect, useMemo, useRef, useState } from "react";
import { useSearchParams } from "react-router";
import { Search } from "lucide-react";
import {
  Alert,
  Box,
  Center,
  Flex,
  Group,
  Loader,
  Stack,
  Text,
  TextInput,
  Title,
  VisuallyHidden,
} from "@mantine/core";
import { rpc, systemScope } from "@/api/rpc";
import type { SandboxList } from "@/api/types";
import { inFlightCount, type SnapshotResult } from "@/api/observability";
import { useFleetSnapshots } from "@/poll/useFleetSnapshots";
import { usePoll } from "@/poll/usePoll";
import { CreateSandboxModal } from "@/pages/fleet/CreateSandboxModal";
import { FleetSummaryBar } from "@/pages/fleet/FleetSummaryBar";
import { SandboxCard } from "@/pages/fleet/SandboxCard";

export function FleetBoard() {
  const [searchParams, setSearchParams] = useSearchParams();
  const filter = searchParams.get("q") ?? "";
  const filterRef = useRef<HTMLInputElement>(null);
  const [createLogs, setCreateLogs] = useState<string[] | null>(null);

  const list = usePoll({
    key: ["fleet", "list_sandboxes"],
    fn: () => rpc<SandboxList>("list_sandboxes", systemScope),
    mode: "slow",
  });
  const snapshot = useFleetSnapshots(list.data?.sandboxes ?? []);
  const lifecycleActive = hasFleetActivity(list.data, snapshot.data, createLogs);
  const listFast = usePoll({
    key: ["fleet", "list_sandboxes", "fast"],
    fn: () => rpc<SandboxList>("list_sandboxes", systemScope),
    mode: "fast",
    enabled: lifecycleActive,
  });

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "/" || event.metaKey || event.ctrlKey || event.altKey) return;
      const target = event.target as HTMLElement | null;
      if (
        target &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable)
      ) {
        return;
      }
      event.preventDefault();
      filterRef.current?.focus();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const sourceFleet = currentFleetList(list.data, listFast.data, lifecycleActive);
  const previousOrder = useRef<string[]>([]);
  const fleet = useMemo(
    () => stabilizeFleetList(sourceFleet, previousOrder.current),
    [sourceFleet],
  );

  useEffect(() => {
    previousOrder.current = fleet?.sandboxes.map((record) => record.id) ?? [];
  }, [fleet]);

  const records = fleet?.sandboxes ?? [];
  const snapshots = new Map(
    (snapshot.data?.sandboxes ?? []).map((entry) => [entry.sandbox_id, entry]),
  );
  const needle = filter.trim().toLowerCase();
  const visible = needle
    ? records.filter(
        (record) =>
          record.id.toLowerCase().includes(needle) ||
          record.state.toLowerCase().includes(needle),
      )
    : records;
  const fleetError = lifecycleActive ? listFast.error ?? list.error : list.error;
  const isInitialLoading = list.isPending && !fleet;

  return (
    <Box data-fleet-board>
      <VisuallyHidden>
        <Title order={1}>Sandbox fleet</Title>
      </VisuallyHidden>
      <Box data-fleet-toolbar>
        <Group justify="space-between" wrap="wrap" gap="sm">
          <TextInput
            data-fleet-filter
            aria-label="Filter sandboxes"
            leftSection={<Search size={14} />}
            placeholder="filter by id or state ( / )"
            ref={filterRef}
            value={filter}
            w={{ base: "100%", sm: 288 }}
            onChange={(event) => {
              const next = new URLSearchParams(searchParams);
              if (event.target.value) next.set("q", event.target.value);
              else next.delete("q");
              setSearchParams(next, { replace: true });
            }}
          />
          <CreateSandboxModal onStream={setCreateLogs} />
        </Group>
      </Box>

      <FleetSummaryBar list={fleet} snapshot={snapshot.data} />

      <Box data-fleet-scroll-owner data-route-scroll-owner="fleet">
        <Stack gap="md" p="md">
          {fleetError && fleet ? (
            <Alert color="danger" title="Fleet refresh failed" variant="light">
              Showing the last confirmed fleet data. {(fleetError as Error).message} — retrying automatically.
            </Alert>
          ) : null}
          {isInitialLoading ? (
            <Center data-fleet-loading py="xl">
              <Group gap="xs">
                <Loader size="sm" />
                <Text c="dimmed" size="sm">Loading fleet…</Text>
              </Group>
            </Center>
          ) : fleetError && !fleet ? (
            <Center data-fleet-error py="xl">
              <Stack align="center" gap="xs" maw={480} ta="center">
                <Text c="danger" fw={700}>Gateway unreachable</Text>
                <Text c="dimmed" size="sm">
                  {(fleetError as Error).message} — retrying automatically.
                </Text>
              </Stack>
            </Center>
          ) : visible.length === 0 && fleet ? (
            <Center data-fleet-empty py="xl">
              <Stack align="center" gap="xs" maw={480} ta="center">
                <Text fw={700}>{records.length === 0 ? "No sandboxes yet" : "No matches"}</Text>
                <Text c="dimmed" size="sm">
                  {records.length === 0
                    ? "Create the first sandbox to get started."
                    : `Nothing matches “${filter}”.`}
                </Text>
              </Stack>
            </Center>
          ) : (
            <Flex data-fleet-card-collection gap="md" wrap="wrap">
              {visible.map((record) => (
                <SandboxCard
                  key={record.id}
                  record={record}
                  snapshot={snapshots.get(record.id)}
                  createLogs={record.state === "creating" ? (createLogs ?? undefined) : undefined}
                />
              ))}
              {createLogs !== null && !records.some((record) => record.state === "creating") ? (
                <Box data-fleet-creation-card>
                  <Text c="run" fw={700} size="xs" tt="uppercase">creating…</Text>
                  <Box mt="sm">
                    <Text c="dimmed" ff="monospace" size="xs">
                      {createLogs.length === 0 ? "starting…" : createLogs.join("\n")}
                    </Text>
                  </Box>
                </Box>
              ) : null}
            </Flex>
          )}
          {list.isFetching && fleet ? (
            <Text aria-live="polite" c="dimmed" data-fleet-refreshing size="xs">
              Refreshing fleet generation…
            </Text>
          ) : null}
        </Stack>
      </Box>
    </Box>
  );
}

export function hasFleetActivity(
  list: SandboxList | undefined,
  snapshot: SnapshotResult | undefined,
  createLogs: string[] | null,
) {
  return (
    createLogs !== null ||
    (list?.sandboxes ?? []).some(
      (record) => record.state === "creating" || record.state === "stopping",
    ) ||
    (snapshot?.sandboxes ?? []).some((entry) => inFlightCount(entry) > 0)
  );
}

export function stabilizeFleetList(
  list: SandboxList | undefined,
  previousOrder: string[],
): SandboxList | undefined {
  if (!list || previousOrder.length === 0) return list;
  const previousPositions = new Map(previousOrder.map((id, index) => [id, index]));
  const incomingPositions = new Map(list.sandboxes.map((record, index) => [record.id, index]));

  return {
    ...list,
    sandboxes: [...list.sandboxes].sort((left, right) => {
      const leftPrevious = previousPositions.get(left.id);
      const rightPrevious = previousPositions.get(right.id);
      if (leftPrevious !== undefined && rightPrevious !== undefined) return leftPrevious - rightPrevious;
      if (leftPrevious !== undefined) return -1;
      if (rightPrevious !== undefined) return 1;
      return (incomingPositions.get(left.id) ?? 0) - (incomingPositions.get(right.id) ?? 0);
    }),
  };
}

export function currentFleetList(
  slow: SandboxList | undefined,
  fast: SandboxList | undefined,
  lifecycleActive: boolean,
): SandboxList | undefined {
  return lifecycleActive ? fast ?? slow : slow;
}
