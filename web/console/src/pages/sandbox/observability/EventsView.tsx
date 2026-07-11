import { useMemo, useRef, useState } from "react";
import { useSearchParams } from "react-router";
import {
  Alert,
  Box,
  Button,
  Group,
  Paper,
  Select,
  Table,
  Text,
  TextInput,
  UnstyledButton,
} from "@mantine/core";
import {
  flexRender,
  getCoreRowModel,
  getSortedRowModel,
  useReactTable,
  type ColumnDef,
  type SortingState,
} from "@tanstack/react-table";
import { useVirtualizer } from "@tanstack/react-virtual";
import { fetchEvents, type TraceEvent } from "@/api/observability";
import { usePoll } from "@/poll/usePoll";
import { useSandbox } from "@/pages/sandbox/SandboxContext";
import { TraceCell } from "@/pages/sandbox/observability/TracesView";
import { formatTimestamp } from "@/lib/format";

const SINCE_CHOICES = [
  { label: "any time", ms: 0 },
  { label: "last 5m", ms: 5 * 60_000 },
  { label: "last 15m", ms: 15 * 60_000 },
  { label: "last 1h", ms: 60 * 60_000 },
];
const VIRTUALIZE_AT = 200;

function eventKey(event: TraceEvent): string {
  return `${event.trace}-${event.ts}-${event.name}`;
}

/**
 * EventStream: newest-first table over the `events` view with the API's
 * exact filters (name, since-ms, last-N) plus a polling live-tail toggle.
 * Trace cells deep-link into the waterfall.
 */
export function EventsView() {
  const { sandboxId } = useSandbox();
  const [searchParams, setSearchParams] = useSearchParams();
  const name = searchParams.get("name") ?? "";
  const sinceMs = Number(searchParams.get("since") ?? 0);
  const lastN = Number(searchParams.get("last") ?? 200);
  const [tail, setTail] = useState(true);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [sorting, setSorting] = useState<SortingState>([{ id: "ts", desc: true }]);
  const scrollRef = useRef<HTMLDivElement>(null);

  const events = usePoll({
    key: ["observability", sandboxId, "events", name, sinceMs, lastN],
    fn: () =>
      fetchEvents(sandboxId, {
        name: name || undefined,
        sinceMs: sinceMs > 0 ? Date.now() - sinceMs : undefined,
        lastN,
      }),
    mode: tail ? "fast" : "slow",
    enabled: tail,
  });

  const apply = (next: { name?: string; since?: number; last?: number }) => {
    const params = new URLSearchParams(searchParams);
    if (next.name !== undefined) {
      if (next.name) params.set("name", next.name);
      else params.delete("name");
    }
    if (next.since !== undefined) {
      if (next.since > 0) params.set("since", String(next.since));
      else params.delete("since");
    }
    if (next.last !== undefined) params.set("last", String(next.last));
    setSearchParams(params, { replace: true });
  };

  const columns = useMemo<ColumnDef<TraceEvent>[]>(
    () => [
      {
        accessorKey: "ts",
        header: "timestamp",
        cell: ({ row }) => (
          <Text ff="monospace" size="xs" c="dimmed" style={{ whiteSpace: "nowrap" }}>
            {formatTimestamp(row.original.ts)}
          </Text>
        ),
      },
      {
        accessorKey: "name",
        header: "name",
        cell: ({ row }) => <Text ff="monospace" size="xs">{row.original.name}</Text>,
      },
      {
        accessorKey: "trace",
        header: "trace",
        cell: ({ row }) => <TraceCell sandboxId={sandboxId} traceId={row.original.trace} />,
      },
      {
        id: "attrs",
        header: "attributes",
        enableSorting: false,
        cell: ({ row }) => {
          const key = row.id;
          const attrsText = JSON.stringify(row.original.attrs);
          const isOpen = expanded === key;
          return (
            <UnstyledButton
              aria-expanded={isOpen}
              onClick={() => setExpanded(isOpen ? null : key)}
              style={{
                display: "block",
                fontFamily: "var(--mantine-font-family-monospace)",
                fontSize: "var(--mantine-font-size-xs)",
                maxWidth: "32rem",
                overflow: isOpen ? "visible" : "hidden",
                textAlign: "left",
                textOverflow: isOpen ? "clip" : "ellipsis",
                whiteSpace: isOpen ? "normal" : "nowrap",
                wordBreak: "break-word",
              }}
              title="Toggle full attributes"
            >
              {attrsText}
            </UnstyledButton>
          );
        },
      },
    ],
    [expanded, sandboxId],
  );
  const table = useReactTable({
    columns,
    data: events.data?.events ?? [],
    getCoreRowModel: getCoreRowModel(),
    getRowId: eventKey,
    getSortedRowModel: getSortedRowModel(),
    onSortingChange: setSorting,
    state: { sorting },
  });
  const rows = table.getRowModel().rows;
  const shouldVirtualize = rows.length > VIRTUALIZE_AT;
  const rowVirtualizer = useVirtualizer({
    count: shouldVirtualize ? rows.length : 0,
    estimateSize: () => 34,
    getItemKey: (index) => rows[index]?.id ?? index,
    getScrollElement: () => scrollRef.current,
    overscan: 12,
  });
  const virtualRows = shouldVirtualize ? rowVirtualizer.getVirtualItems() : [];
  const paddingTop = virtualRows[0]?.start ?? 0;
  const paddingBottom = shouldVirtualize
    ? rowVirtualizer.getTotalSize() - (virtualRows.at(-1)?.end ?? 0)
    : 0;

  return (
    <Box data-events-view display="flex" style={{ flex: 1, flexDirection: "column", height: "100%", minHeight: 0 }}>
      <Paper withBorder radius={0} px="md" py="sm">
        <Group gap="sm" align="end" wrap="wrap">
          <TextInput
            label="Name"
            id="event-name"
            name="name"
            defaultValue={name}
            placeholder="lease.acquired"
            size="xs"
            styles={{ input: { fontFamily: "var(--mantine-font-family-monospace)", width: "13rem" } }}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                apply({ name: event.currentTarget.value });
              }
            }}
          />
          <Select
            label="Since"
            size="xs"
            value={String(sinceMs)}
            onChange={(value) => apply({ since: Number(value ?? 0) })}
            data={SINCE_CHOICES.map((choice) => ({ value: String(choice.ms), label: choice.label }))}
            style={{ width: "8rem" }}
          />
          <TextInput
            label="Last N"
            id="event-last"
            size="xs"
            value={String(lastN)}
            onChange={(event) => {
              const parsed = Number(event.target.value);
              if (Number.isFinite(parsed) && parsed > 0) apply({ last: parsed });
            }}
            inputMode="numeric"
            styles={{ input: { fontFamily: "var(--mantine-font-family-monospace)", width: "5rem" } }}
          />
          <Button
            aria-label={tail ? "tail" : "resume tail"}
            aria-pressed={tail}
            color={tail ? "eyeBlue" : "gray"}
            data-event-tail-state={tail ? "live" : "paused"}
            onClick={() => setTail((current) => !current)}
          >
            {tail ? "Live tail" : "Paused"}
          </Button>
        </Group>
      </Paper>

      <Box ref={scrollRef} style={{ flex: 1, minHeight: 0, overflow: "auto" }}>
        {events.isError ? (
          <Alert color="red" title={events.data ? "Refresh paused on last confirmed events" : "Events unavailable"} m="md">
            {(events.error as Error).message}
          </Alert>
        ) : null}
        {rows.length === 0 ? (
          <Paper withBorder p="xl" m="xl" maw={420} mx="auto" ta="center">
            <Text fw={600}>No events</Text>
            <Text size="sm" c="dimmed" mt="xs">
              Nothing matches these filters yet. Domain facts (leases,
              publishes) appear here as they happen.
            </Text>
          </Paper>
        ) : (
          <Table highlightOnHover stickyHeader stickyHeaderOffset={0} miw={720} verticalSpacing="xs" horizontalSpacing="sm">
            <Table.Thead>
              {table.getHeaderGroups().map((headerGroup) => (
                <Table.Tr key={headerGroup.id}>
                  {headerGroup.headers.map((header) => (
                    <Table.Th key={header.id}>
                      {header.isPlaceholder ? null : header.column.getCanSort() ? (
                        <UnstyledButton
                          onClick={header.column.getToggleSortingHandler()}
                          style={{ fontWeight: 600 }}
                        >
                          {flexRender(header.column.columnDef.header, header.getContext())}
                          {header.column.getIsSorted() === "asc" ? " ↑" : header.column.getIsSorted() === "desc" ? " ↓" : ""}
                        </UnstyledButton>
                      ) : flexRender(header.column.columnDef.header, header.getContext())}
                    </Table.Th>
                  ))}
                </Table.Tr>
              ))}
            </Table.Thead>
            <Table.Tbody>
              {paddingTop > 0 ? <Table.Tr><Table.Td colSpan={4} style={{ height: paddingTop, padding: 0 }} /></Table.Tr> : null}
              {(shouldVirtualize ? virtualRows.map((item) => rows[item.index]) : rows).map((row) => (
                <Table.Tr
                  key={row.id}
                  aria-selected={selected === row.id}
                  data-event-row={row.id}
                  tabIndex={0}
                  onClick={() => setSelected(row.id)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      setSelected(row.id);
                    }
                  }}
                  style={selected === row.id ? { background: "var(--mantine-color-eyeBlue-0)" } : undefined}
                >
                  {row.getVisibleCells().map((cell) => (
                    <Table.Td key={cell.id} style={{ verticalAlign: "top" }}>
                      {flexRender(cell.column.columnDef.cell, cell.getContext())}
                    </Table.Td>
                  ))}
                </Table.Tr>
              ))}
              {paddingBottom > 0 ? <Table.Tr><Table.Td colSpan={4} style={{ height: paddingBottom, padding: 0 }} /></Table.Tr> : null}
            </Table.Tbody>
          </Table>
        )}
      </Box>
    </Box>
  );
}
