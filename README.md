# EphemeralOS

EphemeralOS is a Rust agent harness — the runtime, persisted state, model-facing
tools, and sandbox substrate that turn an LLM into a working agent. It is
organized as two Cargo workspaces: an **agent control plane** (`agent-core/`)
and a **sandbox substrate** (`sandbox/`).

Rust is the only implementation; the workspaces target Rust 2021 with
`rust-version = "1.85"`.

## Layout

```
agent-core/crates/                    # Agent control plane
  eos-runtime                         # Request bootstrap and root-agent entry
  eos-engine                          # Query loop, streaming executor, tool dispatch, background supervisor
  eos-workflow                        # Delegated workflow lifecycle, attempts, context packets, plan DAG
  eos-tools                           # Model-facing tools (sandbox, skills, subagent, submissions)
  eos-state / eos-db                  # Persisted request/task/workflow state DTOs and SQL stores
  eos-sandbox-api / eos-sandbox-host  # Host-side sandbox protocol, provisioning, and lifecycle
  eos-agent-def / eos-skills / eos-plugin-catalog  # Agent profiles, skills, and the plugin catalog
  eos-llm-client                      # Provider client and streaming
  eos-config / eos-types              # Configuration and shared id/timestamp/json/error primitives
  eos-audit / eos-obs-collector       # Write-only audit side channel and reader-side normalization
  eos-testkit                         # Shared test doubles (dev-only)

sandbox/crates/                       # Sandbox substrate
  eosd                                # Daemon binary
  eos-daemon                          # Daemon RPC, dispatch, command/plugin/isolated routing
  eos-protocol                        # Shared wire protocol
  eos-layerstack / eos-occ            # LayerStack and optimistic concurrency control
  eos-overlay / eos-runner            # Overlay execution and namespace runner
  eos-command-session / eos-ns-holder # Command sessions and namespace holder
  eos-workspace-api                   # Workspace API surface
  eos-ephemeral-workspace / eos-isolated-workspace  # Shared and isolated workspace lifecycle
  eos-plugin                          # Plugin PPC
  eos-config                          # Sandbox config
  eos-e2e-test                        # Protocol-level end-to-end suite over a live eosd

.eos-agents/                          # Shipped agent profiles and their coupled skills
docs/architecture/                    # Maintained architecture and codebase-memory bundle
```

## Build

The two workspaces build independently:

```bash
(cd agent-core && cargo build)
(cd sandbox && cargo build)
```

## Test

Run scoped tests from the owning workspace:

```bash
cd agent-core && cargo test -p eos-engine     # a single crate
cd sandbox    && cargo test -p eos-daemon
```

## Architecture

`docs/architecture/index.html` is the maintained codebase-memory and
architecture bundle. It links the module pages for the workflow, agent loops,
tools, and sandbox subsystems, each documenting ownership, invariants, and
refresh triggers.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

MIT.
