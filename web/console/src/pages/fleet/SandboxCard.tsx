import { Link, useNavigate } from "react-router";
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Trash2 } from "lucide-react";
import { Alert, Box, Button, Card, Group, SimpleGrid, Stack, Text } from "@mantine/core";
import { rpcStream, systemScope } from "@/api/rpc";
import type { SandboxRecord } from "@/api/types";
import { inFlightCount, type SandboxSnapshot } from "@/api/observability";
import { formatMegabytes } from "@/lib/format";
import { recordSample } from "@/lib/sparkHistory";
import { ConfirmDestroyDialog } from "@/components/ConfirmDestroyDialog";
import { HealthDot } from "@/components/HealthDot";
import { ResourceSparkline } from "@/components/ResourceSparkline";
import { StateBadge } from "@/components/StateBadge";
import { StreamLogPane } from "@/components/StreamLogPane";
import { useErrorToast } from "@/components/ErrorToast";

export function SandboxCard({
  record,
  snapshot,
  createLogs,
}: {
  record: SandboxRecord;
  snapshot: SandboxSnapshot | undefined;
  createLogs: string[] | undefined;
}) {
  const navigate = useNavigate();
  const spark = recordSample(record.id, snapshot?.resources.latest ?? null);
  const sessions = snapshot?.workspaces.length ?? 0;
  const commands = snapshot ? inFlightCount(snapshot) : 0;
  const latest = snapshot?.resources.latest;
  const memory = latest?.metrics["mem_cur"];
  const cpuDelta = latest?.deltas["cpu_usec"];
  const sampleDeltaMs = latest?.sample_delta_ms ?? 0;
  const cpuPercent = typeof cpuDelta === "number" && sampleDeltaMs > 0
    ? (cpuDelta / (sampleDeltaMs * 1_000)) * 100
    : null;

  return (
    <Card component="article" data-fleet-card padding="md" radius="md" shadow="sm" withBorder>
      <Card.Section inheritPadding py="sm" withBorder>
        <Group gap="xs" wrap="nowrap">
          <Text
            component={Link}
            ff="monospace"
            fw={700}
            size="sm"
            to={`/sandboxes/${encodeURIComponent(record.id)}`}
            truncate
            style={{ flex: 1, minWidth: 0 }}
          >
            {record.id}
          </Text>
          <StateBadge state={record.state} />
          {record.state === "ready" ? <HealthDot endpoint={record.daemon_http} /> : null}
        </Group>
      </Card.Section>

      <Stack gap="sm" mt="sm">
        <Box data-fleet-workspace>
          <Text c="dimmed" fw={700} size="xs" tt="uppercase">Workspace</Text>
          <Text ff="monospace" size="xs" title={record.workspace_root} truncate>
            {record.workspace_root}
          </Text>
        </Box>

        {record.state === "creating" ? (
          <StreamLogPane lines={createLogs ?? []} maxHeight={128} />
        ) : null}

        {record.state === "failed" ? (
          <Alert color="danger" variant="light">
            Sandbox failed to reach ready. Inspect the record for endpoint and state details.
          </Alert>
        ) : null}

        {record.state === "ready" ? (
          <Box data-fleet-activity>
            <Group align="center" justify="space-between" wrap="nowrap">
              <Text c="dimmed" fw={700} size="xs" tt="uppercase">Activity</Text>
              <Text size="sm" truncate>
                {sessions} {sessions === 1 ? "session" : "sessions"} · {commands}{" "}
                {commands === 1 ? "cmd" : "cmds"}
              </Text>
            </Group>
            <SimpleGrid cols={2} data-fleet-resources mt="xs" spacing="sm">
              <ResourceMetric
                label="CPU"
                value={cpuPercent === null ? "–" : `${cpuPercent.toFixed(cpuPercent < 1 ? 2 : 1)}%`}
                values={spark.cpu}
              />
              <ResourceMetric
                label="Memory"
                value={typeof memory === "number" ? formatMegabytes(memory) : "–"}
                values={spark.mem}
              />
            </SimpleGrid>
          </Box>
        ) : null}
      </Stack>

      <Card.Section inheritPadding mt="sm" py="sm" withBorder>
        <Group gap="xs" justify="space-between">
          <Group gap="xs">
            {record.state === "ready" ? (
              <Button
                size="sm"
                variant="filled"
                onClick={() => void navigate(`/sandboxes/${encodeURIComponent(record.id)}`)}
              >
                Open
              </Button>
            ) : null}
            {record.state === "failed" ? (
              <Button
                size="sm"
                onClick={() => void navigate(`/sandboxes/${encodeURIComponent(record.id)}`)}
              >
                Inspect
              </Button>
            ) : null}
          </Group>
          <DestroyAction sandboxId={record.id} />
        </Group>
      </Card.Section>
    </Card>
  );
}

function ResourceMetric({ label, value, values }: { label: string; value: string; values: number[] }) {
  return (
    <Box data-fleet-resource>
      <Group gap="xs" justify="space-between" wrap="nowrap">
        <Text c="dimmed" size="xs">{label}</Text>
        <Text fw={700} size="sm">{value}</Text>
      </Group>
      <ResourceSparkline values={values} label={`${label} history`} />
    </Box>
  );
}

function DestroyAction({ sandboxId }: { sandboxId: string }) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [logs, setLogs] = useState<string[]>([]);
  const { showError } = useErrorToast();
  const queryClient = useQueryClient();
  const navigate = useNavigate();

  const destroy = async () => {
    setBusy(true);
    setLogs([]);
    try {
      await rpcStream(
        "destroy_sandbox",
        systemScope,
        { sandbox_id: sandboxId },
        (line) => setLogs((current) => [...current, line]),
      );
      setOpen(false);
      void queryClient.invalidateQueries({ queryKey: ["fleet"] });
      void navigate("/");
    } catch (error) {
      showError(error);
    } finally {
      setBusy(false);
    }
  };

  return (
    <ConfirmDestroyDialog
      sandboxId={sandboxId}
      open={open}
      onOpenChange={setOpen}
      onConfirm={() => void destroy()}
      busy={busy}
      logLines={logs}
      trigger={(open) => (
        <Button
          size="compact-xs"
          color="danger"
          variant="filled"
          aria-label={`Destroy ${sandboxId}`}
          onClick={open}
        >
          <Trash2 size={12} />
        </Button>
      )}
    />
  );
}
