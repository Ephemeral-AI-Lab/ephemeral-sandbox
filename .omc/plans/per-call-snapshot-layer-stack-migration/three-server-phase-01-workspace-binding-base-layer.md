# Phase 01 - Workspace Binding and Base Layer

**Status:** implemented
**Source:** `three-server-command-exec-workspace-replacement-simplified.md`

## 1. Task Specification

Create the durable workspace binding owned by `layer-stack-server`, build the
assigned workspace base once, and make guarded reads use the layer-stack active
manifest instead of the real `/testbed`.

Implementation scope:

```text
add WorkspaceBinding and workspace.json
add deterministic full /testbed workspace base
bind workspace_root=/testbed to layer_stack_root outside /testbed
route guarded read_file to layer-stack read APIs
fail closed when binding, active manifest, or full-copy representation is missing
```

Out of scope:

```text
no OCC mutation routing
no command execution mount namespace
no Git or gitignore classification inside layer-stack
no recovery rebase API yet
```

Exit condition:

```text
setup can bind and build /testbed into manifest version 1, read_file can read
seeded content from layer-stack, and layer-stack base build has no Git-aware policy.
```

## 2. Main Data Objects

```text
WorkspaceBinding
  workspace_root: /testbed
  layer_stack_root: /tmp/eos-sandbox-runtime/layer-stack
  active_manifest_version
  active_root_hash
  base_manifest_version
  base_root_hash
```

## 3. File/Folder Structure Change

Target additions:

```text
backend/src/sandbox/layer_stack/
+-- workspace.py
+-- workspace_base.py
+-- metrics.py

backend/src/sandbox/runtime/
+-- layer_stack_server.py
+-- layer_stack_handlers.py

backend/tests/unit_test/test_sandbox/test_layer_stack/
+-- test_workspace_base.py
+-- test_workspace_binding.py
```

Expected updates:

```text
backend/src/sandbox/control/ops/setup.py
backend/src/sandbox/api/tool/read.py
backend/src/sandbox/api/status/__init__.py
```

## 4. Workflow Demonstration

```text
status.create_sandbox(project_dir="/testbed")
  -> provider creates sandbox with real /testbed
  -> setup_after_create(...)
  -> start layer-stack-server
  -> layer-stack-server bind_workspace("/testbed", layer_stack_root)
  -> build_workspace_base()
       walk real /testbed as a full copy
       write base layer L000001
       write manifest version 1
       write workspace.json with active root and base root
  -> read_file("src/a.py")
       layer-stack-server reads active manifest
       merged view returns content from layer-stack
```

Failure behavior:

```text
layer_stack_root inside /testbed        -> reject binding
existing manifest without reset         -> reject base build
special file cannot be represented      -> fail before binding
workspace changes during base build     -> fail before binding
missing workspace binding on read       -> fail closed
```

## 5. Naming Conventions and Rationale

| Name | Rationale |
|---|---|
| `WorkspaceBinding` | Names the durable binding between the assigned workspace and layer-stack storage. |
| `workspace_root` | The guarded workspace path, default `/testbed`. |
| `layer_stack_root` | Runtime storage location outside `workspace_root`. |
| no Git names | Layer-stack stores bytes and manifests; OCC owns Git/gitignore policy later. |

## 6. Tests and Exit Criteria

```text
uv run pytest backend/tests/unit_test/test_sandbox/test_layer_stack/test_workspace_base.py -q
uv run pytest backend/tests/unit_test/test_sandbox/test_layer_stack/test_workspace_binding.py -q
uv run pytest backend/tests/unit_test/test_sandbox/test_api/test_read.py -q
```

Required assertions:

- empty stack builds `/testbed` as manifest version 1
- base build stores root hash without policy or report contracts
- repeated base build fails unless explicit reset/recovery is requested
- base build fails if any workspace entry cannot be represented
- read after base build uses layer-stack content only
- base code contains no source-control classification branches

## 7. Implementation Notes

Implemented in:

```text
backend/src/sandbox/layer_stack/workspace.py
backend/src/sandbox/layer_stack/workspace_base.py
backend/src/sandbox/runtime/layer_stack_server.py
backend/src/sandbox/runtime/layer_stack_handlers.py
backend/src/sandbox/control/ops/setup.py
backend/src/sandbox/runtime/api_handlers.py
```

Live `/testbed` read-load coverage:

```text
backend/tests/live_e2e_test/sandbox/layer_stack_overlay_occ/test_workspace_base_read_load.py
```
