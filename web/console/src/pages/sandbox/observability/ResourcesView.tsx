import { useEffect, useMemo, useRef } from "react";
import { useSearchParams } from "react-router";
import uPlot from "uplot";
import { Alert, Box, Group, Paper, Select, SimpleGrid, Stack, Text } from "@mantine/core";
import "uplot/dist/uPlot.min.css";
import { fetchCgroup, type ResourceSample } from "@/api/observability";
import { usePoll } from "@/poll/usePoll";
import { useSandbox } from "@/pages/sandbox/SandboxContext";

const WINDOWS = [
  { label: "60s", ms: 60_000 },
  { label: "300s", ms: 300_000 },
  { label: "600s (max)", ms: 600_000 },
];

const CHARTS = [
  {
    title: "CPU (Δ cpu_usec / s)",
    value: (sample: ResourceSample) => {
      const delta = sample.deltas["cpu_usec"];
      const period = sample.sample_delta_ms ?? 1000;
      return typeof delta === "number" && period > 0 ? delta / (period * 1000) : null;
    },
  },
  {
    title: "Memory (mem_cur bytes)",
    value: (sample: ResourceSample) => {
      const memory = sample.metrics["mem_cur"];
      return typeof memory === "number" ? memory : null;
    },
  },
  {
    title: "IO (Δ read+write bytes)",
    value: (sample: ResourceSample) => {
      const read = sample.deltas["io_rbytes"];
      const write = sample.deltas["io_wbytes"];
      return typeof read !== "number" && typeof write !== "number"
        ? null
        : (typeof read === "number" ? read : 0) + (typeof write === "number" ? write : 0);
    },
  },
  {
    title: "Disk upperdir (bytes)",
    value: (sample: ResourceSample) => {
      const disk = sample.metrics["disk_bytes"];
      return typeof disk === "number" ? disk : null;
    },
  },
] as const;

/**
 * Resource charts over the `cgroup` view: CPU and IO counters render as
 * deltas (the sample format carries monotonic counters in `deltas`), memory
 * and disk as gauges; the scope picker mirrors the op's `--scope` argument
 * (sandbox or one workspace id) and the window caps at the API's 600s max.
 */
export function ResourcesView() {
  const { sandboxId, snapshot } = useSandbox();
  const [searchParams, setSearchParams] = useSearchParams();
  const scope = searchParams.get("scope") ?? "sandbox";
  const windowMs = Number(searchParams.get("window") ?? 60_000);

  const workspaces = snapshot?.sandboxes[0]?.workspaces ?? [];

  const series = usePoll({
    key: ["observability", sandboxId, "cgroup", scope, windowMs],
    fn: () => fetchCgroup(sandboxId, scope, windowMs),
    mode: "slow",
  });

  const samples = useMemo(() => series.data?.series ?? [], [series.data]);
  const unavailable =
    samples.length > 0 &&
    samples.every((sample) => sample.metrics["cgroup_available"] === false);

  const apply = (next: { scope?: string; window?: number }) => {
    const params = new URLSearchParams(searchParams);
    if (next.scope !== undefined) params.set("scope", next.scope);
    if (next.window !== undefined) params.set("window", String(next.window));
    setSearchParams(params, { replace: true });
  };

  return (
    <Stack gap="md" p="md" data-resources-view>
      <Group gap="sm" align="end" wrap="wrap">
        <Select
          label="Scope"
          size="xs"
          value={scope}
          onChange={(value) => apply({ scope: value ?? "sandbox" })}
          data={[
            { value: "sandbox", label: "sandbox" },
            ...workspaces.map((workspace) => ({
              value: workspace.workspace_id,
              label: `workspace · ${workspace.workspace_id}`,
            })),
          ]}
          style={{ width: "16rem" }}
        />
        <Select
          label="Window"
          size="xs"
          value={String(windowMs)}
          onChange={(value) => apply({ window: Number(value ?? WINDOWS[0].ms) })}
          data={WINDOWS.map((window) => ({ value: String(window.ms), label: window.label }))}
          style={{ width: "9rem" }}
        />
        <Text size="xs" c="dimmed" ml="auto">
          auto-refresh · {samples.length} samples
        </Text>
      </Group>

      {series.isError && !series.data ? (
        <Alert color="red" title="Resource metrics unavailable">
          {(series.error as Error).message} — retrying automatically.
        </Alert>
      ) : null}

      {unavailable ? (
        <Alert color="yellow" title="cgroup metrics unavailable">
          cgroup metrics are unavailable in this container (
          {String(samples[samples.length - 1]?.metrics["cgroup_error"] ?? "")})
          — disk metrics still render for workspace scopes.
        </Alert>
      ) : null}

      <SimpleGrid cols={{ base: 1, lg: 2 }} spacing="md">
        {CHARTS.map((chart) => <ChartPane key={chart.title} {...chart} samples={samples} />)}
      </SimpleGrid>
    </Stack>
  );
}

function ChartPane({
  title,
  samples,
  value,
}: {
  title: string;
  samples: ResourceSample[];
  value: (sample: ResourceSample) => number | null;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const plotRef = useRef<uPlot | null>(null);

  const data = useMemo(() => {
    const xs: number[] = [];
    const ys: (number | null)[] = [];
    for (const sample of samples) {
      const y = value(sample);
      if (y === null) continue;
      xs.push(sample.ts / 1000);
      ys.push(y);
    }
    return [xs, ys] as uPlot.AlignedData;
  }, [samples, value]);

  const hasData = data[0].length > 1;

  useEffect(() => {
    const host = hostRef.current;
    if (!host || !hasData) return;
    const plot = new uPlot(
      {
        width: host.clientWidth,
        height: 140,
        legend: { show: false },
        cursor: { show: false },
        series: [
          {},
          {
            stroke: "#2563eb",
            width: 1.4,
            fill: "rgba(37, 99, 235, 0.08)",
            spanGaps: true,
          },
        ],
        axes: [
          { stroke: "#8a93a2", grid: { show: false }, ticks: { show: false } },
          {
            stroke: "#8a93a2",
            grid: { stroke: "#eef0f4", width: 1 },
            ticks: { show: false },
            size: 56,
          },
        ],
      },
      data,
      host,
    );
    plotRef.current = plot;
    const resize = () => plot.setSize({ width: Math.max(host.clientWidth, 1), height: 140 });
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(resize);
    observer?.observe(host);
    return () => {
      observer?.disconnect();
      plot.destroy();
      plotRef.current = null;
    };
  }, [hasData]);

  useEffect(() => {
    if (hasData) plotRef.current?.setData(data);
  }, [data, hasData]);

  const summary = useMemo(() => {
    const values: number[] = [];
    for (const sample of samples) {
      const metric = value(sample);
      if (metric !== null) values.push(metric);
    }
    return values.length === 0
      ? null
      : { latest: values.at(-1)!, min: Math.min(...values), max: Math.max(...values) };
  }, [samples, value]);

  return (
    <Paper withBorder p="md" component="section">
      <Text component="h3" size="sm" fw={600} c="dimmed" mb="sm">{title}</Text>
      {hasData ? (
        <Box ref={hostRef} />
      ) : (
        <Box h={140} style={{ alignItems: "center", display: "flex", justifyContent: "center" }}>
          no samples in this window
        </Box>
      )}
      {summary ? (
        <Group component="dl" gap="md" mt="sm" aria-label={`${title} numerical summary`}>
          <Metric label="Latest" value={summary.latest} />
          <Metric label="Minimum" value={summary.min} />
          <Metric label="Maximum" value={summary.max} />
        </Group>
      ) : null}
    </Paper>
  );
}

function Metric({ label, value }: { label: string; value: number }) {
  return (
    <Box>
      <Text component="dt" size="xs" c="dimmed">{label}</Text>
      <Text component="dd" m={0} ff="monospace" size="xs">{value.toLocaleString(undefined, { maximumFractionDigits: 2 })}</Text>
    </Box>
  );
}
