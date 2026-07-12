import { createHash } from "node:crypto";
import { appendFile, copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, test, type Page, type Request, type TestInfo } from "@playwright/test";

interface RequestLedgerEntry {
  method: string;
  path: string;
  resource_type: string;
  status: number | null;
  failed: boolean;
  failure_text: string | null;
  expected_navigation_abort: boolean;
  from_service_worker: boolean;
  content_type: string | null;
  has_origin: boolean;
  has_mutation_nonce: boolean;
  last_event_id: string | null;
  request_body_sha256: string | null;
}

interface BrowserSentinels {
  ledger: RequestLedgerEntry[];
  consoleErrors: string[];
  reactKeyWarnings: string[];
  networkFailures: string[];
  expectedEventStreamAborts: string[];
  expectedNavigationReadAborts: string[];
  pageErrors: string[];
  requiredRequestFailures: string[];
  serviceWorkerResponses: string[];
  serviceWorkerUrls: string[];
  runCreates: RequestLedgerEntry[];
  headerCaptures: Promise<void>[];
  allowEstablishedEventStreamAborts: () => void;
  allowInFlightReadAbortsForDocumentNavigation: () => string[];
}

const evidenceRoot = process.env.BENCHMARK_EVIDENCE_ROOT;
const progressStartedAt = process.hrtime.bigint();
let progressSequence = 0;
const fatalUiAlertPattern = /(?:Unsupported (?:family|operation definition)|Runner request failed|YAML cannot be parsed|Runner rejected this document|Run did not start|Validation response is empty|Operation definition missing|Definition catalog is empty|Default configuration unavailable|UI definition version mismatch|Plan has no operations|No enabled operations|No expanded cells|Correctness gates failed|Comparison failed|Export failed|Run id is required)/i;
const currentObservationSchemaVersion = 3;
const knownRunStates = new Set([
  "queued",
  "planned",
  "preparing",
  "running",
  "verifying",
  "tearing_down",
  "cancelling",
  "completed",
  "failed",
  "cancelled",
]);
const activeTrialRunStates = new Set(["running", "verifying", "tearing_down"]);

function apiPath(url: string): string {
  const parsed = new URL(url);
  return `${parsed.pathname}${parsed.search}`;
}

function bodyHash(request: Request): string | null {
  const body = request.postData();
  return body === null ? null : createHash("sha256").update(body).digest("hex");
}

function isRequiredApi(request: Request): boolean {
  return request.url().includes("/api/v1/");
}

function isEstablishedEventStream(entry: RequestLedgerEntry | undefined): boolean {
  return entry?.status === 200
    && entry.path.split("?", 1)[0].endsWith("/events")
    && entry.content_type?.includes("text/event-stream") === true;
}

function isEventStreamPath(path: string): boolean {
  return /^\/api\/v1\/runs\/[^/]+\/events$/.test(path.split("?", 1)[0]);
}

function isSafeNavigationRead(entry: RequestLedgerEntry | undefined): entry is RequestLedgerEntry {
  return (entry?.method === "GET" || entry?.method === "HEAD") && !isEventStreamPath(entry.path);
}

function requestFailureLabel(request: Request, failure: string): string {
  const url = new URL(request.url());
  return `${request.method()} ${url.origin}${url.pathname}: ${failure}`;
}

type ProgressDetail = string | number | boolean | null;

async function recordProgress(
  checkpoint: string,
  detail: Record<string, ProgressDetail> = {},
): Promise<void> {
  const record = {
    schema_version: 1,
    sequence: ++progressSequence,
    recorded_at: new Date().toISOString(),
    monotonic_offset_ns: Number((process.hrtime.bigint() - progressStartedAt) / 1_000_000n) * 1_000_000,
    stage: process.env.BENCHMARK_REAL_BACKEND_STAGE ?? "full",
    checkpoint,
    detail,
  };
  const line = `${JSON.stringify(record)}\n`;
  // This is deliberately emitted as well as retained. The node harness tails
  // the retained file, so an operator sees the exact browser checkpoint even
  // when the Playwright reporter is buffering its own output.
  console.log(`[benchmark-progress] ${line.trimEnd()}`);
  if (evidenceRoot) await appendFile(join(evidenceRoot, "inflight-progress.ndjson"), line);
}

async function failForVisibleUiAlert(
  page: Page,
  testInfo: TestInfo,
  checkpoint: string,
): Promise<never> {
  const alerts = (await page.getByRole("alert").allTextContents())
    .map((text) => text.replace(/\s+/g, " ").trim())
    .filter((text) => fatalUiAlertPattern.test(text));
  const capturedAt = new Date().toISOString();
  await retainJson(testInfo, "fatal-ui-alert.json", {
    schema_version: 1,
    captured_at: capturedAt,
    checkpoint,
    route: new URL(page.url()).pathname,
    alerts,
  });
  await retainScreenshot(page, testInfo, "fatal-ui-alert.png");
  await recordProgress("fatal-ui-alert", { checkpoint, alert_count: alerts.length });
  throw new Error(`Browser displayed a fatal UI alert during ${checkpoint}: ${alerts.join(" | ") || "unnamed alert"}`);
}

async function awaitResponseOrVisibleUiAlert<T>(
  page: Page,
  testInfo: TestInfo,
  response: Promise<T>,
  checkpoint: string,
): Promise<T> {
  let settled = false;
  const alert = page.getByRole("alert").filter({ hasText: fatalUiAlertPattern }).first()
    .waitFor({ state: "visible", timeout: 90_000 })
    .then(async () => {
      if (settled) return await new Promise<never>(() => {});
      return await failForVisibleUiAlert(page, testInfo, checkpoint);
    })
    .catch((error: Error) => {
      // A validation response controls the same 90-second bound. Once that
      // response wins the race, a no-alert timeout must not become a detached
      // rejection. Any other locator failure remains a test failure.
      if (/Timeout/.test(error.message)) return new Promise<never>(() => {});
      throw error;
    });
  try {
    return await Promise.race([response, alert]);
  } finally {
    settled = true;
  }
}

function installSentinels(page: Page): BrowserSentinels {
  const sentinels: BrowserSentinels = {
    ledger: [],
    consoleErrors: [],
    reactKeyWarnings: [],
    networkFailures: [],
    expectedEventStreamAborts: [],
    expectedNavigationReadAborts: [],
    pageErrors: [],
    requiredRequestFailures: [],
    serviceWorkerResponses: [],
    serviceWorkerUrls: page.context().serviceWorkers().map((worker) => worker.url()),
    runCreates: [],
    headerCaptures: [],
    allowEstablishedEventStreamAborts: () => {},
    allowInFlightReadAbortsForDocumentNavigation: () => [],
  };
  const byRequest = new Map<Request, RequestLedgerEntry>();
  const activeEventStreams = new Set<Request>();
  const activeApiReads = new Map<Request, RequestLedgerEntry>();
  const allowedEventStreamAborts = new WeakSet<Request>();
  const allowedNavigationReadAborts = new WeakSet<Request>();
  sentinels.allowEstablishedEventStreamAborts = () => {
    for (const request of activeEventStreams) allowedEventStreamAborts.add(request);
  };
  sentinels.allowInFlightReadAbortsForDocumentNavigation = () => {
    const allowed: string[] = [];
    for (const [request, entry] of activeApiReads) {
      // An explicit document navigation legitimately cancels a request already
      // owned by the outgoing page. Only safe reads which were active before
      // that navigation receive this one-shot allowance; mutations and every
      // unrelated network failure remain fatal.
      if (!isSafeNavigationRead(entry)) continue;
      allowedNavigationReadAborts.add(request);
      allowed.push(`${entry.method} ${entry.path}`);
    }
    return allowed;
  };

  page.context().on("serviceworker", (worker) => sentinels.serviceWorkerUrls.push(worker.url()));
  page.on("console", (message) => {
    if (message.type() === "error") sentinels.consoleErrors.push(message.text());
    if (/each child in a list should have a unique ["']key["'] prop/i.test(message.text())) {
      sentinels.reactKeyWarnings.push(message.text());
    }
  });
  page.on("pageerror", (error) => sentinels.pageErrors.push(error.message));
  page.on("request", (request) => {
    if (!request.url().includes("/api/v1/")) return;
    const entry: RequestLedgerEntry = {
      method: request.method(),
      path: apiPath(request.url()),
      resource_type: request.resourceType(),
      status: null,
      failed: false,
      failure_text: null,
      expected_navigation_abort: false,
      from_service_worker: false,
      content_type: null,
      has_origin: false,
      has_mutation_nonce: false,
      last_event_id: null,
      request_body_sha256: bodyHash(request),
    };
    sentinels.ledger.push(entry);
    byRequest.set(request, entry);
    sentinels.headerCaptures.push(request.allHeaders().then((headers) => {
      entry.has_origin = Boolean(headers.origin);
      entry.has_mutation_nonce = Boolean(headers["x-eos-benchmark-nonce"]);
      entry.last_event_id = headers["last-event-id"] ?? null;
    }));
    if (isEventStreamPath(entry.path)) activeEventStreams.add(request);
    if (isSafeNavigationRead(entry)) activeApiReads.set(request, entry);
    if (request.method() === "POST" && apiPath(request.url()) === "/api/v1/runs") {
      sentinels.runCreates.push(entry);
    }
  });
  page.on("requestfinished", (request) => {
    activeEventStreams.delete(request);
    activeApiReads.delete(request);
  });
  page.on("requestfailed", (request) => {
    const failure = request.failure()?.errorText ?? "unknown request failure";
    const entry = byRequest.get(request);
    if (entry) {
      entry.failed = true;
      entry.failure_text = failure;
    }
    const establishedEventStreamAbort = isEstablishedEventStream(entry) && failure === "net::ERR_ABORTED";
    const allowedEventAbort = establishedEventStreamAbort || allowedEventStreamAborts.has(request) && isEstablishedEventStream(entry);
    const allowedNavigationReadAbort = allowedNavigationReadAborts.has(request)
      && isSafeNavigationRead(entry)
      && failure === "net::ERR_ABORTED";
    const allowedAbort = allowedEventAbort || allowedNavigationReadAbort;
    if (entry) entry.expected_navigation_abort = allowedAbort;
    if (establishedEventStreamAbort) sentinels.expectedEventStreamAborts.push(requestFailureLabel(request, failure));
    if (allowedNavigationReadAbort) sentinels.expectedNavigationReadAborts.push(requestFailureLabel(request, failure));
    if (!allowedAbort) sentinels.networkFailures.push(requestFailureLabel(request, failure));
    if (isRequiredApi(request) && !allowedAbort) {
      sentinels.requiredRequestFailures.push(`${apiPath(request.url())}: ${failure}`);
    }
    activeEventStreams.delete(request);
    activeApiReads.delete(request);
  });
  page.on("response", (response) => {
    const entry = byRequest.get(response.request());
    if (entry) {
      entry.status = response.status();
      entry.from_service_worker = response.fromServiceWorker();
      entry.content_type = response.headers()["content-type"] ?? null;
    }
    if (response.fromServiceWorker()) sentinels.serviceWorkerResponses.push(apiPath(response.url()));
    if (isRequiredApi(response.request()) && response.status() >= 400) {
      sentinels.requiredRequestFailures.push(`${apiPath(response.url())}: HTTP ${response.status()}`);
    }
  });
  return sentinels;
}

type DocumentNavigationWaitState = "domcontentloaded" | "networkidle";

async function gotoWithSentinels(
  page: Page,
  sentinels: BrowserSentinels,
  target: string,
  checkpoint: string,
  waitUntil: DocumentNavigationWaitState,
): Promise<void> {
  // This records the precise lifecycle allowance before it is used. The
  // evidence must show a real navigation caused a read abort; it is never a
  // blanket exemption for the remainder of a browser test.
  sentinels.allowEstablishedEventStreamAborts();
  const inFlightSafeReads = sentinels.allowInFlightReadAbortsForDocumentNavigation();
  await recordProgress("document-navigation-started", {
    checkpoint,
    target,
    in_flight_safe_read_count: inFlightSafeReads.length,
  });
  await page.goto(target, { waitUntil });
  await recordProgress("document-navigation-completed", { checkpoint, target });
}

async function retainJson(testInfo: TestInfo, name: string, value: unknown): Promise<void> {
  const formatted = `${JSON.stringify(value, null, 2)}\n`;
  const path = testInfo.outputPath(name);
  await writeFile(path, formatted);
  await testInfo.attach(name, { path, contentType: "application/json" });
  if (evidenceRoot) {
    const apiRoot = join(evidenceRoot, "api-snapshots");
    await mkdir(apiRoot, { recursive: true });
    await writeFile(join(apiRoot, name), formatted);
  }
}

async function retainScreenshot(page: Page, testInfo: TestInfo, name: string): Promise<void> {
  const path = testInfo.outputPath(name);
  await page.screenshot({ path, fullPage: true, animations: "disabled" });
  await testInfo.attach(name, { path, contentType: "image/png" });
  if (evidenceRoot) {
    const screenshotRoot = join(evidenceRoot, "screenshots");
    await mkdir(screenshotRoot, { recursive: true });
    await copyFile(path, join(screenshotRoot, name));
  }
}

interface ArtifactIndexEntry {
  artifact_id: string;
  label: string;
  media_type: string;
  size_bytes: number;
  sha256: string;
}

interface ArtifactIndexResponse {
  schema_version: number;
  run_id: string;
  artifacts: ArtifactIndexEntry[];
}

interface ArtifactContentResponse extends ArtifactIndexEntry {
  schema_version: number;
  encoding: "utf-8" | "base64";
  content: string;
}

interface RunResponse {
  manifest: { run_id: string; state: string };
  progress: {
    completed_trial_batches: number;
    total_trial_batches: number;
    current_family: string | null;
    current_operation: string | null;
    current_cell_id: string | null;
    current_trial_id: string | null;
    trial_kind: string | null;
    phase: string | null;
  };
  latest_sequence: number;
  report_ready: boolean;
}

interface SchemaEnvelope<T> {
  schema_name: string;
  schema_version: number;
  data: T;
}

interface ExpandedQuickSmokeCell {
  cell_id: string;
  family_id: string;
  operation_id: string;
  protocol: {
    warmups: number;
    measured_trials: number;
  };
  operation: {
    operation: string;
    cell: Record<string, unknown>;
  };
}

interface ExpandedQuickSmokePlan {
  schema_version: number;
  runnable: boolean;
  is_customized: boolean;
  plan_hash: string;
  effective_environment: { gateway_mode: string };
  fixed_lifecycle_policy: {
    automatic_retries: number;
    one_active_campaign: boolean;
    sequential_families: boolean;
  };
  cells: ExpandedQuickSmokeCell[];
  execution_blocks: { family_id: string; cell_ids: string[] }[];
  estimates: {
    cell_count: number;
    trial_batch_count: number;
    issued_operation_request_count: number;
  };
  validation: { severity: string; code: string }[];
}

interface RunManifestArtifact {
  run_id: string;
  plan_hash: string;
  starting_preset: { id: string; version: number } | null;
  state: string;
  started_at: string | null;
  ended_at: string | null;
  definition_snapshot: { sha256: string };
  fixed_lifecycle_policy: ExpandedQuickSmokePlan["fixed_lifecycle_policy"];
  gateway_policy: {
    mode: string;
    loopback_only: boolean;
    isolated_runtime_per_execution_block: boolean;
  };
}

interface EnvironmentMetadataArtifact {
  schema_version: number;
  host: { monotonic_clock: string };
  image_reference: string;
  image_digest: string | null;
  client_cohort: string;
  gateway_endpoint_identity: string;
}

interface FailureCounts {
  total_attempted: number;
  warmup: number;
  measured_attempted: number;
  successful: number;
  product_failed: number;
  correctness_failed: number;
  infrastructure_failed: number;
  cleanup_invalid: number;
  missing_primary_latency: number;
}

interface OperationEvidenceEntry {
  trial_id: string;
  request_id: string | null;
  evidence: {
    operation: string;
    evidence: Record<string, unknown>;
  };
}

interface QuickSmokePhaseSummary {
  id: string;
  label: string;
  help: string;
  semantic_revision: number;
  unit: string;
  source: string;
  correlation: string;
  trace_span_name: string;
  attempted: number;
  failed: number;
  duration: { schema_version: number; count: number; median: number | null; p95: number | null };
}

interface QuickSmokeReportCell {
  cell_id: string;
  family_id: string;
  family_label: string;
  operation_id: string;
  counts: FailureCounts;
  phases: QuickSmokePhaseSummary[];
  operation_evidence: OperationEvidenceEntry[];
}

interface QuickSmokeReport {
  schema_version: 4;
  report_derivation_revision: number;
  run_id: string;
  state: string;
  provisional: boolean;
  correctness_verdict: string;
  plan_hash: string;
  definition_snapshot_sha256: string;
  started_at: string | null;
  ended_at: string | null;
  design_counts: {
    test_combinations: number;
    trial_batches: number;
    issued_product_requests: number;
  };
  cells: QuickSmokeReportCell[];
}

const expectedArtifactIds = [
  "run_manifest",
  "intent_plan",
  "expanded_plan",
  "definition_snapshot",
  "environment_metadata",
  "events",
  "observations",
  "summary",
  "report",
  "json_export",
  "csv_export",
] as const;

const layerstackPhaseSpans = new Map([
  ["layerstack_squash", "layerstack.squash"],
  ["layerstack_storage_plan", "layerstack.squash.plan"],
  ["layerstack_flatten", "layerstack.squash.flatten"],
  ["layerstack_commit", "layerstack.squash.commit"],
  ["layerstack_remount_sweep", "layerstack.squash.remount_sweep"],
  ["workspace_session_remount", "workspace_session.remount"],
]);

type QuickSmokeScope = "command" | "files" | "workspace" | "layerstack" | "all";

interface QuickSmokeExpectation {
  ordinal: number;
  role: string;
  route: string;
  scope: QuickSmokeScope;
  familyIds: string[];
  familyLabels: string[];
  operationCells: Record<string, number>;
  requestCounts: Record<string, number>;
  estimates: {
    cell_count: number;
    trial_batch_count: number;
    issued_operation_request_count: number;
  };
  startingPreset: { id: string; version: number } | null;
}

interface PlanDocument {
  name: string;
  configuration_base: { id: string; version: number; scope: QuickSmokeScope };
  protocol: {
    trial_defaults: {
      fast: { warmups: number; measured_trials: number };
      destructive: { warmups: number; measured_trials: number };
    };
  };
  operations: {
    operation: string;
    configuration: {
      enabled: boolean;
      factors: Record<string, unknown>;
    };
  }[];
  [key: string]: unknown;
}

const quickSmokeExpectations = {
  commandReference: {
    ordinal: 1,
    role: "command_reference",
    route: "/benchmark/command",
    scope: "command",
    familyIds: ["command"],
    familyLabels: ["Command"],
    operationCells: { exec_command: 2 },
    requestCounts: { exec_command: 36 },
    estimates: { cell_count: 2, trial_batch_count: 12, issued_operation_request_count: 36 },
    startingPreset: null,
  },
  commandCandidate: {
    ordinal: 2,
    role: "command_candidate",
    route: "/benchmark/command",
    scope: "command",
    familyIds: ["command"],
    familyLabels: ["Command"],
    operationCells: { exec_command: 2 },
    requestCounts: { exec_command: 36 },
    estimates: { cell_count: 2, trial_batch_count: 12, issued_operation_request_count: 36 },
    startingPreset: null,
  },
  files: {
    ordinal: 3,
    role: "files",
    route: "/benchmark/files",
    scope: "files",
    familyIds: ["files"],
    familyLabels: ["File Operations"],
    operationCells: { file_read: 1, file_write: 1 },
    requestCounts: { file_read: 6, file_write: 6 },
    estimates: { cell_count: 2, trial_batch_count: 12, issued_operation_request_count: 12 },
    startingPreset: null,
  },
  workspace: {
    ordinal: 4,
    role: "workspace",
    route: "/benchmark/workspace",
    scope: "workspace",
    familyIds: ["workspace_lifecycle"],
    familyLabels: ["Workspace Lifecycle"],
    operationCells: { create_workspace: 2 },
    requestCounts: { create_workspace: 36 },
    estimates: { cell_count: 2, trial_batch_count: 12, issued_operation_request_count: 36 },
    startingPreset: null,
  },
  layerstack: {
    ordinal: 5,
    role: "layerstack",
    route: "/benchmark/layerstack",
    scope: "layerstack",
    familyIds: ["layer_stack"],
    familyLabels: ["LayerStack"],
    operationCells: { squash_layerstack: 2 },
    requestCounts: { squash_layerstack: 12 },
    estimates: { cell_count: 2, trial_batch_count: 12, issued_operation_request_count: 12 },
    startingPreset: null,
  },
  runAll: {
    ordinal: 6,
    role: "run_all",
    route: "/benchmark",
    scope: "all",
    familyIds: ["command", "files", "workspace_lifecycle", "layer_stack"],
    familyLabels: ["Command", "File Operations", "Workspace Lifecycle", "LayerStack"],
    operationCells: {
      exec_command: 2,
      file_read: 1,
      file_write: 1,
      create_workspace: 2,
      squash_layerstack: 2,
    },
    requestCounts: {
      exec_command: 36,
      file_read: 6,
      file_write: 6,
      create_workspace: 36,
      squash_layerstack: 12,
    },
    estimates: { cell_count: 8, trial_batch_count: 48, issued_operation_request_count: 96 },
    startingPreset: { id: "quick-smoke", version: 1 },
  },
  cancelled: {
    ordinal: 7,
    role: "cancelled_run_all",
    route: "/benchmark",
    scope: "all",
    familyIds: ["command", "files", "workspace_lifecycle", "layer_stack"],
    familyLabels: ["Command", "File Operations", "Workspace Lifecycle", "LayerStack"],
    operationCells: {
      exec_command: 2,
      file_read: 1,
      file_write: 1,
      create_workspace: 2,
      squash_layerstack: 2,
    },
    requestCounts: {
      exec_command: 36,
      file_read: 6,
      file_write: 6,
      create_workspace: 36,
      squash_layerstack: 12,
    },
    estimates: { cell_count: 8, trial_batch_count: 48, issued_operation_request_count: 96 },
    startingPreset: { id: "quick-smoke", version: 1 },
  },
} as const satisfies Record<string, QuickSmokeExpectation>;

const completedExpectations = [
  quickSmokeExpectations.commandReference,
  quickSmokeExpectations.commandCandidate,
  quickSmokeExpectations.files,
  quickSmokeExpectations.workspace,
  quickSmokeExpectations.layerstack,
  quickSmokeExpectations.runAll,
];

type RealBackendStage = "small" | "medium" | "full";

function realBackendStage(): RealBackendStage {
  const stage = process.env.BENCHMARK_REAL_BACKEND_STAGE ?? "full";
  if (stage === "small" || stage === "medium" || stage === "full") return stage;
  throw new Error(`BENCHMARK_REAL_BACKEND_STAGE must be small, medium, or full; received ${stage}`);
}

function stageRunIdentity(expectation: QuickSmokeExpectation, runId: string, state: "completed" | "cancelled" = "completed") {
  return {
    ordinal: expectation.ordinal,
    role: expectation.role,
    scope: expectation.scope,
    run_id: runId,
    state,
    design_counts: {
      test_combinations: expectation.estimates.cell_count,
      trial_batches: expectation.estimates.trial_batch_count,
      issued_product_requests: expectation.estimates.issued_operation_request_count,
    },
  };
}

async function assertStageSentinels(
  sentinels: BrowserSentinels,
  expectedRunCreates: number,
  requireReplay: boolean,
): Promise<void> {
  await Promise.all(sentinels.headerCaptures);
  expect(sentinels.runCreates).toHaveLength(expectedRunCreates);
  expect(sentinels.runCreates.every(({ has_origin, has_mutation_nonce }) => has_origin && has_mutation_nonce)).toBe(true);
  const mutations = sentinels.ledger.filter(({ method }) => method !== "GET" && method !== "HEAD");
  expect(mutations.length).toBeGreaterThan(expectedRunCreates);
  expect(mutations.every(({ has_origin, has_mutation_nonce }) => has_origin && has_mutation_nonce)).toBe(true);
  const eventStreams = sentinels.ledger.filter(({ path }) => isEventStreamPath(path));
  expect(eventStreams.length).toBeGreaterThanOrEqual(expectedRunCreates);
  expect(eventStreams.every(({ status }) => status === 200)).toBe(true);
  expect(eventStreams.every(({ content_type }) => content_type?.includes("text/event-stream") === true)).toBe(true);
  expect(sentinels.ledger.filter(({ failed }) => failed).every(({ expected_navigation_abort }) => expected_navigation_abort)).toBe(true);
  if (requireReplay) expect(eventStreams.some(({ last_event_id }) => Number(last_event_id) > 0)).toBe(true);
  expect(sentinels.ledger.every(({ status, expected_navigation_abort }) =>
    expected_navigation_abort || (status !== null && status >= 200 && status < 300)
  )).toBe(true);
  expect(sentinels.consoleErrors).toEqual([]);
  expect(sentinels.reactKeyWarnings).toEqual([]);
  expect(sentinels.networkFailures).toEqual([]);
  expect(sentinels.pageErrors).toEqual([]);
  expect(sentinels.requiredRequestFailures).toEqual([]);
  expect(sentinels.serviceWorkerResponses).toEqual([]);
  expect(sentinels.serviceWorkerUrls).toEqual([]);
}

function artifactBytes(artifact: ArtifactContentResponse): Buffer {
  return artifact.encoding === "utf-8"
    ? Buffer.from(artifact.content, "utf8")
    : Buffer.from(artifact.content, "base64");
}

function validateArtifactSyntax(artifact: ArtifactContentResponse, requireTabularData: boolean): void {
  if (artifact.media_type === "application/json") {
    expect(() => JSON.parse(artifact.content)).not.toThrow();
    return;
  }
  if (artifact.media_type.includes("ndjson")) {
    const lines = artifact.content.split("\n").filter((line) => line.length > 0);
    expect(lines.length).toBeGreaterThan(0);
    for (const line of lines) expect(() => JSON.parse(line)).not.toThrow();
    return;
  }
  if (artifact.media_type.includes("csv")) {
    const lines = artifact.content.trimEnd().split("\n");
    expect(lines.length).toBeGreaterThan(requireTabularData ? 1 : 0);
    expect(lines[0]).toContain(",");
  }
}

async function assertProductionBootstrap(page: Page, sentinels: BrowserSentinels, testInfo: TestInfo): Promise<void> {
  const healthResponse = page.waitForResponse((response) => apiPath(response.url()) === "/api/v1/health" && response.request().method() === "GET");
  await gotoWithSentinels(page, sentinels, "/benchmark", "production-bootstrap", "networkidle");
  const health = await (await healthResponse).json() as {
    execution_ready: boolean;
    active_run: unknown;
    checks: { id: string; status: string; message: string }[];
  };
  await retainJson(testInfo, "browser-initial-health.json", health);
  expect(health.execution_ready).toBe(true);
  expect(health.active_run).toBeNull();
  expect(health.checks.find(({ id }) => id === "execution_backend")?.status).toBe("pass");
  await expect(page.getByRole("heading", { name: "Plan a local, reproducible benchmark" })).toBeVisible();
  await expect(page.locator('meta[name="eos-benchmark-nonce"]')).toHaveAttribute("content", /^[a-f0-9]{32,}$/);

  const scriptNodes = await page.locator("script[src]").all();
  const scripts = await Promise.all(scriptNodes.map(async (node) => await node.getAttribute("src") ?? ""));
  expect(scripts.length).toBeGreaterThan(0);
  expect(scripts.every((source) => /\/assets\/.+-[A-Za-z0-9_-]{6,}\.js$/.test(source))).toBe(true);
  expect(scripts.some((source) => source.includes("/@vite/client"))).toBe(false);
}

async function chooseQuickSmoke(page: Page): Promise<void> {
  await page.getByRole("combobox", { name: "Preset" }).click();
  await page.getByRole("option", { name: /quick-smoke · v1/i }).click();
  await page.getByRole("button", { name: "Load preset" }).click();
  await expect(page.getByText("Preset quick-smoke/v1", { exact: true })).toBeVisible();
  await expect(page.getByText("Runner-authored canonical estimate", { exact: true })).toBeVisible();
  await expect(page.getByText("Enabled families execute sequentially: Command → Files → Workspace → LayerStack.", { exact: true })).toBeVisible();
}

type PlanValidationEvidence = ExpandedQuickSmokePlan & { canonical_plan: PlanDocument };

function canonicalFamilyQuickSmokePlan(canonicalPlan: PlanDocument, scope: Exclude<QuickSmokeScope, "all">): PlanDocument {
  const plan = structuredClone(canonicalPlan);
  expect(plan.configuration_base.scope).toBe(scope);
  plan.name = `quick-smoke-${scope}`;
  plan.protocol.trial_defaults.fast = { warmups: 1, measured_trials: 5 };
  plan.protocol.trial_defaults.destructive = { warmups: 1, measured_trials: 5 };

  for (const operation of plan.operations) {
    switch (operation.operation) {
      case "exec_command":
        operation.configuration.enabled = true;
        operation.configuration.factors = {
          concurrent_requests: { role: "varied", values: [1, 5], control: 1 },
          workspace_profile: { role: "controlled", values: ["small"] },
          session_mode: { role: "controlled", values: ["explicit"] },
          command_case: { role: "controlled", values: ["noop"] },
        };
        break;
      case "file_read":
        operation.configuration.enabled = true;
        operation.configuration.factors = {
          concurrent_requests: { role: "controlled", values: [1] },
          returned_bytes: { role: "controlled", values: [4096] },
          source: { role: "controlled", values: ["snapshot"] },
          target_mode: { role: "controlled", values: ["independent"] },
        };
        break;
      case "file_write":
        operation.configuration.enabled = true;
        operation.configuration.factors = {
          concurrent_requests: { role: "controlled", values: [1] },
          content_bytes: { role: "controlled", values: [4096] },
          destination: { role: "controlled", values: ["session"] },
          target_mode: { role: "controlled", values: ["independent"] },
        };
        break;
      case "file_edit":
      case "file_blame":
        operation.configuration.enabled = false;
        break;
      case "create_workspace":
        operation.configuration.enabled = true;
        operation.configuration.factors = {
          workspace_count: { role: "varied", values: [1, 5], control: 1 },
          workspace_profile: { role: "controlled", values: ["small"] },
          network_profile: { role: "controlled", values: ["shared"] },
        };
        break;
      case "squash_layerstack":
        operation.configuration.enabled = true;
        operation.configuration.factors = {
          live_sessions: { role: "varied", values: [0, 1], control: 0 },
          requested_migration_ratio: { role: "controlled", values: [1] },
          remount_parallelism: { role: "controlled", values: [4] },
          squashable_blocks: { role: "controlled", values: [1] },
          layers_per_block: { role: "controlled", values: [8] },
          payload_bytes: { role: "controlled", values: [4096] },
          session_activity: { role: "controlled", values: ["idle"] },
        };
        break;
      default:
        throw new Error(`Unexpected operation in ${scope} default: ${operation.operation}`);
    }
  }
  return plan;
}

function assertCanonicalReview(validation: PlanValidationEvidence, expectation: QuickSmokeExpectation): void {
  expect(validation.schema_version).toBe(1);
  expect(validation.runnable).toBe(true);
  expect(validation.plan_hash).toMatch(/^sha256:[a-f0-9]{64}$/);
  expect(validation.estimates).toMatchObject(expectation.estimates);
  expect(validation.cells).toHaveLength(expectation.estimates.cell_count);
  expect(validation.execution_blocks.map(({ family_id }) => family_id)).toEqual(expectation.familyIds);
  expect(validation.fixed_lifecycle_policy).toMatchObject({
    automatic_retries: 0,
    one_active_campaign: true,
    sequential_families: true,
  });
  expect(validation.canonical_plan.configuration_base.scope).toBe(expectation.scope);
  const operationCells = validation.cells.reduce<Record<string, number>>((counts, cell) => {
    counts[cell.operation_id] = (counts[cell.operation_id] ?? 0) + 1;
    return counts;
  }, {});
  expect(operationCells).toEqual(expectation.operationCells);
}

async function reviewAndStartQuickSmoke(
  page: Page,
  testInfo: TestInfo,
  expectation: QuickSmokeExpectation,
): Promise<string> {
  await recordProgress("review-requested", {
    ordinal: expectation.ordinal,
    scope: expectation.scope,
  });
  const reviewed = page.waitForResponse(
    (response) => apiPath(response.url()) === "/api/v1/plans/validate" && response.request().method() === "POST",
    { timeout: 90_000 },
  );
  await page.getByRole("button", { name: /Review (?:customized|default) run/ }).click();
  const reviewResponse = await awaitResponseOrVisibleUiAlert(page, testInfo, reviewed, "review-plan");
  expect(reviewResponse.status()).toBe(200);
  const validation = await reviewResponse.json() as PlanValidationEvidence;
  await retainJson(testInfo, `run-${expectation.ordinal}-review-validation.json`, validation);
  assertCanonicalReview(validation, expectation);
  const dialog = page.getByRole("dialog", { name: "Review exact local benchmark run" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByTestId("review-plan-hash")).toHaveText(validation.plan_hash);
  await expect(dialog.getByText("Test combinations", { exact: true })).toBeVisible();
  await expect(dialog.getByText("Trial batches", { exact: true })).toBeVisible();
  await expect(dialog.getByText("Issued product requests", { exact: true })).toBeVisible();
  await expect(dialog.getByText(new RegExp(`${expectation.familyIds.length} sequential execution block\\(s\\)`))).toBeVisible();

  await recordProgress("run-create-requested", {
    ordinal: expectation.ordinal,
    scope: expectation.scope,
  });
  const created = page.waitForResponse(
    (response) => apiPath(response.url()) === "/api/v1/runs" && response.request().method() === "POST",
    { timeout: 90_000 },
  );
  await dialog.getByRole("button", { name: "Start local run" }).click();
  const response = await awaitResponseOrVisibleUiAlert(page, testInfo, created, "create-run");
  expect(response.status()).toBe(202);
  const createRequest = response.request().postDataJSON() as {
    plan: unknown;
    plan_hash: string;
    client_request_id: string;
    starting_preset: { id: string; version: number } | null;
  };
  await retainJson(testInfo, `run-${expectation.ordinal}-start-request.json`, createRequest);
  expect(createRequest.plan).toEqual(validation.canonical_plan);
  expect(createRequest.plan_hash).toBe(validation.plan_hash);
  expect(createRequest.client_request_id).toMatch(
    /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
  );
  expect(createRequest.starting_preset).toEqual(expectation.startingPreset);
  const body = await response.json() as { schema_version: number; run_id: string; state: string };
  await retainJson(testInfo, `run-${expectation.ordinal}-create.json`, body);
  expect(body).toMatchObject({ schema_version: 1, state: "queued" });
  expect(body.run_id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  await expect(page).toHaveURL(new RegExp(`/benchmark/runs/${body.run_id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`));
  await recordProgress("run-created", {
    ordinal: expectation.ordinal,
    scope: expectation.scope,
    run_id: body.run_id,
  });
  return body.run_id;
}

async function startFamilyQuickSmoke(
  page: Page,
  sentinels: BrowserSentinels,
  testInfo: TestInfo,
  expectation: QuickSmokeExpectation & { scope: Exclude<QuickSmokeScope, "all"> },
): Promise<string> {
  await recordProgress("family-navigation-started", {
    ordinal: expectation.ordinal,
    scope: expectation.scope,
  });
  const initialValidation = page.waitForResponse(
    (response) => apiPath(response.url()) === "/api/v1/plans/validate" && response.request().method() === "POST",
    { timeout: 90_000 },
  );
  await gotoWithSentinels(page, sentinels, expectation.route, `family-${expectation.ordinal}`, "domcontentloaded");
  const initialResponse = await awaitResponseOrVisibleUiAlert(
    page,
    testInfo,
    initialValidation,
    "load-family-definition",
  );
  expect(initialResponse.status()).toBe(200);
  const initial = await initialResponse.json() as PlanValidationEvidence;
  expect(initial.canonical_plan.configuration_base.scope).toBe(expectation.scope);
  await recordProgress("family-default-validation-received", {
    ordinal: expectation.ordinal,
    scope: expectation.scope,
  });
  await page.waitForLoadState("networkidle");

  const quickSmokePlan = canonicalFamilyQuickSmokePlan(initial.canonical_plan, expectation.scope);
  await page.getByRole("button", { name: "Inspect configuration YAML" }).click();
  const drawer = page.getByRole("dialog", { name: "Inspect configuration YAML" });
  await expect(drawer).toBeVisible();
  await drawer.getByLabel("YAML to validate and import").fill(JSON.stringify(quickSmokePlan, null, 2));
  const importedResponse = page.waitForResponse(
    (response) => apiPath(response.url()) === "/api/v1/plans/validate" && response.request().method() === "POST",
    { timeout: 90_000 },
  );
  await drawer.getByRole("button", { name: "Validate & import" }).click();
  const imported = await awaitResponseOrVisibleUiAlert(page, testInfo, importedResponse, "import-family-plan");
  expect(imported.status()).toBe(200);
  const importedValidation = await imported.json() as PlanValidationEvidence;
  assertCanonicalReview(importedValidation, expectation);
  await recordProgress("family-plan-imported", {
    ordinal: expectation.ordinal,
    scope: expectation.scope,
  });
  await page.keyboard.press("Escape");
  await expect(drawer).toBeHidden();
  await expect(page.getByRole("button", { name: "Review customized run" })).toBeVisible();
  return await reviewAndStartQuickSmoke(page, testInfo, expectation);
}

async function startRunAllQuickSmoke(
  page: Page,
  sentinels: BrowserSentinels,
  testInfo: TestInfo,
  expectation: QuickSmokeExpectation & { scope: "all" },
): Promise<string> {
  await recordProgress("run-all-navigation-started", { ordinal: expectation.ordinal });
  await gotoWithSentinels(page, sentinels, expectation.route, `run-all-${expectation.ordinal}`, "networkidle");
  await chooseQuickSmoke(page);
  await recordProgress("run-all-preset-loaded", { ordinal: expectation.ordinal });
  return await reviewAndStartQuickSmoke(page, testInfo, expectation);
}

async function waitForCompletedRun(page: Page, testInfo: TestInfo, runId: string): Promise<void> {
  const timeout = 15 * 60_000;
  const deadline = Date.now() + timeout;
  let lastStatus = "";
  let lastHeartbeatAt = 0;
  await recordProgress("run-awaiting-terminal-state", { run_id: runId, timeout_ms: timeout });
  while (Date.now() < deadline) {
    const alerts = (await page.getByRole("alert").allTextContents()).filter((text) => fatalUiAlertPattern.test(text));
    if (alerts.length > 0) await failForVisibleUiAlert(page, testInfo, "wait-for-run-terminal-state");
    const status = (await page.getByRole("status").allTextContents())
      .map((text) => text.replace(/\s+/g, " ").trim())
      .find((text) => /Run (?:queued|planned|preparing|running|verifying|tearing down|completed|failed|cancelled)/i.test(text)) ?? "awaiting run status";
    const now = Date.now();
    if (status !== lastStatus || now - lastHeartbeatAt >= 15_000) {
      await recordProgress("run-status", { run_id: runId, status });
      lastStatus = status;
      lastHeartbeatAt = now;
    }
    if (/Run completed/i.test(status)) break;
    if (/Run (?:failed|cancelled)/i.test(status)) {
      throw new Error(`Run ${runId} terminated as ${status}; a completed Quick Smoke was required`);
    }
    await page.waitForTimeout(1_000);
  }
  if (!/Run completed/i.test(lastStatus)) {
    throw new Error(`Run ${runId} did not reach completed state within ${timeout} ms`);
  }
  await recordProgress("run-completed", { run_id: runId });
  await expect(page.getByRole("link", { name: "Open report" })).toBeVisible();
  await expect(page.getByText(runId, { exact: true })).toBeVisible();
}

async function currentSseSequence(page: Page): Promise<number> {
  const text = await page.getByText(/Reload recovery resumes from SSE sequence/).textContent();
  return Number((text?.match(/SSE sequence ([0-9,]+)/)?.[1] ?? "0").replaceAll(",", ""));
}

async function currentReplayedCount(page: Page): Promise<number> {
  const text = await page.getByText(/Reload recovery resumes from SSE sequence/).textContent();
  return Number((text?.match(/; ([0-9,]+) record\(s\) were replayed/)?.[1] ?? "0").replaceAll(",", ""));
}

async function inspectAllowlistedArtifacts(
  page: Page,
  testInfo: TestInfo,
  runId: string,
  ordinal: number,
  requireCompleteEvidence: boolean,
): Promise<Map<string, ArtifactContentResponse>> {
  const indexResponsePromise = page.waitForResponse((response) =>
    apiPath(response.url()) === `/api/v1/runs/${runId}/artifacts` && response.request().method() === "GET",
  );
  await page.getByRole("tab", { name: "Methods & data" }).click();
  await expect(page.getByRole("heading", { name: "Allowlisted artifacts & export" })).toBeVisible();
  const indexResponse = await indexResponsePromise;
  expect(indexResponse.status()).toBe(200);
  const index = await indexResponse.json() as ArtifactIndexResponse;
  await recordProgress("artifact-index-received", {
    ordinal,
    run_id: runId,
    artifact_count: index.artifacts.length,
    require_complete_evidence: requireCompleteEvidence,
  });
  await retainJson(testInfo, `run-${ordinal}-artifact-index.json`, index);
  expect(index.schema_version).toBe(1);
  expect(index.run_id).toBe(runId);
  const indexedIds = index.artifacts.map(({ artifact_id }) => artifact_id);
  expect(new Set(indexedIds).size).toBe(indexedIds.length);
  for (const expectedId of expectedArtifactIds) expect(indexedIds).toContain(expectedId);
  const boundedEvidenceIds = indexedIds.filter((id) => !expectedArtifactIds.includes(id as typeof expectedArtifactIds[number]));
  expect(boundedEvidenceIds.every((id) => /^bounded_evidence_[a-f0-9]{64}$/.test(id))).toBe(true);
  if (requireCompleteEvidence) expect(boundedEvidenceIds.length).toBeGreaterThan(0);
  for (const entry of index.artifacts) {
    expect(entry.label.length).toBeGreaterThan(0);
    expect(entry.size_bytes).toBeGreaterThanOrEqual(0);
    expect(entry.sha256).toMatch(/^sha256:[a-f0-9]{64}$/);
    expect(["application/json", "application/x-ndjson", "text/csv; charset=utf-8"]).toContain(entry.media_type);
  }

  const contents = new Map<string, ArtifactContentResponse>();
  for (const [artifactIndex, entry] of index.artifacts.entries()) {
    await recordProgress("artifact-inspection-item-started", {
      ordinal,
      run_id: runId,
      artifact_id: entry.artifact_id,
      artifact_position: artifactIndex + 1,
      artifact_count: index.artifacts.length,
      size_bytes: entry.size_bytes,
    });
    const row = page.getByRole("row").filter({ has: page.getByText(entry.artifact_id, { exact: true }) });
    await expect(row).toHaveCount(1);
    const contentResponsePromise = page.waitForResponse((response) =>
      apiPath(response.url()) === `/api/v1/runs/${runId}/artifacts/${entry.artifact_id}`
        && response.request().method() === "GET",
    );
    await row.getByRole("button", { name: "Inspect" }).click();
    const contentResponse = await contentResponsePromise;
    expect(contentResponse.status()).toBe(200);
    const content = await contentResponse.json() as ArtifactContentResponse;
    const bytes = artifactBytes(content);
    expect(content.schema_version).toBe(1);
    expect(content.artifact_id).toBe(entry.artifact_id);
    expect(content.label).toBe(entry.label);
    expect(content.media_type).toBe(entry.media_type);
    expect(content.encoding).toBe("utf-8");
    expect(content.size_bytes).toBe(entry.size_bytes);
    expect(content.sha256).toBe(entry.sha256);
    expect(bytes.byteLength).toBe(entry.size_bytes);
    expect(`sha256:${createHash("sha256").update(bytes).digest("hex")}`).toBe(entry.sha256);
    validateArtifactSyntax(content, requireCompleteEvidence);
    contents.set(entry.artifact_id, content);
    await retainJson(testInfo, `run-${ordinal}-artifact-${entry.artifact_id}.json`, content);

    if (entry.artifact_id === "json_export" || entry.artifact_id === "csv_export") {
      const downloadPromise = page.waitForEvent("download");
      await page.getByRole("button", { name: "Download" }).click();
      const download = await downloadPromise;
      expect(download.suggestedFilename()).toBe(entry.artifact_id);
      const downloadedPath = testInfo.outputPath(`run-${ordinal}-${entry.artifact_id}-download`);
      await download.saveAs(downloadedPath);
      const downloaded = await readFile(downloadedPath);
      expect(downloaded.equals(bytes)).toBe(true);
      expect(`sha256:${createHash("sha256").update(downloaded).digest("hex")}`).toBe(entry.sha256);
      await testInfo.attach(`run-${ordinal}-${entry.artifact_id}-download`, {
        path: downloadedPath,
        contentType: entry.media_type,
      });
      if (evidenceRoot) {
        const exportRoot = join(evidenceRoot, "exports");
        await mkdir(exportRoot, { recursive: true });
        await copyFile(downloadedPath, join(exportRoot, `run-${ordinal}-${entry.artifact_id}`));
      }
    }
  }
  await recordProgress("artifact-inspection-completed", {
    ordinal,
    run_id: runId,
    artifact_count: contents.size,
    require_complete_evidence: requireCompleteEvidence,
  });
  return contents;
}

function parseJsonEnvelope<T>(
  artifacts: Map<string, ArtifactContentResponse>,
  artifactId: string,
  schemaName: string,
  expectedSchemaVersion = 1,
): T {
  const artifact = artifacts.get(artifactId);
  if (!artifact) throw new Error(`Required artifact ${artifactId} was not returned`);
  expect(artifact.media_type).toBe("application/json");
  expect(artifact.encoding).toBe("utf-8");
  const envelope = JSON.parse(artifact.content) as SchemaEnvelope<T>;
  expect(envelope.schema_name).toBe(schemaName);
  expect(envelope.schema_version).toBe(expectedSchemaVersion);
  return envelope.data;
}

function requiredNumber(value: unknown, field: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new Error(`${field} is not a finite number`);
  }
  return value;
}

function parseObservationRecords(artifacts: Map<string, ArtifactContentResponse>): {
  sequence: number;
  record: { record: string; data: Record<string, unknown> };
}[] {
  const artifact = artifacts.get("observations");
  if (!artifact) throw new Error("Required artifact observations was not returned");
  expect(artifact.media_type).toBe("application/x-ndjson");
  const lines = artifact.content.split("\n").filter((line) => line.length > 0);
  const records = lines.map((line) => {
    const envelope = JSON.parse(line) as SchemaEnvelope<{
      sequence: number;
      record: { record: string; data: Record<string, unknown> };
    }>;
    expect(envelope.schema_name).toBe("eos_benchmark_observation");
    expect(envelope.schema_version).toBe(currentObservationSchemaVersion);
    return envelope.data;
  });
  expect(records.map(({ sequence }) => sequence)).toEqual(
    Array.from({ length: records.length }, (_, index) => index + 1),
  );
  return records;
}

function parseEventRecords(artifacts: Map<string, ArtifactContentResponse>): {
  sequence: number;
  run_id: string;
  monotonic_offset_ns: number;
  data: Record<string, unknown>;
}[] {
  const artifact = artifacts.get("events");
  if (!artifact) throw new Error("Required artifact events was not returned");
  expect(artifact.media_type).toBe("application/x-ndjson");
  const records = artifact.content.split("\n").filter(Boolean).map((line) => {
    const envelope = JSON.parse(line) as SchemaEnvelope<{
      sequence: number;
      run_id: string;
      monotonic_offset_ns: number;
      data: Record<string, unknown>;
    }>;
    expect(envelope.schema_name).toBe("eos_benchmark_event");
    expect(envelope.schema_version).toBe(1);
    return envelope.data;
  });
  expect(records.map(({ sequence }) => sequence)).toEqual(
    Array.from({ length: records.length }, (_, index) => index + 1),
  );
  return records;
}

function assertAvailability(value: unknown, field: string): void {
  if (typeof value !== "object" || value === null) throw new Error(`${field} is not availability-tagged`);
  expect(["available", "unavailable"]).toContain((value as { availability?: unknown }).availability);
}

function assertStorageSnapshot(value: unknown, field: string): void {
  if (typeof value !== "object" || value === null) throw new Error(`${field} is not a storage snapshot`);
  const snapshot = value as Record<string, unknown>;
  expect(typeof snapshot.sampled).toBe("boolean");
  for (const counter of [
    "monotonic_offset_ns",
    "manifest_version",
    "root_hash",
    "active_layer_count",
    "active_lease_count",
    "active_logical_bytes",
    "active_allocated_bytes",
    "storage_logical_bytes",
    "storage_allocated_bytes",
    "staging_entry_count",
  ]) assertAvailability(snapshot[counter], `${field}.${counter}`);
}

function quickSmokeRequestCount(cell: ExpandedQuickSmokeCell): number {
  switch (cell.operation_id) {
    case "exec_command":
    case "file_read":
    case "file_write":
      return requiredNumber(cell.operation.cell.concurrent_requests, `${cell.cell_id}.concurrent_requests`);
    case "create_workspace":
      return requiredNumber(cell.operation.cell.workspace_count, `${cell.cell_id}.workspace_count`);
    case "squash_layerstack":
      return 1;
    default:
      throw new Error(`Unexpected Quick Smoke operation ${cell.operation_id}`);
  }
}

function assertQuickSmokeSemantics(
  report: QuickSmokeReport,
  artifacts: Map<string, ArtifactContentResponse>,
  runId: string,
  expectation: QuickSmokeExpectation,
): void {
  const expanded = parseJsonEnvelope<ExpandedQuickSmokePlan>(
    artifacts,
    "expanded_plan",
    "eos_benchmark_expanded_plan",
  );
  const manifest = parseJsonEnvelope<RunManifestArtifact>(
    artifacts,
    "run_manifest",
    "eos_benchmark_run_manifest",
  );
  const environment = parseJsonEnvelope<EnvironmentMetadataArtifact>(
    artifacts,
    "environment_metadata",
    "eos_benchmark_environment_metadata",
  );
  const persistedReport = parseJsonEnvelope<QuickSmokeReport>(
    artifacts,
    "report",
    "eos_benchmark_report",
    4,
  );
  const definitionSnapshot = artifacts.get("definition_snapshot");
  if (!definitionSnapshot) throw new Error("Required artifact definition_snapshot was not returned");

  expect(persistedReport).toEqual(report);

  expect(expanded.schema_version).toBe(1);
  expect(expanded.runnable).toBe(true);
  expect(expanded.is_customized).toBe(true);
  expect(expanded.plan_hash).toMatch(/^sha256:[a-f0-9]{64}$/);
  expect(expanded.effective_environment.gateway_mode).toBe("isolated");
  expect(expanded.fixed_lifecycle_policy).toMatchObject({
    automatic_retries: 0,
    one_active_campaign: true,
    sequential_families: true,
  });
  expect(expanded.validation.filter(({ severity }) => severity === "error")).toEqual([]);
  expect(expanded.estimates).toMatchObject(expectation.estimates);
  expect(expanded.cells).toHaveLength(expectation.estimates.cell_count);
  expect(expanded.execution_blocks.map(({ family_id }) => family_id)).toEqual(expectation.familyIds);
  const expandedCellIds = expanded.cells.map(({ cell_id }) => cell_id);
  expect(new Set(expandedCellIds).size).toBe(expectation.estimates.cell_count);
  expect(new Set(expanded.execution_blocks.flatMap(({ cell_ids }) => cell_ids))).toEqual(new Set(expandedCellIds));

  const cellsByOperation = new Map<string, ExpandedQuickSmokeCell[]>();
  for (const cell of expanded.cells) {
    expect(cell.operation.operation).toBe(cell.operation_id);
    expect(cell.protocol.warmups).toBe(1);
    expect(cell.protocol.measured_trials).toBe(5);
    cellsByOperation.set(cell.operation_id, [...(cellsByOperation.get(cell.operation_id) ?? []), cell]);
  }
  expect(Object.fromEntries([...cellsByOperation].map(([id, cells]) => [id, cells.length]))).toEqual(
    expectation.operationCells,
  );

  const execCells = cellsByOperation.get("exec_command") ?? [];
  if (execCells.length > 0) {
    expect(execCells.map(({ operation }) => operation.cell.concurrent_requests).sort()).toEqual([1, 5]);
  }
  for (const { operation } of execCells) {
    expect(operation.cell).toMatchObject({
      workspace_profile: "small",
      session_mode: "explicit",
      command_case: "noop",
      resolved_isolation: "reusable_verified_fixture",
    });
  }
  if (cellsByOperation.has("file_read")) {
    expect(cellsByOperation.get("file_read")?.[0]?.operation.cell).toMatchObject({
      concurrent_requests: 1,
      returned_bytes: 4096,
      source: "snapshot",
      target_mode: "independent",
      resolved_isolation: "reusable_verified_fixture",
    });
  }
  if (cellsByOperation.has("file_write")) {
    expect(cellsByOperation.get("file_write")?.[0]?.operation.cell).toMatchObject({
      concurrent_requests: 1,
      content_bytes: 4096,
      destination: "session",
      target_mode: "independent",
      resolved_isolation: "fresh_sessions_per_trial",
    });
  }
  const workspaceCells = cellsByOperation.get("create_workspace") ?? [];
  if (workspaceCells.length > 0) {
    expect(workspaceCells.map(({ operation }) => operation.cell.workspace_count).sort()).toEqual([1, 5]);
  }
  for (const { operation } of workspaceCells) {
    expect(operation.cell).toMatchObject({
      workspace_profile: "small",
      network_profile: "shared",
      resolved_isolation: "prepared_sandbox_per_cell",
    });
  }
  const layerstackCells = cellsByOperation.get("squash_layerstack") ?? [];
  if (layerstackCells.length > 0) {
    expect(layerstackCells.map(({ operation }) => operation.cell.live_sessions).sort()).toEqual([0, 1]);
  }
  for (const { operation } of layerstackCells) {
    expect(operation.cell).toMatchObject({
      requested_migration_ratio: 1,
      remount_parallelism: 4,
      squashable_blocks: 1,
      layers_per_block: 8,
      payload_bytes: 4096,
      session_activity: "idle",
      resolved_isolation: "fresh_topology_per_trial",
    });
  }

  expect(manifest).toMatchObject({
    run_id: runId,
    plan_hash: expanded.plan_hash,
    starting_preset: expectation.startingPreset,
    state: "completed",
    fixed_lifecycle_policy: expanded.fixed_lifecycle_policy,
    gateway_policy: {
      mode: "isolated",
      loopback_only: true,
      isolated_runtime_per_execution_block: true,
    },
  });
  expect(manifest.started_at).toMatch(/^\d{4}-\d{2}-\d{2}T/);
  expect(manifest.ended_at).toMatch(/^\d{4}-\d{2}-\d{2}T/);
  expect(manifest.definition_snapshot.sha256).toBe(definitionSnapshot.sha256);
  expect(environment).toMatchObject({
    schema_version: 1,
    client_cohort: "direct_client",
    gateway_endpoint_identity: "isolated_loopback_per_execution_block",
    host: { monotonic_clock: "std::time::Instant" },
  });
  expect(environment.image_reference.length).toBeGreaterThan(0);
  expect(environment.image_digest).toMatch(/^sha256:[a-f0-9]{64}$/);
  expect(report).toMatchObject({
    schema_version: 4,
    report_derivation_revision: 3,
    run_id: runId,
    state: "completed",
    provisional: false,
    correctness_verdict: "pass",
    plan_hash: expanded.plan_hash,
    definition_snapshot_sha256: definitionSnapshot.sha256,
    design_counts: {
      test_combinations: expectation.estimates.cell_count,
      trial_batches: expectation.estimates.trial_batch_count,
      issued_product_requests: expectation.estimates.issued_operation_request_count,
    },
  });
  expect(report.started_at).toBe(manifest.started_at);
  expect(report.ended_at).toBe(manifest.ended_at);
  expect(report.cells).toHaveLength(expectation.estimates.cell_count);

  const expandedById = new Map(expanded.cells.map((cell) => [cell.cell_id, cell]));
  for (const cell of report.cells) {
    const planned = expandedById.get(cell.cell_id);
    if (!planned) throw new Error(`Report contains unknown cell ${cell.cell_id}`);
    expect(cell.family_id).toBe(planned.family_id);
    expect(cell.operation_id).toBe(planned.operation_id);
    expect(cell.counts).toEqual({
      total_attempted: 6,
      warmup: 1,
      measured_attempted: 5,
      successful: 5,
      product_failed: 0,
      correctness_failed: 0,
      infrastructure_failed: 0,
      cleanup_invalid: 0,
      missing_primary_latency: 0,
    });
    expect(cell.operation_evidence).toHaveLength(5);
    expect(new Set(cell.operation_evidence.map(({ trial_id }) => trial_id)).size).toBe(5);

    for (const entry of cell.operation_evidence) {
      expect(entry.evidence.operation).toBe(cell.operation_id);
      if (cell.operation_id === "exec_command") {
        expect(entry.request_id).toBeNull();
        const evidence = entry.evidence.evidence;
        expect(evidence).toMatchObject({
          command_case: planned.operation.cell.command_case,
          template_revision: planned.operation.cell.template_revision,
          command_sha256: planned.operation.cell.command_sha256,
          exit_code: planned.operation.cell.expected_exit_code,
          stdout: {
            byte_count: 0,
            truncated: false,
            sha256: "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
          },
          stderr: {
            byte_count: 0,
            truncated: false,
            sha256: "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
          },
        });
      } else if (cell.operation_id === "file_read") {
        expect(entry.request_id).toBeNull();
        const evidence = entry.evidence.evidence;
        expect(evidence).toMatchObject({ requested_bytes: 4096, returned_bytes: 4096 });
        expect(requiredNumber(evidence.returned_lines, "file_read.returned_lines")).toBeGreaterThan(0);
        expect(evidence.content_sha256).toMatch(/^sha256:[a-f0-9]{64}$/);
      } else if (cell.operation_id === "file_write") {
        expect(entry.request_id).toBeNull();
        const evidence = entry.evidence.evidence;
        expect(evidence).toMatchObject({
          requested_bytes: 4096,
          observed_bytes: 4096,
          attribution: "workspace_session",
          attributed_layer_count: 0,
        });
        expect(evidence.expected_sha256).toMatch(/^sha256:[a-f0-9]{64}$/);
        expect(evidence.observed_sha256).toBe(evidence.expected_sha256);
      } else if (cell.operation_id === "create_workspace") {
        expect(entry.request_id).toBeNull();
        const count = requiredNumber(planned.operation.cell.workspace_count, `${cell.cell_id}.workspace_count`);
        expect(entry.evidence.evidence).toMatchObject({
          requested_count: count,
          created_count: count,
          ready_count: count,
          destroyed_count: count,
          network_profile_matches: count,
          registry_baseline_restored: true,
        });
      } else if (cell.operation_id === "squash_layerstack") {
        expect(entry.request_id).toBe("squash-layerstack-0");
        const load = requiredNumber(planned.operation.cell.live_sessions, `${cell.cell_id}.live_sessions`);
        const evidence = entry.evidence.evidence;
        const dispositions = evidence.dispositions as Record<string, unknown>;
        const migrated = requiredNumber(dispositions.migrated, "dispositions.migrated");
        const identity = requiredNumber(dispositions.identity, "dispositions.identity");
        const leased = requiredNumber(dispositions.leased, "dispositions.leased");
        const faulty = requiredNumber(dispositions.faulty, "dispositions.faulty");
        const gone = requiredNumber(dispositions.session_gone, "dispositions.session_gone");
        expect(migrated + identity + leased + faulty + gone).toBe(load);
        expect(evidence).toMatchObject({
          requested_live_sessions: load,
          observed_migrated_sessions: migrated,
          observed_non_migrated_sessions: identity + leased + faulty + gone,
          effective_remount_parallelism: 4,
          observed_squashed_block_count: 1,
          observed_replaced_layer_count: 8,
          manifest_reduced: true,
          content_equivalent: true,
          usable_session_count: migrated + identity + leased,
        });
        assertAvailability(evidence.reclaimed_bytes, "reclaimed_bytes");
        assertStorageSnapshot(evidence.s0_baseline, "s0_baseline");
        assertStorageSnapshot(evidence.s1_sampled_peak, "s1_sampled_peak");
        assertStorageSnapshot(evidence.s2_post_commit, "s2_post_commit");
        assertStorageSnapshot(evidence.s3_settled, "s3_settled");
      } else {
        throw new Error(`Unexpected report operation evidence ${cell.operation_id}`);
      }
    }
  }

  const observations = parseObservationRecords(artifacts);
  const trialRecords = observations.filter(({ record }) => record.record === "trial");
  const requestRecords = observations.filter(({ record }) => record.record === "request");
  expect(trialRecords).toHaveLength(expectation.estimates.trial_batch_count);
  expect(requestRecords).toHaveLength(expectation.estimates.issued_operation_request_count);
  const requestCountsByOperation = requestRecords.reduce<Record<string, number>>((counts, { record }) => {
    const operationId = String(record.data.operation_id);
    counts[operationId] = (counts[operationId] ?? 0) + 1;
    return counts;
  }, {});
  expect(requestCountsByOperation).toEqual(expectation.requestCounts);
  for (const { record } of trialRecords) {
    const trialId = String(record.data.trial_id);
    const cellId = String(record.data.cell_id);
    const operationId = String(record.data.operation_id);
    const planned = expandedById.get(cellId);
    if (!planned) throw new Error(`Trial ${trialId} references unknown cell ${cellId}`);
    expect(operationId).toBe(planned.operation_id);
    const requests = requestRecords.filter(({ record: request }) =>
      request.data.cell_id === cellId && request.data.trial_id === trialId,
    );
    expect(requests).toHaveLength(quickSmokeRequestCount(planned));
    expect(new Set(requests.map(({ record: request }) => request.data.request_id)).size).toBe(requests.length);
  }

  const events = parseEventRecords(artifacts);
  expect(events.every(({ run_id }) => run_id === runId)).toBe(true);
  const phases = new Set(events
    .filter(({ data }) => data.kind === "trial_phase")
    .map(({ data }) => data.phase));
  expect(phases).toEqual(new Set(["setup", "operation", "verify", "teardown"]));
  expect(events.some(({ data }) => data.kind === "run_state" && data.state === "completed")).toBe(true);
}

async function assertLayerStackPhaseEvidence(
  page: Page,
  testInfo: TestInfo,
  report: QuickSmokeReport,
  artifacts: Map<string, ArtifactContentResponse>,
  runId: string,
): Promise<void> {
  type RawPhaseObservation = {
    sequence: number;
    cell_id: unknown;
    id: unknown;
    semantic_revision: unknown;
    unit: unknown;
    source: unknown;
    correlation: unknown;
    trace_span_name: unknown;
    request_id: unknown;
    status: unknown;
    start_offset_ns: unknown;
    duration_ns: unknown;
  };
  const expanded = parseJsonEnvelope<ExpandedQuickSmokePlan>(
    artifacts,
    "expanded_plan",
    "eos_benchmark_expanded_plan",
  );
  const layerCells = report.cells.filter(({ operation_id }) => operation_id === "squash_layerstack");
  expect(layerCells).toHaveLength(2);
  const expandedById = new Map(expanded.cells.map((cell) => [cell.cell_id, cell]));
  const rawPhases = parseObservationRecords(artifacts)
    .filter(({ record }) => record.record === "phase")
    .map(({ sequence, record }) => ({ sequence, ...record.data }) as RawPhaseObservation);
  const requiredStorageIds = [
    "layerstack_storage_plan",
    "layerstack_flatten",
    "layerstack_commit",
  ];
  const requiredPerRequestIds = [
    "layerstack_squash",
    ...requiredStorageIds,
    "layerstack_remount_sweep",
  ];
  const reportCells: unknown[] = [];
  const rawCells: unknown[] = [];

  for (const cell of layerCells) {
    const planned = expandedById.get(cell.cell_id);
    if (!planned) throw new Error(`Missing expanded LayerStack cell ${cell.cell_id}`);
    const liveSessions = requiredNumber(planned.operation.cell.live_sessions, `${cell.cell_id}.live_sessions`);
    const expectedIds = liveSessions === 0
      ? requiredPerRequestIds
      : [...requiredPerRequestIds, "workspace_session_remount"];
    expect(cell.phases.map(({ id }) => id).sort()).toEqual([...expectedIds].sort());
    for (const phase of cell.phases) {
      expect(layerstackPhaseSpans.get(phase.id)).toBe(phase.trace_span_name);
      expect(phase).toMatchObject({
        semantic_revision: 1,
        unit: "nanoseconds",
        source: "product_trace",
        correlation: "exact_request_trace_span",
        attempted: 5,
        failed: 0,
      });
      expect(phase.help.length).toBeGreaterThan(0);
      expect(phase.duration.count).toBe(5);
      expect(phase.duration.median).not.toBeNull();
      expect(phase.duration.p95).not.toBeNull();
    }
    for (const id of requiredStorageIds) expect(cell.phases.some((phase) => phase.id === id)).toBe(true);
    expect(cell.phases.some(({ id }) => id === "layerstack_remount_sweep")).toBe(true);
    expect(cell.phases.some(({ id }) => id === "workspace_session_remount")).toBe(liveSessions === 1);

    const cellRaw = rawPhases.filter((phase) => phase.cell_id === cell.cell_id);
    const rawCounts = cellRaw.reduce<Record<string, number>>((counts, phase) => {
      const id = String(phase.id);
      counts[id] = (counts[id] ?? 0) + 1;
      expect(phase.semantic_revision).toBe(1);
      expect(phase.unit).toBe("nanoseconds");
      expect(phase.source).toBe("product_trace");
      expect(phase.correlation).toBe("exact_request_trace_span");
      expect(phase.trace_span_name).toBe(layerstackPhaseSpans.get(id));
      expect(phase.request_id).toBe("squash-layerstack-0");
      expect(phase.status).toBe("succeeded");
      expect(requiredNumber(phase.start_offset_ns, `${id}.start_offset_ns`)).toBeGreaterThanOrEqual(0);
      expect(requiredNumber(phase.duration_ns, `${id}.duration_ns`)).toBeGreaterThanOrEqual(0);
      return counts;
    }, {});
    for (const id of requiredPerRequestIds) expect(rawCounts[id]).toBe(6);
    expect(rawCounts.workspace_session_remount ?? 0).toBe(liveSessions === 1 ? 6 : 0);
    reportCells.push({
      cell_id: cell.cell_id,
      live_sessions: liveSessions,
      phase_ids: cell.phases.map(({ id }) => id),
      storage_phase_ids: requiredStorageIds,
      sweep_phase_id: "layerstack_remount_sweep",
      per_session_phase_id: liveSessions === 1 ? "workspace_session_remount" : null,
      phase_summaries: cell.phases,
    });
    rawCells.push({ cell_id: cell.cell_id, live_sessions: liveSessions, phase_counts: rawCounts });
  }

  await page.getByRole("tab", { name: "Results" }).click();
  const applicationFailure = page.getByText("The interface stopped", { exact: true });
  await expect.poll(async () => {
    if (await applicationFailure.count() > 0) {
      return `failure: ${await applicationFailure.locator("..").textContent()}`;
    }
    return await page.getByRole("heading", { name: "Cell distributions & operation evidence" }).count() > 0
      ? "ready"
      : "waiting";
  }).toBe("ready");
  for (const cell of layerCells) {
    const control = page.getByRole("button").filter({ hasText: cell.cell_id });
    await expect(control).toHaveCount(1);
    if (await control.getAttribute("aria-expanded") !== "true") await control.click();
  }
  await expect(page.getByRole("heading", { name: "Product phase timing" })).toHaveCount(2);
  const renderedTraceCounts: Record<string, number> = {};
  for (const traceSpan of layerstackPhaseSpans.values()) {
    const count = await page.getByText(traceSpan, { exact: true }).count();
    renderedTraceCounts[traceSpan] = count;
    expect(count).toBe(traceSpan === "workspace_session.remount" ? 1 : 2);
  }
  await retainScreenshot(page, testInfo, "layerstack-storage-remount-evidence.png");
  await retainJson(testInfo, "layerstack-phase-proof.json", {
    schema_version: 1,
    run_id: runId,
    report_schema_version: report.schema_version,
    storage_and_remount_are_separate: true,
    report_cells: reportCells,
    raw_observation_cells: rawCells,
    rendered_trace_counts: renderedTraceCounts,
  });
}

async function assertRunAllFamilySequence(
  testInfo: TestInfo,
  runId: string,
  artifacts: Map<string, ArtifactContentResponse>,
): Promise<void> {
  await recordProgress("run-all-family-sequence-validation-started", { run_id: runId });
  const events = parseEventRecords(artifacts);
  const familyEvents = events.filter(({ data }) => data.kind === "family_state");
  const expected = ["command", "files", "workspace_lifecycle", "layerstack"].flatMap((family) => [
    { family, state: "preparing" },
    { family, state: "running" },
    { family, state: "completed" },
  ]);
  const observed = familyEvents.map(({ sequence, monotonic_offset_ns, data }) => ({
    sequence,
    monotonic_offset_ns,
    family: String(data.family),
    state: String(data.state),
  }));
  expect(observed.map(({ family, state }) => ({ family, state }))).toEqual(expected);
  expect(observed.map(({ sequence }) => sequence)).toEqual([...observed.map(({ sequence }) => sequence)].sort((a, b) => a - b));
  expect(observed.map(({ monotonic_offset_ns }) => monotonic_offset_ns)).toEqual(
    [...observed.map(({ monotonic_offset_ns }) => monotonic_offset_ns)].sort((a, b) => a - b),
  );
  const boundaries = expected.slice(0, -3).filter(({ state }) => state === "completed").map(({ family }) => {
    const completed = observed.find((entry) => entry.family === family && entry.state === "completed");
    const nextFamily = expected[expected.findIndex((entry) => entry.family === family && entry.state === "completed") + 1]?.family;
    const nextPreparing = observed.find((entry) => entry.family === nextFamily && entry.state === "preparing");
    if (!completed || !nextPreparing) throw new Error(`Missing Run All boundary after ${family}`);
    expect(completed.sequence).toBeLessThan(nextPreparing.sequence);
    expect(completed.monotonic_offset_ns).toBeLessThanOrEqual(nextPreparing.monotonic_offset_ns);
    return {
      completed_family: family,
      completed_sequence: completed.sequence,
      next_family: nextFamily,
      next_preparing_sequence: nextPreparing.sequence,
      non_overlapping: true,
    };
  });
  await retainJson(testInfo, "run-all-family-sequence.json", {
    schema_version: 1,
    run_id: runId,
    expected,
    observed,
    boundaries,
    exact_sequence: true,
    non_overlapping: true,
  });
  await recordProgress("run-all-family-sequence-validation-completed", {
    run_id: runId,
    family_boundary_count: boundaries.length,
  });
}

async function visibleEventSequences(page: Page): Promise<number[]> {
  const cells = page.getByLabel("Persisted run event log").locator("tbody tr td:first-child");
  const texts = await cells.allTextContents();
  return texts.map((value) => Number(value.replaceAll(",", "").trim()));
}

interface SseReplayProof {
  run_id: string;
  ui_sequence_before_navigation: number;
  disconnected_after_sequence: number;
  last_event_id_header: number;
  persisted_latest_before_reconnect: number;
  expected_replayed_sequence_ids: number[];
  observed_ui_sequence_ids: number[];
  replayed_event_count: number;
  first_live_sequence_id: number;
  sequence_after_live: number;
  response_status: number;
  response_content_type: string;
}

async function proveSseGapReplay(
  page: Page,
  sentinels: BrowserSentinels,
  runId: string,
): Promise<SseReplayProof> {
  await expect.poll(() => currentSseSequence(page), { timeout: 5 * 60_000 }).toBeGreaterThan(0);
  for (let attempt = 0; attempt < 12; attempt += 1) {
    // The DOM is sampled immediately before navigation, but the browser can
    // legitimately accept further events while it tears down the old document.
    // The reconnect request's Last-Event-ID is the authoritative boundary the
    // server must replay from; retain both values as evidence rather than
    // mistaking that narrow teardown interval for lost/reordered SSE data.
    const uiSequenceBeforeNavigation = await currentSseSequence(page);
    await gotoWithSentinels(page, sentinels, "/benchmark", `sse-replay-disconnect-${attempt + 1}`, "domcontentloaded");
    await page.waitForTimeout(250);

    const snapshotPromise = page.waitForResponse((response) =>
      apiPath(response.url()) === `/api/v1/runs/${runId}` && response.request().method() === "GET",
    );
    const replayRequestPromise = page.waitForRequest((request) =>
      apiPath(request.url()) === `/api/v1/runs/${runId}/events` && Boolean(request.headers()["last-event-id"]),
    );
    const replayResponsePromise = page.waitForResponse((response) =>
      apiPath(response.url()) === `/api/v1/runs/${runId}/events`
        && Boolean(response.request().headers()["last-event-id"]),
    );
    await gotoWithSentinels(page, sentinels, `/benchmark/runs/${runId}`, `sse-replay-reconnect-${attempt + 1}`, "domcontentloaded");
    const snapshotResponse = await snapshotPromise;
    const snapshot = await snapshotResponse.json() as RunResponse;
    const request = await replayRequestPromise;
    const lastEventId = Number(request.headers()["last-event-id"]);
    expect(lastEventId).toBeGreaterThanOrEqual(uiSequenceBeforeNavigation);
    expect(Number.isSafeInteger(lastEventId)).toBe(true);
    const replayResponse = await replayResponsePromise;
    expect(replayResponse.status()).toBe(200);
    expect(replayResponse.headers()["content-type"]).toContain("text/event-stream");
    await recordProgress("sse-replay-boundary-observed", {
      attempt: attempt + 1,
      run_id: runId,
      ui_sequence_before_navigation: uiSequenceBeforeNavigation,
      last_event_id_header: lastEventId,
      persisted_latest_before_reconnect: snapshot.latest_sequence,
    });

    if (snapshot.latest_sequence <= lastEventId) {
      await expect.poll(() => currentSseSequence(page), { timeout: 60_000 }).toBeGreaterThanOrEqual(snapshot.latest_sequence);
      continue;
    }

    const expectedReplayed = Array.from(
      { length: snapshot.latest_sequence - lastEventId },
      (_, index) => lastEventId + index + 1,
    );
    expect(expectedReplayed.length).toBeLessThan(450);
    await expect.poll(async () => {
      const visible = await visibleEventSequences(page);
      return expectedReplayed.every((sequence) => visible.includes(sequence));
    }, { timeout: 2 * 60_000 }).toBe(true);
    await expect.poll(() => currentSseSequence(page), { timeout: 2 * 60_000 }).toBeGreaterThan(snapshot.latest_sequence);
    const sequenceAfterLive = await currentSseSequence(page);
    const observedDescending = await visibleEventSequences(page);
    const observedAscending = [...observedDescending].reverse();
    const expectedThroughFirstLive = [...expectedReplayed, snapshot.latest_sequence + 1];
    expect(observedAscending.filter((sequence) =>
      sequence > lastEventId && sequence <= snapshot.latest_sequence + 1,
    )).toEqual(expectedThroughFirstLive);
    return {
      run_id: runId,
      ui_sequence_before_navigation: uiSequenceBeforeNavigation,
      disconnected_after_sequence: lastEventId,
      last_event_id_header: lastEventId,
      persisted_latest_before_reconnect: snapshot.latest_sequence,
      expected_replayed_sequence_ids: expectedReplayed,
      observed_ui_sequence_ids: observedAscending,
      replayed_event_count: await currentReplayedCount(page),
      first_live_sequence_id: snapshot.latest_sequence + 1,
      sequence_after_live: sequenceAfterLive,
      response_status: replayResponse.status(),
      response_content_type: replayResponse.headers()["content-type"] ?? "",
    };
  }
  throw new Error(`Run ${runId} did not create a deterministic persisted SSE replay gap`);
}

async function retainCompletedSseProof(
  testInfo: TestInfo,
  proof: SseReplayProof,
  artifacts: Map<string, ArtifactContentResponse>,
): Promise<void> {
  await recordProgress("sse-replay-artifact-validation-started", {
    run_id: proof.run_id,
    replay_cursor: proof.last_event_id_header,
    persisted_latest_before_reconnect: proof.persisted_latest_before_reconnect,
  });
  const persisted = parseEventRecords(artifacts);
  const persistedArtifactSequenceIds = persisted
    .map(({ sequence }) => sequence)
    .filter((sequence) => sequence > proof.disconnected_after_sequence
      && sequence <= proof.persisted_latest_before_reconnect);
  expect(persistedArtifactSequenceIds).toEqual(proof.expected_replayed_sequence_ids);
  expect(proof.first_live_sequence_id).toBe(proof.persisted_latest_before_reconnect + 1);
  expect(proof.replayed_event_count).toBeGreaterThanOrEqual(proof.expected_replayed_sequence_ids.length);
  await retainJson(testInfo, "sse-reload-replay.json", {
    schema_version: 1,
    ...proof,
    persisted_artifact_sequence_ids: persistedArtifactSequenceIds,
    exact_missed_sequence_match: true,
    replay_before_live: true,
  });
  await recordProgress("sse-replay-artifact-validation-completed", {
    run_id: proof.run_id,
    replayed_event_count: proof.replayed_event_count,
    exact_missed_sequence_match: true,
  });
}

async function captureResponsiveReportEvidence(
  page: Page,
  sentinels: BrowserSentinels,
  testInfo: TestInfo,
  runId: string,
): Promise<void> {
  for (const width of [375, 768, 1024, 1440]) {
    await page.setViewportSize({ width, height: 900 });
    await gotoWithSentinels(page, sentinels, `/benchmark/reports/${runId}`, `responsive-report-${width}`, "networkidle");
    await expect(page.getByText("Scientific report", { exact: true })).toBeVisible();
    await expect(page.getByText("Terminal evidence", { exact: true })).toBeVisible();
    const mobileNavigation = page.getByRole("button", { name: "Open navigation" });
    if (width < 992) await expect(mobileNavigation).toBeVisible();
    else await expect(mobileNavigation).toBeHidden();
    await retainScreenshot(page, testInfo, `run-1-report-${width}.png`);
  }
  await page.setViewportSize({ width: 1280, height: 720 });
}

async function waitForActiveTrial(
  page: Page,
  testInfo: TestInfo,
  runId: string,
): Promise<RunResponse> {
  let active: RunResponse | null = null;
  const deadline = Date.now() + 3 * 60_000;
  while (Date.now() < deadline && active === null) {
    const response = await page.waitForResponse((candidate) =>
      apiPath(candidate.url()) === `/api/v1/runs/${runId}` && candidate.request().method() === "GET",
    { timeout: Math.min(10_000, deadline - Date.now()) }).catch(() => null);
    if (!response) continue;
    expect(response.status()).toBe(200);
    const snapshot = await response.json() as RunResponse;
    const progress = snapshot.progress;
    expect(knownRunStates.has(snapshot.manifest.state)).toBe(true);
    if (
      activeTrialRunStates.has(snapshot.manifest.state)
      && progress.completed_trial_batches > 0
      && progress.completed_trial_batches < progress.total_trial_batches
      && progress.current_trial_id !== null
      && progress.current_cell_id !== null
      && progress.current_family !== null
      && progress.current_operation !== null
      && progress.trial_kind !== null
      && progress.phase !== null
    ) {
      active = snapshot;
    }
  }
  if (!active) throw new Error(`Run ${runId} never exposed an active trial batch`);
  await retainJson(testInfo, "cancel-active-trial.json", {
    captured_at: new Date().toISOString(),
    run: active,
  });
  await expect(page.getByText("Current trial batch", { exact: true }).locator("..")).not.toContainText("Waiting");
  await retainScreenshot(page, testInfo, "cancel-active-trial.png");
  return active;
}

async function openAndVerifyCompletedReport(
  page: Page,
  testInfo: TestInfo,
  runId: string,
  expectation: QuickSmokeExpectation,
): Promise<{ report: QuickSmokeReport; artifacts: Map<string, ArtifactContentResponse> }> {
  await recordProgress("report-open-requested", {
    ordinal: expectation.ordinal,
    run_id: runId,
  });
  const reportResponse = page.waitForResponse((response) =>
    apiPath(response.url()) === `/api/v1/runs/${runId}/report` && response.request().method() === "GET",
  );
  await page.getByRole("link", { name: "Open report" }).click();
  const response = await reportResponse;
  expect(response.status()).toBe(200);
  const report = await response.json() as QuickSmokeReport;
  await retainJson(testInfo, `run-${expectation.ordinal}-report.json`, report);
  expect(report.run_id).toBe(runId);
  expect(report.provisional).toBe(false);
  expect(new Set(report.cells.map(({ family_label }) => family_label))).toEqual(
    new Set(expectation.familyLabels),
  );
  expect(new Set(report.cells.map(({ operation_id }) => operation_id))).toEqual(
    new Set(Object.keys(expectation.operationCells)),
  );
  await expect(page.getByText("Terminal evidence", { exact: true })).toBeVisible();
  const artifacts = await inspectAllowlistedArtifacts(page, testInfo, runId, expectation.ordinal, true);
  assertQuickSmokeSemantics(report, artifacts, runId, expectation);
  if (expectation.scope === "layerstack") {
    await assertLayerStackPhaseEvidence(page, testInfo, report, artifacts, runId);
  }
  await retainScreenshot(page, testInfo, `run-${expectation.ordinal}-terminal-report.png`);
  await recordProgress("report-evidence-retained", {
    ordinal: expectation.ordinal,
    run_id: runId,
  });
  return { report, artifacts };
}

async function openAndVerifyCancelledReport(
  page: Page,
  testInfo: TestInfo,
  runId: string,
  expectation: QuickSmokeExpectation,
): Promise<void> {
  await expect(page.getByRole("link", { name: "Open report" })).toBeVisible();
  const reportResponse = page.waitForResponse((response) =>
    apiPath(response.url()) === `/api/v1/runs/${runId}/report` && response.request().method() === "GET",
  );
  await page.getByRole("link", { name: "Open report" }).click();
  const report = await reportResponse;
  expect(report.status()).toBe(200);
  const reportBody = await report.json() as QuickSmokeReport;
  await retainJson(testInfo, `run-${expectation.ordinal}-cancelled-report.json`, reportBody);
  expect(reportBody).toMatchObject({
    schema_version: 4,
    report_derivation_revision: 3,
    run_id: runId,
    state: "cancelled",
    provisional: false,
    design_counts: {
      test_combinations: expectation.estimates.cell_count,
      trial_batches: expectation.estimates.trial_batch_count,
      issued_product_requests: expectation.estimates.issued_operation_request_count,
    },
  });
  const artifacts = await inspectAllowlistedArtifacts(page, testInfo, runId, expectation.ordinal, false);
  const persistedReport = parseJsonEnvelope<QuickSmokeReport>(
    artifacts,
    "report",
    "eos_benchmark_report",
    4,
  );
  expect(persistedReport).toEqual(reportBody);
  const manifest = parseJsonEnvelope<RunManifestArtifact>(artifacts, "run_manifest", "eos_benchmark_run_manifest");
  expect(manifest).toMatchObject({ run_id: runId, state: "cancelled" });
  expect(manifest.ended_at).toMatch(/^\d{4}-\d{2}-\d{2}T/);
  const observations = parseObservationRecords(artifacts);
  const partialTrials = observations.filter(({ record }) => record.record === "trial");
  expect(partialTrials.length).toBeGreaterThan(0);
  expect(partialTrials.length).toBeLessThan(expectation.estimates.trial_batch_count);
  const events = parseEventRecords(artifacts);
  expect(events.some(({ data }) => data.kind === "trial_phase")).toBe(true);
  expect(events.some(({ data }) => data.kind === "run_state" && data.state === "cancelling")).toBe(true);
  expect(events.some(({ data }) => data.kind === "run_state" && data.state === "cancelled")).toBe(true);
}

async function chooseRun(page: Page, label: "Reference run" | "Candidate run", runId: string): Promise<void> {
  await page.getByRole("combobox", { name: label }).click();
  await page.getByRole("option").filter({ hasText: runId }).click();
}

test.describe.configure({ mode: "serial" });

test("production browser drives four family Quick Smokes, sequential Run All, exact SSE replay, cancellation, cleanup, and comparison", async ({ page, browser }, testInfo) => {
  test.skip(process.env.BENCHMARK_REAL_BACKEND !== "1", "Run through run-real-backend.mjs so the production runner owns the origin.");
  const stage = realBackendStage();
  const sentinels = installSentinels(page);

  try {
    await recordProgress("browser-stage-started", { browser: browser.browserType().name(), selected_stage: stage });
    await retainJson(testInfo, "browser-runtime.json", {
      name: browser.browserType().name(),
      version: browser.version(),
      initial_viewport: page.viewportSize(),
    });
    await assertProductionBootstrap(page, sentinels, testInfo);
    await recordProgress("production-bootstrap-verified");

    if (stage === "small") {
      const layerstackRunId = await startFamilyQuickSmoke(page, sentinels, testInfo, quickSmokeExpectations.layerstack);
      await waitForCompletedRun(page, testInfo, layerstackRunId);
      sentinels.allowEstablishedEventStreamAborts();
      await openAndVerifyCompletedReport(page, testInfo, layerstackRunId, quickSmokeExpectations.layerstack);
      await retainJson(testInfo, "real-run-ids.json", {
        schema_version: 1,
        stage,
        runs: [stageRunIdentity(quickSmokeExpectations.layerstack, layerstackRunId)],
        completed: [layerstackRunId],
        cancelled: null,
        family_runs: { layerstack: layerstackRunId },
        comparison: null,
        run_all: null,
      });
      await assertStageSentinels(sentinels, 1, false);
      await recordProgress("browser-stage-completed", { run_create_count: sentinels.runCreates.length });
      return;
    }

    const commandReferenceId = await startFamilyQuickSmoke(page, sentinels, testInfo, quickSmokeExpectations.commandReference);
    await waitForCompletedRun(page, testInfo, commandReferenceId);
    sentinels.allowEstablishedEventStreamAborts();
    await openAndVerifyCompletedReport(
      page,
      testInfo,
      commandReferenceId,
      quickSmokeExpectations.commandReference,
    );
    await captureResponsiveReportEvidence(page, sentinels, testInfo, commandReferenceId);

    const commandCandidateId = await startFamilyQuickSmoke(page, sentinels, testInfo, quickSmokeExpectations.commandCandidate);
    await waitForCompletedRun(page, testInfo, commandCandidateId);
    sentinels.allowEstablishedEventStreamAborts();
    await openAndVerifyCompletedReport(
      page,
      testInfo,
      commandCandidateId,
      quickSmokeExpectations.commandCandidate,
    );

    const filesRunId = await startFamilyQuickSmoke(page, sentinels, testInfo, quickSmokeExpectations.files);
    await waitForCompletedRun(page, testInfo, filesRunId);
    sentinels.allowEstablishedEventStreamAborts();
    await openAndVerifyCompletedReport(page, testInfo, filesRunId, quickSmokeExpectations.files);

    const workspaceRunId = await startFamilyQuickSmoke(page, sentinels, testInfo, quickSmokeExpectations.workspace);
    await waitForCompletedRun(page, testInfo, workspaceRunId);
    sentinels.allowEstablishedEventStreamAborts();
    await openAndVerifyCompletedReport(page, testInfo, workspaceRunId, quickSmokeExpectations.workspace);

    const layerstackRunId = await startFamilyQuickSmoke(page, sentinels, testInfo, quickSmokeExpectations.layerstack);
    await waitForCompletedRun(page, testInfo, layerstackRunId);
    sentinels.allowEstablishedEventStreamAborts();
    await openAndVerifyCompletedReport(page, testInfo, layerstackRunId, quickSmokeExpectations.layerstack);

    if (stage === "medium") {
      const completedRunIds = [
        commandReferenceId,
        commandCandidateId,
        filesRunId,
        workspaceRunId,
        layerstackRunId,
      ];
      await retainJson(testInfo, "real-run-ids.json", {
        schema_version: 1,
        stage,
        runs: completedExpectations.slice(0, 5).map((expectation, index) => (
          stageRunIdentity(expectation, completedRunIds[index])
        )),
        completed: completedRunIds,
        cancelled: null,
        family_runs: {
          command: commandReferenceId,
          files: filesRunId,
          workspace: workspaceRunId,
          layerstack: layerstackRunId,
        },
        comparison: null,
        run_all: null,
      });
      await assertStageSentinels(sentinels, 5, false);
      await recordProgress("browser-stage-completed", { run_create_count: sentinels.runCreates.length });
      return;
    }

    const runAllId = await startRunAllQuickSmoke(page, sentinels, testInfo, quickSmokeExpectations.runAll);
    await expect(page.getByLabel("Persisted run event log")).toContainText(
      /(?:setup|operation|verify|teardown) phase/i,
      { timeout: 5 * 60_000 },
    );
    const replayProof = await proveSseGapReplay(page, sentinels, runAllId);
    await waitForCompletedRun(page, testInfo, runAllId);
    await expect(page.getByLabel("Persisted run event log")).toContainText("Run completed");
    sentinels.allowEstablishedEventStreamAborts();
    const runAllEvidence = await openAndVerifyCompletedReport(
      page,
      testInfo,
      runAllId,
      quickSmokeExpectations.runAll,
    );
    await retainCompletedSseProof(testInfo, replayProof, runAllEvidence.artifacts);
    await assertRunAllFamilySequence(testInfo, runAllId, runAllEvidence.artifacts);

    const cancelledRunId = await startRunAllQuickSmoke(page, sentinels, testInfo, quickSmokeExpectations.cancelled);
    const activeTrial = await waitForActiveTrial(page, testInfo, cancelledRunId);
    expect(activeTrial.manifest.run_id).toBe(cancelledRunId);
    await page.getByRole("button", { name: "Cancel run" }).click();
    await expect(page.getByRole("dialog", { name: "Cancel this run?" })).toBeVisible();
    const cancelled = page.waitForResponse((response) =>
      apiPath(response.url()) === "/api/v1/runs/" + cancelledRunId + "/cancel"
        && response.request().method() === "POST",
    );
    await page.getByRole("button", { name: "Request cancellation" }).click();
    const cancelledResponse = await cancelled;
    expect(cancelledResponse.status()).toBe(202);
    const cancellation = await cancelledResponse.json() as {
      schema_version: number;
      run_id: string;
      state: string;
      cancellation_requested: boolean;
    };
    await retainJson(testInfo, "run-7-cancel-response.json", cancellation);
    expect(cancellation).toEqual({
      schema_version: 1,
      run_id: cancelledRunId,
      state: "cancelling",
      cancellation_requested: true,
    });
    await expect(page.getByText("Run cancelled", { exact: true })).toBeVisible({ timeout: 10 * 60_000 });
    await retainScreenshot(page, testInfo, "cancelled-cleanup-terminal.png");
    sentinels.allowEstablishedEventStreamAborts();
    await openAndVerifyCancelledReport(
      page,
      testInfo,
      cancelledRunId,
      quickSmokeExpectations.cancelled,
    );

    const completedRunIds = [
      commandReferenceId,
      commandCandidateId,
      filesRunId,
      workspaceRunId,
      layerstackRunId,
      runAllId,
    ];
    await retainJson(testInfo, "real-run-ids.json", {
      schema_version: 1,
      stage,
      runs: [
        ...completedExpectations.map((expectation, index) => ({
          ordinal: expectation.ordinal,
          role: expectation.role,
          scope: expectation.scope,
          run_id: completedRunIds[index],
          state: "completed",
          design_counts: {
            test_combinations: expectation.estimates.cell_count,
            trial_batches: expectation.estimates.trial_batch_count,
            issued_product_requests: expectation.estimates.issued_operation_request_count,
          },
        })),
        {
          ordinal: quickSmokeExpectations.cancelled.ordinal,
          role: quickSmokeExpectations.cancelled.role,
          scope: quickSmokeExpectations.cancelled.scope,
          run_id: cancelledRunId,
          state: "cancelled",
          design_counts: {
            test_combinations: quickSmokeExpectations.cancelled.estimates.cell_count,
            trial_batches: quickSmokeExpectations.cancelled.estimates.trial_batch_count,
            issued_product_requests: quickSmokeExpectations.cancelled.estimates.issued_operation_request_count,
          },
        },
      ],
      completed: completedRunIds,
      cancelled: cancelledRunId,
      family_runs: {
        command: commandReferenceId,
        files: filesRunId,
        workspace: workspaceRunId,
        layerstack: layerstackRunId,
      },
      comparison: {
        reference: commandReferenceId,
        candidate: commandCandidateId,
      },
      run_all: runAllId,
    });

    await gotoWithSentinels(page, sentinels, "/benchmark/compare", "two-run-comparison", "networkidle");
    await chooseRun(page, "Reference run", commandReferenceId);
    await chooseRun(page, "Candidate run", commandCandidateId);
    const compared = page.waitForResponse((response) =>
      apiPath(response.url()) === "/api/v1/compare" && response.request().method() === "POST",
    );
    await page.getByRole("button", { name: "Check compatibility" }).click();
    const compareResponse = await compared;
    expect(compareResponse.status()).toBe(200);
    const comparison = await compareResponse.json() as {
      schema_version: number;
      comparison_derivation_revision: number;
      reference_run_id: string;
      candidate_run_id: string;
      protocol: { declarations_compatible: boolean };
      compatible: boolean;
      descriptive_only: boolean;
      matched_cell_ids: string[];
      matched_cells: unknown[];
      deltas: unknown[];
    };
    await retainJson(testInfo, "two-run-comparison.json", comparison);
    expect(comparison).toMatchObject({
      schema_version: 1,
      comparison_derivation_revision: 3,
      reference_run_id: commandReferenceId,
      candidate_run_id: commandCandidateId,
      protocol: { declarations_compatible: true },
    });
    expect(comparison.compatible).toBe(true);
    expect(comparison.descriptive_only).toBe(false);
    expect(comparison.matched_cell_ids).toHaveLength(2);
    expect(new Set(comparison.matched_cell_ids).size).toBe(2);
    expect(comparison.matched_cells).toHaveLength(2);
    expect(comparison.deltas.length).toBeGreaterThan(0);
    await expect(page.getByText("Runs are scientifically compatible", { exact: true })).toBeVisible();
    await expect(page.getByRole("heading", { name: "Protocol & treatment decision" })).toBeVisible();
    await expect(page.getByRole("heading", { name: "Backend-authored metric deltas" })).toBeVisible();
    const headings = await page.getByRole("heading").allTextContents();
    expect(headings.indexOf("Protocol & treatment decision")).toBeLessThan(
      headings.indexOf("Backend-authored metric deltas"),
    );
    await retainScreenshot(page, testInfo, "compatible-two-run-comparison.png");

    await assertStageSentinels(sentinels, 7, true);
    await recordProgress("browser-stage-completed", { run_create_count: sentinels.runCreates.length });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    try {
      await recordProgress("browser-stage-failed", { message: message.slice(0, 500) });
      await retainJson(testInfo, "failure-context.json", {
        schema_version: 1,
        captured_at: new Date().toISOString(),
        route: page.url(),
        message,
      });
      await retainScreenshot(page, testInfo, "failure-context.png");
    } catch (retentionError) {
      console.error("[benchmark-progress] failure evidence retention failed", retentionError);
    }
    throw error;
  } finally {
    await retainJson(testInfo, "request-ledger.json", sentinels.ledger);
    await retainJson(testInfo, "browser-sentinels.json", {
      console_errors: sentinels.consoleErrors,
      react_key_warnings: sentinels.reactKeyWarnings,
      network_failures: sentinels.networkFailures,
      expected_event_stream_aborts: sentinels.expectedEventStreamAborts,
      expected_navigation_read_aborts: sentinels.expectedNavigationReadAborts,
      page_errors: sentinels.pageErrors,
      required_request_failures: sentinels.requiredRequestFailures,
      service_worker_responses: sentinels.serviceWorkerResponses,
      service_worker_urls: sentinels.serviceWorkerUrls,
      run_create_count: sentinels.runCreates.length,
    });
  }
});
