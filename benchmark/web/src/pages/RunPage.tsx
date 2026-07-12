import {
  Alert,
  Badge,
  Button,
  Card,
  Code,
  Group,
  Modal,
  Progress,
  SimpleGrid,
  Stack,
  Table,
  Text,
  Title,
} from "@mantine/core";
import { useDisclosure } from "@mantine/hooks";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router";
import { benchmarkApi } from "@/api/client";
import { useRunEvents, type EventConnectionState } from "@/api/useRunEvents";
import type { EventData, EventRecord, RunState, WorkState } from "@/api/types";
import { ErrorState, LoadingState } from "@/components/AsyncState";
import { errorMessage, formatDurationNs, formatInteger, formatTimestamp } from "@/lib/format";

function isTerminal(state: RunState): boolean {
  switch (state) {
    case "queued":
    case "planned":
    case "preparing":
    case "running":
    case "verifying":
    case "tearing_down":
    case "cancelling":
      return false;
    case "completed":
    case "failed":
    case "cancelled":
      return true;
  }
}

function EvidenceValue({ label, value, detail }: { label: string; value: string; detail?: string }) {
  return (
    <div>
      <Text size="xs" c="dimmed" tt="uppercase" fw={600}>{label}</Text>
      <Text fw={600} className="wrap-anywhere">{value}</Text>
      {detail ? <Text size="xs" c="dimmed">{detail}</Text> : null}
    </div>
  );
}

function connectionLabel(state: EventConnectionState): string {
  switch (state) {
    case "connecting": return "Connecting";
    case "live": return "Live";
    case "reconnecting": return "Reconnecting";
    case "replaying": return "Replaying";
    case "stale": return "Stale";
  }
}

function eventSummary(data: EventData): string {
  switch (data.kind) {
    case "run_state": return `Run ${data.state}`;
    case "family_state": return `${data.family} family ${data.state}`;
    case "cell_state": return `Cell ${data.cell_id} ${data.state}`;
    case "trial_state": return `${data.warmup ? "Warmup" : "Measured"} trial ${data.trial_id} ${data.state}`;
    case "trial_phase": return `${data.phase} phase ${data.state} · ${data.trial_id}`;
    case "request_state": return `Product request ${data.request_id} ${data.state}`;
    case "resource_window":
      return data.value === null
        ? `${data.metric_id} unavailable · ${data.unavailable_reason ?? "reason not reported"}`
        : `${data.metric_id} ${data.value.toLocaleString()}`;
    case "correctness": return `${data.check_id} ${data.passed ? "passed" : "failed"}`;
    case "warning": return `${data.code} · ${data.message}`;
    case "log": return `${data.level} · ${data.message}`;
    case "report_ready": return data.provisional ? "Provisional report ready" : "Terminal report ready";
  }
}

function eventTone(data: EventData): string {
  if (data.kind === "warning") return "yellow";
  if (data.kind === "correctness") return data.passed ? "green" : "red";
  if (data.kind === "log" && data.level === "error") return "red";
  if (data.kind === "run_state" && data.state === "failed") return "red";
  return "gray";
}

function EventTable({ records }: { records: EventRecord[] }) {
  if (records.length === 0) return <Text c="dimmed">No streamed records have arrived in this browser session.</Text>;
  return (
    <Table.ScrollContainer minWidth={760} mah={480} type="native" tabIndex={0} aria-label="Persisted run event log">
      <Table striped highlightOnHover stickyHeader>
        <Table.Thead>
          <Table.Tr>
            <Table.Th>Sequence</Table.Th>
            <Table.Th>Monotonic offset</Table.Th>
            <Table.Th>Kind</Table.Th>
            <Table.Th>Evidence</Table.Th>
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {[...records].reverse().map((record) => (
            <Table.Tr key={record.sequence}>
              <Table.Td>{formatInteger(record.sequence)}</Table.Td>
              <Table.Td>{formatDurationNs(record.monotonic_offset_ns)}</Table.Td>
              <Table.Td><Badge color={eventTone(record.data)} variant="light">{record.data.kind}</Badge></Table.Td>
              <Table.Td className="wrap-anywhere">{eventSummary(record.data)}</Table.Td>
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
    </Table.ScrollContainer>
  );
}

const WORK_STATES = [
  "pending",
  "preparing",
  "running",
  "verifying",
  "tearing_down",
  "completed",
  "failed",
  "cancelled",
  "skipped",
] as const satisfies readonly WorkState[];

type CellStateEvent = EventRecord & { data: Extract<EventData, { kind: "cell_state" }> };
type TimelineEvent = EventRecord & {
  data: Extract<EventData, { kind: "trial_phase" | "request_state" }>;
};

function isCellStateEvent(record: EventRecord): record is CellStateEvent {
  return record.data.kind === "cell_state";
}

function isTimelineEvent(record: EventRecord): record is TimelineEvent {
  return record.data.kind === "trial_phase" || record.data.kind === "request_state";
}

function workStateTone(state: WorkState): string {
  switch (state) {
    case "pending": return "gray";
    case "preparing": return "cyan";
    case "running": return "blue";
    case "verifying": return "indigo";
    case "tearing_down": return "violet";
    case "completed": return "green";
    case "failed": return "red";
    case "cancelled": return "gray";
    case "skipped": return "yellow";
  }
}

function CellStateMatrix({ records, currentCellId }: { records: EventRecord[]; currentCellId: string | null }) {
  const latestByCell = new Map<string, CellStateEvent>();
  records.filter(isCellStateEvent).forEach((record) => latestByCell.set(record.data.cell_id, record));
  const cells = [...latestByCell.values()].sort((left, right) =>
    left.data.cell_id.localeCompare(right.data.cell_id)
  );

  return (
    <Card withBorder padding="lg">
      <Stack>
        <div>
          <Title order={2} size="h3">Cell-state matrix</Title>
          <Text size="sm" c="dimmed">
            Latest explicit state per cell in this browser&apos;s persisted SSE replay window. Missing cells are not inferred.
          </Text>
        </div>
        {cells.length === 0 ? (
          <Alert color="gray" title="No cell-state event in the replay window">
            {currentCellId
              ? `The run snapshot reports ${currentCellId} as current, but no matching cell-state transition was replayed to this browser.`
              : "The run snapshot has no current cell and no cell-state transition was replayed."}
          </Alert>
        ) : (
          <Table.ScrollContainer minWidth={1_050} type="native" tabIndex={0} aria-label="Cell-state matrix">
            <Table withTableBorder withColumnBorders>
              <Table.Thead>
                <Table.Tr>
                  <Table.Th>Cell</Table.Th>
                  {WORK_STATES.map((state) => <Table.Th key={state}>{state.replaceAll("_", " ")}</Table.Th>)}
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {cells.map((record) => (
                  <Table.Tr key={record.data.cell_id}>
                    <Table.Th scope="row">
                      <Text ff="monospace" size="sm">{record.data.cell_id}</Text>
                      <Text size="xs" c="dimmed">event #{record.sequence} · {formatDurationNs(record.monotonic_offset_ns)}</Text>
                    </Table.Th>
                    {WORK_STATES.map((state) => (
                      <Table.Td key={state} ta="center">
                        {record.data.state === state ? (
                          <Badge color={workStateTone(state)} variant="light">Current</Badge>
                        ) : <Text c="dimmed" aria-label={`${state}: no`}>—</Text>}
                      </Table.Td>
                    ))}
                  </Table.Tr>
                ))}
              </Table.Tbody>
            </Table>
          </Table.ScrollContainer>
        )}
      </Stack>
    </Card>
  );
}

function RequestPhaseTimeline({ records }: { records: EventRecord[] }) {
  const timeline = records
    .filter(isTimelineEvent)
    .sort((left, right) => left.monotonic_offset_ns - right.monotonic_offset_ns || left.sequence - right.sequence)
    .slice(-80);
  const start = timeline[0]?.monotonic_offset_ns ?? 0;
  const end = timeline.at(-1)?.monotonic_offset_ns ?? start;
  const range = Math.max(1, end - start);

  return (
    <Card withBorder padding="lg">
      <Stack>
        <div>
          <Title order={2} size="h3">Synchronized request &amp; phase timeline</Title>
          <Text size="sm" c="dimmed">
            Exact transition markers on one monotonic axis. The browser does not invent spans when a start or end event is outside the replay window.
          </Text>
        </div>
        {timeline.length === 0 ? (
          <Alert color="gray" title="No request or phase transition in the replay window">
            Persisted events will appear here as they are replayed or streamed.
          </Alert>
        ) : (
          <>
            <Group justify="space-between" gap="xs">
              <Text size="xs" ff="monospace">{formatDurationNs(start)}</Text>
              <Text size="xs" c="dimmed">monotonic offset</Text>
              <Text size="xs" ff="monospace">{formatDurationNs(end)}</Text>
            </Group>
            <div className="event-timeline-scroll" tabIndex={0} role="region" aria-label="Request and phase transition timeline">
              <div className="event-timeline">
                {timeline.map((record) => {
                  const phase = record.data.kind === "trial_phase";
                  const label = record.data.kind === "trial_phase"
                    ? `${record.data.phase.replaceAll("_", " ")} · ${record.data.trial_id}`
                    : `${record.data.request_id} · ${record.data.trial_id}`;
                  const state = record.data.state.replaceAll("_", " ");
                  const position = ((record.monotonic_offset_ns - start) / range) * 100;
                  return (
                    <div className="event-timeline-row" key={record.sequence}>
                      <div>
                        <Text size="sm" fw={600}>{phase ? "Phase" : "Request"} · {label}</Text>
                        <Text size="xs" c="dimmed">{state} · event #{record.sequence}</Text>
                      </div>
                      <div className="event-timeline-track" aria-hidden="true">
                        <span className="event-timeline-marker" style={{ left: `${position}%` }} />
                      </div>
                      <Text size="xs" ff="monospace" ta="right">{formatDurationNs(record.monotonic_offset_ns)}</Text>
                    </div>
                  );
                })}
              </div>
            </div>
          </>
        )}
      </Stack>
    </Card>
  );
}

export function RunPage() {
  const { runId = "" } = useParams();
  const [cancelOpened, cancelDialog] = useDisclosure(false);
  const queryClient = useQueryClient();
  const run = useQuery({
    queryKey: ["run", runId],
    queryFn: () => benchmarkApi.run(runId),
    enabled: runId.length > 0,
    refetchInterval: (query) => {
      const state = query.state.data?.manifest.state;
      return state && isTerminal(state) ? false : 2_000;
    },
  });
  const events = useRunEvents(runId, run.data?.latest_sequence ?? null);
  const cancel = useMutation({
    mutationFn: () => benchmarkApi.cancelRun(runId),
    onSuccess: async () => {
      cancelDialog.close();
      await queryClient.invalidateQueries({ queryKey: ["run", runId] });
      await queryClient.invalidateQueries({ queryKey: ["health"] });
    },
  });

  if (!runId) return <Alert color="red" title="Run id is required" />;
  if (run.isPending) return <LoadingState label={`Loading run ${runId}`} />;
  if (run.error) return <ErrorState error={run.error} retry={() => void run.refetch()} />;
  if (!run.data) return null;

  const { manifest, progress } = run.data;
  const terminal = isTerminal(manifest.state);
  const connection = connectionLabel(events.connectionState);
  const completion = progress.total_trial_batches === 0
    ? 0
    : (progress.completed_trial_batches / progress.total_trial_batches) * 100;
  const resourceEvents = events.records.filter((record) => record.data.kind === "resource_window").slice(-8);
  const correctnessEvents = events.records.filter((record) => record.data.kind === "correctness").slice(-8);
  const phaseEvents = events.records.filter((record) => record.data.kind === "trial_phase").slice(-8);

  return (
    <Stack gap="xl">
      <header>
        <Text size="sm" c="dimmed">Live run</Text>
        <Group justify="space-between" align="flex-start" wrap="wrap">
          <div>
            <Title>{manifest.name}</Title>
            <Text ff="monospace" className="wrap-anywhere">{manifest.run_id}</Text>
          </div>
          <Group role="status" aria-live="polite" aria-atomic="true">
            <Badge color={manifest.state === "failed" ? "red" : manifest.state === "completed" ? "green" : manifest.state === "cancelled" ? "gray" : "blue"}>
              Run {manifest.state}
            </Badge>
            <Badge
              color={events.connectionState === "live" ? "green" : events.connectionState === "stale" ? "red" : "yellow"}
            >
              Events {connection}
            </Badge>
          </Group>
        </Group>
      </header>

      <Card withBorder padding="lg">
        <Stack>
          <Group justify="space-between" align="baseline">
            <Title order={2} size="h3">Campaign progress</Title>
            <Text fw={600}>{formatInteger(progress.completed_trial_batches)} / {formatInteger(progress.total_trial_batches)} trial batches</Text>
          </Group>
          <Progress value={completion} size="lg" radius="xs" aria-label="Completed trial batches" />
          <SimpleGrid cols={{ base: 1, sm: 2, lg: 4 }}>
            <EvidenceValue label="Current family" value={progress.current_family ?? "Waiting"} />
            <EvidenceValue label="Current operation" value={progress.current_operation ?? "Waiting"} />
            <EvidenceValue label="Current cell" value={progress.current_cell_id ?? "Waiting"} detail="A test combination, not a request count" />
            <EvidenceValue
              label="Current trial batch"
              value={progress.current_trial_id ?? "Waiting"}
              detail={progress.current_trial_id ? `${progress.trial_kind ?? "trial"} · ${progress.phase ?? "waiting"}` : undefined}
            />
            <EvidenceValue
              label="Issued product requests"
              value={formatInteger(progress.issued_operation_requests)}
              detail="Distinct from cells and trial batches"
            />
            <EvidenceValue label="Failures" value={formatInteger(progress.failure_count)} />
            <EvidenceValue label="Warnings / pressure" value={formatInteger(progress.warning_count)} />
            <EvidenceValue label="Runner ETA" value="Not reported" detail="The browser does not derive an estimate" />
          </SimpleGrid>
          <Text size="sm" c="dimmed">
            Persisted run state and browser connection state are independent. Reload recovery resumes from SSE sequence {formatInteger(events.lastEventId ?? 0)}; {formatInteger(events.replayedEventCount)} record(s) were replayed in this session.
          </Text>
          {events.error ? (
            <Alert color="yellow" title={`${connection}; persisted run state is unchanged`}>
              {events.error.message}
            </Alert>
          ) : null}
        </Stack>
      </Card>

      <SimpleGrid cols={{ base: 1, xl: 2 }}>
        <CellStateMatrix records={events.records} currentCellId={progress.current_cell_id} />
        <RequestPhaseTimeline records={events.records} />
      </SimpleGrid>

      <SimpleGrid cols={{ base: 1, lg: 3 }}>
        <Card withBorder padding="lg">
          <Stack>
            <Title order={2} size="h3">Phase trail</Title>
            {phaseEvents.length === 0 ? <Text c="dimmed">No phase events yet.</Text> : phaseEvents.map((record) => (
              <Text key={record.sequence} size="sm"><Code>#{record.sequence}</Code> {eventSummary(record.data)}</Text>
            ))}
          </Stack>
        </Card>
        <Card withBorder padding="lg">
          <Stack>
            <Title order={2} size="h3">Correctness evidence</Title>
            {correctnessEvents.length === 0 ? <Text c="dimmed">No correctness checks streamed yet.</Text> : correctnessEvents.map((record) => (
              <Alert key={record.sequence} color={eventTone(record.data)}>{eventSummary(record.data)}</Alert>
            ))}
          </Stack>
        </Card>
        <Card withBorder padding="lg">
          <Stack>
            <Title order={2} size="h3">Resource telemetry</Title>
            {resourceEvents.length === 0 ? <Text c="dimmed">No resource windows streamed yet.</Text> : resourceEvents.map((record) => (
              <Text key={record.sequence} size="sm"><Code>#{record.sequence}</Code> {eventSummary(record.data)}</Text>
            ))}
            <Text size="xs" c="dimmed">Unavailable counters are labeled unavailable and never rendered as zero.</Text>
          </Stack>
        </Card>
      </SimpleGrid>

      <Card withBorder padding="lg">
        <Stack>
          <Group justify="space-between" align="baseline" wrap="wrap">
            <Title order={2} size="h3">Persisted event log</Title>
            <Text size="sm" c="dimmed">Latest 500 records in this browser session</Text>
          </Group>
          <EventTable records={events.records} />
        </Stack>
      </Card>

      <Card withBorder padding="lg">
        <Stack>
          <Title order={2} size="h3">Run identity & recovery</Title>
          <Text>Started {formatTimestamp(manifest.started_at)}</Text>
          <Text>Ended {formatTimestamp(manifest.ended_at)}</Text>
          <Text ff="monospace" className="wrap-anywhere">Plan {manifest.plan_hash}</Text>
          <Text>Source {manifest.source_commit}{manifest.source_dirty ? " (dirty)" : ""}</Text>
          <Text ff="monospace" className="wrap-anywhere">Environment {manifest.environment_fingerprint}</Text>
          <Group>
            {!terminal ? (
              <Button color="red" variant="light" onClick={cancelDialog.open} disabled={manifest.state === "cancelling"}>
                {manifest.state === "cancelling" ? "Cancellation requested" : "Cancel run"}
              </Button>
            ) : null}
            {run.data.report_ready ? (
              <Button component={Link} to={`/benchmark/reports/${encodeURIComponent(runId)}`}>Open report</Button>
            ) : (
              <Button disabled>Report not ready</Button>
            )}
          </Group>
        </Stack>
      </Card>

      <Modal opened={cancelOpened} onClose={cancelDialog.close} title="Cancel this run?" centered>
        <Stack>
          <Text>Future trials will stop. Evidence already recorded is retained and teardown is still attempted.</Text>
          <Alert color="yellow" title="Cleanup remains mandatory">
            The runner will attempt session, topology, and fixture cleanup before reaching a terminal cancelled state.
          </Alert>
          {cancel.error ? <Alert color="red">{errorMessage(cancel.error)}</Alert> : null}
          <Group justify="flex-end">
            <Button variant="default" onClick={cancelDialog.close}>Keep running</Button>
            <Button color="red" onClick={() => cancel.mutate()} loading={cancel.isPending}>Request cancellation</Button>
          </Group>
        </Stack>
      </Modal>
    </Stack>
  );
}
