import { Alert, Button, Center, Loader, Stack, Text } from "@mantine/core";
import { AlertTriangle } from "lucide-react";
import { errorMessage } from "@/lib/format";

export function LoadingState({ label = "Loading runner data" }: { label?: string }) {
  return (
    <Center mih={180} role="status" aria-live="polite">
      <Stack align="center" gap="sm">
        <Loader aria-hidden="true" />
        <Text c="dimmed">{label}…</Text>
      </Stack>
    </Center>
  );
}

export function ErrorState({ error, retry }: { error: unknown; retry?: () => void }) {
  return (
    <Alert color="red" icon={<AlertTriangle aria-hidden="true" />} title="Runner request failed" role="alert">
      <Stack gap="sm" align="flex-start">
        <Text>{errorMessage(error)}</Text>
        {retry ? (
          <Button color="red" variant="light" onClick={retry}>
            Try again
          </Button>
        ) : null}
      </Stack>
    </Alert>
  );
}
