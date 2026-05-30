# Module `runtime` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/runtime/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**1 classes across 1 files.**

The `runtime` module owns the process-wide runtime bootstrap surface for non-server entrypoints (benchmarks, the live-e2e harness, CLI helpers), resurrecting the runtime symbols that previously lived in the deleted `server.app_factory`. Its sole class is `RuntimeConfig`, a durable dataclass carrying the agent working directory, an optional external streaming provider client, seed messages, and an optional per-agent `event_source_factory` (used by the mock harness to drive the real query loop against a scripted source); it is read by `engine.agent.factory` and `task_center.launcher`. Beyond the class, the module exposes module-level store singletons (`task_center_store`, `agent_run_store`, `model_store`) and the idempotent `ensure_runtime_stores_ready` bootstrap, which binds those stores to the SQLAlchemy session factory and seeds the model registry from `models/registry.json`, falling back to file-only persistence when no database is configured.

## Contents

- **`runtime/app_factory.py`** — `RuntimeConfig`

---

## `runtime/app_factory.py`

#### `RuntimeConfig`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L45]

Durable runtime configuration shared by request-scoped agents.

**Fields**

| name | type | default |
|------|------|---------|
| `cwd` | `str` |  |
| `external_api_client` | `'SupportsStreamingMessages \| None'` | `None` |
| `_initial_messages` | `list[dict] \| None` | `field(default=None, repr=False)` |
| `event_source_factory` | `'Callable[[AgentDefinition], EventSource] \| None'` | `None` |

<details><summary>Methods (1)</summary>

`resolve_settings`

</details>

