import type {
  ApiErrorBody,
  ArtifactContentResponse,
  ArtifactIndexResponse,
  ComparisonRequest,
  ComparisonResponse,
  DefinitionsResponse,
  HealthResponse,
  PlanValidationRequest,
  PlanValidationResponse,
  ReportResponse,
  RunCancelResponse,
  RunCreateRequest,
  RunCreateResponse,
  RunListResponse,
  RunResponse,
  SettingsResponse,
  SettingsUpdateRequest,
} from "./types";

const API_ROOT = "/api/v1";
const NONCE_META = "eos-benchmark-nonce";

export class ApiClientError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code: string,
    readonly requestId: string | null,
  ) {
    super(message);
    this.name = "ApiClientError";
  }
}

function mutationNonce(): string {
  const nonce = document.querySelector<HTMLMetaElement>(`meta[name="${NONCE_META}"]`)?.content.trim();
  if (!nonce) {
    throw new ApiClientError("The runner did not provide a mutation nonce.", 0, "missing_nonce", null);
  }
  return nonce;
}

function isApiErrorBody(value: unknown): value is ApiErrorBody {
  if (typeof value !== "object" || value === null || !("error" in value)) return false;
  const error = value.error;
  return (
    typeof error === "object" &&
    error !== null &&
    "code" in error &&
    typeof error.code === "string" &&
    "message" in error &&
    typeof error.message === "string" &&
    "request_id" in error &&
    typeof error.request_id === "string"
  );
}

async function requestJson<T>(path: string, init: RequestInit = {}): Promise<T> {
  const method = (init.method ?? "GET").toUpperCase();
  const headers = new Headers(init.headers);

  if (method !== "GET" && method !== "HEAD") {
    headers.set("Content-Type", "application/json");
    headers.set("X-EOS-Benchmark-Nonce", mutationNonce());
  }

  const response = await fetch(`${API_ROOT}${path}`, {
    ...init,
    headers,
    credentials: "same-origin",
    cache: "no-store",
  });
  const body: unknown = await response.json().catch(() => null);

  if (!response.ok) {
    if (isApiErrorBody(body)) {
      throw new ApiClientError(body.error.message, response.status, body.error.code, body.error.request_id);
    }
    throw new ApiClientError(
      `The runner returned HTTP ${response.status}.`,
      response.status,
      "invalid_error_response",
      null,
    );
  }

  return body as T;
}

function runPath(runId: string, suffix = ""): string {
  return `/runs/${encodeURIComponent(runId)}${suffix}`;
}

export const benchmarkApi = {
  health: () => requestJson<HealthResponse>("/health"),
  settings: () => requestJson<SettingsResponse>("/settings"),
  updateSettings: (request: SettingsUpdateRequest) =>
    requestJson<SettingsResponse>("/settings", { method: "PUT", body: JSON.stringify(request) }),
  definitions: () => requestJson<DefinitionsResponse>("/definitions"),
  validatePlan: (request: PlanValidationRequest) =>
    requestJson<PlanValidationResponse>("/plans/validate", {
      method: "POST",
      body: JSON.stringify(request),
    }),
  createRun: (request: RunCreateRequest) =>
    requestJson<RunCreateResponse>("/runs", { method: "POST", body: JSON.stringify(request) }),
  listRuns: (cursor?: string) =>
    requestJson<RunListResponse>(`/runs${cursor ? `?cursor=${encodeURIComponent(cursor)}` : ""}`),
  run: (runId: string) => requestJson<RunResponse>(runPath(runId)),
  cancelRun: (runId: string) =>
    requestJson<RunCancelResponse>(runPath(runId, "/cancel"), {
      method: "POST",
      body: JSON.stringify({}),
    }),
  report: (runId: string) => requestJson<ReportResponse>(runPath(runId, "/report")),
  artifacts: (runId: string) => requestJson<ArtifactIndexResponse>(runPath(runId, "/artifacts")),
  artifact: (runId: string, artifactId: string) =>
    requestJson<ArtifactContentResponse>(
      runPath(runId, `/artifacts/${encodeURIComponent(artifactId)}`),
    ),
  compare: (request: ComparisonRequest) =>
    requestJson<ComparisonResponse>("/compare", { method: "POST", body: JSON.stringify(request) }),
  eventsUrl: (runId: string) => `${API_ROOT}${runPath(runId, "/events")}`,
};
