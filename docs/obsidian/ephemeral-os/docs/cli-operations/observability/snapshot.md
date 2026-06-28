---
title: snapshot
tags:
  - ephemeral-os
  - cli
  - observability
status: ready
---

# snapshot

**Execution space:** `observability` (read-only) · **Family:** `observability`

Show live sandbox state.

> Every `observability` operation resolves to the daemon op `get_observability` with the operation name as the `view`; `--sandbox-id` selects which sandbox's daemon to query (it is routing, not an op argument).

## Manual

Show current state from the runtime registry: sandbox lifecycle state, workspaces (with layer counts), in-flight executions, and the latest resource sample per scope. Served live; does not read the log.

| Argument | Flag | Kind | Required | Default | Description |
|---|---|---|---|---|---|
| `sandbox_id` | `--sandbox-id` | string | yes | — | Target sandbox id (selects the daemon to query). |

**Usage**

```
sandbox-cli observability snapshot --sandbox-id ID
```

**Examples**

```sh
sandbox-cli observability snapshot --sandbox-id eos-abc
```

## Expected output

```json
{
  "sandbox_id": "eos-abc",
  "lifecycle_state": "ready",
  "availability": "available",
  "sampled_at_unix_ms": 1751240400000,
  "errors": [],
  "daemon": { "daemon_pid": 4711, "runtime_dir": "/run/eos/eos-abc" },
  "resources": {
    "latest": { "ts": 1751240400000, "sample_delta_ms": 1000, "metrics": { "cpu_usec": 1200000, "mem_cur": 10485760, "mem_max": 268435456 }, "deltas": { "cpu_usec": 30000 } },
    "history": []
  },
  "workspaces": [
    {
      "workspace_id": "ws-1",
      "lifecycle_state": "active",
      "network_profile": "shared",
      "layers": { "base_root_hash": "sha256:…", "layer_count": 2 },
      "namespace_fd_count": 5,
      "resources": { "latest": { "ts": 1751240400000, "sample_delta_ms": 1000, "metrics": { "disk_bytes": 4096, "files": 12 }, "deltas": {} }, "history": [] },
      "active_namespace_executions": [
        { "namespace_execution_id": "cmd-1", "operation": "exec_command", "lifecycle_state": "running" }
      ]
    }
  ],
  "stack": { "layer_count": 2, "layers_bytes": 1048576, "active_leases": 1 }
}
```

`availability` is `available | partial` (`partial` when some workspace state could not be read; the reasons are in `errors`). Metric keys are emitted only when present: `cpu_usec`, `mem_cur`, `mem_max`, `mem_max_unlimited`, `cgroup_available`, `cgroup_error`, and per-workspace `disk_bytes`, `files`, `disk_truncated`.

## Related

- [[trace]]
- [[cgroup]]
- [[layerstack]]
