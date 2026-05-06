# Phase 07 - Raw Exec Workspace Blocking and Recovery

**Status:** draft implementation plan
**Source:** `three-server-command-exec-workspace-replacement-simplified.md`

## 1. Task Specification

Close the divergence hole where raw/setup execution can mutate real `/testbed`
after workspace base build. Public `raw_exec` remains a provider/runtime escape
hatch for non-workspace state, but it must fail closed when it may write the
assigned workspace after workspace base build. Explicit recovery APIs own rebuild-base/rebase
behavior.

Implementation scope:

```text
block supported raw/setup writes under /testbed after workspace base build
distinguish guarded RuntimeEnvelope calls from public raw_exec
expose workspace binding/base diagnostics
add explicit recovery-only rebuild-base path
draft or add explicit recovery-only rebase path if required
add optional real /testbed scanner for audit/recovery diagnostics
```

Out of scope:

```text
no implicit divergence state machine
no automatic background reconciliation
no raw exec wrapper for guarded write/edit/shell
no full-root versioning
```

Exit condition:

```text
after workspace base build, supported raw/setup execution cannot silently mutate real
/testbed in a way that guarded read_file would miss. Recovery requires an
explicit user/API action.
```

## 2. Main Data Objects

```text
RawExecPolicyDecision
  allowed
  reason
  workspace_binding
  command_classification

WorkspaceRecoveryRequest
  mode: rebuild_base | rebase | scan
  workspace_ref
  actor_id
  reason

WorkspaceRecoveryReport
  base_binding
  real_workspace_hash
  layer_stack_active_hash
  changed_paths
  conflicts
  action_taken

WorkspaceScannerResult
  tree_hash
  path_count
  byte_count
  suspected_workspace_writes
```

## 3. File/Folder Structure Change

Target additions and updates:

```text
backend/src/sandbox/api/tool/
|-- raw_exec.py

backend/src/sandbox/control/ops/
|-- runtime_services.py
|-- setup.py

backend/src/sandbox/layer_stack/
+-- workspace_recovery.py
+-- workspace_scanner.py

backend/tests/unit_test/test_sandbox/test_api/
+-- test_raw_exec_workspace_blocking.py

backend/tests/unit_test/test_sandbox/test_layer_stack/
+-- test_workspace_recovery.py
```

## 4. Workflow Demonstration

Blocked raw workspace mutation:

```text
host raw_exec("bash -lc 'echo bad > /testbed/src/a.py'")
  -> raw_exec policy checks workspace binding
  -> command may mutate /testbed after workspace base build
  -> reject with workspace_mutation_blocked
```

Allowed non-workspace raw execution:

```text
host raw_exec("bash -lc 'echo x > /tmp/outside'")
  -> raw_exec policy checks workspace binding
  -> command is outside assigned workspace and allowed by runtime policy
  -> execute as provider/runtime state
```

Explicit rebuild-base recovery:

```text
workspace_recovery.rebuild_base(workspace_ref, reason)
  -> require explicit recovery request
  -> scan real /testbed as a full workspace copy
  -> archive or discard previous layer-stack workspace state by recovery mode
  -> build real /testbed as the new workspace base
  -> write recovery report
```

Explicit rebase recovery:

```text
workspace_recovery.rebase(workspace_ref, reason)
  -> scan real /testbed
  -> compute diff against recorded base/active hash
  -> convert diff to typed changes
  -> submit through OCC
  -> report accepted/conflict/skipped paths
```

## 5. Naming Conventions and Rationale

| Name | Rationale |
|---|---|
| `raw_exec` | Stays the provider/runtime escape hatch, not a guarded workspace API wrapper. |
| `workspace_mutation_blocked` | Names the failure mode directly. |
| `WorkspaceRecoveryRequest` | Makes recovery explicit and auditable. |
| `rebuild_base` | Replaces layer-stack truth from real `/testbed` only by explicit recovery action. |
| `rebase` | Converts real workspace drift into typed OCC changes when that behavior is intentionally requested. |

## 6. Tests and Exit Criteria

```text
uv run pytest backend/tests/unit_test/test_sandbox/test_api/test_raw_exec_workspace_blocking.py -q
uv run pytest backend/tests/unit_test/test_sandbox/test_layer_stack/test_workspace_recovery.py -q
```

Required assertions:

- raw mutation under `/testbed` is rejected after workspace base build
- guarded runtime envelopes are not implemented as public raw_exec calls
- reads never check real `/testbed` to decide normal freshness
- rebuild-base/rebase require explicit recovery mode and produce reports
- optional scanner is diagnostic only and does not mutate layer-stack by itself
