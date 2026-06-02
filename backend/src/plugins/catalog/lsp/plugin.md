---
name: lsp
description: Pyright-backed LSP tools for Python - hover, find_definitions, find_references, diagnostics, query_symbols, rename, format, code_actions, apply_code_action, apply_workspace_edit.
kind: language_server
tools:
  - name: lsp.hover
    module: tools/hover.py
  - name: lsp.find_definitions
    module: tools/find_definitions.py
  - name: lsp.find_references
    module: tools/find_references.py
  - name: lsp.diagnostics
    module: tools/diagnostics.py
  - name: lsp.query_symbols
    module: tools/query_symbols.py
  - name: lsp.apply_workspace_edit
    module: tools/apply_workspace_edit.py
  - name: lsp.rename
    module: tools/rename.py
  - name: lsp.format
    module: tools/format.py
  - name: lsp.code_actions
    module: tools/code_actions.py
  - name: lsp.apply_code_action
    module: tools/apply_code_action.py
setup: setup.sh
runtime: runtime/server.py
---

# LSP Plugin

Provides Python language-server tools backed by `pyright-langserver --stdio`.
The plugin runs inside the sandbox; the host calls into it through
`call_plugin`. The plugin keeps a long-lived Pyright child per layer-stack
root, rooted directly at the daemon overlay workspace (`/testbed`). Each tool
call enters through the daemon overlay freshness gate before talking to
Pyright, so the session sees the latest workspace state without materialized
projection paths.

## Tools

- `lsp.hover` — symbol info / type info at a `(file_path, line, character)`
  cursor.
- `lsp.find_definitions` — definition locations for a symbol cursor.
- `lsp.find_references` — references to the symbol at a cursor.
- `lsp.diagnostics` — diagnostics for a file (errors, warnings, hints).
- `lsp.query_symbols` — workspace symbol search by name fragment.
- `lsp.apply_workspace_edit` — apply a provided LSP WorkspaceEdit and publish it.
- `lsp.rename` — compute a Pyright rename edit, apply it, and publish it.
- `lsp.format` — compute a formatting edit, apply it, and publish it.
- `lsp.code_actions` — return Pyright code actions for a file range.
- `lsp.apply_code_action` — apply and publish a WorkspaceEdit from a code action.

## Setup

The host-side plugin installer downloads the pinned Linux Node archive and
Pyright npm tarball locally, uploads them with `put_archive` into
`/eos/plugin-packages/lsp`, then runs `setup.sh` offline in the sandbox.
`setup.sh` extracts Node into `/eos/plugin-packages/lsp/node`, installs the
uploaded Pyright tarball there, and writes a marker so re-runs are cheap.

## Constraints

- Plugin cache state must stay outside `/testbed`.
- WorkspaceEdit application supports standard `changes`, text-document
  `documentChanges`, and LSP create/delete/rename file operations.
