import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiClientError, benchmarkApi } from "@/api/client";

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  document.head.querySelector('meta[name="eos-benchmark-nonce"]')?.remove();
});

describe("benchmark API client", () => {
  it("uses same-origin paths and adds the bootstrap nonce only to mutations", async () => {
    const meta = document.createElement("meta");
    meta.name = "eos-benchmark-nonce";
    meta.content = "test-nonce";
    document.head.append(meta);
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(jsonResponse({ schema_version: 1 }))
      .mockResolvedValueOnce(jsonResponse({ schema_version: 1 }));
    vi.stubGlobal("fetch", fetchMock);

    await benchmarkApi.health();
    await benchmarkApi.updateSettings({ test_workspace_root: "/tmp/owned-benchmark-root" });

    expect(fetchMock).toHaveBeenNthCalledWith(
      1,
      "/api/v1/health",
      expect.objectContaining({ credentials: "same-origin" }),
    );
    const getHeaders = fetchMock.mock.calls[0]?.[1]?.headers as Headers;
    expect(getHeaders.has("X-EOS-Benchmark-Nonce")).toBe(false);

    const mutationHeaders = fetchMock.mock.calls[1]?.[1]?.headers as Headers;
    expect(fetchMock.mock.calls[1]?.[0]).toBe("/api/v1/settings");
    expect(mutationHeaders.get("Content-Type")).toBe("application/json");
    expect(mutationHeaders.get("X-EOS-Benchmark-Nonce")).toBe("test-nonce");
  });

  it("fails closed before a mutation when the runner nonce is missing", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    await expect(
      benchmarkApi.updateSettings({ test_workspace_root: "/tmp/owned-benchmark-root" }),
    ).rejects.toMatchObject({ code: "missing_nonce" } satisfies Partial<ApiClientError>);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
