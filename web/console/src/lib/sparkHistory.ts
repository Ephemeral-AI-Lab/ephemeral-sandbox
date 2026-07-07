import type { ResourceSample } from "@/api/observability";

export interface SparkSeries {
  cpu: number[];
  mem: number[];
}

const RING_SIZE = 24;

const rings = new Map<string, { lastTs: number; cpu: number[]; mem: number[] }>();

/**
 * Client-side sample accumulator: the snapshot op returns only the latest
 * cgroup sample, so sparklines build their window from successive polls.
 * Samples are deduplicated by timestamp; series are capped to a fixed ring.
 */
export function recordSample(key: string, sample: ResourceSample | null): SparkSeries {
  let ring = rings.get(key);
  if (!ring) {
    ring = { lastTs: 0, cpu: [], mem: [] };
    rings.set(key, ring);
  }
  if (sample && sample.ts !== ring.lastTs) {
    ring.lastTs = sample.ts;
    const cpuDelta = sample.deltas["cpu_usec"];
    const periodMs = sample.sample_delta_ms > 0 ? sample.sample_delta_ms : 1000;
    if (typeof cpuDelta === "number") {
      ring.cpu.push(cpuDelta / (periodMs * 1000));
      if (ring.cpu.length > RING_SIZE) ring.cpu.shift();
    }
    const mem = sample.metrics["mem_cur"];
    if (typeof mem === "number") {
      ring.mem.push(mem);
      if (ring.mem.length > RING_SIZE) ring.mem.shift();
    }
  }
  return { cpu: [...ring.cpu], mem: [...ring.mem] };
}

export function dropSample(key: string) {
  rings.delete(key);
}
