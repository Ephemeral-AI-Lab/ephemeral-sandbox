---
title: inspect_sandbox
tags:
  - ephemeral-os
  - cli
  - manager
  - management
status: ready
---

# inspect_sandbox

**Execution space:** `manager` (system scope) · **Family:** `management`

Inspect one sandbox record.

## Manual

Inspect one sandbox record, including lifecycle state, workspace root, and configured daemon endpoint metadata.

| Argument | Flag | Kind | Required | Default | Description |
|---|---|---|---|---|---|
| `sandbox_id` | `--sandbox-id` | string | yes | — | Sandbox id. |

**Usage**

```
sandbox-cli manager inspect_sandbox --sandbox-id ID
```

**Examples**

```sh
sandbox-cli manager inspect_sandbox --sandbox-id sbox-1
```

## Expected output

Success — the single sandbox record:

```json
{
  "id": "sbox-1",
  "workspace_root": "/testbed",
  "state": "ready",
  "daemon": { "host": "127.0.0.1", "port": 53124 }
}
```

Error — unknown sandbox:

```json
{ "error": { "kind": "invalid_request", "message": "sandbox not found: sbox-1", "details": {} } }
```

## Related

- [[list_sandboxes]]
