import {
  Accordion,
  Alert,
  Badge,
  Box,
  Button,
  Card,
  Checkbox,
  Divider,
  Group,
  List,
  Modal,
  NumberInput,
  Select,
  SimpleGrid,
  Stack,
  Table,
  Text,
  Title,
} from "@mantine/core";
import { useDisclosure } from "@mantine/hooks";
import { useMutation, useQuery } from "@tanstack/react-query";
import { AlertTriangle, CheckCircle2, RotateCcw, SlidersHorizontal } from "lucide-react";
import { useState } from "react";
import { useNavigate } from "react-router";
import { benchmarkApi } from "@/api/client";
import type {
  ConfigurationScope,
  DefinitionsResponse,
  ExpandedOperationCell,
  ExperimentPlan,
  Factor,
  FactorId,
  FactorRole,
  HealthResponse,
  OperationPlan,
  PlanValidationResponse,
  PresetFile,
  PresetRef,
} from "@/api/types";
import { ErrorState, LoadingState } from "@/components/AsyncState";
import {
  hasCompleteUiDefinitions,
  OperationControlSwitch,
  setOperationEnabled,
} from "@/components/OperationControls";
import { YamlPlanDrawer } from "@/components/YamlPlanDrawer";
import { errorMessage, formatBytes, formatDurationNs, formatInteger, formatNumber } from "@/lib/format";

function clonePlan(plan: ExperimentPlan): ExperimentPlan {
  return structuredClone(plan);
}

function EstimateValue({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <Text size="xs" c="dimmed" tt="uppercase" fw={600}>{label}</Text>
      <Text ff="monospace" fw={600}>{value}</Text>
    </div>
  );
}

function ExpandedWorkLabel({ operation }: { operation: ExpandedOperationCell }) {
  switch (operation.operation) {
    case "exec_command":
      return <Text size="sm">{operation.cell.concurrent_requests} request(s); command <Text span ff="monospace">{operation.cell.command}</Text></Text>;
    case "file_read":
    case "file_write":
    case "file_edit":
    case "file_blame":
      return <Text size="sm">{operation.cell.concurrent_requests} product request(s) in the batch</Text>;
    case "create_workspace":
      return <Text size="sm">{operation.cell.workspace_count} independent workspace create request(s)</Text>;
    case "squash_layerstack":
      return <Text size="sm">One squash request after preparing N={operation.cell.live_sessions} live session(s)</Text>;
  }
}

interface FactorSummaryEntry {
  operation: OperationPlan["operation"];
  factorId: FactorId;
  role: FactorRole;
  values: unknown[];
  control: unknown | null;
}

function summaryEntry(
  operation: OperationPlan["operation"],
  factorId: FactorId,
  factor: Factor<unknown>,
): FactorSummaryEntry {
  return { operation, factorId, ...factor };
}

function operationFactorSummary(operation: OperationPlan): FactorSummaryEntry[] {
  switch (operation.operation) {
    case "exec_command": {
      const factors = operation.configuration.factors;
      return [
        summaryEntry(operation.operation, "concurrent_requests", factors.concurrent_requests),
        summaryEntry(operation.operation, "workspace_profile", factors.workspace_profile),
        summaryEntry(operation.operation, "session_mode", factors.session_mode),
        summaryEntry(operation.operation, "command_case", factors.command_case),
      ];
    }
    case "file_read": {
      const factors = operation.configuration.factors;
      return [
        summaryEntry(operation.operation, "concurrent_requests", factors.concurrent_requests),
        summaryEntry(operation.operation, "returned_bytes", factors.returned_bytes),
        summaryEntry(operation.operation, "read_source", factors.source),
        summaryEntry(operation.operation, "target_mode", factors.target_mode),
      ];
    }
    case "file_write": {
      const factors = operation.configuration.factors;
      return [
        summaryEntry(operation.operation, "concurrent_requests", factors.concurrent_requests),
        summaryEntry(operation.operation, "content_bytes", factors.content_bytes),
        summaryEntry(operation.operation, "mutation_destination", factors.destination),
        summaryEntry(operation.operation, "target_mode", factors.target_mode),
      ];
    }
    case "file_edit": {
      const factors = operation.configuration.factors;
      return [
        summaryEntry(operation.operation, "concurrent_requests", factors.concurrent_requests),
        summaryEntry(operation.operation, "file_bytes", factors.file_bytes),
        summaryEntry(operation.operation, "replacement_count", factors.replacement_count),
        summaryEntry(operation.operation, "match_density", factors.match_density),
        summaryEntry(operation.operation, "mutation_destination", factors.destination),
        summaryEntry(operation.operation, "target_mode", factors.target_mode),
      ];
    }
    case "file_blame": {
      const factors = operation.configuration.factors;
      return [
        summaryEntry(operation.operation, "concurrent_requests", factors.concurrent_requests),
        summaryEntry(operation.operation, "line_count", factors.line_count),
        summaryEntry(operation.operation, "ownership_segments", factors.ownership_segments),
        summaryEntry(operation.operation, "auditability_event_count", factors.auditability_event_count),
      ];
    }
    case "create_workspace": {
      const factors = operation.configuration.factors;
      return [
        summaryEntry(operation.operation, "workspace_count", factors.workspace_count),
        summaryEntry(operation.operation, "workspace_profile", factors.workspace_profile),
        summaryEntry(operation.operation, "network_profile", factors.network_profile),
      ];
    }
    case "squash_layerstack": {
      const factors = operation.configuration.factors;
      return [
        summaryEntry(operation.operation, "live_sessions", factors.live_sessions),
        summaryEntry(operation.operation, "requested_migration_ratio", factors.requested_migration_ratio),
        summaryEntry(operation.operation, "remount_parallelism", factors.remount_parallelism),
        summaryEntry(operation.operation, "squashable_blocks", factors.squashable_blocks),
        summaryEntry(operation.operation, "layers_per_block", factors.layers_per_block),
        summaryEntry(operation.operation, "payload_bytes", factors.payload_bytes),
        summaryEntry(operation.operation, "session_activity", factors.session_activity),
      ];
    }
    default:
      return assertNeverOperation(operation);
  }
}

function assertNeverOperation(operation: never): never {
  throw new Error(`Unhandled operation summary: ${JSON.stringify(operation)}`);
}

function FactorRoleSummary({
  plan,
  definitions,
}: {
  plan: ExperimentPlan;
  definitions: DefinitionsResponse;
}) {
  const entries = plan.operations
    .filter(({ configuration }) => configuration.enabled)
    .flatMap(operationFactorSummary);
  const valueLabel = (entry: FactorSummaryEntry, value: unknown): string => {
    const definition = definitions.catalog.operations
      .find(({ id }) => id === entry.operation)?.factors
      .find(({ id }) => id === entry.factorId);
    if (typeof value === "number") {
      if (definition?.unit === "bytes") return formatBytes(value);
      return definition?.unit === "ratio" ? formatNumber(value, 3) : formatInteger(value);
    }
    return String(value).replaceAll("_", " ");
  };
  const renderFactor = (entry: FactorSummaryEntry) => {
    const operation = definitions.catalog.operations.find(({ id }) => id === entry.operation);
    const factor = operation?.factors.find(({ id }) => id === entry.factorId);
    const values = entry.values.map((value) => valueLabel(entry, value)).join(", ");
    const control = entry.role === "varied" && entry.control !== null
      ? `; control ${valueLabel(entry, entry.control)}`
      : "";
    return `${factor?.label ?? entry.factorId}: ${values}${control}`;
  };
  const groups = (role: FactorRole) => plan.operations.flatMap((operation) => {
    if (!operation.configuration.enabled) return [];
    const operationEntries = entries.filter((entry) =>
      entry.operation === operation.operation && entry.role === role
    );
    if (operationEntries.length === 0) return [];
    const definition = definitions.catalog.operations.find(({ id }) => id === operation.operation);
    return [{
      operation: operation.operation,
      label: definition?.label ?? operation.operation,
      summary: operationEntries.map(renderFactor).join(" · "),
    }];
  });

  const varied = groups("varied");
  const controlled = groups("controlled");

  return (
    <SimpleGrid cols={{ base: 1, md: 2 }}>
      <div>
        <Text fw={700}>Changes across test combinations</Text>
        {varied.length === 0 ? (
          <Text size="sm" c="dimmed" mt={4}>No factors vary in this bounded setup.</Text>
        ) : (
          <List size="sm" spacing={4} mt={4}>
            {varied.map(({ operation, label, summary }) => (
              <List.Item key={operation}><Text span fw={600}>{label}</Text> — {summary}</List.Item>
            ))}
          </List>
        )}
      </div>
      <div>
        <Text fw={700}>Values held constant</Text>
        {controlled.length === 0 ? (
          <Text size="sm" c="dimmed" mt={4}>No factors are held constant.</Text>
        ) : (
          <List size="sm" spacing={4} mt={4}>
            {controlled.map(({ operation, label, summary }) => (
              <List.Item key={operation}><Text span fw={600}>{label}</Text> — {summary}</List.Item>
            ))}
          </List>
        )}
      </div>
    </SimpleGrid>
  );
}

export function PlanReviewModal({
  opened,
  close,
  validation,
  health,
  healthPending,
  healthError,
  retryHealth,
  startingPreset,
}: {
  opened: boolean;
  close: () => void;
  validation: PlanValidationResponse;
  health: HealthResponse | undefined;
  healthPending: boolean;
  healthError: unknown;
  retryHealth: () => void;
  startingPreset: PresetRef | null;
}) {
  const navigate = useNavigate();
  const start = useMutation({
    mutationFn: () =>
      benchmarkApi.createRun({
        plan: validation.canonical_plan,
        plan_hash: validation.plan_hash,
        client_request_id: crypto.randomUUID(),
        starting_preset: startingPreset,
      }),
    onSuccess: ({ run_id }) => void navigate(`/benchmark/runs/${encodeURIComponent(run_id)}`),
  });
  const estimates = validation.estimates;
  const activeRunId = health?.active_run?.run_id ?? null;
  const runnerReady = health?.execution_ready === true && activeRunId === null;
  return (
    <Modal
      opened={opened}
      onClose={close}
      title="Review exact local benchmark run"
      size="xl"
      classNames={{ body: "review-modal-body" }}
    >
      <Stack>
        <Alert
          color={validation.runnable ? "green" : "red"}
          icon={validation.runnable ? <CheckCircle2 aria-hidden="true" /> : <AlertTriangle aria-hidden="true" />}
          title={validation.runnable ? "Canonical validation passed" : "This plan cannot run"}
        >
          Counts, cells, protocol, command preview, and estimates below are authored by the runner from this exact canonical plan.
        </Alert>
        <SimpleGrid cols={{ base: 1, sm: 3 }}>
          <EstimateValue label="Test combinations" value={formatInteger(estimates.cell_count)} />
          <EstimateValue label="Trial batches" value={formatInteger(estimates.trial_batch_count)} />
          <EstimateValue label="Issued product requests" value={formatInteger(estimates.issued_operation_request_count)} />
          <EstimateValue label="Estimated duration" value={`${formatDurationNs(estimates.duration_range.minimum_ns)}–${formatDurationNs(estimates.duration_range.maximum_ns)}`} />
          <EstimateValue label="Estimated peak disk" value={formatBytes(estimates.estimated_peak_disk_bytes)} />
          <EstimateValue label="Free space required" value={formatBytes(estimates.required_free_space_bytes)} />
        </SimpleGrid>
        <Divider />
        <SimpleGrid cols={{ base: 1, sm: 2 }}>
          <div>
            <Text size="sm" c="dimmed">Effective test workspace root</Text>
            <Text ff="monospace" className="wrap-anywhere">{validation.effective_environment.test_workspace_root}</Text>
          </div>
          <div>
            <Text size="sm" c="dimmed">Validated plan hash used at start</Text>
            <Text ff="monospace" className="wrap-anywhere" data-testid="review-plan-hash">{validation.plan_hash}</Text>
          </div>
        </SimpleGrid>
        <Text>
          {validation.execution_blocks.length} sequential execution block(s); {estimates.gateway_restart_count} isolated gateway restart(s). Automatic retries: {validation.fixed_lifecycle_policy.automatic_retries}.
        </Text>
        <Alert color="blue" title="Fixed local safety and cleanup boundary">
          Isolated gateway · one active campaign: {validation.fixed_lifecycle_policy.one_active_campaign ? "yes" : "no"} · sequential families: {validation.fixed_lifecycle_policy.sequential_families ? "yes" : "no"} · lifecycle revision {validation.fixed_lifecycle_policy.lifecycle_revision}. Cleanup is runner-owned and cannot be overridden by this plan.
        </Alert>
        {startingPreset ? (
          <Text>Starting preset provenance: <Text span ff="monospace">{startingPreset.id}/v{startingPreset.version}</Text>. It is not part of the plan hash.</Text>
        ) : null}
        <div>
          <Title order={3} size="h4" mb="sm">Expanded cells and effective protocol</Title>
          {validation.cells.length === 0 ? (
            <Alert color="red" title="No expanded cells">The runner returned no executable test combinations.</Alert>
          ) : (
            <Table.ScrollContainer
              minWidth={760}
              className="review-cell-table"
              scrollAreaProps={{ viewportProps: { tabIndex: 0, "aria-label": "Expanded benchmark cells" } }}
            >
              <Table>
                <Table.Thead>
                  <Table.Tr>
                    <Table.Th>Cell</Table.Th>
                    <Table.Th>Operation</Table.Th>
                    <Table.Th>Effective protocol</Table.Th>
                    <Table.Th>Work in one trial</Table.Th>
                  </Table.Tr>
                </Table.Thead>
                <Table.Tbody>
                  {validation.cells.map((cell) => (
                    <Table.Tr key={cell.cell_id}>
                      <Table.Td><Text ff="monospace" size="xs">{cell.cell_id}</Text></Table.Td>
                      <Table.Td>{cell.operation_id}</Table.Td>
                      <Table.Td>
                        {cell.protocol.warmups} warmup(s) + {cell.protocol.measured_trials} measured trial(s)
                        <Text size="xs" c="dimmed">
                          {cell.protocol.destructive ? "Destructive isolation" : "Reusable verified fixture"} · timeout {formatInteger(cell.protocol.timeout_ms)} ms · cleanup {cell.protocol.cleanup.replaceAll("_", " ")}
                        </Text>
                        <Text size="xs" c="dimmed">Operation rev {cell.operation_semantic_revision} · factor schema rev {cell.factor_schema_revision}</Text>
                      </Table.Td>
                      <Table.Td><ExpandedWorkLabel operation={cell.operation} /></Table.Td>
                    </Table.Tr>
                  ))}
                </Table.Tbody>
              </Table>
            </Table.ScrollContainer>
          )}
        </div>
        {validation.validation.length > 0 ? (
          <List spacing="xs" aria-label="Validation findings">
            {validation.validation.map((finding, index) => (
              <List.Item key={`${finding.code}-${finding.path ?? "plan"}-${index}`}>
                <Badge color={finding.severity === "error" ? "red" : finding.severity === "warning" ? "yellow" : "gray"} mr="xs">
                  {finding.severity}
                </Badge>
                {finding.path ? <Text span ff="monospace">{finding.path}: </Text> : null}{finding.message}
              </List.Item>
            ))}
          </List>
        ) : null}
        {estimates.warnings.map((warning, index) => (
          <Alert key={`${warning}-${index}`} color="yellow" title="Estimate warning">{warning}</Alert>
        ))}
        {healthPending ? <Alert color="gray" title="Checking execution readiness">Start remains disabled while the runner admission state is loading.</Alert> : null}
        {healthError ? <ErrorState error={healthError} retry={retryHealth} /> : null}
        {activeRunId ? (
          <Alert color="yellow" title="Another campaign is active">
            Run {activeRunId} must reach a terminal state before this campaign can start.
          </Alert>
        ) : health && !health.execution_ready ? (
          <Alert color="yellow" title="Runner admission is not ready">
            Start remains disabled until the runner confirms execution readiness.
          </Alert>
        ) : null}
        {start.error ? <Alert color="red" title="Run did not start">{errorMessage(start.error)}</Alert> : null}
        <Group className="review-footer" justify="flex-end">
          <Button variant="default" onClick={close}>Back</Button>
          <Button
            onClick={() => start.mutate()}
            loading={start.isPending}
            disabled={!validation.runnable || !runnerReady || healthPending || Boolean(healthError)}
          >
            Start local run
          </Button>
        </Group>
      </Stack>
    </Modal>
  );
}

function ValidationState({ validation }: { validation: ReturnType<typeof useQuery<PlanValidationResponse>> }) {
  if (validation.isPending) {
    return <Text role="status" c="dimmed">Updating canonical validation and estimates…</Text>;
  }
  if (validation.error) {
    return <ErrorState error={validation.error} retry={() => void validation.refetch()} />;
  }
  if (!validation.data) {
    return <Alert color="red" title="Validation response is empty">Review and start are blocked.</Alert>;
  }
  const errors = validation.data.validation.filter(({ severity }) => severity === "error");
  const warnings = validation.data.validation.filter(({ severity }) => severity === "warning");
  return (
    <Stack gap="xs">
      <Group gap="xs">
        <Badge color={validation.data.runnable ? warnings.length > 0 ? "yellow" : "green" : "red"}>
          {validation.data.runnable ? warnings.length > 0 ? "Current with warnings" : "Current" : "Invalid"}
        </Badge>
        <Text size="sm">Runner-authored canonical estimate</Text>
      </Group>
      {[...errors, ...warnings].map((finding, index) => (
        <Alert key={`${finding.code}-${finding.path ?? "plan"}-${index}`} color={finding.severity === "error" ? "red" : "yellow"} title={finding.code}>
          {finding.path ? <Text span ff="monospace">{finding.path}: </Text> : null}{finding.message}
        </Alert>
      ))}
    </Stack>
  );
}

function ProtocolAndEnvironmentControls({
  plan,
  definitions,
  onChange,
}: {
  plan: ExperimentPlan;
  definitions: DefinitionsResponse;
  onChange: (plan: ExperimentPlan) => void;
}) {
  const enabledDefinitions = plan.operations
    .filter(({ configuration }) => configuration.enabled)
    .map((operation) => definitions.catalog.operations.find(({ id }) => id === operation.operation));
  const supportedCohorts = enabledDefinitions[0]?.supported_cohorts.filter((cohort) =>
    enabledDefinitions.every((definition) => definition?.supported_cohorts.includes(cohort)),
  ) ?? [];
  const number = (value: string | number, fallback: number) => typeof value === "number" ? value : fallback;

  return (
    <Card withBorder padding="lg">
      <Stack>
        <div>
          <Title order={3} size="h4">Protocol and environment cohort</Title>
          <Text size="sm" c="dimmed">These are portable plan inputs. Workspace binding and safety policy remain runner-owned and read-only.</Text>
        </div>
        <SimpleGrid cols={{ base: 1, sm: 2, lg: 3 }}>
          <NumberInput label="Seed" value={plan.seed} min={0} allowDecimal={false} onChange={(value) => onChange({ ...plan, seed: number(value, plan.seed) })} />
          <NumberInput label="Resource interval" description="Milliseconds; runner validates 20–1000 ms." suffix=" ms" value={plan.protocol.resource_interval_ms} min={20} max={1000} allowDecimal={false} onChange={(value) => onChange({ ...plan, protocol: { ...plan.protocol, resource_interval_ms: number(value, plan.protocol.resource_interval_ms) } })} />
          <Select label="Client cohort" data={supportedCohorts.map((value) => ({ value, label: value === "direct_client" ? "Direct client" : "CLI end to end" }))} value={plan.environment.client_cohort} onChange={(value) => { if (value === "direct_client" || value === "cli_e2e") onChange({ ...plan, environment: { ...plan.environment, client_cohort: value } }); }} allowDeselect={false} />
          <NumberInput label="Fast warmups" value={plan.protocol.trial_defaults.fast.warmups} min={0} allowDecimal={false} onChange={(value) => onChange({ ...plan, protocol: { ...plan.protocol, trial_defaults: { ...plan.protocol.trial_defaults, fast: { ...plan.protocol.trial_defaults.fast, warmups: number(value, plan.protocol.trial_defaults.fast.warmups) } } } })} />
          <NumberInput label="Fast measured trials" value={plan.protocol.trial_defaults.fast.measured_trials} min={1} allowDecimal={false} onChange={(value) => onChange({ ...plan, protocol: { ...plan.protocol, trial_defaults: { ...plan.protocol.trial_defaults, fast: { ...plan.protocol.trial_defaults.fast, measured_trials: number(value, plan.protocol.trial_defaults.fast.measured_trials) } } } })} />
          <NumberInput label="Destructive warmups" value={plan.protocol.trial_defaults.destructive.warmups} min={0} allowDecimal={false} onChange={(value) => onChange({ ...plan, protocol: { ...plan.protocol, trial_defaults: { ...plan.protocol.trial_defaults, destructive: { ...plan.protocol.trial_defaults.destructive, warmups: number(value, plan.protocol.trial_defaults.destructive.warmups) } } } })} />
          <NumberInput label="Destructive measured trials" value={plan.protocol.trial_defaults.destructive.measured_trials} min={1} allowDecimal={false} onChange={(value) => onChange({ ...plan, protocol: { ...plan.protocol, trial_defaults: { ...plan.protocol.trial_defaults, destructive: { ...plan.protocol.trial_defaults.destructive, measured_trials: number(value, plan.protocol.trial_defaults.destructive.measured_trials) } } } })} />
        </SimpleGrid>
      </Stack>
    </Card>
  );
}

function isFileOperation(operation: OperationPlan): boolean {
  switch (operation.operation) {
    case "file_read":
    case "file_write":
    case "file_edit":
    case "file_blame":
      return true;
    case "exec_command":
    case "create_workspace":
    case "squash_layerstack":
      return false;
  }
}

function FileOperationEditor({
  operations,
  definitions,
  onChange,
}: {
  operations: OperationPlan[];
  definitions: DefinitionsResponse;
  onChange: (index: number, operation: OperationPlan) => void;
}) {
  const items = operations
    .map((operation, index) => ({
      operation,
      index,
      definition: definitions.catalog.operations.find(({ id }) => id === operation.operation),
    }))
    .filter(({ operation }) => isFileOperation(operation));
  const [selectedOperation, setSelectedOperation] = useState<OperationPlan["operation"] | null>(
    items[0]?.operation.operation ?? null,
  );
  const selected = items.find(({ operation }) => operation.operation === selectedOperation) ?? items[0];
  const renderControls = ({ operation, index, definition }: (typeof items)[number]) => definition ? (
    <OperationControlSwitch
      operation={operation}
      definition={definition}
      profiles={definitions.catalog.workspace_profiles}
      onChange={(next) => onChange(index, next)}
      showIncludeToggle={false}
    />
  ) : (
    <Alert color="red" title="Operation definition missing">
      No explicit control can be rendered for <Text span ff="monospace">{operation.operation}</Text>.
    </Alert>
  );
  const include = ({ operation, index, definition }: (typeof items)[number]) => (
    <Checkbox
      className="file-operation-checkbox"
      aria-label={`Include ${definition?.label ?? operation.operation}`}
      checked={operation.configuration.enabled}
      onChange={(event) => onChange(index, setOperationEnabled(operation, event.currentTarget.checked))}
    />
  );

  if (items.length === 0) {
    return <Alert color="red" title="No file operations">This Files plan has no typed file operation controls.</Alert>;
  }

  return (
    <>
      <Box visibleFrom="md">
        <div className="file-master-detail">
          <Stack gap="xs" role="list" aria-label="File operations included">
            {items.map((item) => (
              <Group key={item.operation.operation} gap="xs" wrap="nowrap" role="listitem">
                {include(item)}
                <Button
                  variant={selected?.operation.operation === item.operation.operation ? "light" : "subtle"}
                  fullWidth
                  justify="space-between"
                  onClick={() => setSelectedOperation(item.operation.operation)}
                  aria-pressed={selected?.operation.operation === item.operation.operation}
                >
                  <span>{item.definition?.label ?? item.operation.operation}</span>
                  <Badge color={item.operation.configuration.enabled ? "green" : "gray"} variant="light">
                    {item.operation.configuration.enabled ? "Included" : "Excluded"}
                  </Badge>
                </Button>
              </Group>
            ))}
          </Stack>
          <Box className="file-operation-detail">
            {selected ? renderControls(selected) : null}
          </Box>
        </div>
      </Box>
      <Box hiddenFrom="md">
        <Accordion multiple defaultValue={items.filter(({ operation }) => operation.configuration.enabled).map(({ operation }) => operation.operation)}>
          {items.map((item) => (
            <Accordion.Item key={item.operation.operation} value={item.operation.operation}>
              <Group gap="xs" wrap="nowrap" className="file-accordion-heading">
                {include(item)}
                <Accordion.Control>
                  <Group gap="xs">
                    <Text fw={600}>{item.definition?.label ?? item.operation.operation}</Text>
                    <Badge color={item.operation.configuration.enabled ? "green" : "gray"} variant="light">
                      {item.operation.configuration.enabled ? "Included" : "Excluded"}
                    </Badge>
                  </Group>
                </Accordion.Control>
              </Group>
              <Accordion.Panel>{renderControls(item)}</Accordion.Panel>
            </Accordion.Item>
          ))}
        </Accordion>
      </Box>
    </>
  );
}

function LoadedPlanLauncher({
  scope,
  definitions,
  defaultPlan,
}: {
  scope: ConfigurationScope;
  definitions: DefinitionsResponse;
  defaultPlan: ExperimentPlan;
}) {
  const [reviewOpened, review] = useDisclosure(false);
  const [yamlOpened, yaml] = useDisclosure(false);
  const [customizing, setCustomizing] = useState(false);
  const [draft, setDraft] = useState(() => clonePlan(defaultPlan));
  const [startingPreset, setStartingPreset] = useState<PresetRef | null>(null);
  const [selectedPresetKey, setSelectedPresetKey] = useState<string | null>(null);
  const currentPlan = customizing ? draft : defaultPlan;
  const presets = definitions.presets.filter((preset) => preset.plan.configuration_base.scope === scope);
  const validation = useQuery<PlanValidationResponse>({
    queryKey: ["plan-validation", currentPlan, startingPreset],
    queryFn: () => benchmarkApi.validatePlan({ plan: currentPlan, starting_preset: startingPreset }),
    retry: false,
    staleTime: 0,
  });
  const health = useQuery({ queryKey: ["health"], queryFn: benchmarkApi.health });
  const uiDefinitionsComplete = hasCompleteUiDefinitions(currentPlan.operations, definitions.catalog);
  const selectedPreset = presets.find(({ id, version }) => `${id}:${version}` === selectedPresetKey);
  const quickSmoke = scope === "all" ? presets.find(({ id }) => id === "quick-smoke") : undefined;

  const operationNames = currentPlan.operations
    .filter(({ configuration }) => configuration.enabled)
    .map(({ operation }) => definitions.catalog.operations.find(({ id }) => id === operation)?.label ?? operation);
  const updateOperation = (index: number, operation: OperationPlan) => {
    setDraft((plan) => ({
      ...plan,
      operations: plan.operations.map((candidate, candidateIndex) => candidateIndex === index ? operation : candidate),
    }));
  };
  const loadPreset = (preset: PresetFile) => {
    setDraft(clonePlan(preset.plan));
    setStartingPreset({ id: preset.id, version: preset.version });
    setCustomizing(true);
  };
  const resetAll = () => {
    setDraft(clonePlan(defaultPlan));
    setStartingPreset(null);
    setSelectedPresetKey(null);
    setCustomizing(false);
    review.close();
  };
  const importPlan = (plan: ExperimentPlan) => {
    setDraft(clonePlan(plan));
    setStartingPreset(null);
    setSelectedPresetKey(null);
    setCustomizing(true);
  };
  const reviewPlan = async () => {
    const result = await validation.refetch();
    if (result.data?.runnable && uiDefinitionsComplete) review.open();
  };
  const currentValidation = validation.data;

  return (
    <Stack gap="lg">
      <Card withBorder padding="lg">
        <Stack>
          <Group justify="space-between" align="flex-start" wrap="wrap">
            <div>
              <Text size="sm" c="dimmed">{customizing ? currentValidation?.is_customized ? "Customized configuration" : "Customizing — unchanged" : "Default configuration"}</Text>
              <Title order={2} size="h3">{currentPlan.name}</Title>
            </div>
            <Group gap="xs">
              {startingPreset ? <Badge color="blue">Preset {startingPreset.id}/v{startingPreset.version}</Badge> : null}
              <Badge variant="light">Default v{defaultPlan.configuration_base.version}</Badge>
            </Group>
          </Group>
          {operationNames.length > 0 ? <Text>{operationNames.join(" · ")}</Text> : <Alert color="red" title="No enabled operations">At least one registered operation must be enabled.</Alert>}
          <Text size="sm" c="dimmed">
            {scope === "all"
              ? "Enabled families execute sequentially: Command → Files → Workspace → LayerStack."
              : "The runner expands this typed family plan and authors all counts and estimates."}
          </Text>
          {currentValidation ? (
            <SimpleGrid cols={{ base: 1, sm: 2, lg: 4 }} className="estimate-strip">
              <EstimateValue label="Test combinations" value={formatInteger(currentValidation.estimates.cell_count)} />
              <EstimateValue label="Trial batches" value={formatInteger(currentValidation.estimates.trial_batch_count)} />
              <EstimateValue label="Issued product requests" value={formatInteger(currentValidation.estimates.issued_operation_request_count)} />
              <EstimateValue label="Estimated duration" value={`${formatDurationNs(currentValidation.estimates.duration_range.minimum_ns)}–${formatDurationNs(currentValidation.estimates.duration_range.maximum_ns)}`} />
              <EstimateValue label="Estimated peak disk" value={formatBytes(currentValidation.estimates.estimated_peak_disk_bytes)} />
              <EstimateValue label="Required free space" value={formatBytes(currentValidation.estimates.required_free_space_bytes)} />
              <EstimateValue label="Available free space" value={formatBytes(currentValidation.effective_environment.free_space_bytes)} />
              <EstimateValue label="Gateway restarts" value={formatInteger(currentValidation.estimates.gateway_restart_count)} />
            </SimpleGrid>
          ) : null}
          <FactorRoleSummary plan={currentPlan} definitions={definitions} />
          <SimpleGrid cols={{ base: 1, sm: 2, lg: 3 }}>
            <Text size="sm"><strong>Fast protocol</strong><br />{currentPlan.protocol.trial_defaults.fast.warmups} warmup(s) + {currentPlan.protocol.trial_defaults.fast.measured_trials} measured trial(s)</Text>
            <Text size="sm"><strong>Destructive protocol</strong><br />{currentPlan.protocol.trial_defaults.destructive.warmups} warmup(s) + {currentPlan.protocol.trial_defaults.destructive.measured_trials} measured trial(s)</Text>
            <Text size="sm"><strong>Cleanup and isolation</strong><br />{currentValidation ? [...new Set(currentValidation.cells.map(({ protocol }) => protocol.cleanup.replaceAll("_", " ")))].join(" · ") : "Updating from canonical validation"}</Text>
          </SimpleGrid>
          {currentValidation?.estimates.warnings.map((warning, index) => (
            <Alert key={`${warning}-${index}`} color="yellow" title="Estimate warning">{warning}</Alert>
          ))}
          <ValidationState validation={validation} />
          {!uiDefinitionsComplete ? (
            <Alert color="red" title="UI definition version mismatch" role="alert">
              A registered operation or factor has no explicit control mapping in this UI version. Generic fallback controls are forbidden, so review and execution are blocked.
            </Alert>
          ) : null}
          <Group>
            <Button
              onClick={() => void reviewPlan()}
              loading={validation.isFetching}
              disabled={!currentValidation?.runnable || !uiDefinitionsComplete || validation.isPending}
            >
              {customizing && currentValidation?.is_customized ? "Review customized run" : "Review default run"}
            </Button>
            {!customizing ? (
              <Button variant="default" leftSection={<SlidersHorizontal size={17} aria-hidden="true" />} onClick={() => { setDraft(clonePlan(defaultPlan)); setStartingPreset(null); setCustomizing(true); }}>
                Customize
              </Button>
            ) : (
              <Button variant="default" leftSection={<RotateCcw size={17} aria-hidden="true" />} onClick={resetAll}>
                Reset all
              </Button>
            )}
            {!customizing && quickSmoke ? (
              <Button variant="light" onClick={() => loadPreset(quickSmoke)}>Use Quick Smoke instead</Button>
            ) : null}
            <Button variant="subtle" onClick={yaml.open}>Inspect configuration YAML</Button>
          </Group>
        </Stack>
      </Card>

      <Card withBorder padding="lg">
        <Stack>
          <div>
            <Title order={3} size="h4">Load another preset</Title>
            <Text size="sm" c="dimmed">Presets are complete server-authored plans. Loading one starts customization and never changes the Default configuration.</Text>
          </div>
          {presets.length === 0 ? (
            <Text c="dimmed">No versioned preset is available for this scope.</Text>
          ) : (
            <Group align="flex-end" wrap="wrap">
              <Select
                label="Preset"
                placeholder="Select a bounded study"
                data={presets.map((preset) => ({ value: `${preset.id}:${preset.version}`, label: `${preset.plan.name} · v${preset.version}` }))}
                value={selectedPresetKey}
                onChange={setSelectedPresetKey}
                searchable
                className="preset-select"
              />
              <Button variant="light" disabled={!selectedPreset} onClick={() => { if (selectedPreset) loadPreset(selectedPreset); }}>
                Load preset
              </Button>
            </Group>
          )}
        </Stack>
      </Card>

      {customizing ? (
        <Stack gap="lg" aria-label="Typed experiment configuration">
          <ProtocolAndEnvironmentControls plan={draft} definitions={definitions} onChange={setDraft} />
          <Card withBorder padding="lg">
            <Stack>
              <div>
                <Title order={3} size="h4">Operation factors</Title>
                <Text size="sm" c="dimmed">Each registered operation has an explicit typed control component. Definitions provide labels, bounds, choices, and scientific revisions—not layout.</Text>
              </div>
              {draft.operations.length === 0 ? (
                <Alert color="red" title="Plan has no operations">Review and execution are blocked.</Alert>
              ) : scope === "files" ? (
                <FileOperationEditor
                  operations={draft.operations}
                  definitions={definitions}
                  onChange={updateOperation}
                />
              ) : (
                <Accordion multiple defaultValue={draft.operations.filter(({ configuration }) => configuration.enabled).map(({ operation }) => operation)}>
                  {draft.operations.map((operation, index) => {
                    const definition = definitions.catalog.operations.find(({ id }) => id === operation.operation);
                    return (
                      <Accordion.Item key={operation.operation} value={operation.operation}>
                        <Accordion.Control>
                          <Group gap="xs">
                            <Text fw={600}>{definition?.label ?? operation.operation}</Text>
                            <Badge color={operation.configuration.enabled ? "green" : "gray"} variant="light">
                              {operation.configuration.enabled ? "Included" : "Excluded"}
                            </Badge>
                          </Group>
                        </Accordion.Control>
                        <Accordion.Panel>
                          {definition ? (
                            <OperationControlSwitch
                              operation={operation}
                              definition={definition}
                              profiles={definitions.catalog.workspace_profiles}
                              onChange={(next) => updateOperation(index, next)}
                            />
                          ) : (
                            <Alert color="red" title="Operation definition missing">
                              No explicit control can be rendered for <Text span ff="monospace">{operation.operation}</Text>.
                            </Alert>
                          )}
                        </Accordion.Panel>
                      </Accordion.Item>
                    );
                  })}
                </Accordion>
              )}
            </Stack>
          </Card>
        </Stack>
      ) : null}

      {currentValidation ? (
        <PlanReviewModal
          opened={reviewOpened}
          close={review.close}
          validation={currentValidation}
          health={health.data}
          healthPending={health.isPending}
          healthError={health.error}
          retryHealth={() => void health.refetch()}
          startingPreset={startingPreset}
        />
      ) : null}
      <YamlPlanDrawer
        opened={yamlOpened}
        close={yaml.close}
        plan={currentValidation?.canonical_plan ?? currentPlan}
        defaultPlan={defaultPlan}
        customized={currentValidation?.is_customized ?? false}
        onImport={importPlan}
        onReset={resetAll}
      />
    </Stack>
  );
}

export function DefaultPlanLauncher({ scope }: { scope: ConfigurationScope }) {
  const definitions = useQuery({ queryKey: ["definitions"], queryFn: benchmarkApi.definitions });

  if (definitions.isPending) return <LoadingState label="Loading the Default configuration and presets" />;
  if (definitions.error) return <ErrorState error={definitions.error} retry={() => void definitions.refetch()} />;
  if (!definitions.data) {
    return <Alert color="red" title="Definition catalog is empty">The runner returned no configuration data.</Alert>;
  }
  const plan = definitions.data.defaults.find((candidate) => candidate.configuration_base.scope === scope);
  if (!plan) {
    return (
      <Alert color="red" title="Default configuration unavailable" role="alert">
        The runner did not return a versioned Default configuration for scope <Text span ff="monospace">{scope}</Text>.
      </Alert>
    );
  }

  return (
    <LoadedPlanLauncher
      key={`${plan.configuration_base.id}:${plan.configuration_base.version}:${plan.configuration_base.scope}`}
      scope={scope}
      definitions={definitions.data}
      defaultPlan={plan}
    />
  );
}
