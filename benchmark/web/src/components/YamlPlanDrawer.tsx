import { Alert, Badge, Button, Code, Drawer, Group, Modal, Stack, Text, Textarea, Title } from "@mantine/core";
import { useDisclosure } from "@mantine/hooks";
import { useMutation } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { parse, stringify } from "yaml";
import { benchmarkApi } from "@/api/client";
import type { ExperimentPlan, PlanValidationResponse } from "@/api/types";
import { errorMessage } from "@/lib/format";

function parsePlan(source: string): ExperimentPlan {
  const value: unknown = parse(source);
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("The YAML document must contain one experiment plan mapping.");
  }
  return value as ExperimentPlan;
}

export function YamlPlanDrawer({
  opened,
  close,
  plan,
  defaultPlan,
  customized,
  onImport,
  onReset,
}: {
  opened: boolean;
  close: () => void;
  plan: ExperimentPlan;
  defaultPlan: ExperimentPlan;
  customized: boolean;
  onImport: (plan: ExperimentPlan) => void;
  onReset: () => void;
}) {
  const [confirmOpened, confirm] = useDisclosure(false);
  const currentYaml = useMemo(() => stringify(plan, { lineWidth: 100 }), [plan]);
  const defaultYaml = useMemo(() => stringify(defaultPlan, { lineWidth: 100 }), [defaultPlan]);
  const [source, setSource] = useState(currentYaml);
  const [parseError, setParseError] = useState<string | null>(null);
  const [pending, setPending] = useState<PlanValidationResponse | null>(null);
  const validation = useMutation({
    mutationFn: (candidate: ExperimentPlan) => benchmarkApi.validatePlan({ plan: candidate, starting_preset: null }),
    onSuccess: (response) => {
      setPending(response);
      setParseError(null);
      if (customized) confirm.open();
      else applyImport(response);
    },
  });

  const applyImport = (response: PlanValidationResponse) => {
    onImport(response.canonical_plan);
    setSource(stringify(response.canonical_plan, { lineWidth: 100 }));
    setPending(null);
    confirm.close();
  };
  const validateImport = () => {
    try {
      setParseError(null);
      validation.mutate(parsePlan(source));
    } catch (error) {
      setParseError(errorMessage(error));
    }
  };
  const copy = async () => {
    await navigator.clipboard.writeText(currentYaml);
  };

  return (
    <>
      <Drawer
        opened={opened}
        onClose={close}
        title="Inspect configuration YAML"
        position="right"
        size={640}
        className="yaml-plan-drawer"
      >
        <Stack>
          <Alert color="blue" title="Portable experiment document">
            This YAML contains factors, protocol, image, and client cohort. It never contains the Test workspace root, credentials, gateway mode, retention, logging, internal paths, or safety caps.
          </Alert>
          <Group justify="space-between" wrap="wrap">
            <Group>
              <Badge color={customized ? "yellow" : "green"}>{customized ? "Differs from Default" : "Matches Default"}</Badge>
              <Text size="sm">Semantic round-trip preserves explicit roles, values, and controls.</Text>
            </Group>
            <Button variant="default" onClick={() => void copy()}>Copy current YAML</Button>
          </Group>

          <Textarea
            label="YAML to validate and import"
            description="Import is accepted only after the runner parses, validates, and returns its canonical plan."
            value={source}
            onChange={(event) => setSource(event.currentTarget.value)}
            autosize
            minRows={18}
            maxRows={30}
            styles={{ input: { fontFamily: '"Fira Code", ui-monospace, monospace', fontSize: "0.8rem" } }}
          />
          {parseError ? <Alert color="red" title="YAML cannot be parsed">{parseError}</Alert> : null}
          {validation.error ? <Alert color="red" title="Runner rejected this document">{errorMessage(validation.error)}</Alert> : null}
          {validation.data && !validation.data.runnable ? (
            <Alert color="yellow" title="Canonical plan imported with validation findings">
              The typed document is valid, but the runner reports {validation.data.validation.filter(({ severity }) => severity === "error").length} blocking finding(s). The main builder will show them.
            </Alert>
          ) : null}
          <Group>
            <Button onClick={validateImport} loading={validation.isPending}>Validate & import</Button>
            <Button variant="default" onClick={() => setSource(currentYaml)}>Restore current text</Button>
            <Button
              variant="light"
              color="yellow"
              onClick={() => { onReset(); setSource(defaultYaml); }}
              disabled={!customized}
            >
              Reset to Default
            </Button>
          </Group>

          <details>
            <summary><Text span fw={600}>Compare with Default YAML</Text></summary>
            <Title order={3} size="h5" mt="md">Discovered Default configuration</Title>
            <Code block className="yaml-default-preview">{defaultYaml}</Code>
          </details>
        </Stack>
      </Drawer>

      <Modal opened={confirmOpened} onClose={confirm.close} title="Discard the current customized draft?" centered>
        <Stack>
          <Text>Importing the runner-canonical YAML replaces every current plan field. The Default configuration remains unchanged.</Text>
          <Group justify="flex-end">
            <Button variant="default" onClick={confirm.close}>Keep current draft</Button>
            <Button color="yellow" onClick={() => { if (pending) applyImport(pending); }}>Discard & import</Button>
          </Group>
        </Stack>
      </Modal>
    </>
  );
}
