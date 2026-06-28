---
title: destroy_sandbox
tags:
  - ephemeral-os
  - cli
  - manager
  - management
status: ready
---

# destroy_sandbox

**Execution space:** `manager` (system scope) · **Family:** `management`

Destroy a host-side sandbox and remove it from the registry.

## Manual

Stop the sandbox daemon, destroy the runtime sandbox, and remove the host-side sandbox record. The record is moved to `stopping`, the daemon is stopped, the runtime sandbox destroyed, and the record removed. A sandbox already in `creating` or `stopping` is rejected.

| Argument | Flag | Kind | Required | Default | Description |
|---|---|---|---|---|---|
| `sandbox_id` | `--sandbox-id` | string | yes | — | Sandbox id. |

**Usage**

```
sandbox-cli manager destroy_sandbox --sandbox-id ID
```

**Examples**

```sh
sandbox-cli manager destroy_sandbox --sandbox-id sbox-1
```

## Expected output

Success — the removed record (final state `stopped`):

```json
{
  "id": "sbox-1",
  "workspace_root": "/testbed",
  "state": "stopped",
  "daemon": { "host": "127.0.0.1", "port": 53124 }
}
```

Error — unknown sandbox:

```json
{ "error": { "kind": "invalid_request", "message": "sandbox not found: sbox-1", "details": {} } }
```

If the runtime destroy fails, the record is left in `failed` and the error is returned.

## Related

- [[list_sandboxes]]
- [[inspect_sandbox]]
