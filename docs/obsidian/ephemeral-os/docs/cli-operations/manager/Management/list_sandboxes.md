---
title: list_sandboxes
tags:
  - ephemeral-os
  - cli
  - manager
  - management
status: ready
---

# list_sandboxes

**Execution space:** `manager` (system scope) · **Family:** `management`

List sandbox records known to the manager.

## Manual

List sandbox records known to the manager, including lifecycle state and configured daemon endpoint metadata. Records are returned sorted by sandbox id.

This operation takes no arguments.

**Usage**

```
sandbox-cli manager list_sandboxes
```

**Examples**

```sh
sandbox-cli manager list_sandboxes
```

## Expected output

Success — an array of sandbox records (empty when none exist):

```json
{
  "sandboxes": [
    {
      "id": "sbox-1",
      "workspace_root": "/testbed",
      "state": "ready",
      "daemon": { "host": "127.0.0.1", "port": 53124 }
    },
    {
      "id": "sbox-2",
      "workspace_root": "/work",
      "state": "creating",
      "daemon": null
    }
  ]
}
```

`daemon` is `null` until the sandbox reaches `ready`. `state` is one of `creating | ready | stopping | stopped | failed`.

## Related

- [[inspect_sandbox]]
- [[docs/cli-operations/manager/Management/create_sandbox]]
