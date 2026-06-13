# eos-coding-agent

Host project for composing `eos-agent-sdk` into the coding-agent product.

The root package is the application. Keep host-private composition, config, and
profile wiring under `src/`; reserve `packages/` for real package boundaries.
Nothing should live under `packages/app`.

| Location | Owner |
|---|---|
| `src/bootstrap.ts` | composition root over SDK, config, tools, and workflow providers |
| `src/config/` | `.eos-agents` config, profile, hook, LLM-client, and workflow loaders |
| `src/agents/` | concrete `buildAgentFactory` and advisory support |
| `src/tools/` | model-visible tool implementations |
| `src/workflows/` | `WorkflowHub`, provider contracts, pursuit provider adapter, and context-script wiring |
| `packages/workflows/pursuit/` | pursuit domain contracts, state, DB, context projection, and service |
| `packages/scripts/` | subprocess JSON command runner |
| `packages/testkit/` | `.eos-agents` fixture building |

Run package-manager commands from this directory with `pnpm`.
