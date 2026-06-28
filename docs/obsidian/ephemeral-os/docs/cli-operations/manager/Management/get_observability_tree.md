---
title: get_observability_tree
tags:
  - ephemeral-os
  - cli
  - manager
  - management
status: ready
---

# get_observability_tree

**Execution space:** `manager` (system scope) · **Family:** `management`

Aggregate daemon observability snapshots for manager-known sandboxes.

## Manual

Aggregate daemon-local observability snapshots for ready manager-known sandboxes without reading daemon storage from the manager. The manager fans out a private `get_observability` (view `snapshot`) request to each ready sandbox's daemon and assembles the responses into one tree. Unreachable or non-ready sandboxes become `unavailable` nodes rather than failing the whole call.

| Argument | Flag | Kind | Required | Default | Description |
|---|---|---|---|---|---|
| `sandbox_id` | `--sandbox-id` | string | no | all ready sandboxes | Optional manager sandbox id. When omitted, all ready sandboxes with daemon endpoints are queried. |
| `resource_window_ms` | `--resource-window-ms` | integer | no | — | Optional bounded resource history window in milliseconds. |

**Usage**

```
sandbox-cli manager get_observability_tree [--sandbox-id ID] [--resource-window-ms MS]
```

**Examples**

```sh
sandbox-cli manager get_observability_tree
sandbox-cli manager get_observability_tree --sandbox-id sbox-1
sandbox-cli manager get_observability_tree --resource-window-ms 60000
```

## Expected output

Success — one node per selected sandbox:

```json
{
  "sandboxes": [
    {
      "sandbox_id": "sbox-1",
      "lifecycle_state": "ready",
      "availability": "available",
      "sampled_at_unix_ms": 1751240400000,
      "errors": [],
      "daemon": { "host": "127.0.0.1", "port": 53124, "daemon_pid": 4711, "runtime_dir": "/run/eos/sbox-1" },
      "resources": {
        "latest": { "ts": 1751240400000, "sample_delta_ms": 1000, "metrics": { "cpu_usec": 1200000, "mem_cur": 10485760 }, "deltas": { "cpu_usec": 30000 } },
        "history": []
      },
      "workspaces": [
        {
          "workspace_id": "ws-1",
          "lifecycle_state": "active",
          "network_profile": "shared",
          "layers": { "base_root_hash": "sha256:…", "layer_count": 2 },
          "namespace_fd_count": 5,
          "resources": { "latest": null, "history": [] },
          "active_namespace_executions": []
        }
      ]
    }
  ]
}
```

`availability` is `available | partial | unavailable`. A sandbox whose daemon times out or is not ready returns an `unavailable` node with the reason in `errors`:

```json
{
  "sandbox_id": "sbox-2",
  "lifecycle_state": "ready",
  "availability": "unavailable",
  "sampled_at_unix_ms": null,
  "errors": ["daemon sbox-2 timed out"],
  "daemon": { "host": "127.0.0.1", "port": 53125, "daemon_pid": null, "runtime_dir": null },
  "resources": { "latest": null, "history": [] },
  "workspaces": []
}
```

## Related

- [[list_sandboxes]]
- [[inspect_sandbox]]
- [[snapshot]]
