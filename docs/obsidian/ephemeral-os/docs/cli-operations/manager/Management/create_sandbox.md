---
title: create_sandbox
tags:
  - ephemeral-os
  - cli
  - manager
  - management
status: ready
---

# create_sandbox

**Execution space:** `manager` (system scope) · **Family:** `management`

Create a host-side sandbox record and runtime sandbox.

## Manual

Create a host-side sandbox record, create the runtime sandbox, and start its daemon. The manager creates the runtime sandbox first, records it, then provisions and starts the in-sandbox `sandbox-daemon`; any failure rolls back the daemon, runtime sandbox, and record.

| Argument | Flag | Kind | Required | Default | Description |
|---|---|---|---|---|---|
| `image` | `--image` | string | yes | — | Container image used to create the sandbox. |
| `workspace_root` | `--workspace-root` | path | yes | — | Absolute workspace root mounted inside this sandbox. |

**Usage**

```
sandbox-cli manager create_sandbox --image IMAGE --workspace-root PATH
```

**Examples**

```sh
sandbox-cli manager create_sandbox --image ubuntu:24.04 --workspace-root /testbed
```

## Expected output

Success — the new sandbox record (`state` is `ready` once the daemon is up):

```json
{
  "id": "eos-abc",
  "workspace_root": "/testbed",
  "state": "ready",
  "daemon": { "host": "127.0.0.1", "port": 53124 }
}
```

`id` is assigned by the runtime provider. `state` is one of `creating | ready | stopping | stopped | failed`.

Error — invalid/empty image (record and runtime sandbox are rolled back):

```json
{ "error": { "kind": "invalid_request", "message": "invalid image: ", "details": {} } }
```

## Related

- [[list_sandboxes]]
- [[inspect_sandbox]]
- [[destroy_sandbox]]
