import { createHash } from "node:crypto";
import { createReadStream, createWriteStream } from "node:fs";
import { appendFile, lstat, mkdir, readFile, readdir, readlink, realpath, stat, unlink, writeFile } from "node:fs/promises";
import { basename, dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn, spawnSync } from "node:child_process";
import { isDeepStrictEqual } from "node:util";

const webRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const repoRoot = resolve(webRoot, "../..");
const timestamp = new Date().toISOString().replaceAll(":", "-").replaceAll(".", "-");
const evidenceRoot = resolve(
  process.env.BENCHMARK_EVIDENCE_ROOT ?? join(repoRoot, "..", "ephemeral-sandbox-benchmark-evidence", timestamp),
);
const workspaceRoot = join(evidenceRoot, "test-workspace-root");
const logPath = join(evidenceRoot, "sandbox-benchmark.log");
const inFlightProgressPath = join(evidenceRoot, "inflight-progress.ndjson");
const harnessProgressPath = join(evidenceRoot, "harness-progress.ndjson");
const outsideWorkspaceSentinelPath = join(evidenceRoot, "outside-workspace-sentinel.txt");
const gateStartedAt = new Date().toISOString();
const executedCommands = [];
const harnessProgressStartedAt = process.hrtime.bigint();
let harnessProgressSequence = 0;
const harnessHeartbeatIntervalMs = 30_000;
const commandTimeouts = {
  probe: 2 * 60_000,
  dockerPull: 15 * 60_000,
  build: 30 * 60_000,
  browser: 45 * 60_000,
  browserStall: 5 * 60_000,
  health: 15_000,
};
const repositorySnapshotExclusions = [
  ".git",
  "target",
  "benchmark/web/node_modules",
  "benchmark/web/dist",
  "benchmark/web/playwright-report",
  "benchmark/web/test-results",
  ".omc",
  ".claude/scheduled_tasks.lock",
];
const coreArtifactFiles = new Map([
  ["run_manifest", "run-manifest.json"],
  ["intent_plan", "intent-plan.json"],
  ["expanded_plan", "expanded-plan.json"],
  ["definition_snapshot", "definition-snapshot.json"],
  ["environment_metadata", "environment-metadata.json"],
  ["events", "events.ndjson"],
  ["observations", "observations.ndjson"],
  ["summary", "summary.json"],
  ["report", "report.json"],
  ["json_export", "export.json"],
  ["csv_export", "export.csv"],
]);
// The artifact id is a hash of the canonical relative path, while the immutable
// evidence filename is a hash of its content.  Both hashes are intentional and
// independently verified below.
const boundedEvidenceLabel = /^cells\/[A-Za-z0-9_:-]{1,128}\/trials\/[A-Za-z0-9_:-]{1,128}\/bounded-evidence\/operation-evidence-[a-f0-9]{64}\.json$/;
const expectedBrowserRuns = [
  {
    ordinal: 1,
    role: "command_reference",
    scope: "command",
    state: "completed",
    design_counts: { test_combinations: 2, trial_batches: 12, issued_product_requests: 36 },
    starting_preset: null,
  },
  {
    ordinal: 2,
    role: "command_candidate",
    scope: "command",
    state: "completed",
    design_counts: { test_combinations: 2, trial_batches: 12, issued_product_requests: 36 },
    starting_preset: null,
  },
  {
    ordinal: 3,
    role: "files",
    scope: "files",
    state: "completed",
    design_counts: { test_combinations: 2, trial_batches: 12, issued_product_requests: 12 },
    starting_preset: null,
  },
  {
    ordinal: 4,
    role: "workspace",
    scope: "workspace",
    state: "completed",
    design_counts: { test_combinations: 2, trial_batches: 12, issued_product_requests: 36 },
    starting_preset: null,
  },
  {
    ordinal: 5,
    role: "layerstack",
    scope: "layerstack",
    state: "completed",
    design_counts: { test_combinations: 2, trial_batches: 12, issued_product_requests: 12 },
    starting_preset: null,
  },
  {
    ordinal: 6,
    role: "run_all",
    scope: "all",
    state: "completed",
    design_counts: { test_combinations: 8, trial_batches: 48, issued_product_requests: 96 },
    starting_preset: { id: "quick-smoke", version: 1 },
  },
  {
    ordinal: 7,
    role: "cancelled_run_all",
    scope: "all",
    state: "cancelled",
    design_counts: { test_combinations: 8, trial_batches: 48, issued_product_requests: 96 },
    starting_preset: { id: "quick-smoke", version: 1 },
  },
];
const realBackendStage = process.env.BENCHMARK_REAL_BACKEND_STAGE ?? "full";
if (!["small", "medium", "full"].includes(realBackendStage)) {
  throw new Error(`BENCHMARK_REAL_BACKEND_STAGE must be small, medium, or full; received ${realBackendStage}`);
}
const expectedStageRuns = realBackendStage === "small"
  ? [expectedBrowserRuns[4]]
  : realBackendStage === "medium"
    ? expectedBrowserRuns.slice(0, 5)
    : expectedBrowserRuns;
const expectedCompletedRunCount = realBackendStage === "full" ? 6 : expectedStageRuns.length;

function stageTopologyMatches(ids, runIds) {
  if (!isDeepStrictEqual(ids.completed, runIds.slice(0, expectedCompletedRunCount))) return false;
  if (realBackendStage === "small") {
    return ids.cancelled === null
      && ids.run_all === null
      && ids.comparison === null
      && isDeepStrictEqual(ids.family_runs, { layerstack: runIds[0] });
  }
  if (realBackendStage === "medium") {
    return ids.cancelled === null
      && ids.run_all === null
      && ids.comparison === null
      && isDeepStrictEqual(ids.family_runs, {
        command: runIds[0],
        files: runIds[2],
        workspace: runIds[3],
        layerstack: runIds[4],
      });
  }
  return typeof ids.cancelled === "string"
    && ids.cancelled === runIds[6]
    && ids.run_all === runIds[5]
    && isDeepStrictEqual(ids.comparison, { reference: runIds[0], candidate: runIds[1] })
    && isDeepStrictEqual(ids.family_runs, {
      command: runIds[0],
      files: runIds[2],
      workspace: runIds[3],
      layerstack: runIds[4],
    });
}

function containsPath(parent, child) {
  const childRelative = relative(parent, child);
  return childRelative === "" || (!childRelative.startsWith("..") && !isAbsolute(childRelative));
}

async function prospectiveRealPath(path) {
  const missing = [];
  let cursor = path;
  while (true) {
    try {
      return resolve(await realpath(cursor), ...missing.reverse());
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
      const parent = dirname(cursor);
      if (parent === cursor) throw error;
      missing.push(basename(cursor));
      cursor = parent;
    }
  }
}

if (containsPath(repoRoot, evidenceRoot) || containsPath(evidenceRoot, repoRoot)) {
  throw new Error(`Evidence root must be outside and must not contain the repository: ${evidenceRoot}`);
}

const [canonicalRepositoryRoot, prospectiveEvidenceRoot] = await Promise.all([
  realpath(repoRoot),
  prospectiveRealPath(evidenceRoot),
]);
if (containsPath(canonicalRepositoryRoot, prospectiveEvidenceRoot) || containsPath(prospectiveEvidenceRoot, canonicalRepositoryRoot)) {
  throw new Error(`Canonical evidence root must be outside and must not contain the repository: ${prospectiveEvidenceRoot}`);
}
await mkdir(evidenceRoot, { recursive: true });
const canonicalEvidenceRoot = await realpath(evidenceRoot);
if (containsPath(canonicalRepositoryRoot, canonicalEvidenceRoot) || containsPath(canonicalEvidenceRoot, canonicalRepositoryRoot)) {
  throw new Error(`Canonical evidence root must be outside and must not contain the repository: ${canonicalEvidenceRoot}`);
}
const existingEvidenceEntries = await readdir(evidenceRoot);
if (existingEvidenceEntries.length > 0) {
  throw new Error(`Evidence root must start empty: ${canonicalEvidenceRoot}`);
}
try {
  await lstat(workspaceRoot);
  throw new Error(`Dedicated test workspace must not already exist: ${workspaceRoot}`);
} catch (error) {
  if (error?.code !== "ENOENT") throw error;
}
await mkdir(workspaceRoot);
const writableProbe = join(workspaceRoot, ".release-gate-writable-probe");
await writeFile(writableProbe, "writable\n", { flag: "wx" });
await unlink(writableProbe);
await writeFile(outsideWorkspaceSentinelPath, `outside-workspace sentinel ${gateStartedAt}\n`, { flag: "wx" });
await writeFile(inFlightProgressPath, `${JSON.stringify({
  schema_version: 1,
  sequence: 0,
  recorded_at: new Date().toISOString(),
  monotonic_offset_ns: 0,
  stage: process.env.BENCHMARK_REAL_BACKEND_STAGE ?? "full",
  checkpoint: "node-harness-ready",
  detail: { watchdog_stall_timeout_ms: commandTimeouts.browserStall },
})}\n`, { flag: "wx" });
await writeFile(harnessProgressPath, `${JSON.stringify({
  schema_version: 1,
  sequence: harnessProgressSequence,
  recorded_at: new Date().toISOString(),
  monotonic_offset_ns: 0,
  stage: realBackendStage,
  checkpoint: "node-harness-ready",
  detail: { command_heartbeat_interval_ms: harnessHeartbeatIntervalMs },
})}\n`, { flag: "wx" });
const outsideWorkspaceSentinelBefore = {
  exists: true,
  type: "file",
  bytes: (await stat(outsideWorkspaceSentinelPath)).size,
  sha256: await hashFile(outsideWorkspaceSentinelPath),
};

function gitStatus() {
  return captureRaw("git", ["status", "--porcelain=v1", "--untracked-files=all"], repoRoot);
}

function killProcessGroup(pid, signal) {
  if (!Number.isSafeInteger(pid) || pid <= 0) return;
  try {
    process.kill(process.platform === "win32" ? pid : -pid, signal);
  } catch (error) {
    if (error?.code !== "ESRCH") throw error;
  }
}

function captureRaw(command, args, cwd = repoRoot, timeoutMs = commandTimeouts.probe) {
  const record = {
    command: commandText(command, args),
    command_redacted: redactedCommand(command, args),
    cwd,
    cwd_redacted: displayDirectory(cwd),
    started_at: new Date().toISOString(),
    ended_at: null,
    exit_code: null,
    signal: null,
    timeout_ms: timeoutMs,
    timed_out: false,
  };
  executedCommands.push(record);
  const result = spawnSync(command, args, {
    cwd,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    timeout: timeoutMs,
    killSignal: "SIGTERM",
    detached: process.platform !== "win32",
  });
  record.ended_at = new Date().toISOString();
  record.exit_code = result.status;
  record.signal = result.signal;
  record.timed_out = result.error?.code === "ETIMEDOUT";
  if (record.timed_out) {
    killProcessGroup(result.pid, "SIGKILL");
    throw new Error(`${command} ${args.join(" ")} exceeded its ${timeoutMs} ms timeout`);
  }
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed: ${result.stderr || `exit ${result.status}`}`);
  }
  return result.stdout;
}

function capture(command, args, cwd = repoRoot, timeoutMs = commandTimeouts.probe) {
  return captureRaw(command, args, cwd, timeoutMs).trim();
}

function commandText(command, args) {
  return [command, ...args].map((part) => /^[A-Za-z0-9_./:=@+-]+$/.test(part) ? part : JSON.stringify(part)).join(" ");
}

function displayDirectory(path) {
  if (path === repoRoot) return "<repo-root>";
  if (path === webRoot) return "<repo-root>/benchmark/web";
  return path.replace(evidenceRoot, "<evidence-root>");
}

function redactText(value) {
  return value
    .replaceAll(workspaceRoot, "<evidence-root>/test-workspace-root")
    .replaceAll(evidenceRoot, "<evidence-root>")
    .replaceAll(repoRoot, "<repo-root>");
}

function redactedCommand(command, args) {
  return redactText(commandText(command, args));
}

function evidenceCommands() {
  return executedCommands.map(({ command_redacted, cwd_redacted, ...record }) => ({
    ...record,
    command: command_redacted ?? redactText(record.command),
    cwd: cwd_redacted ?? displayDirectory(record.cwd),
  }));
}

async function recordHarnessProgress(checkpoint, detail = {}) {
  const record = {
    schema_version: 1,
    sequence: ++harnessProgressSequence,
    recorded_at: new Date().toISOString(),
    monotonic_offset_ns: Number((process.hrtime.bigint() - harnessProgressStartedAt) / 1_000_000n) * 1_000_000,
    stage: realBackendStage,
    checkpoint,
    detail,
  };
  const line = JSON.stringify(record);
  // Command telemetry is deliberately separate from browser checkpoints. That
  // keeps the browser's monotonic sequence authoritative while making builds,
  // image pulls, and teardown observable even before Playwright starts.
  try {
    await appendFile(harnessProgressPath, `${line}\n`);
    console.log(`[benchmark-harness] ${line}`);
  } catch (error) {
    // Telemetry must never turn an otherwise useful command failure into an
    // opaque secondary failure. The command's normal error path still wins.
    console.error("[benchmark-harness] could not retain progress", error);
  }
}

function run(command, args, cwd, env = process.env, timeoutMs = commandTimeouts.build, progress = null) {
  const record = {
    command: commandText(command, args),
    command_redacted: redactedCommand(command, args),
    cwd,
    cwd_redacted: displayDirectory(cwd),
    started_at: new Date().toISOString(),
    ended_at: null,
    exit_code: null,
    signal: null,
    timeout_ms: timeoutMs,
    timed_out: false,
    progress_log: progress === null ? null : "<evidence-root>/inflight-progress.ndjson",
    progress_stall_timeout_ms: progress?.stallTimeoutMs ?? null,
    last_progress_at: null,
    stalled: false,
    harness_progress_log: "<evidence-root>/harness-progress.ndjson",
  };
  executedCommands.push(record);
  return (async () => {
    await recordHarnessProgress("command-started", {
      command: record.command_redacted,
      cwd: record.cwd_redacted,
      timeout_ms: timeoutMs,
      browser_progress_watchdog: progress !== null,
    });
    return await new Promise((resolvePromise, reject) => {
    const child = spawn(command, args, {
      cwd,
      stdio: "inherit",
      env,
      detached: process.platform !== "win32",
    });
    let settled = false;
    let forceTimer = null;
    let progressOffset = 0;
    let progressRemainder = "";
    let lastProgressAt = Date.now();
    let progressPolling = false;
    const startedAtMs = Date.now();
    const heartbeat = setInterval(() => {
      void recordHarnessProgress("command-heartbeat", {
        command: record.command_redacted,
        elapsed_ms: Date.now() - startedAtMs,
        timeout_ms: timeoutMs,
        browser_progress_watchdog: progress !== null,
      });
    }, harnessHeartbeatIntervalMs);
    heartbeat.unref();
    const pollProgress = async () => {
      if (settled || record.stalled || progress === null || progressPolling) return;
      progressPolling = true;
      try {
        const content = await readFile(progress.path);
        if (content.byteLength > progressOffset) {
          const appended = content.subarray(progressOffset).toString("utf8");
          progressOffset = content.byteLength;
          const lines = `${progressRemainder}${appended}`.split("\n");
          progressRemainder = lines.pop() ?? "";
          for (const line of lines.filter(Boolean)) {
            // This is intentionally a direct harness stream rather than a
            // Playwright reporter message: it remains visible while a test
            // action is pending and is also retained verbatim for diagnosis.
            console.log(`[benchmark-inflight] ${line}`);
            lastProgressAt = Date.now();
            record.last_progress_at = new Date(lastProgressAt).toISOString();
          }
        }
      } catch (error) {
        if (error?.code !== "ENOENT") throw error;
      } finally {
        progressPolling = false;
      }
      if (Date.now() - lastProgressAt > progress.stallTimeoutMs) {
        record.stalled = true;
        record.timed_out = true;
        console.error(`[benchmark-inflight] watchdog: no browser checkpoint for ${progress.stallTimeoutMs} ms; terminating ${command}`);
        void recordHarnessProgress("browser-progress-watchdog-fired", {
          command: record.command_redacted,
          stall_timeout_ms: progress.stallTimeoutMs,
        });
        killProcessGroup(child.pid, "SIGTERM");
        forceTimer = setTimeout(() => killProcessGroup(child.pid, "SIGKILL"), 5_000);
        forceTimer.unref();
      }
    };
    const progressWatcher = progress === null ? null : setInterval(() => {
      void pollProgress().catch((error) => {
        if (!settled) {
          record.stalled = true;
          record.timed_out = true;
          console.error("[benchmark-inflight] watchdog could not read progress evidence", error);
          void recordHarnessProgress("browser-progress-watchdog-read-failed", {
            command: record.command_redacted,
          });
          killProcessGroup(child.pid, "SIGTERM");
        }
      });
    }, 2_000);
    progressWatcher?.unref();
    const timeout = setTimeout(() => {
      record.timed_out = true;
      void recordHarnessProgress("command-timeout-fired", {
        command: record.command_redacted,
        timeout_ms: timeoutMs,
      });
      killProcessGroup(child.pid, "SIGTERM");
      forceTimer = setTimeout(() => killProcessGroup(child.pid, "SIGKILL"), 5_000);
      forceTimer.unref();
    }, timeoutMs);
    timeout.unref();
    child.once("error", async (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      clearInterval(heartbeat);
      if (progressWatcher) clearInterval(progressWatcher);
      if (forceTimer) clearTimeout(forceTimer);
      record.ended_at = new Date().toISOString();
      await recordHarnessProgress("command-spawn-error", {
        command: record.command_redacted,
        elapsed_ms: Date.now() - startedAtMs,
        message: String(error?.message ?? error),
      });
      reject(error);
    });
    child.once("exit", async (code, signal) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      clearInterval(heartbeat);
      if (progressWatcher) clearInterval(progressWatcher);
      if (forceTimer) clearTimeout(forceTimer);
      record.ended_at = new Date().toISOString();
      record.exit_code = code;
      record.signal = signal;
      await recordHarnessProgress("command-finished", {
        command: record.command_redacted,
        elapsed_ms: Date.now() - startedAtMs,
        exit_code: code,
        signal,
        timed_out: record.timed_out,
        stalled: record.stalled,
      });
      if (record.stalled) reject(new Error(`${command} ${args.join(" ")} stopped after its browser progress watchdog observed no checkpoint for ${progress.stallTimeoutMs} ms`));
      else if (record.timed_out) reject(new Error(`${command} ${args.join(" ")} exceeded its ${timeoutMs} ms timeout`));
      else if (code === 0) resolvePromise();
      else reject(new Error(`${command} ${args.join(" ")} failed with code ${code ?? "none"} signal ${signal ?? "none"}`));
    });
    });
  })();
}

async function fetchJson(url, timeoutMs = commandTimeouts.health) {
  const response = await fetch(url, { signal: AbortSignal.timeout(timeoutMs) });
  if (!response.ok) throw new Error(`${url} returned HTTP ${response.status}`);
  return await response.json();
}

async function sourceIdentity() {
  const status = gitStatus();
  const diff = capture("git", ["diff", "--binary", "HEAD", "--"], repoRoot);
  return {
    commit: capture("git", ["rev-parse", "HEAD"], repoRoot),
    branch: capture("git", ["rev-parse", "--abbrev-ref", "HEAD"], repoRoot),
    dirty: status.length > 0,
    status_porcelain: status === "" ? [] : status.trimEnd().split("\n"),
    status_sha256: createHash("sha256").update(status).digest("hex"),
    tracked_diff_sha256: createHash("sha256").update(diff).digest("hex"),
  };
}

function excludedRepositoryPath(path) {
  return repositorySnapshotExclusions.some((excluded) => path === excluded || path.startsWith(`${excluded}/`));
}

async function contentSnapshot(root, exclusions = () => false) {
  const entries = [];
  async function visit(path, pathRelative) {
    if (pathRelative && exclusions(pathRelative)) return;
    const metadata = await lstat(path);
    const mode = metadata.mode & 0o777;
    if (metadata.isSymbolicLink()) {
      entries.push({ path: pathRelative, type: "symlink", mode, target: await readlink(path) });
      return;
    }
    if (metadata.isDirectory()) {
      if (pathRelative) entries.push({ path: pathRelative, type: "directory", mode });
      for (const name of (await readdir(path)).sort()) {
        await visit(join(path, name), pathRelative ? `${pathRelative}/${name}` : name);
      }
      return;
    }
    if (metadata.isFile()) {
      const bytes = await readFile(path);
      entries.push({
        path: pathRelative,
        type: "file",
        mode,
        links: metadata.nlink,
        bytes: bytes.byteLength,
        sha256: createHash("sha256").update(bytes).digest("hex"),
      });
      return;
    }
    entries.push({ path: pathRelative, type: "other", mode, bytes: metadata.size });
  }
  await visit(root, "");
  return {
    root,
    entries,
    digest: createHash("sha256").update(JSON.stringify(entries)).digest("hex"),
  };
}

async function optionalContentSnapshot(root) {
  try {
    return await contentSnapshot(root);
  } catch (error) {
    if (error?.code === "ENOENT") {
      const entries = [];
      return { root, entries, digest: createHash("sha256").update(JSON.stringify(entries)).digest("hex") };
    }
    throw error;
  }
}

function snapshotDiff(before, after) {
  const beforeByPath = new Map(before.entries.map((entry) => [entry.path, entry]));
  const afterByPath = new Map(after.entries.map((entry) => [entry.path, entry]));
  const paths = [...new Set([...beforeByPath.keys(), ...afterByPath.keys()])].sort();
  return paths.flatMap((path) => {
    const previous = beforeByPath.get(path) ?? null;
    const current = afterByPath.get(path) ?? null;
    return JSON.stringify(previous) === JSON.stringify(current) ? [] : [{ path, before: previous, after: current }];
  });
}

function parseJsonLines(text) {
  return text === "" ? [] : text.split("\n").filter(Boolean).map((line) => JSON.parse(line));
}

function sortedBy(values, key) {
  return values.sort((left, right) => key(left).localeCompare(key(right)));
}

function dockerSnapshot() {
  const containerRows = parseJsonLines(capture("docker", ["container", "ls", "-a", "--no-trunc", "--format", "{{json .}}"]));
  const inspectedContainers = containerRows.length === 0
    ? []
    : JSON.parse(capture("docker", ["container", "inspect", ...containerRows.map((item) => item.ID)]));
  const containers = inspectedContainers.map((item) => {
    const mounts = (item.Mounts ?? []).map((mount) => ({
      type: mount.Type,
      name: mount.Name ?? null,
      source: mount.Source,
      destination: mount.Destination,
      mode: mount.Mode,
      rw: mount.RW,
      propagation: mount.Propagation,
    }));
    const networks = Object.entries(item.NetworkSettings?.Networks ?? {})
      .map(([name, network]) => ({
        name,
        network_id: network.NetworkID,
        endpoint_id: network.EndpointID,
        gateway: network.Gateway,
        ip_address: network.IPAddress,
        mac_address: network.MacAddress,
      }));
    return {
      id: item.Id,
      image: item.Config?.Image ?? null,
      image_id: item.Image,
      name: item.Name?.replace(/^\//, "") ?? null,
      created: item.Created,
      state: {
        status: item.State?.Status ?? null,
        running: item.State?.Running ?? null,
        paused: item.State?.Paused ?? null,
        restarting: item.State?.Restarting ?? null,
        oom_killed: item.State?.OOMKilled ?? null,
        dead: item.State?.Dead ?? null,
        pid: item.State?.Pid ?? null,
        exit_code: item.State?.ExitCode ?? null,
        started_at: item.State?.StartedAt ?? null,
        finished_at: item.State?.FinishedAt ?? null,
      },
      cgroup_parent: item.HostConfig?.CgroupParent ?? null,
      cgroup_namespace_mode: item.HostConfig?.CgroupnsMode ?? null,
      mounts_sha256: createHash("sha256")
        .update(JSON.stringify(sortedBy(mounts, (mount) => `${mount.destination}:${mount.source}`)))
        .digest("hex"),
      network_attachments_sha256: createHash("sha256")
        .update(JSON.stringify(sortedBy(networks, (network) => network.name)))
        .digest("hex"),
    };
  });
  const images = parseJsonLines(capture("docker", ["image", "ls", "--no-trunc", "--digests", "--format", "{{json .}}"])).map((item) => ({
    id: item.ID,
    repository: item.Repository,
    tag: item.Tag,
    digest: item.Digest,
  }));
  const networks = parseJsonLines(capture("docker", ["network", "ls", "--no-trunc", "--format", "{{json .}}"])).map((item) => ({
    id: item.ID,
    name: item.Name,
    driver: item.Driver,
    scope: item.Scope,
    ipv6: item.IPv6,
    internal: item.Internal,
  }));
  const volumes = parseJsonLines(capture("docker", ["volume", "ls", "--format", "{{json .}}"])).map((item) => ({
    name: item.Name,
    driver: item.Driver,
    scope: item.Scope,
  }));
  return {
    captured_at: new Date().toISOString(),
    containers: sortedBy(containers, (item) => item.id),
    images: sortedBy(images, (item) => `${item.id}:${item.repository}:${item.tag}`),
    networks: sortedBy(networks, (item) => item.id),
    volumes: sortedBy(volumes, (item) => item.name),
  };
}

function dockerDiff(before, after) {
  const collections = {
    containers: (item) => item.id,
    images: (item) => `${item.id}:${item.repository}:${item.tag}`,
    networks: (item) => item.id,
    volumes: (item) => item.name,
  };
  return Object.fromEntries(Object.entries(collections).map(([name, key]) => {
    const previous = new Map(before[name].map((item) => [key(item), item]));
    const current = new Map(after[name].map((item) => [key(item), item]));
    const ids = [...new Set([...previous.keys(), ...current.keys()])].sort();
    return [name, ids.flatMap((id) => JSON.stringify(previous.get(id)) === JSON.stringify(current.get(id))
      ? []
      : [{ id, before: previous.get(id) ?? null, after: current.get(id) ?? null }])];
  }));
}

async function cargoTargetDirectory() {
  return JSON.parse(capture("cargo", ["metadata", "--format-version", "1", "--no-deps"], repoRoot)).target_directory;
}

async function startRunner(binary) {
  const log = createWriteStream(logPath, { flags: "a" });
  const args = [
    "serve",
    "--repo", repoRoot,
    "--test-workspace-root", workspaceRoot,
    "--bind", "127.0.0.1:0",
  ];
  const record = {
    command: commandText(binary, args),
    command_redacted: commandText("<cargo-target>/release/sandbox-benchmark", [
      "serve",
      "--repo", "<repo-root>",
      "--test-workspace-root", "<evidence-root>/test-workspace-root",
      "--bind", "127.0.0.1:0",
    ]),
    cwd: repoRoot,
    cwd_redacted: "<repo-root>",
    started_at: new Date().toISOString(),
    ended_at: null,
    exit_code: null,
    signal: null,
  };
  executedCommands.push(record);
  const child = spawn(binary, args, { cwd: repoRoot, env: process.env, stdio: ["ignore", "pipe", "pipe"] });
  const closed = new Promise((resolvePromise) => child.once("close", resolvePromise));
  child.stdout.pipe(log, { end: false });
  child.stderr.pipe(log, { end: false });
  child.once("exit", (code, signal) => {
    record.ended_at = new Date().toISOString();
    record.exit_code = code;
    record.signal = signal;
  });

  let url;
  try {
    url = await new Promise((resolvePromise, reject) => {
      let buffer = "";
      const timer = setTimeout(() => reject(new Error("sandbox-benchmark did not publish its loopback URL within 60 seconds")), 60_000);
      const onData = (chunk) => {
        buffer += chunk.toString();
        const match = buffer.match(/sandbox-benchmark listening on (http:\/\/127\.0\.0\.1:\d+\/)/);
        if (!match) return;
        clearTimeout(timer);
        child.stdout.off("data", onData);
        resolvePromise(match[1].replace(/\/$/, ""));
      };
      child.stdout.on("data", onData);
      child.once("error", (error) => { clearTimeout(timer); reject(error); });
      child.once("exit", (code) => { clearTimeout(timer); reject(new Error(`sandbox-benchmark exited before readiness with code ${code}`)); });
    });
  } catch (readinessError) {
    try {
      await stopRunner({ child, log, closed });
    } catch (shutdownError) {
      throw new AggregateError([readinessError, shutdownError], "sandbox-benchmark readiness and shutdown both failed");
    }
    throw readinessError;
  }
  return { child, url, log, record, closed };
}

async function waitForChildExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) return true;
  return await new Promise((resolvePromise) => {
    const onExit = () => {
      clearTimeout(timer);
      resolvePromise(true);
    };
    const timer = setTimeout(() => {
      child.off("exit", onExit);
      resolvePromise(false);
    }, timeoutMs);
    child.once("exit", onExit);
  });
}

async function stopRunner(runner) {
  if (runner.child.exitCode !== null || runner.child.signalCode !== null) {
    await runner.closed;
    await new Promise((resolvePromise) => runner.log.end(resolvePromise));
    return;
  }
  runner.child.kill("SIGINT");
  const graceful = await waitForChildExit(runner.child, 45_000);
  if (!graceful) {
    runner.child.kill("SIGKILL");
    const forced = await waitForChildExit(runner.child, 5_000);
    if (forced) await runner.closed;
    await new Promise((resolvePromise) => runner.log.end(resolvePromise));
    if (!forced) throw new Error("sandbox-benchmark did not stop after SIGINT or SIGKILL");
    throw new Error("sandbox-benchmark required SIGKILL after ignoring the 45 second SIGINT cleanup window");
  }
  await runner.closed;
  await new Promise((resolvePromise) => runner.log.end(resolvePromise));
}

async function listFiles(root) {
  const result = [];
  async function visit(directory) {
    for (const name of await readdir(directory)) {
      const path = join(directory, name);
      const metadata = await lstat(path);
      if (metadata.isSymbolicLink()) throw new Error(`Retained evidence contains a symbolic link: ${relative(root, path)}`);
      if (metadata.isDirectory()) await visit(path);
      else if (metadata.isFile()) result.push(path);
    }
  }
  await visit(root);
  return result.sort();
}

async function snapshotNamedEntries(root, names) {
  const matches = [];
  async function visit(directory) {
    for (const name of await readdir(directory)) {
      const path = join(directory, name);
      const metadata = await lstat(path);
      if (names.has(name)) {
        const entry = {
          path: relative(root, path),
          mode: metadata.mode & 0o777,
          type: metadata.isSymbolicLink()
            ? "symlink"
            : metadata.isDirectory()
              ? "directory"
              : metadata.isFile()
                ? "file"
                : "other",
        };
        if (metadata.isSymbolicLink()) entry.target = await readlink(path);
        if (metadata.isFile()) {
          const bytes = await readFile(path);
          entry.bytes = bytes.byteLength;
          entry.sha256 = createHash("sha256").update(bytes).digest("hex");
        }
        matches.push(entry);
      }
      if (metadata.isDirectory() && !metadata.isSymbolicLink()) await visit(path);
    }
  }
  await visit(root);
  return sortedBy(matches, (entry) => entry.path);
}

async function hashFile(path) {
  return createHash("sha256").update(await readFile(path)).digest("hex");
}

async function optionalFileIdentity(path) {
  try {
    const metadata = await lstat(path);
    if (!metadata.isFile() || metadata.isSymbolicLink()) {
      return { exists: true, type: metadata.isSymbolicLink() ? "symlink" : "non_file", bytes: metadata.size, sha256: null };
    }
    return { exists: true, type: "file", bytes: metadata.size, sha256: await hashFile(path) };
  } catch (error) {
    if (error?.code === "ENOENT") return { exists: false, type: "missing", bytes: null, sha256: null };
    throw error;
  }
}

async function optionalJson(path) {
  try {
    return JSON.parse(await readFile(path, "utf8"));
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

async function collectRunEvidence() {
  const apiRoot = join(evidenceRoot, "api-snapshots");
  const ids = await optionalJson(join(apiRoot, "real-run-ids.json"));
  if (!ids) return { run_ids: null, runs: [] };
  if (
    ids.schema_version !== 1
    || ids.stage !== realBackendStage
    || !Array.isArray(ids.runs)
    || ids.runs.length !== expectedStageRuns.length
    || !Array.isArray(ids.completed)
    || ids.completed.length !== expectedCompletedRunCount
  ) {
    throw new Error(`Browser run identity evidence must describe the ${realBackendStage} stage topology`);
  }
  const runIds = ids.runs.map(({ run_id: runId }) => runId);
  if (new Set(runIds).size !== expectedStageRuns.length || runIds.some((runId) => !/^[A-Za-z0-9-]{1,64}$/.test(runId))) {
    throw new Error(`Browser run identity evidence must contain ${expectedStageRuns.length} distinct canonical run IDs`);
  }
  if (!stageTopologyMatches(ids, runIds)) {
    throw new Error(`Browser run role mappings do not match the required ${realBackendStage} stage topology`);
  }
  const runs = [];
  for (let index = 0; index < expectedStageRuns.length; index += 1) {
    const expected = expectedStageRuns[index];
    const identity = ids.runs[index];
    const { ordinal } = expected;
    if (
      identity?.ordinal !== expected.ordinal
      || identity?.role !== expected.role
      || identity?.scope !== expected.scope
      || identity?.state !== expected.state
      || identity?.run_id !== runIds[index]
      || !isDeepStrictEqual(identity?.design_counts, expected.design_counts)
    ) {
      throw new Error(`Run ${ordinal} identity does not match the required ${expected.role} topology`);
    }
    const reportName = expected.state === "cancelled" ? `run-${ordinal}-cancelled-report.json` : `run-${ordinal}-report.json`;
    const report = await optionalJson(join(apiRoot, reportName));
    const review = await optionalJson(join(apiRoot, `run-${ordinal}-review-validation.json`));
    const start = await optionalJson(join(apiRoot, `run-${ordinal}-start-request.json`));
    const create = await optionalJson(join(apiRoot, `run-${ordinal}-create.json`));
    const reportArtifact = await optionalJson(join(apiRoot, `run-${ordinal}-artifact-report.json`));
    if (!report || report.run_id !== runIds[index] || report.state !== expected.state) {
      throw new Error(`Run ${runIds[index]} has no matching ${expected.state} report evidence`);
    }
    if (
      report.schema_version !== 4
      || report.report_derivation_revision !== 3
      || !isDeepStrictEqual(report.design_counts, expected.design_counts)
    ) {
      throw new Error(`Run ${runIds[index]} report is not the required settled version-4 ${expected.scope} Quick Smoke projection`);
    }
    if (
      review?.schema_version !== 1
      || review.runnable !== true
      || review.estimates?.cell_count !== expected.design_counts.test_combinations
      || review.estimates?.trial_batch_count !== expected.design_counts.trial_batches
      || review.estimates?.issued_operation_request_count !== expected.design_counts.issued_product_requests
      || !/^sha256:[a-f0-9]{64}$/.test(review.plan_hash ?? "")
    ) {
      throw new Error(`Run ${runIds[index]} has no matching canonical ${expected.scope} Quick Smoke review evidence`);
    }
    if (
      start?.plan_hash !== review.plan_hash
      || !isDeepStrictEqual(start?.plan, review.canonical_plan)
      || !isDeepStrictEqual(start?.starting_preset, expected.starting_preset)
      || !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(start?.client_request_id ?? "")
    ) {
      throw new Error(`Run ${runIds[index]} start request does not match its canonical review`);
    }
    if (
      create?.schema_version !== 1
      || create.run_id !== runIds[index]
      || create.state !== "queued"
      || report.plan_hash !== review.plan_hash
    ) {
      throw new Error(`Run ${runIds[index]} create/report identity does not match its reviewed plan hash`);
    }
    let persistedReport = null;
    try {
      persistedReport = JSON.parse(reportArtifact?.content ?? "null");
    } catch {
      // The exact envelope failure is reported below with the run identity.
    }
    if (
      reportArtifact?.media_type !== "application/json"
      || persistedReport?.schema_name !== "eos_benchmark_report"
      || persistedReport?.schema_version !== 4
      || !isDeepStrictEqual(persistedReport?.data, report)
    ) {
      throw new Error(`Run ${runIds[index]} persisted version-4 report differs from its regenerated API report`);
    }
    if (
      typeof report.started_at !== "string"
      || typeof report.ended_at !== "string"
      || Number.isNaN(Date.parse(report.started_at))
      || Number.isNaN(Date.parse(report.ended_at))
      || typeof report.plan_hash !== "string"
      || !/^sha256:[a-f0-9]{64}$/.test(report.plan_hash)
    ) {
      throw new Error(`Run ${runIds[index]} report is missing exact timestamps or its canonical plan hash`);
    }
    runs.push({
      run_id: runIds[index],
      ordinal,
      role: expected.role,
      scope: expected.scope,
      expected_state: expected.state,
      started_at: report.started_at,
      ended_at: report.ended_at,
      plan_hash: report.plan_hash,
      report_schema_version: report.schema_version,
      report_derivation_revision: report.report_derivation_revision,
      design_counts: report.design_counts,
    });
  }
  return { run_ids: ids, runs };
}

function addParentDirectories(path, directories) {
  const components = path.split("/");
  for (let index = 1; index < components.length; index += 1) {
    directories.add(components.slice(0, index).join("/"));
  }
}

async function validateRetainedResults(baseline, final, runEvidence) {
  const errors = [];
  const expectedRunIds = runEvidence.runs.map(({ run_id }) => run_id);
  const expectedRunIdSet = new Set(expectedRunIds);
  if (baseline.entries.length !== 0) errors.push("The dedicated results root was not empty before execution");
  if (expectedRunIdSet.size !== expectedRunIds.length) errors.push("Browser evidence contains duplicate run IDs");
  for (const runId of expectedRunIds) {
    if (typeof runId !== "string" || !/^[A-Za-z0-9-]{1,64}$/.test(runId)) {
      errors.push(`Invalid retained run ID: ${String(runId)}`);
    }
  }

  const topLevel = final.entries.filter(({ path }) => !path.includes("/"));
  const actualRunIds = topLevel.filter(({ type }) => type === "directory").map(({ path }) => path).sort();
  for (const entry of topLevel) {
    if (entry.type !== "directory") errors.push(`Unexpected non-directory in results root: ${entry.path}`);
  }
  if (JSON.stringify(actualRunIds) !== JSON.stringify([...expectedRunIds].sort())) {
    errors.push(`Result run directories differ: expected ${[...expectedRunIds].sort().join(", ")}; found ${actualRunIds.join(", ")}`);
  }

  const allowedFiles = new Set();
  const allowedDirectories = new Set(expectedRunIds);
  const indexes = [];
  for (let index = 0; index < expectedRunIds.length; index += 1) {
    const ordinal = runEvidence.runs[index]?.ordinal;
    const runId = expectedRunIds[index];
    if (!Number.isSafeInteger(ordinal)) {
      errors.push(`Run ${runId} has no retained browser ordinal`);
      continue;
    }
    const apiIndex = await optionalJson(join(evidenceRoot, "api-snapshots", `run-${ordinal}-artifact-index.json`));
    indexes.push({ ordinal, run_id: runId, artifact_index: apiIndex });
    if (!apiIndex || apiIndex.schema_version !== 1 || apiIndex.run_id !== runId || !Array.isArray(apiIndex.artifacts)) {
      errors.push(`Run ${runId} has no matching version-1 browser artifact index`);
      continue;
    }
    const ids = new Set();
    const labels = new Set();
    for (const artifact of apiIndex.artifacts) {
      if (typeof artifact?.artifact_id !== "string" || ids.has(artifact.artifact_id)) {
        errors.push(`Run ${runId} has an invalid or duplicate artifact ID`);
        continue;
      }
      ids.add(artifact.artifact_id);
      if (typeof artifact.label !== "string" || labels.has(artifact.label)) {
        errors.push(`Run ${runId} has an invalid or duplicate artifact label for ${artifact.artifact_id}`);
        continue;
      }
      labels.add(artifact.label);
      const coreLabel = coreArtifactFiles.get(artifact.artifact_id);
      const dynamicId = /^bounded_evidence_[a-f0-9]{64}$/.test(artifact.artifact_id);
      if (coreLabel !== undefined) {
        if (artifact.label !== coreLabel) errors.push(`Run ${runId} maps ${artifact.artifact_id} to unexpected label ${artifact.label}`);
      } else if (!dynamicId || !boundedEvidenceLabel.test(artifact.label)) {
        errors.push(`Run ${runId} exposes non-allowlisted artifact ${artifact.artifact_id}: ${artifact.label}`);
      } else {
        const expectedDynamicId = `bounded_evidence_${createHash("sha256").update(artifact.label).digest("hex")}`;
        if (artifact.artifact_id !== expectedDynamicId) errors.push(`Run ${runId} bounded evidence ID does not match its canonical path: ${artifact.label}`);
      }
      if (!Number.isSafeInteger(artifact.size_bytes) || artifact.size_bytes < 0) {
        errors.push(`Run ${runId} artifact ${artifact.artifact_id} has invalid size`);
      }
      if (typeof artifact.sha256 !== "string" || !/^sha256:[a-f0-9]{64}$/.test(artifact.sha256)) {
        errors.push(`Run ${runId} artifact ${artifact.artifact_id} has invalid SHA-256`);
      }
      const resultPath = `${runId}/${artifact.label}`;
      allowedFiles.add(resultPath);
      addParentDirectories(resultPath, allowedDirectories);
      const stored = final.entries.find(({ path }) => path === resultPath);
      if (!stored || stored.type !== "file") {
        errors.push(`Run ${runId} indexed artifact is not an owned regular file: ${artifact.label}`);
      } else {
        if (stored.links !== 1) errors.push(`Run ${runId} artifact is not singly linked: ${artifact.label}`);
        if (stored.bytes !== artifact.size_bytes) errors.push(`Run ${runId} artifact size differs for ${artifact.label}`);
        if (`sha256:${stored.sha256}` !== artifact.sha256) errors.push(`Run ${runId} artifact hash differs for ${artifact.label}`);
        if (dynamicId) {
          const contentDigest = basename(artifact.label).slice("operation-evidence-".length, -".json".length);
          if (stored.sha256 !== contentDigest) errors.push(`Run ${runId} bounded evidence filename does not match its content: ${artifact.label}`);
        }
      }
    }
    for (const [artifactId, fileName] of coreArtifactFiles) {
      if (!ids.has(artifactId) || !labels.has(fileName)) errors.push(`Run ${runId} is missing terminal artifact ${artifactId}`);
    }
  }

  for (const entry of final.entries) {
    const runId = entry.path.split("/", 1)[0];
    if (!expectedRunIdSet.has(runId)) {
      errors.push(`Result path belongs to an unknown run: ${entry.path}`);
    } else if (entry.type === "file" && !allowedFiles.has(entry.path)) {
      errors.push(`Result tree contains an unindexed file: ${entry.path}`);
    } else if (entry.type === "directory" && !allowedDirectories.has(entry.path)) {
      errors.push(`Result tree contains an unindexed directory: ${entry.path}`);
    } else if (entry.type !== "file" && entry.type !== "directory") {
      errors.push(`Result tree contains unsafe ${entry.type}: ${entry.path}`);
    }
  }

  return {
    schema_version: 1,
    baseline_empty: baseline.entries.length === 0,
    expected_run_ids: expectedRunIds,
    actual_run_ids: actualRunIds,
    changes: snapshotDiff(baseline, final),
    artifact_indexes: indexes,
    errors: [...new Set(errors)],
    valid: errors.length === 0,
  };
}

const secretRules = [
  ["private_key", /-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----/i],
  ["aws_access_key_id", /\b(?:AKIA|ASIA)[A-Z0-9]{16}\b/],
  ["aws_secret_access_key", /AWS_SECRET_ACCESS_KEY\s*[:=]\s*["']?[A-Za-z0-9/+=]{20,}/i],
  ["authorization_credential", /authorization\s*[:=]\s*["']?(?:bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}/i],
  ["api_key_header", /x-api-key\s*[:=]\s*["']?[A-Za-z0-9._~+/=-]{8,}/i],
  ["openai_key", /\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b/],
  ["github_token", /\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b/],
  ["assigned_credential", /(?:api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|password|passwd)\s*[:=]\s*["']?(?!redacted\b|<redacted>|null\b|none\b)[A-Za-z0-9._~+/=-]{12,}/i],
];

function inspectSecretText(text, path, view, hits) {
  for (const [rule, pattern] of secretRules) {
    if (pattern.test(text) && !hits.some((hit) => hit.path === path && hit.view === view && hit.rule === rule)) {
      hits.push({ path, view, rule });
    }
  }
}

async function scanReadableForSecrets(readable, path, view, scan) {
  let tail = "";
  let bytes = 0;
  for await (const chunk of readable) {
    const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    bytes += buffer.byteLength;
    const text = tail + buffer.toString("latin1");
    inspectSecretText(text, path, view, scan.hits);
    tail = text.slice(-4096);
  }
  scan.bytes_scanned += bytes;
  return bytes;
}

async function scanFileForSecrets(path, scan) {
  const pathRelative = relative(evidenceRoot, path);
  await scanReadableForSecrets(createReadStream(path), pathRelative, "raw", scan);
  scan.raw_files_scanned += 1;
}

async function scanZipForSecrets(path, scan) {
  const args = ["-p", path];
  const record = {
    command: commandText("unzip", args),
    command_redacted: redactedCommand("unzip", args),
    cwd: evidenceRoot,
    cwd_redacted: "<evidence-root>",
    started_at: new Date().toISOString(),
    ended_at: null,
    exit_code: null,
    signal: null,
  };
  executedCommands.push(record);
  const child = spawn("unzip", args, { cwd: evidenceRoot, stdio: ["ignore", "pipe", "pipe"] });
  let stderr = "";
  child.stderr.on("data", (chunk) => {
    if (stderr.length < 64 * 1024) stderr += chunk.toString();
  });
  const closed = new Promise((resolvePromise, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => {
      record.ended_at = new Date().toISOString();
      record.exit_code = code;
      record.signal = signal;
      resolvePromise({ code, signal });
    });
  });
  const pathRelative = relative(evidenceRoot, path);
  await scanReadableForSecrets(child.stdout, pathRelative, "decompressed_zip", scan);
  const { code, signal } = await closed;
  if (code !== 0) throw new Error(`unzip secret scan failed for ${pathRelative}: ${stderr || `exit ${code} signal ${signal ?? "none"}`}`);
  scan.zip_archives_expanded += 1;
}

async function scanRetainedEvidence() {
  const scan = {
    schema_version: 1,
    started_at: new Date().toISOString(),
    ended_at: null,
    method: "Every retained file is streamed as raw bytes; every ZIP is additionally streamed through unzip -p. Matches record only path, view, and rule, never credential material.",
    rules: secretRules.map(([name]) => name),
    raw_files_scanned: 0,
    zip_archives_expanded: 0,
    bytes_scanned: 0,
    self_buffer_scanned: true,
    hits: [],
    passed: false,
  };
  const files = await listFiles(evidenceRoot);
  for (const path of files) {
    await scanFileForSecrets(path, scan);
    if (path.toLowerCase().endsWith(".zip")) await scanZipForSecrets(path, scan);
  }
  return scan;
}

async function collectImageIdentity() {
  const artifact = await optionalJson(join(
    evidenceRoot,
    "api-snapshots",
    `run-${expectedStageRuns[0].ordinal}-artifact-environment_metadata.json`,
  ));
  if (!artifact?.content) return null;
  const envelope = JSON.parse(artifact.content);
  const environment = envelope.data;
  const inspected = JSON.parse(capture("docker", ["image", "inspect", environment.image_reference]))[0];
  const identity = {
    reference: environment.image_reference,
    artifact_digest: environment.image_digest,
    docker_id: inspected.Id,
    repo_digests: inspected.RepoDigests ?? [],
    repo_tags: inspected.RepoTags ?? [],
    created: inspected.Created,
    architecture: inspected.Architecture,
    operating_system: inspected.Os,
    digest_matches_docker_id: environment.image_digest === inspected.Id,
  };
  if (!identity.digest_matches_docker_id) {
    throw new Error(`Artifact image digest ${environment.image_digest} does not match Docker image id ${inspected.Id}`);
  }
  return identity;
}

async function validateRequiredEvidenceFiles(exitCode) {
  const files = await listFiles(evidenceRoot);
  const byRelativePath = new Map(files.map((path) => [relative(evidenceRoot, path), path]));
  const required = [
    "sandbox-benchmark.log",
    "inflight-progress.ndjson",
    "harness-progress.ndjson",
    "production-web-assets.json",
    "playwright-report.json",
    "playwright-html/index.html",
    "api-snapshots/browser-runtime.json",
    "api-snapshots/request-ledger.json",
    "api-snapshots/browser-sentinels.json",
    "api-snapshots/real-run-ids.json",
    "api-snapshots/layerstack-phase-proof.json",
    "screenshots/layerstack-storage-remount-evidence.png",
  ];
  for (const expected of expectedStageRuns) {
    const { ordinal } = expected;
    required.push(`api-snapshots/run-${ordinal}-review-validation.json`);
    required.push(`api-snapshots/run-${ordinal}-start-request.json`);
    required.push(`api-snapshots/run-${ordinal}-create.json`);
    required.push(`api-snapshots/run-${ordinal}-artifact-index.json`);
    required.push(`api-snapshots/run-${ordinal}-artifact-report.json`);
    required.push(`exports/run-${ordinal}-json_export`);
    required.push(`exports/run-${ordinal}-csv_export`);
    required.push(`api-snapshots/run-${ordinal}-${expected.state === "cancelled" ? "cancelled-" : ""}report.json`);
    if (expected.state === "completed") required.push(`screenshots/run-${ordinal}-terminal-report.png`);
  }
  if (realBackendStage !== "small") {
    for (const width of [375, 768, 1024, 1440]) required.push(`screenshots/run-1-report-${width}.png`);
  }
  if (realBackendStage === "full") {
    required.push(
      "api-snapshots/sse-reload-replay.json",
      "api-snapshots/run-all-family-sequence.json",
      "api-snapshots/two-run-comparison.json",
      "api-snapshots/cancel-active-trial.json",
      "api-snapshots/run-7-cancel-response.json",
      "screenshots/cancel-active-trial.png",
      "screenshots/cancelled-cleanup-terminal.png",
      "screenshots/compatible-two-run-comparison.png",
    );
  }
  const missing = required.filter((path) => !byRelativePath.has(path));
  const empty = [];
  for (const path of required.filter((item) => byRelativePath.has(item))) {
    if ((await stat(byRelativePath.get(path))).size === 0) empty.push(path);
  }
  const trace_archives = [...byRelativePath.keys()].filter((path) => /(?:^|\/)trace\.zip$/.test(path)).sort();
  if (trace_archives.length === 0) missing.push("playwright-results/**/trace.zip");

  const invalid = [];
  const runIds = await optionalJson(join(evidenceRoot, "api-snapshots", "real-run-ids.json"));
  const browserSentinels = await optionalJson(join(evidenceRoot, "api-snapshots", "browser-sentinels.json"));
  for (const field of [
    "console_errors",
    "react_key_warnings",
    "network_failures",
    "page_errors",
    "required_request_failures",
    "service_worker_responses",
    "service_worker_urls",
  ]) {
    if (!Array.isArray(browserSentinels?.[field]) || browserSentinels[field].length !== 0) {
      invalid.push(`browser-sentinels.json ${field} must be an empty array`);
    }
  }
  if (browserSentinels?.run_create_count !== expectedStageRuns.length) {
    invalid.push(`browser-sentinels.json must prove exactly ${expectedStageRuns.length} browser-created ${realBackendStage}-stage runs`);
  }
  if (
    runIds?.schema_version !== 1
    || runIds?.stage !== realBackendStage
    || !Array.isArray(runIds?.runs)
    || runIds.runs.length !== expectedStageRuns.length
    || !runIds.runs.every((run, index) => {
      const expected = expectedStageRuns[index];
      return run?.ordinal === expected.ordinal
        && run?.role === expected.role
        && run?.scope === expected.scope
        && run?.state === expected.state
        && isDeepStrictEqual(run?.design_counts, expected.design_counts);
    })
    || !stageTopologyMatches(runIds, runIds.runs.map(({ run_id: runId }) => runId))
  ) {
    invalid.push(`real-run-ids.json does not retain the required ${realBackendStage} stage topology`);
  }
  const productionAssets = await optionalJson(join(evidenceRoot, "production-web-assets.json"));
  if (
    productionAssets?.schema_version !== 1
    || productionAssets?.root !== "<repo-root>/benchmark/web/dist"
    || !/^[a-f0-9]{64}$/.test(productionAssets?.digest ?? "")
    || !Array.isArray(productionAssets?.entries)
    || productionAssets.entry_count !== productionAssets.entries.length
    || productionAssets.entry_count < 2
    || !productionAssets.entries.some((entry) => entry?.path === "index.html" && entry?.type === "file")
    || !productionAssets.entries.some((entry) => entry?.type === "file" && /^assets\/.*\.js$/.test(entry?.path ?? ""))
    || productionAssets.entries.some((entry) => isAbsolute(entry?.path ?? "") || String(entry?.path ?? "").split("/").includes(".."))
  ) {
    invalid.push("production-web-assets.json does not identify the production-built index and JavaScript asset tree");
  }
  if (realBackendStage === "full") {
  const replay = await optionalJson(join(evidenceRoot, "api-snapshots", "sse-reload-replay.json"));
  const replayStart = replay?.disconnected_after_sequence;
  const replayLatest = replay?.persisted_latest_before_reconnect;
  const expectedReplay = Number.isSafeInteger(replayStart) && Number.isSafeInteger(replayLatest) && replayLatest > replayStart
    ? Array.from({ length: replayLatest - replayStart }, (_, index) => replayStart + index + 1)
    : [];
  const observedReplayThroughFirstLive = Array.isArray(replay?.observed_ui_sequence_ids)
    ? replay.observed_ui_sequence_ids.filter((sequence) => sequence > replayStart && sequence <= replay?.first_live_sequence_id)
    : [];
  if (
    replay?.schema_version !== 1
    || replay?.run_id !== runIds?.run_all
    || !Number.isSafeInteger(replayStart)
    || replayStart < 1
    || !Number.isSafeInteger(replay?.last_event_id_header)
    || replay.last_event_id_header !== replayStart
    || !Number.isSafeInteger(replayLatest)
    || replayLatest <= replayStart
    || !isDeepStrictEqual(replay?.expected_replayed_sequence_ids, expectedReplay)
    || !isDeepStrictEqual(replay?.persisted_artifact_sequence_ids, expectedReplay)
    || replay?.first_live_sequence_id !== replayLatest + 1
    || !isDeepStrictEqual(observedReplayThroughFirstLive, [...expectedReplay, replayLatest + 1])
    || !Number.isSafeInteger(replay?.replayed_event_count)
    || replay.replayed_event_count < expectedReplay.length
    || !Number.isSafeInteger(replay?.sequence_after_live)
    || replay.sequence_after_live < replay.first_live_sequence_id
    || replay?.exact_missed_sequence_match !== true
    || replay?.replay_before_live !== true
    || replay.response_status !== 200
    || !String(replay.response_content_type ?? "").includes("text/event-stream")
  ) {
    invalid.push("sse-reload-replay.json does not prove the exact persisted gap replayed before the first live SSE event");
  }
  const familySequence = await optionalJson(join(evidenceRoot, "api-snapshots", "run-all-family-sequence.json"));
  const expectedFamilyStates = ["command", "files", "workspace_lifecycle", "layerstack"].flatMap((family) => [
    { family, state: "preparing" },
    { family, state: "running" },
    { family, state: "completed" },
  ]);
  const observedFamilyStates = Array.isArray(familySequence?.observed)
    ? familySequence.observed.map(({ family, state }) => ({ family, state }))
    : [];
  const familySequences = Array.isArray(familySequence?.observed)
    ? familySequence.observed.map(({ sequence }) => sequence)
    : [];
  const familyOffsets = Array.isArray(familySequence?.observed)
    ? familySequence.observed.map(({ monotonic_offset_ns: offset }) => offset)
    : [];
  if (
    familySequence?.schema_version !== 1
    || familySequence?.run_id !== runIds?.run_all
    || !isDeepStrictEqual(familySequence?.expected, expectedFamilyStates)
    || !isDeepStrictEqual(observedFamilyStates, expectedFamilyStates)
    || !familySequences.every((sequence, index) => Number.isSafeInteger(sequence) && (index === 0 || sequence > familySequences[index - 1]))
    || !familyOffsets.every((offset, index) => Number.isSafeInteger(offset) && offset >= 0 && (index === 0 || offset >= familyOffsets[index - 1]))
    || !Array.isArray(familySequence?.boundaries)
    || familySequence.boundaries.length !== 3
    || !familySequence.boundaries.every(({ non_overlapping: nonOverlapping }) => nonOverlapping === true)
    || familySequence?.exact_sequence !== true
    || familySequence?.non_overlapping !== true
  ) {
    invalid.push("run-all-family-sequence.json does not prove exact sequential Command to Files to Workspace to LayerStack execution");
  }
  }
  const phaseProof = await optionalJson(join(evidenceRoot, "api-snapshots", "layerstack-phase-proof.json"));
  const storagePhaseIds = ["layerstack_storage_plan", "layerstack_flatten", "layerstack_commit"];
  const commonPhaseIds = ["layerstack_squash", ...storagePhaseIds, "layerstack_remount_sweep"];
  const renderedTraceCounts = {
    "layerstack.squash": 2,
    "layerstack.squash.plan": 2,
    "layerstack.squash.flatten": 2,
    "layerstack.squash.commit": 2,
    "layerstack.squash.remount_sweep": 2,
    "workspace_session.remount": 1,
  };
  const validPhaseCell = (reportCell, rawCell, liveSessions) => {
    const phaseIds = liveSessions === 0 ? commonPhaseIds : [...commonPhaseIds, "workspace_session_remount"];
    const summaries = new Map((reportCell?.phase_summaries ?? []).map((summary) => [summary?.id, summary]));
    return reportCell?.live_sessions === liveSessions
      && rawCell?.live_sessions === liveSessions
      && Array.isArray(reportCell?.phase_ids)
      && Array.isArray(reportCell?.storage_phase_ids)
      && isDeepStrictEqual([...reportCell.phase_ids].sort(), [...phaseIds].sort())
      && isDeepStrictEqual(reportCell.storage_phase_ids, storagePhaseIds)
      && reportCell.sweep_phase_id === "layerstack_remount_sweep"
      && reportCell.per_session_phase_id === (liveSessions === 1 ? "workspace_session_remount" : null)
      && phaseIds.every((id) => {
        const summary = summaries.get(id);
        return summary?.semantic_revision === 1
          && summary?.unit === "nanoseconds"
          && summary?.source === "product_trace"
          && summary?.correlation === "exact_request_trace_span"
          && summary?.attempted === 5
          && summary?.failed === 0
          && summary?.duration?.count === 5;
      })
      && phaseIds.every((id) => rawCell?.phase_counts?.[id] === 6)
      && (liveSessions === 1 || (rawCell?.phase_counts?.workspace_session_remount ?? 0) === 0);
  };
  const reportPhaseCells = Array.isArray(phaseProof?.report_cells)
    ? [...phaseProof.report_cells].sort((left, right) => left.live_sessions - right.live_sessions)
    : [];
  const rawPhaseCells = Array.isArray(phaseProof?.raw_observation_cells)
    ? [...phaseProof.raw_observation_cells].sort((left, right) => left.live_sessions - right.live_sessions)
    : [];
  if (
    phaseProof?.schema_version !== 1
    || phaseProof?.run_id !== runIds?.family_runs?.layerstack
    || phaseProof?.report_schema_version !== 4
    || phaseProof?.storage_and_remount_are_separate !== true
    || reportPhaseCells.length !== 2
    || rawPhaseCells.length !== 2
    || !validPhaseCell(reportPhaseCells[0], rawPhaseCells[0], 0)
    || !validPhaseCell(reportPhaseCells[1], rawPhaseCells[1], 1)
    || !isDeepStrictEqual(phaseProof?.rendered_trace_counts, renderedTraceCounts)
  ) {
    invalid.push("layerstack-phase-proof.json does not separately prove storage phases and workspace-session remount in report v4, raw observations, and UI");
  }
  if (realBackendStage === "full") {
  const cancellation = await optionalJson(join(evidenceRoot, "api-snapshots", "run-7-cancel-response.json"));
  if (
    cancellation?.schema_version !== 1
    || cancellation?.run_id !== runIds?.cancelled
    || cancellation?.state !== "cancelling"
    || cancellation?.cancellation_requested !== true
  ) {
    invalid.push("run-7-cancel-response.json does not prove accepted active cancellation");
  }
  const comparison = await optionalJson(join(evidenceRoot, "api-snapshots", "two-run-comparison.json"));
  if (
    comparison?.schema_version !== 1
    || comparison?.comparison_derivation_revision !== 3
    || comparison?.reference_run_id !== runIds?.comparison?.reference
    || comparison?.candidate_run_id !== runIds?.comparison?.candidate
    || comparison?.protocol?.declarations_compatible !== true
    || comparison?.compatible !== true
    || comparison?.descriptive_only !== false
    || comparison?.matched_cell_ids?.length !== 2
    || comparison?.matched_cells?.length !== 2
    || !Array.isArray(comparison?.deltas)
    || comparison.deltas.length === 0
  ) {
    invalid.push("two-run-comparison.json is not a compatible settled comparison of the two command-family Quick Smoke cells");
  }
  }

  const responsive_screenshots = [];
  for (const width of realBackendStage === "small" ? [] : [375, 768, 1024, 1440]) {
    const path = `screenshots/run-1-report-${width}.png`;
    const absolute = byRelativePath.get(path);
    if (!absolute) continue;
    const bytes = await readFile(absolute);
    const png = bytes.length >= 24 && bytes.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
    const actualWidth = png ? bytes.readUInt32BE(16) : null;
    const actualHeight = png ? bytes.readUInt32BE(20) : null;
    responsive_screenshots.push({ path, expected_width: width, actual_width: actualWidth, actual_height: actualHeight });
    if (actualWidth !== width || actualHeight === null || actualHeight < 1) {
      missing.push(`${path} with exact ${width}px viewport width`);
    }
  }

  return {
    schema_version: 1,
    complete_browser_run: exitCode === 0,
    required_files: required,
    missing,
    empty,
    invalid,
    trace_archives,
    responsive_screenshots,
    valid: exitCode === 0 && missing.length === 0 && empty.length === 0 && invalid.length === 0,
  };
}

async function writeEvidenceReport({
  url,
  initialStatus,
  finalStatus,
  exitCode,
  source,
  toolVersions,
  dockerEngine,
  imageIdentity,
  cleanup,
  outsideRootGuard,
  runEvidence,
  retainedEvidenceValidation,
  secretScan,
  gateEndedAt,
}) {
  const statusLines = source.status_porcelain.length === 0
    ? "    (clean)"
    : source.status_porcelain.map((line) => `    ${line}`).join("\n");
  const commandLines = evidenceCommands().map((record, index) => {
    return `${index + 1}. cwd ${record.cwd}\n\n        ${record.command}\n\n   Started ${record.started_at}; ended ${record.ended_at}; exit ${record.exit_code}; signal ${record.signal ?? "none"}.`;
  }).join("\n\n");
  const runLines = runEvidence.runs.length === 0
    ? "No run IDs were retained because the gate failed before run creation."
    : [
        "| Role | Scope | State | Run ID | Started | Ended | Plan hash |",
        "| --- | --- | --- | --- | --- | --- | --- |",
        ...runEvidence.runs.map((run) => `| ${run.role} | ${run.scope} | ${run.expected_state} | ${run.run_id} | ${run.started_at ?? "unavailable"} | ${run.ended_at ?? "unavailable"} | ${run.plan_hash ?? "unavailable"} |`),
      ].join("\n");
  const artifactHashRows = cleanup.retained_results.artifact_indexes.flatMap(({ run_id: runId, artifact_index: index }) => {
    if (!Array.isArray(index?.artifacts)) return [];
    return index.artifacts.map((artifact) => {
      const safeId = String(artifact.artifact_id).replaceAll("|", "\\|").replaceAll("\n", " ");
      const safeLabel = String(artifact.label).replaceAll("|", "\\|").replaceAll("\n", " ");
      return `| ${runId} | ${safeId} | ${safeLabel} | ${artifact.size_bytes} | ${artifact.sha256} |`;
    });
  });
  const artifactHashLines = artifactHashRows.length === 0
    ? "No product artifact hashes were retained because the gate failed before artifact inspection."
    : [
        "| Run ID | Artifact ID | Retained path | Bytes | SHA-256 |",
        "| --- | --- | --- | ---: | --- |",
        ...artifactHashRows,
      ].join("\n");
  const versionBlock = JSON.stringify(toolVersions, null, 2).split("\n").map((line) => `    ${line}`).join("\n");
  const dockerBlock = JSON.stringify({ engine: dockerEngine, image: imageIdentity }, null, 2).split("\n").map((line) => `    ${line}`).join("\n");
  const retainedFiles = (await listFiles(evidenceRoot))
    .map((path) => relative(evidenceRoot, path))
    .filter((path) => path !== "FINAL-EVIDENCE.md" && path !== "evidence-manifest.json")
    .sort();
  const retainedFileLinks = [
    ...retainedFiles.map((path) => `- [${path.replaceAll("[", "\\[").replaceAll("]", "\\]")}](<${path}>)`),
    "- [evidence-manifest.json](evidence-manifest.json)",
  ].join("\n");
  const finalReport = [
    "# EphemeralOS Benchmark Laboratory real-backend evidence",
    "",
    `- Gate start: ${gateStartedAt}`,
    `- Gate end: ${gateEndedAt}`,
    `- Validation stage: ${realBackendStage}`,
    `- Loopback origin: ${url ?? "not started"}`,
    "- Test workspace: <evidence-root>/test-workspace-root (redacted)",
    "- Product path: production web assets → real sandbox-benchmark → isolated gateway/daemon → Docker EphemeralOS backend",
    "- Production web asset identity: [production-web-assets.json](production-web-assets.json)",
    `- Playwright exit: ${exitCode}`,
    `- Initial/final admission ready: ${initialStatus?.execution_ready === true}/${finalStatus?.execution_ready === true}`,
    `- Final active run cleared: ${finalStatus?.active_run === null}`,
    `- Docker resources restored: ${cleanup.docker.restored}`,
    `- Session registry restored: ${cleanup.session_registry.restored}`,
    `- Container/cgroup baseline restored: ${cleanup.cgroup.restored}`,
    `- Execution scratch restored: ${cleanup.scratch.restored}`,
    `- Immutable result trees match allowlisted artifact indexes: ${cleanup.retained_results.valid}`,
    `- Repository content outside evidence root unchanged: ${outsideRootGuard.unchanged}`,
    `- Outside-workspace sentinel unchanged: ${outsideRootGuard.outside_workspace_sentinel.unchanged}`,
    `- Required screenshots, exports, logs, and trace retained: ${retainedEvidenceValidation.valid}`,
    `- Full retained-evidence secret scan passed: ${secretScan.passed}`,
    "",
    "## Exact release-gate commands",
    "",
    commandLines,
    "",
    "## Tool and browser versions",
    "",
    versionBlock,
    "",
    "## Source identity and dirty detail",
    "",
    `Commit ${source.commit}; branch ${source.branch}; dirty ${source.dirty}; status SHA-256 ${source.status_sha256}; tracked diff SHA-256 ${source.tracked_diff_sha256}.`,
    "",
    statusLines,
    "",
    "## Docker engine and EphemeralOS image identity",
    "",
    dockerBlock,
    "",
    "## Run timestamps and IDs",
    "",
    runLines,
    "",
    "## Product artifact hashes",
    "",
    artifactHashLines,
    "",
    "## Responsive screenshots",
    "",
    ...(realBackendStage === "small" ? ["- Responsive command-report screenshots are exercised from the medium stage onward."] : [
      "- [375 px](screenshots/run-1-report-375.png)",
      "- [768 px](screenshots/run-1-report-768.png)",
      "- [1024 px](screenshots/run-1-report-1024.png)",
      "- [1440 px](screenshots/run-1-report-1440.png)",
    ]),
    ...(realBackendStage === "full" ? [
      "- [Active-trial cancellation](screenshots/cancel-active-trial.png)",
      "- [Cancelled terminal cleanup](screenshots/cancelled-cleanup-terminal.png)",
      "- [Two-run comparison](screenshots/compatible-two-run-comparison.png)",
    ] : []),
    "",
    "## Retained evidence",
    "",
    retainedFileLinks,
    "",
    "The manifest hashes every retained file except itself. A request, cleanup, retained-result, outside-root, image-identity, or secret-scan failure makes this gate fail.",
    "",
  ].join("\n");
  const reportSecretHits = [];
  inspectSecretText(finalReport, "FINAL-EVIDENCE.md", "generated_buffer", reportSecretHits);
  if (reportSecretHits.length > 0) throw new Error("Generated final evidence report contains secret-like material");
  await writeFile(join(evidenceRoot, "FINAL-EVIDENCE.md"), finalReport);
  const files = (await listFiles(evidenceRoot)).filter((path) => !path.endsWith("evidence-manifest.json"));
  const entries = [];
  for (const path of files) entries.push({ path: relative(evidenceRoot, path), bytes: (await stat(path)).size, sha256: await hashFile(path) });
  const manifest = {
    schema_version: 3,
    validation_stage: realBackendStage,
    generated_at: new Date().toISOString(),
    gate_started_at: gateStartedAt,
    gate_ended_at: gateEndedAt,
    loopback_url: url,
    test_workspace_root: "<evidence-root>/test-workspace-root",
    source,
    commands: evidenceCommands(),
    tool_versions: toolVersions,
    docker_engine: dockerEngine,
    image_identity: imageIdentity,
    run_evidence: runEvidence,
    initial_health: initialStatus,
    final_health: finalStatus,
    playwright_exit_code: exitCode,
    cleanup,
    outside_root_guard: outsideRootGuard,
    retained_evidence_validation: retainedEvidenceValidation,
    secret_scan: secretScan,
    files: entries,
  };
  const manifestText = `${JSON.stringify(manifest, null, 2)}\n`;
  const manifestSecretHits = [];
  inspectSecretText(manifestText, "evidence-manifest.json", "generated_buffer", manifestSecretHits);
  if (manifestSecretHits.length > 0) throw new Error("Generated evidence manifest contains secret-like material");
  await writeFile(join(evidenceRoot, "evidence-manifest.json"), manifestText);
}

async function rejectMockedRealSuite() {
  const specPath = join(webRoot, "tests/browser/real-backend/benchmark-laboratory.spec.ts");
  const configPath = join(webRoot, "playwright.config.ts");
  const packagePath = join(webRoot, "package.json");
  const sources = [
    { label: "real-backend spec", text: await readFile(specPath, "utf8") },
    { label: "Playwright config", text: await readFile(configPath, "utf8") },
  ];
  const forbidden = [
    ["request or WebSocket interception", /\.(?:route|routeWebSocket)\s*\(/],
    ["HAR interception", /\brouteFromHAR\s*\(/],
    ["mock service worker", /\b(?:msw|setupWorker|setupServer)\b/],
    ["DOM or script injection", /\.(?:evaluate|evaluateAll|setContent|addInitScript|addScriptTag|addStyleTag)\s*\(/],
    ["direct browser HTTP client", /\b(?:fetch|XMLHttpRequest)\s*\(|\.(?:request)\.(?:get|post|put|patch|delete|fetch)\s*\(|\bAPIRequestContext\b/],
    ["custom Playwright fixture", /\btest\.extend\s*\(/],
    ["fake adapter", /fake[_ -]?adapter/i],
  ];
  const violations = sources.flatMap(({ label: sourceLabel, text }) =>
    forbidden
      .filter(([, pattern]) => pattern.test(text))
      .map(([mechanism]) => `${sourceLabel}: ${mechanism}`),
  );
  const allowedImports = new Set([
    "@playwright/test",
    "node:crypto",
    "node:fs/promises",
    "node:path",
  ]);
  for (const { label: sourceLabel, text } of sources) {
    for (const match of text.matchAll(/\bfrom\s+["']([^"']+)["']/g)) {
      if (!allowedImports.has(match[1])) violations.push(`${sourceLabel}: unapproved helper import ${match[1]}`);
    }
    if (/\bimport\s*\(/.test(text) || /\bimport\s+["']/.test(text)) {
      violations.push(`${sourceLabel}: dynamic or side-effect helper import`);
    }
  }
  const packageJson = JSON.parse(await readFile(packagePath, "utf8"));
  const packageNames = Object.keys({ ...packageJson.dependencies, ...packageJson.devDependencies });
  if (packageNames.some((name) => name === "msw" || name.startsWith("@mswjs/"))) {
    violations.push("package.json: mock-service-worker dependency");
  }
  const configSource = sources.find(({ label }) => label === "Playwright config")?.text ?? "";
  if (/\bglobalSetup\b|\bglobalTeardown\b/.test(configSource)) {
    violations.push("Playwright config: global setup/teardown can inject hidden test behavior");
  }
  if (violations.length > 0) throw new Error(`Real-backend suite contains forbidden test mechanisms: ${violations.join(", ")}`);
}

let source = await sourceIdentity();
let runner = null;
let exitCode = 1;
let initialHealth = null;
let finalHealth = null;
let dockerEngine = null;
let dockerBefore = null;
let dockerAfter = null;
let repositoryBefore = await contentSnapshot(repoRoot, excludedRepositoryPath);
let repositoryAfter = null;
let productionWebAssets = null;
let workspaceBaseline = null;
let workspaceFinal = null;
let registryBaseline = [];
let registryFinal = [];
let ownershipBaseline = [];
let ownershipFinal = [];
let gateError = null;
let toolVersions = { node: process.version };

function rememberFailure(error) {
  if (gateError === null) gateError = error instanceof Error ? error : new Error(String(error));
}

try {
  await rejectMockedRealSuite();
  dockerEngine = JSON.parse(capture("docker", ["version", "--format", "{{json .}}"]));
  capture("docker", ["pull", "ubuntu:24.04"], repoRoot, commandTimeouts.dockerPull);
  await run("npm", ["run", "build"], webRoot, process.env, commandTimeouts.build);
  const builtAssets = await contentSnapshot(join(webRoot, "dist"));
  productionWebAssets = {
    schema_version: 1,
    root: "<repo-root>/benchmark/web/dist",
    digest: builtAssets.digest,
    entry_count: builtAssets.entries.length,
    entries: builtAssets.entries,
  };
  await writeFile(join(evidenceRoot, "production-web-assets.json"), `${JSON.stringify(productionWebAssets, null, 2)}\n`);
  await run("cargo", ["build", "--release", "-p", "sandbox-benchmark", "-p", "sandbox-gateway", "-p", "sandbox-daemon"], repoRoot, process.env, commandTimeouts.build);
  toolVersions = {
    node: process.version,
    npm: capture("npm", ["--version"], webRoot),
    cargo: capture("cargo", ["--version"], repoRoot),
    rustc: capture("rustc", ["--version"], repoRoot),
    git: capture("git", ["--version"], repoRoot),
    playwright: capture(resolve(webRoot, "node_modules/.bin/playwright"), ["--version"], webRoot),
    unzip: capture("unzip", ["-v"]).split("\n", 1)[0],
    docker_client: dockerEngine.Client?.Version ?? null,
    docker_server: dockerEngine.Server?.Version ?? null,
    browser: null,
  };
  dockerBefore = dockerSnapshot();
  await writeFile(join(evidenceRoot, "source-identity.json"), `${JSON.stringify(source, null, 2)}\n`);
  await writeFile(join(evidenceRoot, "docker-before.json"), `${JSON.stringify(dockerBefore, null, 2)}\n`);

  const runsRoot = join(workspaceRoot, "benchmark", "runs");
  const resultsRoot = join(workspaceRoot, "benchmark", "results");
  const runtimeRoot = join(workspaceRoot, "benchmark", "runtime");
  workspaceBaseline = {
    captured_at: new Date().toISOString(),
    runs: await optionalContentSnapshot(runsRoot),
    results: await optionalContentSnapshot(resultsRoot),
    runtime: await optionalContentSnapshot(runtimeRoot),
  };
  registryBaseline = await snapshotNamedEntries(workspaceRoot, new Set(["registry.json"]));
  ownershipBaseline = await snapshotNamedEntries(workspaceRoot, new Set([".eos-benchmark-owned"]));
  await writeFile(join(evidenceRoot, "execution-workspace-baseline.json"), `${JSON.stringify({ ...workspaceBaseline, registry_files: registryBaseline, ownership_markers: ownershipBaseline }, null, 2)}\n`);

  const target = await cargoTargetDirectory();
  runner = await startRunner(join(target, "release", "sandbox-benchmark"));
  initialHealth = await fetchJson(`${runner.url}/api/v1/health`);
  await writeFile(join(evidenceRoot, "initial-health.json"), `${JSON.stringify(initialHealth, null, 2)}\n`);
  if (initialHealth.execution_ready !== true || initialHealth.active_run !== null) {
    throw new Error("Initial health did not prove an execution-ready runner with no active campaign");
  }

  await run(resolve(webRoot, "node_modules/.bin/playwright"), ["test", "--project=real-backend", "--trace=on"], webRoot, {
    ...process.env,
    BENCHMARK_REAL_BACKEND: "1",
    BENCHMARK_REAL_BACKEND_URL: runner.url,
    BENCHMARK_EVIDENCE_ROOT: evidenceRoot,
  }, commandTimeouts.browser, {
    path: inFlightProgressPath,
    stallTimeoutMs: commandTimeouts.browserStall,
  });
  exitCode = 0;
  finalHealth = await fetchJson(`${runner.url}/api/v1/health`);
  await writeFile(join(evidenceRoot, "final-health.json"), `${JSON.stringify(finalHealth, null, 2)}\n`);
  if (finalHealth.execution_ready !== true || finalHealth.active_run !== null) {
    throw new Error("Final health did not prove cleanup and admission recovery");
  }
} catch (error) {
  rememberFailure(error);
} finally {
  if (runner) {
    finalHealth = await fetchJson(`${runner.url}/api/v1/health`).catch(() => finalHealth);
    if (finalHealth) await writeFile(join(evidenceRoot, "final-health.json"), `${JSON.stringify(finalHealth, null, 2)}\n`);
    try {
      await stopRunner(runner);
    } catch (error) {
      rememberFailure(error);
    }
  }
  try {
    if (workspaceBaseline) {
      workspaceFinal = {
        captured_at: new Date().toISOString(),
        runs: await optionalContentSnapshot(join(workspaceRoot, "benchmark", "runs")),
        results: await optionalContentSnapshot(join(workspaceRoot, "benchmark", "results")),
        runtime: await optionalContentSnapshot(join(workspaceRoot, "benchmark", "runtime")),
      };
      registryFinal = await snapshotNamedEntries(workspaceRoot, new Set(["registry.json"]));
      ownershipFinal = await snapshotNamedEntries(workspaceRoot, new Set([".eos-benchmark-owned"]));
    }
    if (dockerBefore) dockerAfter = dockerSnapshot();
    if (repositoryBefore) repositoryAfter = await contentSnapshot(repoRoot, excludedRepositoryPath);
  } catch (error) {
    rememberFailure(error);
  }
}

let runEvidence = { run_ids: null, runs: [] };
try {
  runEvidence = await collectRunEvidence();
} catch (error) {
  rememberFailure(error);
}
let retainedResults = {
  schema_version: 1,
  baseline_empty: false,
  expected_run_ids: runEvidence.runs.map(({ run_id }) => run_id),
  actual_run_ids: [],
  changes: [],
  artifact_indexes: [],
  errors: ["Results baseline or final snapshot unavailable"],
  valid: false,
};
if (workspaceBaseline && workspaceFinal) {
  try {
    retainedResults = await validateRetainedResults(workspaceBaseline.results, workspaceFinal.results, runEvidence);
  } catch (error) {
    rememberFailure(error);
    retainedResults.errors.push(error instanceof Error ? error.message : String(error));
  }
}

const emptyDockerChanges = { containers: [], images: [], networks: [], volumes: [] };
const dockerChanges = dockerBefore && dockerAfter ? dockerDiff(dockerBefore, dockerAfter) : emptyDockerChanges;
const dockerRestored = dockerBefore !== null
  && dockerAfter !== null
  && Object.values(dockerChanges).every((changes) => changes.length === 0);
const runsChanges = workspaceBaseline && workspaceFinal ? snapshotDiff(workspaceBaseline.runs, workspaceFinal.runs) : [{ path: "baseline unavailable" }];
const runtimeChanges = workspaceBaseline && workspaceFinal ? snapshotDiff(workspaceBaseline.runtime, workspaceFinal.runtime) : [{ path: "baseline unavailable" }];
const registryRestored = workspaceBaseline !== null && JSON.stringify(registryBaseline) === JSON.stringify(registryFinal);
const ownershipRestored = workspaceBaseline !== null && JSON.stringify(ownershipBaseline) === JSON.stringify(ownershipFinal);
const scratchVolume = (name) => /(workspace|layer-stack|scratch|eos-benchmark)/i.test(name);
const scratchVolumesBefore = dockerBefore?.volumes.map(({ name }) => name).filter(scratchVolume) ?? [];
const scratchVolumesAfter = dockerAfter?.volumes.map(({ name }) => name).filter(scratchVolume) ?? [];
const cgroupHandles = (snapshot) => snapshot?.containers.map((container) => ({
  container_id: container.id,
  container_name: container.name,
  cgroup_parent: container.cgroup_parent,
  cgroup_namespace_mode: container.cgroup_namespace_mode,
  engine_pid: container.state.pid,
  running: container.state.running,
})) ?? [];
const cleanup = {
  schema_version: 1,
  checked_at: new Date().toISOString(),
  health: {
    execution_ready: finalHealth?.execution_ready === true,
    active_run_cleared: finalHealth?.active_run === null,
  },
  session_registry: {
    baseline: registryBaseline,
    final: registryFinal,
    restored: registryRestored,
  },
  ownership_markers: {
    baseline: ownershipBaseline,
    final: ownershipFinal,
    restored: ownershipRestored,
  },
  cgroup: {
    method: "Docker inspect container IDs, cgroup parents/namespaces, and engine PIDs are the owning lifecycle handles; exact restoration proves campaign cgroups were removed without changing unrelated handles.",
    baseline_handles: cgroupHandles(dockerBefore),
    final_handles: cgroupHandles(dockerAfter),
    restored: dockerBefore !== null && dockerAfter !== null && dockerChanges.containers.length === 0,
  },
  scratch: {
    run_root_changes: runsChanges,
    scratch_volumes_before: scratchVolumesBefore,
    scratch_volumes_after: scratchVolumesAfter,
    restored: runsChanges.length === 0 && JSON.stringify(scratchVolumesBefore) === JSON.stringify(scratchVolumesAfter),
  },
  runtime: {
    changes: runtimeChanges,
    restored: runtimeChanges.length === 0,
  },
  docker: {
    changes: dockerChanges,
    restored: dockerRestored,
  },
  retained_results: retainedResults,
};
cleanup.restored = cleanup.health.execution_ready
  && cleanup.health.active_run_cleared
  && cleanup.session_registry.restored
  && cleanup.ownership_markers.restored
  && cleanup.cgroup.restored
  && cleanup.scratch.restored
  && cleanup.runtime.restored
  && cleanup.docker.restored
  && cleanup.retained_results.valid;

const repositoryChanges = repositoryBefore && repositoryAfter ? snapshotDiff(repositoryBefore, repositoryAfter) : [{ path: "baseline unavailable" }];
const currentStatus = gitStatus();
const currentStatusLines = currentStatus === "" ? [] : currentStatus.trimEnd().split("\n");
const outsideWorkspaceSentinelAfter = await optionalFileIdentity(outsideWorkspaceSentinelPath);
const outsideWorkspaceSentinelUnchanged = JSON.stringify(outsideWorkspaceSentinelBefore) === JSON.stringify(outsideWorkspaceSentinelAfter);
const outsideRootGuard = {
  schema_version: 1,
  exclusions: repositorySnapshotExclusions,
  baseline_digest: repositoryBefore?.digest ?? null,
  baseline_entry_count: repositoryBefore?.entries.length ?? 0,
  final_digest: repositoryAfter?.digest ?? null,
  final_entry_count: repositoryAfter?.entries.length ?? 0,
  git_status_unchanged: JSON.stringify(source.status_porcelain) === JSON.stringify(currentStatusLines),
  outside_workspace_sentinel: {
    path: "<evidence-root>/outside-workspace-sentinel.txt",
    baseline: outsideWorkspaceSentinelBefore,
    final: outsideWorkspaceSentinelAfter,
    unchanged: outsideWorkspaceSentinelUnchanged,
  },
  unrelated_docker: {
    method: "Exact whole-engine container, image, network, and volume snapshot diff; any unrelated Docker change also fails the isolated gate.",
    changes: dockerChanges,
    unchanged: dockerRestored,
  },
  changes: repositoryChanges,
};
outsideRootGuard.unchanged = repositoryChanges.length === 0
  && outsideRootGuard.git_status_unchanged
  && outsideRootGuard.outside_workspace_sentinel.unchanged
  && outsideRootGuard.unrelated_docker.unchanged;

await writeFile(join(evidenceRoot, "docker-after.json"), `${JSON.stringify(dockerAfter, null, 2)}\n`);
await writeFile(join(evidenceRoot, "execution-workspace-final.json"), `${JSON.stringify({ ...workspaceFinal, registry_files: registryFinal, ownership_markers: ownershipFinal }, null, 2)}\n`);
await writeFile(join(evidenceRoot, "cleanup-validation.json"), `${JSON.stringify(cleanup, null, 2)}\n`);
await writeFile(join(evidenceRoot, "outside-root-guard.json"), `${JSON.stringify(outsideRootGuard, null, 2)}\n`);

const browserRuntime = await optionalJson(join(evidenceRoot, "api-snapshots", "browser-runtime.json"));
toolVersions.browser = browserRuntime;
await writeFile(join(evidenceRoot, "run-evidence.json"), `${JSON.stringify(runEvidence, null, 2)}\n`);
let imageIdentity = null;
try {
  imageIdentity = await collectImageIdentity();
} catch (error) {
  rememberFailure(error);
}
await writeFile(join(evidenceRoot, "image-identity.json"), `${JSON.stringify(imageIdentity, null, 2)}\n`);
let retainedEvidenceValidation;
try {
  retainedEvidenceValidation = await validateRequiredEvidenceFiles(exitCode);
} catch (error) {
  rememberFailure(error);
  retainedEvidenceValidation = {
    schema_version: 1,
    complete_browser_run: exitCode === 0,
    required_files: [],
    missing: ["Retained evidence validation did not complete"],
    empty: [],
    invalid: [],
    trace_archives: [],
    responsive_screenshots: [],
    valid: false,
  };
}
await writeFile(join(evidenceRoot, "retained-evidence-validation.json"), `${JSON.stringify(retainedEvidenceValidation, null, 2)}\n`);

if (!cleanup.restored) rememberFailure(new Error("Docker, cgroup, registry, scratch, ownership, runtime, health, or retained-result validation failed"));
if (!outsideRootGuard.unchanged) rememberFailure(new Error("The real-backend gate changed repository content, the outside-workspace sentinel, or the unrelated Docker baseline"));
if (exitCode === 0 && (runEvidence.runs.length !== expectedStageRuns.length || imageIdentity === null || !retainedEvidenceValidation.valid)) {
  rememberFailure(new Error(`Successful ${realBackendStage} browser execution did not retain its expected runs, image identity, production web identity, screenshots, exports, logs, and trace evidence`));
}

let secretScan;
try {
  secretScan = await scanRetainedEvidence();
} catch (error) {
  rememberFailure(error);
  secretScan = {
    schema_version: 1,
    started_at: new Date().toISOString(),
    ended_at: null,
    method: "Retained-evidence scan could not complete",
    rules: secretRules.map(([name]) => name),
    raw_files_scanned: 0,
    zip_archives_expanded: 0,
    bytes_scanned: 0,
    self_buffer_scanned: true,
    hits: [{ path: "<scan>", view: "scanner", rule: "scan_incomplete" }],
    passed: false,
  };
}
const executedCommandsPath = join(evidenceRoot, "executed-commands.json");
await writeFile(executedCommandsPath, `${JSON.stringify(evidenceCommands(), null, 2)}\n`);
try {
  await scanFileForSecrets(executedCommandsPath, secretScan);
} catch (error) {
  rememberFailure(error);
  secretScan.hits.push({ path: "executed-commands.json", view: "raw", rule: "scan_incomplete" });
}
secretScan.ended_at = new Date().toISOString();
secretScan.passed = secretScan.hits.length === 0;
let secretScanText = `${JSON.stringify(secretScan, null, 2)}\n`;
const selfHits = [];
inspectSecretText(secretScanText, "secret-scan.json", "generated_buffer", selfHits);
if (selfHits.length > 0) {
  secretScan.hits.push(...selfHits);
  secretScan.passed = false;
  secretScanText = `${JSON.stringify(secretScan, null, 2)}\n`;
}
await writeFile(join(evidenceRoot, "secret-scan.json"), secretScanText);
if (!secretScan.passed) {
  rememberFailure(new Error(`Secret scan failed: ${secretScan.hits.map(({ path, view, rule }) => `${path} (${view}/${rule})`).join(", ")}`));
}

const gateEndedAt = new Date().toISOString();
await writeEvidenceReport({
  url: runner?.url ?? null,
  initialStatus: initialHealth,
  finalStatus: finalHealth,
  exitCode,
  source,
  toolVersions,
  dockerEngine,
  imageIdentity,
  cleanup,
  outsideRootGuard,
  runEvidence,
  retainedEvidenceValidation,
  secretScan,
  gateEndedAt,
});

if (gateError) throw gateError;
