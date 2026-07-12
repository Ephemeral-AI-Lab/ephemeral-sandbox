import { ApiClientError } from "@/api/client";
import type { MetricUnit } from "@/api/types";

const integer = new Intl.NumberFormat(undefined, { maximumFractionDigits: 0 });

export function formatInteger(value: number): string {
  return integer.format(value);
}

export function formatBytes(value: number | null): string {
  if (value === null) return "Unavailable";
  if (value < 1024) return `${integer.format(value)} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"] as const;
  let amount = value / 1024;
  let unitIndex = 0;
  while (amount >= 1024 && unitIndex < units.length - 1) {
    amount /= 1024;
    unitIndex += 1;
  }
  return `${amount.toLocaleString(undefined, { maximumFractionDigits: 1 })} ${units[unitIndex]}`;
}

export function formatDurationNs(value: number): string {
  if (value < 1_000) return `${integer.format(value)} ns`;
  if (value < 1_000_000) return `${(value / 1_000).toLocaleString(undefined, { maximumFractionDigits: 1 })} µs`;
  if (value < 1_000_000_000) {
    return `${(value / 1_000_000).toLocaleString(undefined, { maximumFractionDigits: 1 })} ms`;
  }
  return `${(value / 1_000_000_000).toLocaleString(undefined, { maximumFractionDigits: 1 })} s`;
}

export function formatTimestamp(value: string | null): string {
  if (value === null) return "In progress";
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? value : date.toLocaleString();
}

export function formatNumber(value: number | null, maximumFractionDigits = 3): string {
  if (value === null) return "Unavailable";
  return value.toLocaleString(undefined, { maximumFractionDigits });
}

export function formatMetricValue(value: number | null, unit: MetricUnit): string {
  if (value === null) return "Unavailable";
  switch (unit) {
    case "bytes": return formatBytes(value);
    case "nanoseconds": return formatDurationNs(value);
    case "bytes_per_second": return `${formatBytes(value)}/s`;
    case "operations_per_second": return `${formatNumber(value, 2)} ops/s`;
    case "count": return formatNumber(value);
    case "ratio": return formatNumber(value, 4);
  }
}

export function labelIdentifier(value: string): string {
  const label = value.replaceAll("_", " ").replaceAll(".", " · ");
  return label.charAt(0).toUpperCase() + label.slice(1);
}

export function errorMessage(error: unknown): string {
  if (error instanceof ApiClientError) {
    return error.requestId ? `${error.message} Request ${error.requestId}.` : error.message;
  }
  return error instanceof Error ? error.message : "The runner request failed.";
}
