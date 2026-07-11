import { useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { ChevronDown, ChevronRight, Flag } from "lucide-react";
import {
  Alert,
  Box,
  Button,
  Drawer,
  Group,
  Paper,
  Stack,
  Text,
  UnstyledButton,
} from "@mantine/core";
import { useMediaQuery } from "@mantine/hooks";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  fetchEvents,
  fetchTrace,
  type TraceNode,
} from "@/api/observability";
import { usePoll } from "@/poll/usePoll";
import { useSandbox } from "@/pages/sandbox/SandboxContext";
import { formatTimestamp } from "@/lib/format";

const STATUS_COLORS: Record<string, string> = {
  completed: "var(--mantine-color-success-6)",
  error: "var(--mantine-color-danger-6)",
  cancelled: "var(--mantine-color-neutral-6)",
  timed_out: "var(--mantine-color-warning-6)",
};

/**
 * Traces are discovered from the latest 200 events because the service has no
 * trace-enumeration operation. The selected trace remains addressable even
 * when it is outside that discovery window.
 */
export function TracesView() {
  const { sandboxId } = useSandbox();
  const params = useParams();
  const navigate = useNavigate();
  const selected = params.traceId ?? "last";
  const narrow = useMediaQuery("(max-width: 47.99em)");
  const [pickerOpen, setPickerOpen] = useState(false);

  const recentEvents = usePoll({
    key: ["observability", sandboxId, "events", "for-traces"],
    fn: () => fetchEvents(sandboxId, { lastN: 200 }),
    mode: "slow",
  });
  const trace = usePoll({
    key: ["observability", sandboxId, "trace", selected],
    fn: () => fetchTrace(sandboxId, selected),
    mode: "slow",
    retry: false,
  });

  const knownTraces = useMemo(() => {
    const ordered: { id: string; ts: number }[] = [];
    const seen = new Set<string>();
    for (const event of recentEvents.data?.events ?? []) {
      if (!seen.has(event.trace)) {
        seen.add(event.trace);
        ordered.push({ id: event.trace, ts: event.ts });
      }
    }
    return ordered.sort((a, b) => b.ts - a.ts);
  }, [recentEvents.data]);

  const selectTrace = (id: string) => {
    setPickerOpen(false);
    void navigate(id === "last" ? "../traces" : `../traces/${encodeURIComponent(id)}`);
  };
  const picker = (
    <TracePicker
      knownTraces={knownTraces}
      onSelect={selectTrace}
      selected={selected}
    />
  );

  return (
    <Box data-traces-view display="flex" style={{ flex: 1, height: "100%", minHeight: 0, minWidth: 0 }}>
      {!narrow ? (
        <Paper withBorder radius={0} w={288} style={{ flexShrink: 0, minHeight: 0 }}>
          {picker}
        </Paper>
      ) : (
        <Drawer opened={pickerOpen} onClose={() => setPickerOpen(false)} title="Trace selector" size="85%">
          {picker}
        </Drawer>
      )}
      <Stack gap="sm" p="md" style={{ flex: 1, minHeight: 0, minWidth: 0 }}>
        {narrow ? (
          <Button variant="default" onClick={() => setPickerOpen(true)} style={{ alignSelf: "flex-start" }}>
            Choose trace
          </Button>
        ) : null}
        {trace.isError ? (
          <Alert color="red" title="Trace unavailable">
            {(trace.error as Error).message}
          </Alert>
        ) : trace.data ? (
          <Waterfall traceId={trace.data.trace} roots={trace.data.spans} />
        ) : (
          <Text size="sm" c="dimmed">loading trace…</Text>
        )}
      </Stack>
    </Box>
  );
}

function TracePicker({
  knownTraces,
  onSelect,
  selected,
}: {
  knownTraces: { id: string; ts: number }[];
  onSelect: (id: string) => void;
  selected: string;
}) {
  return (
    <Stack gap={2} p="sm" style={{ height: "100%" }}>
      <Text size="xs" fw={600} c="dimmed" tt="uppercase" px="xs" py="xs">Traces</Text>
      <Box style={{ flex: 1, minHeight: 0, overflowY: "auto" }}>
        <TraceRow id="last" label="last trace" hint="the most recent flow" active={selected === "last"} onClick={() => onSelect("last")} />
        {knownTraces.map((known) => (
          <TraceRow
            key={known.id}
            id={known.id}
            label={known.id}
            hint={formatTimestamp(known.ts)}
            active={selected === known.id}
            onClick={() => onSelect(known.id)}
          />
        ))}
        {knownTraces.length === 0 ? (
          <Text size="xs" c="dimmed" p="sm">
            No traces discovered from the latest 200 events yet. Deep links and the last selector still resolve.
          </Text>
        ) : null}
      </Box>
    </Stack>
  );
}

function TraceRow({
  id,
  label,
  hint,
  active,
  onClick,
}: {
  id: string;
  label: string;
  hint: string;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <UnstyledButton
      aria-current={active ? "page" : undefined}
      onClick={onClick}
      px="xs"
      py={6}
      style={{
        borderRadius: "var(--mantine-radius-sm)",
        display: "block",
        width: "100%",
        background: active ? "var(--mantine-color-eyeBlue-0)" : undefined,
      }}
      title={id}
    >
      <Text ff="monospace" fw={active ? 600 : 400} size="xs" c={active ? "eyeBlue.8" : undefined} truncate>
        {label}
      </Text>
      <Text size="xs" c="dimmed" truncate>{hint}</Text>
    </UnstyledButton>
  );
}

interface FlatSpan {
  depth: number;
  node: TraceNode;
}

function flatten(nodes: TraceNode[]): FlatSpan[] {
  const flattened: FlatSpan[] = [];
  const stack = [...nodes].reverse().map((node) => ({ depth: 0, node }));
  while (stack.length > 0) {
    const row = stack.pop()!;
    flattened.push(row);
    for (let index = row.node.children.length - 1; index >= 0; index -= 1) {
      stack.push({ depth: row.depth + 1, node: row.node.children[index] });
    }
  }
  return flattened;
}

export function Waterfall({ traceId, roots }: { traceId: string; roots: TraceNode[] }) {
  const rows = useMemo(() => flatten(roots), [roots]);
  const totalMs = useMemo(
    () => Math.max(1, ...rows.map((row) => row.node.offset_ms + (row.node.span.dur_ms ?? 0))),
    [rows],
  );
  const [openSpans, setOpenSpans] = useState<Set<string>>(() => new Set());
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: rows.length,
    estimateSize: (index) => openSpans.has(rows[index]?.node.span.span) ? 124 : 34,
    getItemKey: (index) => rows[index]?.node.span.span ?? index,
    getScrollElement: () => scrollRef.current,
    initialRect: { height: 480, width: 800 },
    overscan: 14,
  });
  const virtualRows = virtualizer.getVirtualItems();
  const renderedRows = virtualRows.length > 0
    ? virtualRows
    : rows.slice(0, Math.min(rows.length, 20)).map((row, index) => ({
        index,
        key: row.node.span.span,
        start: index * 34,
      }));

  useEffect(() => virtualizer.measure(), [openSpans, virtualizer]);

  if (rows.length === 0) {
    return <Text size="sm" c="dimmed">Trace <Text component="span" ff="monospace">{traceId}</Text> has no spans.</Text>;
  }

  return (
    <Stack gap="xs" style={{ flex: 1, minHeight: 0 }}>
      <Group gap="md">
        <Text ff="monospace" fw={600} size="sm">{traceId}</Text>
        <Text size="xs" c="dimmed">total {totalMs.toFixed(0)}ms · {rows.length.toLocaleString()} spans</Text>
      </Group>
      <Paper
        withBorder
        data-trace-waterfall
        ref={scrollRef}
        style={{ flex: 1, minHeight: 0, overflow: "auto", position: "relative" }}
      >
        <Box style={{ height: virtualizer.getTotalSize(), minWidth: "42rem", position: "relative" }}>
          {renderedRows.map((virtualRow) => {
            const row = rows[virtualRow.index];
            const spanId = row.node.span.span;
            return (
              <Box
                data-index={virtualRow.index}
                data-trace-span={spanId}
                key={virtualRow.key}
                ref={virtualizer.measureElement}
                style={{ left: 0, position: "absolute", top: 0, transform: `translateY(${virtualRow.start}px)`, width: "100%" }}
              >
                <SpanRow
                  open={openSpans.has(spanId)}
                  row={row}
                  totalMs={totalMs}
                  onToggle={() => setOpenSpans((current) => {
                    const next = new Set(current);
                    next.has(spanId) ? next.delete(spanId) : next.add(spanId);
                    return next;
                  })}
                />
              </Box>
            );
          })}
        </Box>
      </Paper>
    </Stack>
  );
}

function SpanRow({
  open,
  row,
  totalMs,
  onToggle,
}: {
  open: boolean;
  row: FlatSpan;
  totalMs: number;
  onToggle: () => void;
}) {
  const span = row.node.span;
  const duration = span.dur_ms ?? 0;
  const attrs = Object.entries(span.attrs);
  const left = (row.node.offset_ms / totalMs) * 100;
  const width = Math.max((duration / totalMs) * 100, 0.5);

  return (
    <Box style={{ borderBottom: "1px solid var(--mantine-color-neutral-3)", padding: "2px 6px" }}>
      <Group gap="sm" wrap="nowrap" style={{ height: 27 }}>
        <UnstyledButton
          aria-expanded={attrs.length > 0 ? open : undefined}
          onClick={onToggle}
          style={{ alignItems: "center", display: "flex", flex: "0 0 16rem", gap: 4, minWidth: 0, paddingLeft: row.depth * 14 }}
          title={`${span.name} attributes`}
        >
          {attrs.length > 0 ? open ? <ChevronDown size={12} /> : <ChevronRight size={12} /> : <Box w={12} />}
          <Text ff="monospace" size="xs" truncate>{span.name}</Text>
        </UnstyledButton>
        <Box style={{ background: "var(--mantine-color-neutral-1)", borderRadius: "var(--mantine-radius-xs)", flex: 1, height: 16, minWidth: 0, overflow: "hidden", position: "relative" }}>
          <Box
            style={{ background: STATUS_COLORS[span.status] ?? "var(--mantine-color-neutral-6)", borderRadius: "var(--mantine-radius-xs)", height: 16, left: `${left}%`, opacity: 0.75, position: "absolute", top: 0, width: `${width}%` }}
            title={`${span.name} · ${duration.toFixed(1)}ms · ${span.status}`}
          />
          {row.node.events.map((event) => (
            <Box
              component="span"
              key={`${event.event.name}-${event.event.ts}`}
              style={{ color: "var(--mantine-color-warning-6)", left: `${Math.min(Math.max((event.offset_ms / totalMs) * 100, 0), 99)}%`, position: "absolute", top: -2 }}
              title={`⚑ ${event.event.name}`}
            >
              <Flag size={11} />
            </Box>
          ))}
        </Box>
        <Text ff="monospace" size="xs" c="dimmed" ta="right" w={80}>{duration.toFixed(1)}ms</Text>
        <Text size="xs" c="dimmed" ta="right" w={80}>{span.status}</Text>
      </Group>
      {open && attrs.length > 0 ? (
        <Box
          component="dl"
          style={{
            background: "var(--mantine-color-neutral-0)",
            border: "1px solid var(--mantine-color-neutral-3)",
            borderRadius: "var(--mantine-radius-sm)",
            display: "grid",
            gap: "2px 12px",
            gridTemplateColumns: "auto minmax(0, 1fr)",
            margin: `4px 0 4px ${row.depth * 14 + 16}px`,
            padding: 8,
          }}
        >
          {attrs.map(([name, value]) => <SpanAttr key={name} name={name} value={value} />)}
        </Box>
      ) : null}
    </Box>
  );
}

function SpanAttr({ name, value }: { name: string; value: unknown }) {
  return (
    <>
      <Text component="dt" ff="monospace" size="xs" c="dimmed">{name}</Text>
      <Text component="dd" ff="monospace" m={0} size="xs" style={{ overflowWrap: "anywhere" }}>
        {typeof value === "string" ? value : JSON.stringify(value)}
      </Text>
    </>
  );
}

export function traceLink(sandboxId: string, traceId: string): string {
  return `/sandboxes/${encodeURIComponent(sandboxId)}/observability/traces/${encodeURIComponent(traceId)}`;
}

export function TraceCell({ sandboxId, traceId }: { sandboxId: string; traceId: string }) {
  return (
    <Link to={traceLink(sandboxId, traceId)} style={{ color: "var(--mantine-primary-color-filled)", fontFamily: "var(--mantine-font-family-monospace)" }} title={traceId}>
      {traceId.length > 12 ? `${traceId.slice(0, 12)}…` : traceId}
    </Link>
  );
}
