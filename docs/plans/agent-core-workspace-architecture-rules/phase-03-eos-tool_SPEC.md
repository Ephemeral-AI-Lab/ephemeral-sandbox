# Phase 03 - eos-tool Spec

Status: Draft
Date: 2026-06-09
Owner: eos-tool

## Scope

This phase rebuilds `eos-tool` as the single owner of the tool framework and
concrete model-callable tool behavior.

It folds the current `eos-tool-ports` crate into `eos-tool` and collapses the
current one-file-per-tool command layout into family-level handlers.

## Local Architecture

`eos-tool` owns:

- tool names and keys,
- tool intent and output shape,
- execution metadata facts,
- tool result DTOs,
- registered tool entries,
- tool registry,
- tool executor trait,
- hook definitions and hook execution,
- concrete model-callable tool behavior,
- skill registry and skill package loading,
- runtime resources passed into registry construction by `eos-agent-core` and
  used during execution by `eos-engine`.

`eos-tool` does not own:

- agent-loop turn control,
- model provider streaming,
- agent-run lifecycle rows,
- workflow state transitions,
- sandbox daemon protocol internals.

## Resulting File Structure

```text
agent-core/crates/eos-tool/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── error.rs
│   ├── model.rs
│   ├── registry.rs
│   ├── hooks.rs
│   ├── tools.rs
│   ├── tools/
│   │   ├── sandbox.rs
│   │   ├── command.rs
│   │   ├── workflow.rs
│   │   ├── subagent.rs
│   │   ├── submission.rs
│   │   ├── skills.rs
│   │   └── terminal.rs
└── tests/
    ├── registry/
    ├── sandbox/
    ├── workflow/
    ├── subagent/
    ├── submission/
    └── skills/
```

No `builtins.rs` or `builtins/` folder is required in the first target. The
built-in tool set is closed and should be represented through default registry
registration plus family handlers in `tools/`.

No first-target `catalog.rs`, `executor.rs`, `handles.rs`, or `hooks/` folder is
allowed. Those splits are only acceptable later if one file becomes materially
harder to understand after implementation.

## Module Collapse Plan

| Current pattern | Target |
| --- | --- |
| `tools/sandbox/exec_command.rs` | `registry.rs` default entry plus `tools/command.rs` handler |
| `tools/sandbox/read_file.rs` | `registry.rs` default entry plus `tools/sandbox.rs` handler |
| `tools/sandbox/write_file.rs` | `registry.rs` default entry plus `tools/sandbox.rs` handler |
| `tools/sandbox/edit_file.rs` | `registry.rs` default entry plus `tools/sandbox.rs` handler |
| `tools/sandbox/multi_edit.rs` | `registry.rs` default entry plus `tools/sandbox.rs` handler |
| `tools/sandbox/write_stdin.rs` | `registry.rs` default entry plus `tools/command.rs` handler |
| `tools/workflow/*.rs` | `registry.rs` default entry plus `tools/workflow.rs` handler |
| `tools/subagent/*.rs` | `registry.rs` default entry plus `tools/subagent.rs` handler |
| `tools/submission/**/*.rs` | `registry.rs` default entry plus `tools/submission.rs` handler |
| `tools/skills/*.rs` | `registry.rs` default entry plus `tools/skills.rs` handler |
| `tools/ask_helper/*.rs` | `registry.rs` default entry plus `tools/subagent.rs` helper path |
| `tools/terminal.rs` | `tools/terminal.rs` |

## Runtime Resource Rules

`eos-tool` should not export `*Service` types. It exports a small runtime
resource struct passed into registry construction and captured by concrete
tools.

The first target uses `ToolRuntime` in `registry.rs`; it does not create
`handles.rs`.

Allowed `ToolRuntime` fields:

| Resource | Built by | Used by |
| --- | --- |
| sandbox resource | `eos-agent-core` | `tools/sandbox.rs`, isolated-workspace behavior |
| command-session resource | `eos-engine` | `tools/command.rs`, hook policy |
| workflow resource | `eos-agent-core` or `eos-engine` wrapper | `tools/workflow.rs`, hook policy |
| subagent resource | `eos-agent-run` / `eos-engine` wrapper | `tools/subagent.rs`, hook policy |
| submission resource | `eos-agent-core`, `eos-agent-run` if needed | `tools/submission.rs` |
| skill resource | `eos-agent-core` | `tools/skills.rs` |
| hook policy facts | `eos-agent-core` + `eos-engine` | `hooks.rs` |

Rejected `Service` names:

| Pattern | Replacement |
| --- | --- |
| private tool executor resource group | `ToolRuntime` |
| static registry config holder | `ToolRegistry` default entries |
| hook-only private state | `HookPolicy` or private fields in `ToolRuntime` |
| test-only helper | test fixture name |

## Public Surface

Target `lib.rs` exports only:

```rust
pub use error::ToolError;
pub use model::{ExecutionMetadata, ToolIntent, ToolKey, ToolName, ToolResult};
pub use registry::{RegisteredTool, ToolExecutor, ToolRegistry, ToolRuntime};
pub use hooks::{Hook, HookOutcome};
```

The exact names may change during implementation, but the surface must stay
small and owner-accurate.

## Progress Tracker

| Item | Status |
| --- | --- |
| Create `eos-tool` crate target or rename `eos-tools` | Not started |
| Fold `eos-tool-ports` model types | Not started |
| Fold registry, executor trait, and default tool registration into `registry.rs` | Not started |
| Move hooks into `eos-tool/hooks.rs` | Not started |
| Move concrete tool behavior into `tools/` family modules | Not started |
| Define `ToolRuntime` in `registry.rs` | Not started |
| Collapse sandbox command files | Not started |
| Collapse workflow/subagent/submission files | Not started |
| Fold advisor helper behavior into `tools/subagent.rs` | Not started |
| Collapse skill tool files | Not started |
| Remove obsolete one-file-per-tool deep tree | Not started |
| Update engine and agent-core imports | Not started |

## Acceptance Criteria

- No `eos-tool-ports` crate remains.
- `eos-tool` has `tools.rs` and family-level `tools/` modules.
- `eos-tool` has `hooks.rs`.
- `eos-tool` has no first-target `catalog.rs`, `executor.rs`, `handles.rs`, or
  `hooks/` folder.
- `eos-tool` has no `services.rs` or `services/` module.
- `eos-tool` has no one-file-per-tool-command module tree.
- `eos-tool` exports no `*Service` types.
- Private resource groups are fields on `ToolRuntime`, not `Service`.
- `eos-engine` imports tool framework contracts from `eos-tool`.
- `eos-agent-core` builds `ToolRuntime` through `eos-tool`.
- `cargo test -p eos-tool` passes.
- `cargo check -p eos-engine --all-targets` and
  `cargo check -p eos-agent-core --all-targets` pass after import updates.
- `eos-tool` final module count is at or below 16.
