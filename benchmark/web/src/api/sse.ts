import type { EventData, EventRecord } from "./types";

const EVENT_KINDS = new Set<EventData["kind"]>([
  "run_state",
  "family_state",
  "cell_state",
  "trial_state",
  "trial_phase",
  "request_state",
  "resource_window",
  "correctness",
  "warning",
  "log",
  "report_ready",
]);

export interface DecodedEventStream {
  records: EventRecord[];
  remainder: string;
}

function isEventRecord(value: unknown): value is EventRecord {
  if (typeof value !== "object" || value === null) return false;
  const record = value as Record<string, unknown>;
  if (
    typeof record.sequence !== "number" ||
    !Number.isSafeInteger(record.sequence) ||
    record.sequence < 0 ||
    typeof record.run_id !== "string" ||
    typeof record.monotonic_offset_ns !== "number" ||
    !Number.isSafeInteger(record.monotonic_offset_ns) ||
    record.monotonic_offset_ns < 0 ||
    typeof record.data !== "object" ||
    record.data === null
  ) {
    return false;
  }
  const kind = (record.data as Record<string, unknown>).kind;
  return typeof kind === "string" && EVENT_KINDS.has(kind as EventData["kind"]);
}

function frameBoundary(buffer: string): { index: number; length: number } | null {
  const lf = buffer.indexOf("\n\n");
  const crlf = buffer.indexOf("\r\n\r\n");
  if (lf < 0 && crlf < 0) return null;
  if (crlf >= 0 && (lf < 0 || crlf < lf)) return { index: crlf, length: 4 };
  return { index: lf, length: 2 };
}

function fieldValue(line: string, prefix: string): string | null {
  if (!line.startsWith(prefix)) return null;
  const value = line.slice(prefix.length);
  return value.startsWith(" ") ? value.slice(1) : value;
}

function decodeFrame(frame: string): EventRecord | null {
  let eventName = "message";
  let eventId: string | null = null;
  const data: string[] = [];

  for (const line of frame.split(/\r?\n/)) {
    if (line === "" || line.startsWith(":")) continue;
    const id = fieldValue(line, "id:");
    if (id !== null) {
      eventId = id;
      continue;
    }
    const event = fieldValue(line, "event:");
    if (event !== null) {
      eventName = event;
      continue;
    }
    const value = fieldValue(line, "data:");
    if (value !== null) data.push(value);
  }

  if (data.length === 0) return null;
  const parsed: unknown = JSON.parse(data.join("\n"));
  if (!isEventRecord(parsed)) throw new Error("The runner emitted an invalid benchmark event record.");
  if (eventId !== String(parsed.sequence)) {
    throw new Error("The benchmark SSE id does not match the persisted event sequence.");
  }
  if (eventName !== parsed.data.kind) {
    throw new Error("The benchmark SSE event name does not match its typed payload.");
  }
  return parsed;
}

export function decodeEventStream(buffer: string): DecodedEventStream {
  const records: EventRecord[] = [];
  let remainder = buffer;
  let boundary = frameBoundary(remainder);

  while (boundary) {
    const frame = remainder.slice(0, boundary.index);
    remainder = remainder.slice(boundary.index + boundary.length);
    const record = decodeFrame(frame);
    if (record) records.push(record);
    boundary = frameBoundary(remainder);
  }

  return { records, remainder };
}
