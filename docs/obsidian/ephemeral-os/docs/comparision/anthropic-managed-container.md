---
title: Anthropic Managed Container vs EphemeralOS
tags:
  - ephemeral-os
  - comparison
  - anthropic
  - managed-agents
  - code-execution
status: draft
---

# Anthropic Managed Container vs EphemeralOS

"Anthropic's managed container" means a container **Anthropic provisions and runs
for you** — you operate no infrastructure. It maps to two distinct surfaces in
the Claude platform, both first-party to the model vendor. Neither occupies
[[ephemeral-os|EphemeralOS]]'s shared-overlay + OCC square; both sit in the
*"let the vendor run the box"* corner of the [[landscape]].

> [!note] Evidence basis
> EphemeralOS rows are verified against this repo's code. Anthropic rows are from
> the Claude API skill docs (code execution tool + Managed Agents), current as of
> 2026-06.

## The two things called "managed container"

### 1. Code Execution Tool container (one server-side tool)

A sandboxed container Claude runs code in, declared as a single tool in a normal
Messages API call. You execute nothing client-side.

```json
{ "type": "code_execution_20260120", "name": "code_execution" }
```

- Isolated container: **1 CPU, 5 GiB RAM, 5 GiB disk**.
- **No internet access** (fully sandboxed).
- **Python 3.11** + data-science libs preloaded; `pip install` at runtime.
- Auto-grants `bash_code_execution` + `text_editor_code_execution` sub-tools.
- **Persists 30 days, reusable** via `response.container.id` → `container=<id>`.
- Pricing: free alongside web search/fetch; else $0.05/hr after 1,550 free
  hrs/month per org.

The narrow answer: a managed code sandbox for "run this code, I won't host the
runtime."

### 2. Managed Agents (CMA) per-session container (the agent workspace)

The broader answer, and the one closest to this comparison. Anthropic runs the
**agent loop** on its orchestration layer *and* provisions a **container per
session** where the agent's tools (bash, file ops, code) execute. The loop acts
*on* the container via tool calls; it does not run inside it.

- **Environment** = reusable container-provisioning template. `config.type:
  "cloud"` (Anthropic-hosted) or **`"self_hosted"`** (container on *your* infra
  via an outbound-polling worker; loop still Anthropic-side).
- **Networking:** `unrestricted` (full egress) or `limited` (deny-by-default;
  allowlist hosts / package managers / MCP).
- **Resources mount in:** uploaded files, GitHub repos, memory stores. Outputs
  land in `/mnt/session/outputs/`, retrieved via the Files API.
- **Lifecycle managed:** provision → run/idle → terminate, with compaction,
  prompt caching, and extended thinking built in.

"Managed" in both = Anthropic owns provisioning, isolation, scaling, teardown.
The difference: #1 is *one tool for running code*; #2 is a *hosted-agent product
where the container is the workspace*.

## Side by side

| Axis | **Anthropic managed container** | **EphemeralOS** |
|---|---|---|
| **What it is** | Vendor-run sandbox: code-execution tool (single tool) or CMA per-session workspace | Self-hosted sandbox runtime over a CoW layerstack |
| **Who runs it** | Anthropic (cloud); optionally your infra via CMA `self_hosted` worker | You — privileged Linux container + kernel overlayfs |
| **Isolation unit** | One container per code-exec session / per agent session | Namespace-isolated command over an overlay workspace |
| **Multi-agent model** | **Per-session isolation** — each session its own container | **Shared-base** — agents publish to one layerstack |
| **Reconciliation (code)** | **Git**: mount repo → edit → push branch via `bash` → PR via GitHub MCP | **OCC publish** against a moving layerstack base |
| **Networking** | `unrestricted` / `limited` (allowlist) | `shared` / `isolated` netns profile per workspace |
| **Storage backend** | Anthropic-managed container FS (ephemeral; code-exec persists 30d) | Content-addressed layerstack + overlay upperdir |
| **Auditability** | Event stream, session outputs, usage telemetry (closed) | Observability snapshots/traces + per-layer attribution |
| **Footprint** | Zero — fully hosted (or your worker for `self_hosted`) | Privileged Linux host + overlayfs |
| **Coupling** | Bundled with Claude (the loop + tools ship together) | Model-agnostic; any client over JSON-RPC |

## Where it sits vs EphemeralOS

- **Camp:** the first-party, fully-managed member of the **isolation-only /
  session-isolation infra** camp — the cloud counterpart to E2B / Modal /
  Daytona, except run by the model vendor with the agent loop bundled (CMA).
- **vs shared-base + OCC:** no shared mutable mainline, no OCC publish. Each
  session gets its **own** container. For *code*, reconciliation is **git, not
  overlay** (push a branch, open a PR via GitHub MCP) — so for coding it behaves
  like the [[container-use|container-use]] / git-merge camp; the raw container is
  session-isolation infra.
- **`self_hosted` nuance:** CMA's self-hosted environment is the one real overlap
  with EphemeralOS — BYO-infra container execution via an outbound worker. But it
  still isolates per session and reconciles via git, not a shared layerstack.
- **Auditability:** managed/closed (event stream + outputs + usage), not a
  queryable filesystem log ([[agentfs|AgentFS]]) or a content-addressed layer
  history (EphemeralOS).

Net: it's the *"let the vendor run the box"* point on the map. It competes with
the infra camp on hosting and with container-use on the coding-agent workflow,
but does not occupy EphemeralOS's shared-overlay-mainline + OCC square.

## Sources

- Code Execution Tool — Claude API skill (`shared/tool-use-concepts.md`); docs:
  `https://platform.claude.com/docs/en/agents-and-tools/tool-use/code-execution-tool.md`
- Managed Agents — Claude API skill (`shared/managed-agents-*.md`); docs:
  `https://platform.claude.com/docs/en/managed-agents/overview.md`
- Self-hosted sandboxes —
  `https://platform.claude.com/docs/en/managed-agents/self-hosted-sandboxes.md`
