import { Alert, Badge, Button, Card, Code, Group, Stack, Table, Text, Title } from "@mantine/core";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { benchmarkApi } from "@/api/client";
import type { ArtifactContentResponse } from "@/api/types";
import { ErrorState, LoadingState } from "@/components/AsyncState";
import { formatBytes } from "@/lib/format";

function artifactBytes(artifact: ArtifactContentResponse): Uint8Array<ArrayBuffer> {
  if (artifact.encoding === "utf-8") return new TextEncoder().encode(artifact.content);
  const decoded = window.atob(artifact.content);
  const bytes = new Uint8Array(decoded.length);
  for (let index = 0; index < decoded.length; index += 1) bytes[index] = decoded.charCodeAt(index);
  return bytes;
}

export function downloadArtifact(artifact: ArtifactContentResponse) {
  // Keep the generated allowlisted ID exact. Chromium otherwise appends an
  // extension derived from the display media type, making the downloaded name
  // differ from the server-authored artifact identifier.
  const blob = new Blob([artifactBytes(artifact)], { type: "application/octet-stream" });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = artifact.artifact_id;
  anchor.click();
  URL.revokeObjectURL(url);
}

export function ArtifactBrowser({ runId }: { runId: string }) {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const index = useQuery({
    queryKey: ["artifacts", runId],
    queryFn: () => benchmarkApi.artifacts(runId),
  });
  const content = useQuery({
    queryKey: ["artifact", runId, selectedId],
    queryFn: () => benchmarkApi.artifact(runId, selectedId ?? ""),
    enabled: selectedId !== null,
  });

  if (index.isPending) return <LoadingState label="Loading allowlisted artifacts" />;
  if (index.error) return <ErrorState error={index.error} retry={() => void index.refetch()} />;
  if (!index.data || index.data.artifacts.length === 0) {
    return <Text c="dimmed">The runner returned no allowlisted artifacts for this run.</Text>;
  }

  return (
    <Stack>
      <Alert color="blue" title="Immutable, allowlisted evidence">
        Artifact identifiers are server-defined. Paths and arbitrary filesystem reads are never accepted by this browser.
      </Alert>
      <Table.ScrollContainer
        minWidth={760}
        scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Allowlisted run artifacts" } }}
      >
        <Table striped highlightOnHover>
          <Table.Thead>
            <Table.Tr>
              <Table.Th>Artifact</Table.Th>
              <Table.Th>Media type</Table.Th>
              <Table.Th>Size</Table.Th>
              <Table.Th>SHA-256</Table.Th>
              <Table.Th><span className="sr-only">Actions</span></Table.Th>
            </Table.Tr>
          </Table.Thead>
          <Table.Tbody>
            {index.data.artifacts.map((artifact) => (
              <Table.Tr key={artifact.artifact_id}>
                <Table.Td><Text fw={600}>{artifact.label}</Text><Code>{artifact.artifact_id}</Code></Table.Td>
                <Table.Td><Badge variant="light">{artifact.media_type}</Badge></Table.Td>
                <Table.Td>{formatBytes(artifact.size_bytes)}</Table.Td>
                <Table.Td><Code className="wrap-anywhere">{artifact.sha256}</Code></Table.Td>
                <Table.Td><Button variant="light" size="compact-sm" onClick={() => setSelectedId(artifact.artifact_id)}>Inspect</Button></Table.Td>
              </Table.Tr>
            ))}
          </Table.Tbody>
        </Table>
      </Table.ScrollContainer>

      {selectedId ? (
        <Card withBorder padding="md">
          {content.isPending ? <LoadingState label={`Loading ${selectedId}`} /> : null}
          {content.error ? <ErrorState error={content.error} retry={() => void content.refetch()} /> : null}
          {content.data ? (
            <Stack>
              <Group justify="space-between" align="flex-start">
                <div>
                  <Title order={3} size="h4">{content.data.label}</Title>
                  <Text size="xs" ff="monospace" className="wrap-anywhere">SHA-256 {content.data.sha256}</Text>
                </div>
                <Button variant="default" onClick={() => downloadArtifact(content.data)}>Download</Button>
              </Group>
              {content.data.encoding === "utf-8" ? (
                <pre className="artifact-preview" tabIndex={0}>{content.data.content}</pre>
              ) : (
                <Text c="dimmed">Binary evidence is available for download; the base64 transport is not rendered as text.</Text>
              )}
            </Stack>
          ) : null}
        </Card>
      ) : null}
    </Stack>
  );
}
