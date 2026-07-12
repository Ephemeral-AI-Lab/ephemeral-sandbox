import { afterEach, describe, expect, it, vi } from "vitest";
import { downloadArtifact } from "@/components/ArtifactBrowser";
import type { ArtifactContentResponse } from "@/api/types";

const artifact: ArtifactContentResponse = {
  schema_version: 1,
  artifact_id: "json_export",
  label: "export.json",
  media_type: "application/json",
  encoding: "utf-8",
  content: "{\"schema\":\"eos_benchmark_json_export\"}",
  size_bytes: 38,
  sha256: "sha256:0000000000000000000000000000000000000000000000000000000000000000",
};

afterEach(() => vi.unstubAllGlobals());

describe("allowlisted artifact downloads", () => {
  it("keeps the server-authored artifact ID exact without MIME-derived suffixes", async () => {
    const anchor = document.createElement("a");
    const click = vi.spyOn(anchor, "click").mockImplementation(() => undefined);
    const blobs: Blob[] = [];
    const createObjectURL = vi.fn((blob: Blob) => {
      blobs.push(blob);
      return "blob:benchmark-artifact";
    });
    const revokeObjectURL = vi.fn();
    vi.stubGlobal("URL", { createObjectURL, revokeObjectURL });
    vi.spyOn(document, "createElement").mockReturnValue(anchor);

    downloadArtifact(artifact);

    expect(anchor.download).toBe(artifact.artifact_id);
    expect(anchor.href).toBe("blob:benchmark-artifact");
    expect(click).toHaveBeenCalledOnce();
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:benchmark-artifact");
    const [blob] = blobs;
    expect(blob).toBeDefined();
    expect(blob.type).toBe("application/octet-stream");
  });
});
