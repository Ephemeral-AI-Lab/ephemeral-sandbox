import {
  ActionIcon,
  Alert,
  Box,
  Button,
  Divider,
  Group,
  MultiSelect,
  NumberInput,
  Select,
  SimpleGrid,
  Stack,
  Switch,
  Text,
  Title,
} from "@mantine/core";
import { Minus, Plus } from "lucide-react";
import type { ReactNode } from "react";
import type {
  DefinitionCatalog,
  Factor,
  FactorDefinition,
  FactorId,
  OperationDefinition,
  OperationId,
  OperationPlan,
  WorkspaceProfileCatalog,
} from "@/api/types";
import { formatBytes, formatInteger } from "@/lib/format";

const OPERATION_FACTOR_IDS = {
  exec_command: ["concurrent_requests", "workspace_profile", "session_mode", "command_case"],
  file_read: ["concurrent_requests", "returned_bytes", "read_source", "target_mode"],
  file_write: ["concurrent_requests", "content_bytes", "mutation_destination", "target_mode"],
  file_edit: [
    "concurrent_requests",
    "file_bytes",
    "replacement_count",
    "match_density",
    "mutation_destination",
    "target_mode",
  ],
  file_blame: [
    "concurrent_requests",
    "line_count",
    "ownership_segments",
    "auditability_event_count",
  ],
  create_workspace: ["workspace_count", "workspace_profile", "network_profile"],
  squash_layerstack: [
    "live_sessions",
    "requested_migration_ratio",
    "remount_parallelism",
    "squashable_blocks",
    "layers_per_block",
    "payload_bytes",
    "session_activity",
  ],
} as const satisfies Record<OperationId, readonly FactorId[]>;

const OPERATION_IDS = Object.keys(OPERATION_FACTOR_IDS) as OperationId[];

export function hasCompleteUiDefinitions(
  operations: readonly OperationPlan[],
  catalog: DefinitionCatalog,
): boolean {
  return operations.every((operation) => {
    const expectedFactorIds = OPERATION_FACTOR_IDS[operation.operation as OperationId];
    if (!expectedFactorIds) return false;
    const definition = catalog.operations.find(({ id }) => id === operation.operation);
    return (
      definition !== undefined &&
      definition.factors.length === expectedFactorIds.length &&
      expectedFactorIds.every((factorId) =>
        definition.factors.some(({ id }) => id === factorId),
      )
    );
  });
}

function factorDefinition(
  operation: OperationDefinition,
  factorId: FactorId,
): FactorDefinition | undefined {
  return operation.factors.find(({ id }) => id === factorId);
}

function humanize(value: string): string {
  return value
    .split("_")
    .map((part) => `${part.slice(0, 1).toUpperCase()}${part.slice(1)}`)
    .join(" ");
}

function choiceValues<T extends string>(
  definition: FactorDefinition | undefined,
  current: readonly T[],
): T[] {
  const defined = definition?.constraint.kind === "choices" ? definition.constraint.values : [];
  return [...new Set([...defined, ...current])] as T[];
}

function FactorFrame({
  definition,
  fallbackLabel,
  children,
}: {
  definition: FactorDefinition | undefined;
  fallbackLabel: string;
  children: ReactNode;
}) {
  return (
    <Box className="factor-field">
      <Text fw={600}>{definition?.label ?? fallbackLabel}</Text>
      <Text size="sm" c="dimmed" mb="sm">
        {definition?.help ?? "This runner did not provide the required factor definition."}
      </Text>
      {children}
    </Box>
  );
}

function nextNumericValue(factor: Factor<number>, definition: FactorDefinition | undefined): number {
  const step = definition?.value_kind === "unit_ratio" ? 0.1 : 1;
  const last = factor.values.at(-1) ?? (definition?.constraint.kind === "positive" ? 1 : 0);
  let candidate = last + step;
  while (factor.values.includes(candidate)) candidate += step;
  return definition?.constraint.kind === "unit_interval" ? Math.min(candidate, 1) : candidate;
}

function numericBounds(definition: FactorDefinition | undefined): {
  min: number | undefined;
  max: number | undefined;
  step: number;
  allowDecimal: boolean;
} {
  if (definition?.constraint.kind === "unit_interval") {
    return { min: 0, max: 1, step: 0.1, allowDecimal: true };
  }
  return {
    min: definition?.constraint.kind === "positive" ? 1 : 0,
    max: undefined,
    step: 1,
    allowDecimal: false,
  };
}

function NumericFactorEditor({
  factor,
  definition,
  fallbackLabel,
  onChange,
}: {
  factor: Factor<number>;
  definition: FactorDefinition | undefined;
  fallbackLabel: string;
  onChange: (factor: Factor<number>) => void;
}) {
  const bounds = numericBounds(definition);
  const displayValue = (value: number) =>
    definition?.unit === "bytes" ? formatBytes(value) : formatInteger(value);
  const setRole = (role: string | null) => {
    if (role === "controlled") {
      const value = factor.control ?? factor.values[0] ?? bounds.min ?? 0;
      onChange({ role, values: [value], control: null });
    } else if (role === "varied") {
      const values = factor.values.length > 0 ? factor.values : [bounds.min ?? 0];
      onChange({ role, values, control: factor.control ?? values[0] ?? null });
    }
  };
  const setValue = (index: number, next: string | number) => {
    if (typeof next !== "number" || !Number.isFinite(next)) return;
    const previous = factor.values[index];
    const values = factor.values.map((value, valueIndex) => (valueIndex === index ? next : value));
    onChange({
      ...factor,
      values,
      control: factor.control === previous ? next : factor.control,
    });
  };
  const removeValue = (index: number) => {
    if (factor.values.length <= 1) return;
    const removed = factor.values[index];
    const values = factor.values.filter((_, valueIndex) => valueIndex !== index);
    onChange({
      ...factor,
      values,
      control: factor.control === removed ? values[0] ?? null : factor.control,
    });
  };

  return (
    <FactorFrame definition={definition} fallbackLabel={fallbackLabel}>
      <Stack gap="sm">
        <Select
          label="Factor role"
          data={[
            { value: "controlled", label: "Controlled (fixed)" },
            { value: "varied", label: "Varied (series)" },
          ]}
          value={factor.role}
          onChange={setRole}
          allowDeselect={false}
        />
        <Stack gap="xs" aria-label={`${definition?.label ?? fallbackLabel} values`}>
          {factor.values.map((value, index) => (
            <Group key={index} align="flex-end" wrap="nowrap">
              <NumberInput
                label={factor.role === "varied" ? `Value ${index + 1}` : "Value"}
                value={value}
                onChange={(next) => setValue(index, next)}
                min={bounds.min}
                max={bounds.max}
                step={bounds.step}
                allowDecimal={bounds.allowDecimal}
                clampBehavior="strict"
                description={definition?.unit ? displayValue(value) : undefined}
                className="factor-value-input"
              />
              {factor.role === "varied" ? (
                <ActionIcon
                  variant="default"
                  aria-label={`Remove value ${displayValue(value)}`}
                  onClick={() => removeValue(index)}
                  disabled={factor.values.length <= 1}
                >
                  <Minus size={18} aria-hidden="true" />
                </ActionIcon>
              ) : null}
            </Group>
          ))}
        </Stack>
        {factor.role === "varied" ? (
          <>
            <Button
              variant="subtle"
              size="compact-sm"
              leftSection={<Plus size={16} aria-hidden="true" />}
              onClick={() =>
                onChange({ ...factor, values: [...factor.values, nextNumericValue(factor, definition)] })
              }
              disabled={definition?.constraint.kind === "unit_interval" && factor.values.includes(1)}
            >
              Add series value
            </Button>
            <Select
              label="Control value"
              description="The starred reference within this varied factor."
              data={factor.values.map((value) => ({ value: String(value), label: displayValue(value) }))}
              value={factor.control === null ? null : String(factor.control)}
              onChange={(value) =>
                onChange({ ...factor, control: value === null ? null : Number(value) })
              }
              allowDeselect={false}
            />
          </>
        ) : null}
      </Stack>
    </FactorFrame>
  );
}

function ChoiceFactorEditor<T extends string>({
  factor,
  definition,
  fallbackLabel,
  values,
  onChange,
}: {
  factor: Factor<T>;
  definition: FactorDefinition | undefined;
  fallbackLabel: string;
  values: readonly T[];
  onChange: (factor: Factor<T>) => void;
}) {
  const options = [...new Set([...values, ...factor.values])].map((value) => ({
    value,
    label: humanize(value),
  }));
  const setRole = (role: string | null) => {
    if (role === "controlled") {
      const value = factor.control ?? factor.values[0] ?? values[0];
      if (value) onChange({ role, values: [value], control: null });
    } else if (role === "varied") {
      const selected = factor.values.length > 0 ? factor.values : values.slice(0, 1);
      onChange({ role, values: [...selected], control: factor.control ?? selected[0] ?? null });
    }
  };

  return (
    <FactorFrame definition={definition} fallbackLabel={fallbackLabel}>
      <Stack gap="sm">
        <Select
          label="Factor role"
          data={[
            { value: "controlled", label: "Controlled (fixed)" },
            { value: "varied", label: "Varied (series)" },
          ]}
          value={factor.role}
          onChange={setRole}
          allowDeselect={false}
        />
        {factor.role === "controlled" ? (
          <Select
            label="Value"
            data={options}
            value={factor.values[0] ?? null}
            onChange={(value) => {
              if (value !== null) onChange({ role: "controlled", values: [value as T], control: null });
            }}
            allowDeselect={false}
          />
        ) : (
          <>
            <MultiSelect
              label="Series values"
              data={options}
              value={factor.values}
              onChange={(selected) => {
                const selectedValues = selected as T[];
                onChange({
                  ...factor,
                  values: selectedValues,
                  control: selectedValues.includes(factor.control as T)
                    ? factor.control
                    : selectedValues[0] ?? null,
                });
              }}
              clearable={false}
              searchable
            />
            <Select
              label="Control value"
              description="The starred reference within this varied factor."
              data={options.filter(({ value }) => factor.values.includes(value as T))}
              value={factor.control}
              onChange={(value) => onChange({ ...factor, control: value as T | null })}
              allowDeselect={false}
            />
          </>
        )}
      </Stack>
    </FactorFrame>
  );
}

interface RendererProps {
  operation: OperationPlan;
  definition: OperationDefinition;
  profiles: WorkspaceProfileCatalog;
  onChange: (operation: OperationPlan) => void;
  showIncludeToggle?: boolean;
}

export function setOperationEnabled(operation: OperationPlan, enabled: boolean): OperationPlan {
  switch (operation.operation) {
    case "exec_command":
      return { ...operation, configuration: { ...operation.configuration, enabled } };
    case "file_read":
      return { ...operation, configuration: { ...operation.configuration, enabled } };
    case "file_write":
      return { ...operation, configuration: { ...operation.configuration, enabled } };
    case "file_edit":
      return { ...operation, configuration: { ...operation.configuration, enabled } };
    case "file_blame":
      return { ...operation, configuration: { ...operation.configuration, enabled } };
    case "create_workspace":
      return { ...operation, configuration: { ...operation.configuration, enabled } };
    case "squash_layerstack":
      return { ...operation, configuration: { ...operation.configuration, enabled } };
  }
}

function OperationFrame({
  operation,
  definition,
  onChange,
  showIncludeToggle = true,
  children,
}: RendererProps & { children: ReactNode }) {
  return (
    <Stack gap="md">
      <Group justify="space-between" align="flex-start" wrap="wrap">
        <div>
          <Title order={3} size="h4">{definition.label}</Title>
          <Text size="sm" c="dimmed">{definition.measured_boundary}</Text>
        </div>
        {showIncludeToggle ? (
          <Switch
            label="Include operation"
            checked={operation.configuration.enabled}
            onChange={(event) => onChange(setOperationEnabled(operation, event.currentTarget.checked))}
          />
        ) : null}
      </Group>
      <Alert color="blue" title="Count semantics">
        {definition.count_semantics_help}
      </Alert>
      <Box opacity={operation.configuration.enabled ? 1 : 0.62}>{children}</Box>
    </Stack>
  );
}

type OperationControlRenderer = (props: RendererProps) => ReactNode;

const OPERATION_CONTROLS = {
  exec_command: ({ operation, definition, profiles, onChange, showIncludeToggle }) => {
    if (operation.operation !== "exec_command") return null;
    const factors = operation.configuration.factors;
    const update = (next: typeof factors) =>
      onChange({ ...operation, configuration: { ...operation.configuration, factors: next } });
    return (
      <OperationFrame operation={operation} definition={definition} profiles={profiles} onChange={onChange} showIncludeToggle={showIncludeToggle}>
        <Alert color="yellow" title="Allowlisted bounded shell case" mb="md">
          Select a registered case id only. The review shows the exact backend-rendered command; arbitrary shell text is unavailable.
        </Alert>
        <SimpleGrid cols={{ base: 1, lg: 2 }}>
          <NumericFactorEditor factor={factors.concurrent_requests} definition={factorDefinition(definition, "concurrent_requests")} fallbackLabel="Concurrent requests" onChange={(value) => update({ ...factors, concurrent_requests: value })} />
          <ChoiceFactorEditor factor={factors.workspace_profile} definition={factorDefinition(definition, "workspace_profile")} fallbackLabel="Workspace profile" values={profiles.profiles.map(({ id }) => id)} onChange={(value) => update({ ...factors, workspace_profile: value })} />
          <ChoiceFactorEditor factor={factors.session_mode} definition={factorDefinition(definition, "session_mode")} fallbackLabel="Session boundary" values={choiceValues(factorDefinition(definition, "session_mode"), factors.session_mode.values)} onChange={(value) => update({ ...factors, session_mode: value })} />
          <ChoiceFactorEditor factor={factors.command_case} definition={factorDefinition(definition, "command_case")} fallbackLabel="Command case" values={choiceValues(factorDefinition(definition, "command_case"), factors.command_case.values)} onChange={(value) => update({ ...factors, command_case: value })} />
        </SimpleGrid>
      </OperationFrame>
    );
  },
  file_read: ({ operation, definition, profiles, onChange, showIncludeToggle }) => {
    if (operation.operation !== "file_read") return null;
    const factors = operation.configuration.factors;
    const update = (next: typeof factors) => onChange({ ...operation, configuration: { ...operation.configuration, factors: next } });
    return (
      <OperationFrame operation={operation} definition={definition} profiles={profiles} onChange={onChange} showIncludeToggle={showIncludeToggle}>
        <SimpleGrid cols={{ base: 1, lg: 2 }}>
          <NumericFactorEditor factor={factors.concurrent_requests} definition={factorDefinition(definition, "concurrent_requests")} fallbackLabel="Concurrent requests" onChange={(value) => update({ ...factors, concurrent_requests: value })} />
          <NumericFactorEditor factor={factors.returned_bytes} definition={factorDefinition(definition, "returned_bytes")} fallbackLabel="Returned bytes" onChange={(value) => update({ ...factors, returned_bytes: value })} />
          <ChoiceFactorEditor factor={factors.source} definition={factorDefinition(definition, "read_source")} fallbackLabel="Read source" values={choiceValues(factorDefinition(definition, "read_source"), factors.source.values)} onChange={(value) => update({ ...factors, source: value })} />
          <ChoiceFactorEditor factor={factors.target_mode} definition={factorDefinition(definition, "target_mode")} fallbackLabel="Target mode" values={choiceValues(factorDefinition(definition, "target_mode"), factors.target_mode.values)} onChange={(value) => update({ ...factors, target_mode: value })} />
        </SimpleGrid>
      </OperationFrame>
    );
  },
  file_write: ({ operation, definition, profiles, onChange, showIncludeToggle }) => {
    if (operation.operation !== "file_write") return null;
    const factors = operation.configuration.factors;
    const update = (next: typeof factors) => onChange({ ...operation, configuration: { ...operation.configuration, factors: next } });
    return (
      <OperationFrame operation={operation} definition={definition} profiles={profiles} onChange={onChange} showIncludeToggle={showIncludeToggle}>
        <SimpleGrid cols={{ base: 1, lg: 2 }}>
          <NumericFactorEditor factor={factors.concurrent_requests} definition={factorDefinition(definition, "concurrent_requests")} fallbackLabel="Concurrent requests" onChange={(value) => update({ ...factors, concurrent_requests: value })} />
          <NumericFactorEditor factor={factors.content_bytes} definition={factorDefinition(definition, "content_bytes")} fallbackLabel="Content bytes" onChange={(value) => update({ ...factors, content_bytes: value })} />
          <ChoiceFactorEditor factor={factors.destination} definition={factorDefinition(definition, "mutation_destination")} fallbackLabel="Mutation destination" values={choiceValues(factorDefinition(definition, "mutation_destination"), factors.destination.values)} onChange={(value) => update({ ...factors, destination: value })} />
          <ChoiceFactorEditor factor={factors.target_mode} definition={factorDefinition(definition, "target_mode")} fallbackLabel="Target mode" values={choiceValues(factorDefinition(definition, "target_mode"), factors.target_mode.values)} onChange={(value) => update({ ...factors, target_mode: value })} />
        </SimpleGrid>
      </OperationFrame>
    );
  },
  file_edit: ({ operation, definition, profiles, onChange, showIncludeToggle }) => {
    if (operation.operation !== "file_edit") return null;
    const factors = operation.configuration.factors;
    const update = (next: typeof factors) => onChange({ ...operation, configuration: { ...operation.configuration, factors: next } });
    return (
      <OperationFrame operation={operation} definition={definition} profiles={profiles} onChange={onChange} showIncludeToggle={showIncludeToggle}>
        <SimpleGrid cols={{ base: 1, lg: 2 }}>
          <NumericFactorEditor factor={factors.concurrent_requests} definition={factorDefinition(definition, "concurrent_requests")} fallbackLabel="Concurrent requests" onChange={(value) => update({ ...factors, concurrent_requests: value })} />
          <NumericFactorEditor factor={factors.file_bytes} definition={factorDefinition(definition, "file_bytes")} fallbackLabel="File bytes" onChange={(value) => update({ ...factors, file_bytes: value })} />
          <NumericFactorEditor factor={factors.replacement_count} definition={factorDefinition(definition, "replacement_count")} fallbackLabel="Replacement count" onChange={(value) => update({ ...factors, replacement_count: value })} />
          <NumericFactorEditor factor={factors.match_density} definition={factorDefinition(definition, "match_density")} fallbackLabel="Match density" onChange={(value) => update({ ...factors, match_density: value })} />
          <ChoiceFactorEditor factor={factors.destination} definition={factorDefinition(definition, "mutation_destination")} fallbackLabel="Mutation destination" values={choiceValues(factorDefinition(definition, "mutation_destination"), factors.destination.values)} onChange={(value) => update({ ...factors, destination: value })} />
          <ChoiceFactorEditor factor={factors.target_mode} definition={factorDefinition(definition, "target_mode")} fallbackLabel="Target mode" values={choiceValues(factorDefinition(definition, "target_mode"), factors.target_mode.values)} onChange={(value) => update({ ...factors, target_mode: value })} />
        </SimpleGrid>
      </OperationFrame>
    );
  },
  file_blame: ({ operation, definition, profiles, onChange, showIncludeToggle }) => {
    if (operation.operation !== "file_blame") return null;
    const factors = operation.configuration.factors;
    const update = (next: typeof factors) => onChange({ ...operation, configuration: { ...operation.configuration, factors: next } });
    return (
      <OperationFrame operation={operation} definition={definition} profiles={profiles} onChange={onChange} showIncludeToggle={showIncludeToggle}>
        <Alert color="blue" title="EphemeralOS ownership evidence" mb="md">
          This measures publish auditability and per-line EphemeralOS owners, not Git blame.
        </Alert>
        <SimpleGrid cols={{ base: 1, lg: 2 }}>
          <NumericFactorEditor factor={factors.concurrent_requests} definition={factorDefinition(definition, "concurrent_requests")} fallbackLabel="Concurrent requests" onChange={(value) => update({ ...factors, concurrent_requests: value })} />
          <NumericFactorEditor factor={factors.line_count} definition={factorDefinition(definition, "line_count")} fallbackLabel="Line count" onChange={(value) => update({ ...factors, line_count: value })} />
          <NumericFactorEditor factor={factors.ownership_segments} definition={factorDefinition(definition, "ownership_segments")} fallbackLabel="Ownership segments" onChange={(value) => update({ ...factors, ownership_segments: value })} />
          <NumericFactorEditor factor={factors.auditability_event_count} definition={factorDefinition(definition, "auditability_event_count")} fallbackLabel="Auditability events" onChange={(value) => update({ ...factors, auditability_event_count: value })} />
        </SimpleGrid>
      </OperationFrame>
    );
  },
  create_workspace: ({ operation, definition, profiles, onChange, showIncludeToggle }) => {
    if (operation.operation !== "create_workspace") return null;
    const factors = operation.configuration.factors;
    const update = (next: typeof factors) => onChange({ ...operation, configuration: { ...operation.configuration, factors: next } });
    return (
      <OperationFrame operation={operation} definition={definition} profiles={profiles} onChange={onChange} showIncludeToggle={showIncludeToggle}>
        <SimpleGrid cols={{ base: 1, lg: 2 }}>
          <NumericFactorEditor factor={factors.workspace_count} definition={factorDefinition(definition, "workspace_count")} fallbackLabel="Workspace count" onChange={(value) => update({ ...factors, workspace_count: value })} />
          <ChoiceFactorEditor factor={factors.workspace_profile} definition={factorDefinition(definition, "workspace_profile")} fallbackLabel="Workspace profile" values={profiles.profiles.map(({ id }) => id)} onChange={(value) => update({ ...factors, workspace_profile: value })} />
          <ChoiceFactorEditor factor={factors.network_profile} definition={factorDefinition(definition, "network_profile")} fallbackLabel="Network profile" values={choiceValues(factorDefinition(definition, "network_profile"), factors.network_profile.values)} onChange={(value) => update({ ...factors, network_profile: value })} />
        </SimpleGrid>
      </OperationFrame>
    );
  },
  squash_layerstack: ({ operation, definition, profiles, onChange, showIncludeToggle }) => {
    if (operation.operation !== "squash_layerstack") return null;
    const factors = operation.configuration.factors;
    const update = (next: typeof factors) => onChange({ ...operation, configuration: { ...operation.configuration, factors: next } });
    return (
      <OperationFrame operation={operation} definition={definition} profiles={profiles} onChange={onChange} showIncludeToggle={showIncludeToggle}>
        <Alert color="yellow" title="One squash request per trial" mb="md">
          Live sessions N are prepared load for the remount sweep. N is never squash-request concurrency; N=0 is the squash-only control.
        </Alert>
        <Title order={4} size="h5">Storage topology</Title>
        <SimpleGrid cols={{ base: 1, lg: 2 }} mt="sm">
          <NumericFactorEditor factor={factors.squashable_blocks} definition={factorDefinition(definition, "squashable_blocks")} fallbackLabel="Squashable blocks" onChange={(value) => update({ ...factors, squashable_blocks: value })} />
          <NumericFactorEditor factor={factors.layers_per_block} definition={factorDefinition(definition, "layers_per_block")} fallbackLabel="Layers per block" onChange={(value) => update({ ...factors, layers_per_block: value })} />
          <NumericFactorEditor factor={factors.payload_bytes} definition={factorDefinition(definition, "payload_bytes")} fallbackLabel="Layer payload" onChange={(value) => update({ ...factors, payload_bytes: value })} />
        </SimpleGrid>
        <Divider my="md" />
        <Title order={4} size="h5">Live-session load</Title>
        <SimpleGrid cols={{ base: 1, lg: 2 }} mt="sm">
          <NumericFactorEditor factor={factors.live_sessions} definition={factorDefinition(definition, "live_sessions")} fallbackLabel="Live sessions N" onChange={(value) => update({ ...factors, live_sessions: value })} />
          <NumericFactorEditor factor={factors.requested_migration_ratio} definition={factorDefinition(definition, "requested_migration_ratio")} fallbackLabel="Requested migration ratio" onChange={(value) => update({ ...factors, requested_migration_ratio: value })} />
          <ChoiceFactorEditor factor={factors.session_activity} definition={factorDefinition(definition, "session_activity")} fallbackLabel="Session activity" values={choiceValues(factorDefinition(definition, "session_activity"), factors.session_activity.values)} onChange={(value) => update({ ...factors, session_activity: value })} />
        </SimpleGrid>
        <Divider my="md" />
        <Title order={4} size="h5">Remount policy</Title>
        <Box mt="sm" maw={520}>
          <NumericFactorEditor factor={factors.remount_parallelism} definition={factorDefinition(definition, "remount_parallelism")} fallbackLabel="Remount parallelism W" onChange={(value) => update({ ...factors, remount_parallelism: value })} />
        </Box>
        <Alert mt="md" color="gray" title="Outcomes to measure (read-only)">
          The runner records observed migrated M, non-migrated I, dispositions, storage phases, remount spans, reclaimed bytes, and session usability.
        </Alert>
      </OperationFrame>
    );
  },
} satisfies Record<OperationId, OperationControlRenderer>;

export function OperationControlSwitch(props: RendererProps) {
  const runtimeId = String(props.operation.operation);
  if (!OPERATION_IDS.includes(runtimeId as OperationId)) {
    return (
      <Alert color="red" title="Unsupported operation definition" role="alert">
        This UI version has no explicit control component for <Text span ff="monospace">{runtimeId}</Text>. Review and execution are blocked until the UI is upgraded.
      </Alert>
    );
  }
  return OPERATION_CONTROLS[runtimeId as OperationId](props);
}
