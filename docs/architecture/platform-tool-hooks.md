# Platform Tool Hooks

Platform tool hooks are the in-process interception system for tool execution.
They replace the legacy user-configurable subprocess hook bus and are owned by
the runtime, not by user configuration.

This design note defines the current execution contract after migration. It is
intentionally limited to control flow, user/API visibility, and concurrency
semantics.

## Goals

- Run tool pre-hooks and post-hooks as first-class phases in the tool execution
  timeline.
- Preserve exactly one API-facing tool result for every model tool use.
- Show advisory hook messages to users immediately without sending those
  advisory messages to the model API.
- Keep foreground tool calls concurrent across a model turn.
- Preserve strict hook ordering within each individual tool call.
- Allow pre-hooks to transform parsed tool arguments before later pre-hooks and
  the tool body see them.
- Abort tool execution immediately when a pre-hook fails or denies the call.
- Let post-hooks inspect the final tool result and optionally replace it with a
  failed tool result.

## Non-Goals

- Do not keep user-configurable command, HTTP, prompt, or agent subprocess hooks.
- Do not send advisory notifications to the model as system reminders.
- Do not serialize all foreground tool calls just because one or more tool calls
  have hooks.
- Do not use a notification drain as the main hook delivery mechanism.
- Do not batch advisory messages into one aggregate notification.
- Do not require a complex advisory object when the tool name plus message is
  enough.

## Legacy Hook Removal

The old hook bus accepted user configuration that could run shell commands,
HTTP calls, model prompts, or agent-like validation. The migration removes that
feature. Hooking becomes a platform-owned interception mechanism registered in
code.

The legacy event enum is no longer the right dispatch shape. Platform tool hooks
dispatch by tool name, phase, and priority. The only meaningful phases are
pre-tool and post-tool, and they are not lifecycle events exposed to user config.

Removing the old bus means deleting the schema types for command, HTTP, prompt,
and agent hooks; removing the executor paths for those hook types; removing
settings-based hook loading; removing the settings field that stores configured
hooks; and removing runtime wiring for `hook_executor`.

## Hook Phases

Each tool call has three ordered phases:

1. Pre-hook phase.
2. Tool execution phase.
3. Post-hook phase.

The pre-hook and post-hook phases are blocking for that individual tool call.
The tool execution phase may stream normal tool events while the tool is
running. Other tool calls in the same model turn may run concurrently and may
interleave their events.

## End-To-End Workflow

The platform hook runner owns one tool call from parsed model request through
the final API-facing tool result. The workflow below is the normative ordering
for one tool use.

| Step | Runtime action | User-visible event | API-visible effect |
| --- | --- | --- | --- |
| 1 | Receive model tool use and find the tool definition. | None. | None. |
| 2 | Validate raw tool input with the tool input model. | None unless validation is surfaced by normal tool-result rendering. | Invalid input returns one failed tool result. |
| 3 | Run matching pre-hooks in priority order. | Each advisory emits immediately as its own `SystemNotification`. | No API effect unless a pre-hook fails or denies. |
| 4 | Apply any pre-hook argument mutation before the next pre-hook and before tool execution. | Optional advisory may explain the mutation. | The API does not see advisory text. |
| 5 | Stop on pre-hook failure or denial. | Optional normal tool-completion event may show the failure result. | One failed tool result is returned; the tool body and post-hooks are skipped. |
| 6 | Run the tool with final transformed arguments. | Normal started, progress, and completion events may stream. | The eventual tool result is held until post-hooks finish. |
| 7 | If no post-hooks match, finalize the tool result. | Tool completion is emitted. | The tool result is returned. |
| 8 | If post-hooks match, run them in priority order. | Each advisory emits immediately as its own `SystemNotification`. | No API effect unless a post-hook fails or denies. |
| 9 | Stop on post-hook failure or denial. | Optional normal tool-completion event may show the failure result. | One failed tool result replaces the original API-facing result. |
| 10 | Finalize the selected result. | Tool completion is emitted if it has not already been emitted by the runner. | Exactly one tool result is returned for the model tool use. |

Diagram: one tool call lifecycle.

| Phase | Ordered path |
| --- | --- |
| Input | model tool use -> tool lookup -> input validation |
| Pre-hook | pre-hook 1 -> optional advisory notification -> optional arg mutation -> pre-hook 2 -> optional advisory notification -> continue until done or denied |
| Tool | final args -> tool starts -> optional progress -> tool returns result |
| Post-hook | post-hook 1 -> optional advisory notification -> post-hook 2 -> continue until done or denied |
| Result | selected final result -> one API tool result |

## Pre-Hook Phase

Pre-hooks run sequentially in priority order for one tool call.

The pre-hook pipeline starts with the parsed tool arguments. Each hook receives
the current arguments. If a hook returns transformed arguments, the transformed
arguments become the current arguments for the next hook. The final transformed
arguments are passed to the tool body.

If a pre-hook returns an advisory, the runtime emits one user-visible
`SystemNotification` immediately for each advisory. The notification text must
include the tool name and the advisory message. Advisory notifications are not
sent to the model API and are not appended to conversation history as
`SystemReminderBlock` messages.

If a pre-hook fails or denies the call, the pre-hook chain stops immediately.
Remaining pre-hooks do not run. The tool body does not run. The runtime returns
a failed tool result to the model API for that tool use. The failed tool result
is the only pre-hook message that reaches the API.

Pre-hook advisories do not stop the pipeline. They also are not accumulated for
later emission.

## Pre-Hook Workflow

Pre-hook execution is a strict chain. The chain is local to one tool call and is
independent from other concurrently running tool calls.

| Current hook outcome | Runtime action | Next step |
| --- | --- | --- |
| No effect | Keep current arguments. | Run the next pre-hook. |
| Argument mutation | Replace current arguments with the transformed arguments. | Run the next pre-hook with transformed arguments. |
| One advisory | Emit one `SystemNotification` that names the tool and message. | Run the next pre-hook. |
| Multiple advisories | Emit one separate `SystemNotification` per advisory, in the order returned by the hook. | Run the next pre-hook. |
| Denial | Stop the pre-hook chain. | Return a failed tool result; skip tool execution and post-hooks. |
| Hook exception | Stop the pre-hook chain. | Return a failed tool result; skip tool execution and post-hooks. |

Diagram: pre-hook argument flow.

| Link | Input args | Hook result | Output args |
| --- | --- | --- | --- |
| Start | Parsed tool args | None. | Parsed tool args. |
| Pre-hook A | Parsed tool args | Mutates args and emits advisory. | Args from pre-hook A. |
| Pre-hook B | Args from pre-hook A | No effect. | Args from pre-hook A. |
| Pre-hook C | Args from pre-hook A | Mutates args again. | Args from pre-hook C. |
| Tool body | Args from pre-hook C | Tool executes. | Tool result. |

Diagram: pre-hook denial flow.

| Link | Runtime state |
| --- | --- |
| Parsed input | Validated successfully. |
| Pre-hook A | Advisory emitted to user only; chain continues. |
| Pre-hook B | Denial returned. |
| Stop point | Pre-hook C does not run. |
| Tool body | Does not run. |
| Post-hooks | Do not run. |
| API result | Failed tool result with pre-hook denial message. |

## Tool Execution Phase

Tool execution starts only after the pre-hook phase finishes without error.

The tool receives the final transformed arguments from the pre-hook pipeline.
Normal tool execution events remain streamable. Long-running tools may emit
progress while they run. The runtime still produces one eventual tool result for
the model API.

If no post-hooks match the tool, the tool result is final as soon as the tool
execution phase completes.

## Tool Execution Workflow

The tool execution phase is the only phase that may emit normal tool progress.
Pre-hooks and post-hooks are blocking phases around it.

| Tool execution event | Runtime action | User-visible event | API-visible effect |
| --- | --- | --- | --- |
| Tool starts | Runtime starts the tool with final transformed args. | `ToolExecutionStarted`. | None yet. |
| Tool emits progress | Runtime forwards progress from the tool or background manager. | `ToolExecutionProgress`. | None yet. |
| Tool returns success | Runtime validates and holds the result. | Completion may be delayed until post-hooks finish. | Result is not final until post-hooks finish. |
| Tool returns failure | Runtime holds the failed result. | Completion may be delayed until post-hooks finish. | Result is not final until post-hooks finish. |
| Tool raises unexpected exception | Runtime converts it to failed tool result. | Completion may be delayed until post-hooks finish. | Failed result is not final until post-hooks finish. |

If there are no post-hooks for the tool, the held tool result is immediately
selected as the final result.

## Post-Hook Phase

Post-hooks run sequentially after the tool body has produced a result. They run
only when the tool body actually executed. A pre-hook denial skips post-hooks.

Post-hooks receive the tool name, the final arguments used by the tool, the
execution context, and the tool result. Post-hook advisories are emitted
immediately as user-only `SystemNotification` events. Each notification includes
the tool name and advisory message. These notifications are not sent to the API
and are not accumulated.

If a post-hook fails or denies the result, the post-hook chain stops
immediately. The runtime returns a failed tool result to the model API. The
failed post-hook result replaces the original API-facing tool result, but the
runtime may preserve original result details in metadata or telemetry for audit.

If post-hooks all complete without failure, the original tool result remains the
API-facing result.

## Post-Hook Workflow

Post-hooks are validators and observers for a completed tool execution. They do
not mutate tool arguments. They may emit user-only advisories. They may replace
the API-facing result with a failed result if policy requires.

| Current hook outcome | Runtime action | Next step |
| --- | --- | --- |
| No effect | Keep the held tool result unchanged. | Run the next post-hook. |
| One advisory | Emit one `SystemNotification` that names the tool and message. | Run the next post-hook. |
| Multiple advisories | Emit one separate `SystemNotification` per advisory, in the order returned by the hook. | Run the next post-hook. |
| Denial | Stop the post-hook chain. | Replace the held result with a failed post-hook result. |
| Hook exception | Stop the post-hook chain. | Replace the held result with a failed post-hook result. |

Diagram: post-hook success flow.

| Link | Runtime state |
| --- | --- |
| Tool body | Tool returned success or failure. |
| Post-hook A | Advisory emitted to user only; chain continues. |
| Post-hook B | No effect; chain continues. |
| End of chain | Original tool result remains selected. |
| API result | Original tool result is returned to the model. |

Diagram: post-hook denial flow.

| Link | Runtime state |
| --- | --- |
| Tool body | Tool returned success or failure. |
| Post-hook A | Advisory emitted to user only; chain continues. |
| Post-hook B | Denial returned. |
| Stop point | Post-hook C does not run. |
| Result selection | Original tool result is replaced for API purposes. |
| API result | Failed post-hook result is returned to the model. |

## Advisory Visibility

Advisories are for users and operators, not for the model.

The runtime emits advisories as `SystemNotification` stream events only. It does
not append advisory messages to `display_messages`. It does not wrap advisories
in `SystemReminderBlock`. It does not include advisory text in the API request
history unless a separate product decision changes that behavior.

The advisory text format is contractually fixed so the UI and tests can rely on
it:

- Pre-hook: `SystemNotification(text="[pre-hook advisory] {tool_name}: {message}", category="pre_hook_advisory")`.
- Post-hook: `SystemNotification(text="[post-hook advisory] {tool_name}: {message}", category="post_hook_advisory")`.

The tool name plus advisory message is sufficient. The `category` field exists
for UI filtering; the message itself must stand alone.

## Notification Workflow

Advisory notification delivery is synchronous with the hook that produced it.
The runtime does not stage advisory messages in metadata for later draining.

| Advisory source | Runtime delivery | Conversation history | API request history |
| --- | --- | --- | --- |
| Pre-hook advisory | Immediate `SystemNotification`. | Not appended. | Not included. |
| Post-hook advisory | Immediate `SystemNotification`. | Not appended. | Not included. |
| Pre-hook denial | Normal failed tool result. | Appended through normal tool-result flow. | Included as the required tool result. |
| Post-hook denial | Normal failed tool result. | Appended through normal tool-result flow. | Included as the required tool result. |

Diagram: advisory path.

| Producer | User stream | Model API |
| --- | --- | --- |
| Pre-hook returns advisory for `daytona_shell`. | Emits `SystemNotification` naming `daytona_shell`. | No message is added. |
| Pre-hook later allows execution. | Tool starts normally. | Still no advisory message is added. |
| Tool result is finalized. | Tool completion is shown normally. | One tool result is sent. |

## Error Visibility

Hook failures and denials are API-visible because the model must receive a
result for each tool use it requested.

A pre-hook denial returns a failed tool result that states the tool was blocked
before execution and includes the hook error message. A post-hook denial returns
a failed tool result that states post-hook validation failed and includes the
hook error message.

Hook failure messages should include the tool name and enough local context to
make the next model turn actionable. They should not include a batch of prior
advisories, because advisories are already emitted to users separately.

## Error Workflow

Errors are the only hook outputs that cross into the API-facing tool result.

| Error source | Tool body runs? | Post-hooks run? | API-facing result |
| --- | --- | --- | --- |
| Input validation failure | No. | No. | Invalid-input failed tool result. |
| Pre-hook denial | No. | No. | Pre-hook blocked failed tool result. |
| Pre-hook exception | No. | No. | Pre-hook failed tool result. |
| Tool failure | Yes. | Yes, if matching post-hooks exist. | Tool failed result unless post-hook replaces it. |
| Post-hook denial | Yes. | Stops at denying post-hook. | Post-hook blocked failed tool result. |
| Post-hook exception | Yes. | Stops at failing post-hook. | Post-hook failed tool result. |

## Concurrency Model

Foreground tool calls remain concurrent across a model turn.

The runtime does not switch to sequential foreground execution when hooks are
present. Instead, each tool call owns its own ordered execution stream. Within a
single tool call, pre-hooks, tool execution, and post-hooks are ordered. Across
different tool calls, events may interleave.

This means users may see advisory and progress events from different tools
interleaved. That is acceptable as long as every hook advisory and hook error
names the tool.

The query loop should multiplex per-tool execution streams and continue to
collect exactly one final tool result per tool use. Final tool results are then
fed back to the model in the normal tool-result collection step.

## Concurrent Multi-Tool Workflow

When the model requests multiple foreground tools in one turn, the runtime starts
one execution stream per tool call. Each stream preserves local ordering. The
query loop multiplexes events from all active streams.

Diagram: concurrent tool-call streams.

| Time | Tool A stream | Tool B stream | Query loop output |
| --- | --- | --- | --- |
| 1 | Pre-hook A1 emits advisory. | Waiting or running independently. | System notification naming Tool A. |
| 2 | Pre-hook A2 mutates args. | Pre-hook B1 emits advisory. | System notification naming Tool B. |
| 3 | Tool A starts. | Tool B starts. | Tool started events may appear in either completion order. |
| 4 | Tool A emits progress. | Tool B completes tool body. | Progress for Tool A; then Tool B moves to post-hooks. |
| 5 | Tool A continues running. | Post-hook B1 emits advisory. | System notification naming Tool B. |
| 6 | Tool A completes. | Tool B final result selected. | Completion events for both tools as their streams finish. |
| 7 | All streams finished. | All streams finished. | Query loop appends exactly one result per tool use. |

The query loop must not infer advisory ownership from ordering. Every advisory
message must name the originating tool.

## Background Tool Workflow

Background tools still need the same hook semantics. The launch path may return
a foreground acknowledgement to the model while the actual background task runs
later, but the hooked execution of the background task remains a normal tool
execution stream owned by that background task.

| Stage | Runtime action | Hook behavior |
| --- | --- | --- |
| Background request arrives | Runtime validates whether the tool supports background execution. | Platform hooks have not run yet unless the request itself is represented as a normal tool execution. |
| Background task is launched | The model receives a launch acknowledgement. | Launch acknowledgement remains the API-facing result for the original model tool use. |
| Background task executes | The background worker runs the actual tool body. | Pre-hooks run before the background tool body; post-hooks run after it. |
| Background advisories | User stream receives notifications tied to the background task's tool name. | Advisory notifications are not added to API history. |
| Background completion | Runtime delivers the background result through existing background completion flow. | Post-hook denial may replace the background task result. |

The migration must make this boundary explicit so hooks do not accidentally run
only for foreground tools.

## External Trigger Workflow

External triggers execute tools outside the main model streaming loop but still
use production tool execution semantics. They must run platform hooks through
the same execution primitive used by foreground and background tools.

| Stage | Runtime action | Hook behavior |
| --- | --- | --- |
| Trigger builds constrained tool request. | Runtime validates tool input. | Invalid input returns trigger-local failed result. |
| Trigger executes the tool. | Runtime invokes the shared hook-aware execution primitive. | Pre-hooks, tool execution, and post-hooks run in normal order. |
| Advisory produced. | Runtime emits or records trigger-visible notification according to trigger capabilities. | Advisory is not added to model API history. |
| Hook denial produced. | Runtime returns failed tool result to the trigger path. | Denial is visible as the tool result. |

External triggers must not call tool bodies directly if the tool is covered by
platform hook policy.

When a caller cannot surface advisories (headless triggers, tests), it must
still pass a valid `EmitStreamEvent`. The contract is:

- A no-op async `emit` that drops events is legal. The pipeline never assumes
  subscribers.
- `emit` must still be awaitable; the pipeline awaits it for every advisory.
- Callers that want to capture advisories for logs or telemetry can substitute
  an async callable that records events instead of forwarding them.

## Runtime Boundary

The existing plain `run_tool_safely()` shape is not sufficient as the top-level
orchestration boundary because it returns only `ToolResult` and cannot directly
emit ordered stream events.

The migration should introduce a tool-call execution primitive that can emit
stream events while still returning one final `ToolResultBlock`. That primitive
owns pre-hook execution, tool execution, post-hook execution, and final
API-facing result selection.

Lower-level validation and normalization helpers may remain reusable. The
important boundary is that user-visible hook notifications are emitted from the
active execution stream, not staged in metadata for later draining.

## Current Module Shape

The platform hook implementation lives under `tools.core`, not under the
legacy top-level `hooks` package. This avoids retaining the old user-configured
hook concept while making the relationship to tool execution explicit.

```text
backend/src/tools/core/hooks/
  __init__.py
  outcomes.py
  registry.py
  pipeline.py
  execution.py
```

Responsibilities:

- `outcomes.py`: pre-hook and post-hook outcome dataclasses and callable
  protocols.
- `registry.py`: process-global registry keyed by tool glob, phase, and
  priority.
- `pipeline.py`: sequential pre-hook and post-hook chain runners.
- `execution.py`: hook-aware tool execution primitive that emits stream events
  and returns one final `ToolResult`.

## Policy Hook Module Layout

Policy hooks should live with the toolkit that owns the policy. They are hook
modules, not tools. The generic framework belongs in `tools.core.hooks`; Daytona
write-scope and daytona_shell policy belongs in `tools.daytona_toolkit.hooks`.

Current Daytona layout:

```text
backend/src/tools/daytona_toolkit/hooks/
  __init__.py
  _common.py

  prehook/
    __init__.py
    _shell_common.py
    write_scope_hard_block.py
    write_scope_advisory.py
    write_scope_deny.py
    move_src_hard_block.py
    move_src_scope_deny.py
    move_dst_scope_advisory.py
    shell_destructive_git.py
    shell_destructive_shell.py
    shell_stderr_suppression_policy.py
    shell_file_edit_policy.py
    shell_output_pipeline_policy.py
    shell_package_mutation_policy.py

  posthook/
    __init__.py
    audited_write_policy.py
    ambient_change_warning.py
```

The `prehook` and `posthook` package initializers may import and register every
module in their package. Registration must be idempotent so repeated imports or
test reloads do not duplicate entries in the process-global registry.

Each hook module should expose a registration function and keep its policy
implementation local. The package initializer should be the central place that
invokes those registration functions.

Underscore-prefixed modules (for example `_common.py`, `_shell_common.py`) are
package-internal helpers and must not be re-exported from the package
`__init__`. The Daytona hooks package auto-registers its modules at import time;
tests that need isolation must clear the process-global registry or pass a
fresh `ToolHookRegistry` instance.

### Hook Naming Convention

- Module filename: `{policy_area}_{detail}.py`, snake_case, one policy per
  file. Include a tool-family prefix only when it disambiguates otherwise
  identical policy names across tools that share the package (for example
  `move_src_hard_block.py` vs `write_scope_hard_block.py`).
- Registration name: `{tool_name}:{policy_suffix}`. The suffix should drop
  redundant tool-family prefixes already implied by the tool name (for example
  `daytona_move_file:src_hard_block`, `daytona_shell:destructive_shell`)
  but keep policy-area prefixes that are not redundant (for example
  `daytona_write_file:write_scope_hard_block` — `write_scope_` is the policy
  area, not a rename of the tool).

### Priority Convention

The registry orders matching hooks by `(priority, name)` ascending, so lower
numbers run first. Priority is the only lever for inter-hook ordering within a
single tool, so the number ordering is a contract.

Guidelines:

- `0–9`: argument mutation / normalization. Must run before any policy that
  reads the final args.
- `10–19`: default-bucket blocks, denials, and write-scope policies keyed on a
  single argument (examples: `daytona_write_file:write_scope_hard_block` at 10,
  `daytona_delete_file:write_scope_deny` at 15).
- `20+`: later blocks that depend on earlier blocks not having fired, or
  advisories that should only emit when the call has already survived earlier
  hard blocks. Do not assume "advisory only" at any range — check the policy,
  not the priority.

Two invariants the numbers must uphold:

1. Mutation hooks run before any hook that reads their output.
2. When two hooks can deny the same call, the one whose message is more
   actionable for the caller runs first.

Post-hook priority uses the same numeric space; 10 is the default for
audit-style post-hooks that may deny.

Example module shape:

```python
from tools.core.hooks import PreHookOutcome, ToolHookRegistry, default_registry


PRIORITY = 10
TOOL_GLOB = "daytona_write_file"
NAME = "daytona_write_file:write_scope_hard_block"


async def hook(tool_name, args, context) -> PreHookOutcome:
    ...


def register(registry: ToolHookRegistry | None = None) -> None:
    reg = registry or default_registry()
    reg.register(TOOL_GLOB, "pre", PRIORITY, hook, name=NAME)
```

The registry deduplicates on `(tool_glob, phase, priority, name)`: registering
a second entry with the same four-tuple replaces the previous entry rather than
appending a duplicate. Note that priority is part of the key, so re-registering
the same hook at a different priority is treated as a new entry — use a stable
priority per hook. Registry-level idempotence is preferred over per-package
guards because it protects all hook packages consistently.

The old `tools.daytona_toolkit.guards` module has been removed. Daytona policy
registration now flows only through `tools.daytona_toolkit.hooks`.

## Daytona Pre-Hook Inventory

The initial Daytona pre-hook set should be split into one module per policy:

| Module | Phase | Tools | Purpose |
| --- | --- | --- | --- |
| `repo_operation_guard.py` | pre | `daytona_delete_file`, `daytona_move_file` | Blocks repo-root, outside-repo, and invalid self/nested move operations before destructive tool bodies run. |
| `write_scope_hard_block.py` | pre | `daytona_write_file`, `daytona_edit_file`, `daytona_delete_file` | Blocks unauthorized test-file edits in coordinated team lanes. |
| `write_scope_advisory.py` | pre | `daytona_edit_file` | Emits outside-scope edit advisories without blocking. |
| `write_scope_deny.py` | pre | `daytona_delete_file` | Blocks delete operations outside write scope, including enumerated folder members. |
| `move_src_hard_block.py` | pre | `daytona_move_file` | Applies test-file hard-block policy to the move source. |
| `move_src_scope_deny.py` | pre | `daytona_move_file` | Blocks move operations whose source is outside write scope, including enumerated folder members. |
| `move_dst_scope_advisory.py` | pre | `daytona_move_file` | Emits advisory for destination outside write scope when policy allows the move. |
| `rename_scope_policy.py` | pre | `daytona_rename_symbol` | Builds the rename plan once, applies test-file and write-scope policy to planned paths, and caches the approved plan for the tool body. |
| `shell_destructive_git.py` | pre | `daytona_shell` | Blocks destructive git commands and other git metadata/worktree mutation commands that bypass OCC/write-scope audit. |
| `shell_destructive_shell.py` | pre | `daytona_shell` | Blocks destructive shell commands against workspace roots and dangerous devices. |
| `shell_stderr_suppression_policy.py` | pre | `daytona_shell` | Blocks shell commands that suppress stderr with `/dev/null`. |
| `shell_file_edit_policy.py` | pre | `daytona_shell` | Blocks shell file-edit side channels (`sed -i`, `tee`, redirect writes) when `daytona_shell` edit policy is active. |
| `shell_output_pipeline_policy.py` | pre | `daytona_shell` | Sanitizes output-shaping syntax (pipes, `head`/`tail`, output redirects, leading repo-root `cd`) before execution. |
| `shell_package_mutation_policy.py` | pre | `daytona_shell` | Blocks package or environment mutation commands (`pip install`, `uv sync`, `npm install`, etc.) on coordinated lanes. |

The move hooks are intentionally split by source and destination behavior. The
destination advisory has different semantics from source denial because a move
from an already-owned source can be a rename-like operation that extends scope
on success.

## Daytona Post-Hook Inventory

Post-hooks should live in `tools.daytona_toolkit.hooks.posthook`.

| Module | Phase | Tools | Purpose |
| --- | --- | --- | --- |
| `audited_write_policy.py` | post | `daytona_shell` | Inspects changed paths and can replace the API-facing result when audited write policy fails. |
| `ambient_change_warning.py` | post | `daytona_shell` | Emits a user-only advisory when the shell command touched paths outside its declared write set. |
| `move_extend_scope.py` | post | `daytona_move_file` | Extends in-memory write scope to the move destination after a successful owned-source move. |
| `write_extend_scope.py` | post | `daytona_write_file` | Extends in-memory write scope to a successful write target. |

These post-hooks read from ``result.metadata["changed_paths"]`` /
``ambient_changed_paths``, which the ``tools.daytona_toolkit._commit`` façade
writes uniformly for every OCC-gated tool. The shared audit primitive lives in
``tools.daytona_toolkit._audit`` and accepts ``tool_name`` so the same helper
can back multiple registrations.

``audited_write_policy`` is registered only on ``daytona_shell`` by design.
Codeact commits paths its input does not name (shell side effects, ambient
edits), so a post-commit audit is the only layer that can see the actual
changed set. The pure OCC tools
(``daytona_write_file``, ``daytona_edit_file``, ``daytona_delete_file``,
``daytona_move_file``) preserve path identity between input and commit — see
``code_intelligence/routing/service.py::_write_spec_to_change`` and
``editing/write_coordinator.py`` — so the pre-hook ``write_scope_advisory``
already surfaces the same paths a post-hook audit would for edit operations,
and adding a post-hook audit registration would duplicate the signal. For
``daytona_write_file``, outside-scope advisory registration is intentionally
omitted; a successful committed write instead widens the lane's in-memory
``write_scope`` through ``write_extend_scope``.

These policies are post-hook-only because they depend on the tool result and
committed path set. They must not be forced into the pre-hook package.

## Outcome Types

Pre-hooks use one flat outcome shape. The hook author can allow, mutate
arguments, deny, or emit advisories from the same return type. The runtime
enforces invalid combinations.

```python
from dataclasses import dataclass, field
from pydantic import BaseModel


@dataclass(frozen=True)
class PreHookOutcome:
    tool_input: BaseModel | None = None
    has_error: bool = False
    error_message: str | None = None
    advisories: tuple[str, ...] = field(default_factory=tuple)

    def __post_init__(self) -> None:
        if self.has_error and not self.error_message:
            raise ValueError("error_message is required when has_error=True")
        if self.has_error and self.tool_input is not None:
            raise ValueError("error outcomes cannot also mutate tool_input")
        if self.has_error and self.advisories:
            raise ValueError("error outcomes cannot also emit advisories")
```

Post-hooks cannot mutate arguments. They can emit advisories or deny the final
result.

```python
from dataclasses import dataclass, field


@dataclass(frozen=True)
class PostHookOutcome:
    has_error: bool = False
    error_message: str | None = None
    advisories: tuple[str, ...] = field(default_factory=tuple)

    def __post_init__(self) -> None:
        if self.has_error and not self.error_message:
            raise ValueError("error_message is required when has_error=True")
        if self.has_error and self.advisories:
            raise ValueError("error outcomes cannot also emit advisories")
```

The pre-hook pipeline result does not carry advisories. Advisories are emitted
immediately and are not accumulated.

```python
from dataclasses import dataclass
from pydantic import BaseModel


@dataclass(frozen=True)
class PreHookPipelineResult:
    tool_input: BaseModel
    has_error: bool = False
    error_message: str | None = None
```

## Hook Callable Contracts

Hook callables may be async or sync. The pipeline awaits awaitable results.

```python
from collections.abc import Awaitable, Callable
from typing import Protocol
from pydantic import BaseModel

from tools.core.base import ToolExecutionContext, ToolResult


class PreToolHook(Protocol):
    def __call__(
        self,
        tool_name: str,
        args: BaseModel,
        context: ToolExecutionContext,
    ) -> PreHookOutcome | Awaitable[PreHookOutcome]: ...


class PostToolHook(Protocol):
    def __call__(
        self,
        tool_name: str,
        args: BaseModel,
        context: ToolExecutionContext,
        result: ToolResult,
    ) -> PostHookOutcome | Awaitable[PostHookOutcome]: ...
```

## Registry API

The registry keeps deterministic priority ordering and glob matching by tool
name.

```python
from dataclasses import dataclass
from typing import Literal

Phase = Literal["pre", "post"]


@dataclass(frozen=True)
class HookEntry:
    tool_glob: str
    phase: Phase
    priority: int
    target: PreToolHook | PostToolHook
    name: str


class ToolHookRegistry:
    def register(
        self,
        tool_glob: str,
        phase: Phase,
        priority: int,
        target: PreToolHook | PostToolHook,
        *,
        name: str | None = None,
    ) -> None: ...

    def matching(self, tool_name: str, phase: Phase) -> list[HookEntry]: ...
```

## Pipeline API

The pipeline receives an `emit` callback so advisory notifications are emitted
in order at the point where each hook produces them. The callback is part of the
active execution stream, not a metadata drain.

```python
from collections.abc import Awaitable, Callable

from message.stream_events import StreamEvent, SystemNotification

EmitStreamEvent = Callable[[StreamEvent], Awaitable[None]]


async def run_pre_hooks(
    tool_name: str,
    args: BaseModel,
    context: ToolExecutionContext,
    *,
    emit: EmitStreamEvent,
    registry: ToolHookRegistry | None = None,
) -> PreHookPipelineResult:
    current_args = args
    reg = registry or default_registry()

    for entry in reg.matching(tool_name, "pre"):
        try:
            outcome = await invoke_pre_hook(entry, tool_name, current_args, context)
        except Exception as exc:
            return PreHookPipelineResult(
                tool_input=current_args,
                has_error=True,
                error_message=f"{entry.name}: {exc}",
            )

        if outcome.has_error:
            return PreHookPipelineResult(
                tool_input=current_args,
                has_error=True,
                error_message=outcome.error_message,
            )

        for advisory in outcome.advisories:
            await emit(
                SystemNotification(
                    text=f"[pre-hook advisory] {tool_name}: {advisory}",
                    category="pre_hook_advisory",
                )
            )

        if outcome.tool_input is not None:
            current_args = outcome.tool_input

    return PreHookPipelineResult(tool_input=current_args)
```

Post-hooks mirror the same immediate advisory behavior and stop on denial or
exception.

```python
async def run_post_hooks(
    tool_name: str,
    args: BaseModel,
    context: ToolExecutionContext,
    result: ToolResult,
    *,
    emit: EmitStreamEvent,
    registry: ToolHookRegistry | None = None,
) -> PostHookOutcome:
    reg = registry or default_registry()

    for entry in reg.matching(tool_name, "post"):
        try:
            outcome = await invoke_post_hook(entry, tool_name, args, context, result)
        except Exception as exc:
            return PostHookOutcome(
                has_error=True,
                error_message=f"{entry.name}: {exc}",
            )

        if outcome.has_error:
            return outcome

        for advisory in outcome.advisories:
            await emit(
                SystemNotification(
                    text=f"[post-hook advisory] {tool_name}: {advisory}",
                    category="post_hook_advisory",
                )
            )

    return PostHookOutcome()
```

## Streaming Tool-Call Primitive

The hook-aware tool-call primitive emits stream events and returns exactly one
`ToolResult`. It becomes the orchestration boundary for foreground, streaming,
background, and external-trigger paths. The caller is responsible for wrapping
the final `ToolResult` in a `ToolResultBlock` keyed by `tool_use_id` and for
emitting `ToolExecutionCompleted` with the final result; the primitive only
owns pre-hook, tool body, and post-hook ordering plus `ToolExecutionStarted`.

Current signature (see `backend/src/tools/core/hooks/execution.py`):

```python
async def execute_tool_with_hooks(
    tool: BaseTool,
    raw_input: dict[str, Any],
    context: ToolExecutionContext,
    *,
    emit: EmitStreamEvent,
    emit_started: bool = True,
) -> ToolResult:
    parsed = parse_tool_input(tool, raw_input)
    if parsed.error is not None:
        return parsed.error
    assert parsed.args is not None

    pre = await run_pre_hooks(tool.name, parsed.args, context, emit=emit)
    if pre.has_error:
        return ToolResult(
            output=f"pre-hook blocked {tool.name}: {pre.error_message}",
            is_error=True,
            metadata={"blocked_by": "pre_hook"},
        )

    effective_args = pre.tool_input
    if emit_started:
        await emit(
            ToolExecutionStarted(
                tool_name=tool.name,
                tool_input=effective_args.model_dump(mode="json"),
            )
        )

    result = await execute_tool_body(tool, effective_args, context)
    validated = validate_tool_output(tool, result)

    post = await run_post_hooks(tool.name, effective_args, context, validated, emit=emit)
    if post.has_error:
        return ToolResult(
            output=f"post-hook failed {tool.name}: {post.error_message}",
            is_error=True,
            metadata={
                **validated.metadata,
                "blocked_by": "post_hook",
                "original_tool_is_error": validated.is_error,
            },
        )
    return validated
```

Helpers extracted from the legacy `run_tool_safely()` path — `parse_tool_input`,
`execute_tool_body`, `validate_tool_output` — live in `tools.core.base` and are
reused directly by the primitive.

## Concurrent Multiplexing Sketch

The query loop should keep foreground tool calls concurrent. Each tool call gets
an event-emitting task. Events flow through a shared queue and are yielded as
they arrive.

```python
async def execute_foreground_tools_concurrently(
    context: QueryContext,
    tool_calls: list[ToolUseBlock],
) -> AsyncIterator[StreamEvent | ToolResultBlock]:
    queue: asyncio.Queue[StreamEvent | tuple[str, ToolResultBlock]] = asyncio.Queue()

    async def emit(event: StreamEvent) -> None:
        await queue.put(event)

    async def run_one(tool_call: ToolUseBlock) -> None:
        result = await execute_tool_call_streaming(
            context=context,
            tool_name=tool_call.name,
            tool_use_id=tool_call.id,
            raw_input=tool_call.input,
            emit=emit,
        )
        await queue.put(("result", result))

    tasks = [asyncio.create_task(run_one(tc)) for tc in tool_calls]
    remaining = len(tasks)

    while remaining:
        item = await queue.get()
        if isinstance(item, tuple) and item[0] == "result":
            remaining -= 1
            yield item[1]
        else:
            yield item

    await asyncio.gather(*tasks)
```

This sketch is intentionally not the final query-loop implementation. It shows
the required shape: per-tool local ordering, global interleaving, immediate
advisory events, and exactly one final result per tool use.

## Result Selection Rules

For each tool use, the runtime returns exactly one API-facing result:

- Invalid tool input returns a failed tool result before hooks run.
- Pre-hook denial returns a failed tool result and skips the tool body.
- Tool success with no post-hook denial returns the successful tool result.
- Tool failure with no post-hook denial returns the tool failure result.
- Post-hook denial after tool success returns a failed post-hook result.
- Post-hook denial after tool failure returns a failed post-hook result unless a
  later policy decides to preserve the original tool failure as primary.

Post-hook result replacement is a policy mechanism. It must be explicit and
observable in metadata or telemetry so operators can distinguish tool failure
from post-hook failure.

## Migration Notes

The old `tools.core.guards` package was removed rather than retained as a
compatibility layer. Its useful behavior was promoted into `tools.core.hooks`:
priority ordering, argument mutation, advisory outcomes, and denial
short-circuiting.

The migration should remove the legacy `hooks` package behavior only after the
new streaming execution primitive is wired through foreground execution,
streaming execution, background execution, and external triggers.

The migration should update tests around:

- Sequential pre-hook argument transformation.
- Pre-hook denial short-circuiting.
- Immediate user-only advisory notifications.
- No advisory insertion into API history.
- Post-hook execution after successful tool execution.
- Post-hook execution after failed tool execution.
- Post-hook denial replacing the API-facing result.
- Concurrent foreground tool calls with interleaved hook notifications.
- Exactly one final tool result per model tool use.

## Files Added

New files:

```text
backend/src/tools/core/hooks/__init__.py
backend/src/tools/core/hooks/outcomes.py
backend/src/tools/core/hooks/registry.py
backend/src/tools/core/hooks/pipeline.py
backend/src/tools/core/hooks/execution.py
backend/src/tools/daytona_toolkit/hooks/__init__.py
backend/src/tools/daytona_toolkit/hooks/_common.py
backend/src/tools/daytona_toolkit/hooks/prehook/__init__.py
backend/src/tools/daytona_toolkit/hooks/prehook/_shell_common.py
backend/src/tools/daytona_toolkit/hooks/prehook/write_scope_hard_block.py
backend/src/tools/daytona_toolkit/hooks/prehook/write_scope_advisory.py
backend/src/tools/daytona_toolkit/hooks/prehook/write_scope_deny.py
backend/src/tools/daytona_toolkit/hooks/prehook/move_src_hard_block.py
backend/src/tools/daytona_toolkit/hooks/prehook/move_src_scope_deny.py
backend/src/tools/daytona_toolkit/hooks/prehook/move_dst_scope_advisory.py
backend/src/tools/daytona_toolkit/hooks/prehook/shell_destructive_git.py
backend/src/tools/daytona_toolkit/hooks/prehook/shell_destructive_shell.py
backend/src/tools/daytona_toolkit/hooks/prehook/shell_stderr_suppression_policy.py
backend/src/tools/daytona_toolkit/hooks/prehook/shell_file_edit_policy.py
backend/src/tools/daytona_toolkit/hooks/posthook/__init__.py
backend/src/tools/daytona_toolkit/hooks/posthook/audited_write_policy.py
backend/src/tools/daytona_toolkit/hooks/posthook/ambient_change_warning.py
backend/src/tools/daytona_toolkit/hooks/posthook/move_extend_scope.py
backend/src/tools/daytona_toolkit/hooks/posthook/write_extend_scope.py
backend/src/tools/daytona_toolkit/_commit.py
backend/src/tools/daytona_toolkit/_audit.py
backend/tests/test_tools/test_hooks/__init__.py
backend/tests/test_tools/test_hooks/test_pipeline.py
backend/tests/test_tools/test_hooks/test_execution.py
backend/tests/test_tools/test_daytona_toolkit/conftest.py
backend/tests/test_tools/test_daytona_toolkit/test_commit.py
```

## Files Removed

Legacy subprocess hook files removed:

```text
backend/src/hooks/schemas.py
backend/src/hooks/executor.py
backend/src/hooks/events.py
backend/src/hooks/loader.py
backend/src/hooks/types.py
backend/src/hooks/_factory.py
backend/src/hooks/__init__.py
```

Legacy guard files and tests removed:

```text
backend/src/tools/core/guards/__init__.py
backend/src/tools/core/guards/types.py
backend/src/tools/core/guards/registry.py
backend/src/tools/core/guards/pipeline.py
backend/src/tools/daytona_toolkit/guards.py
backend/tests/test_tools/test_guards/__init__.py
backend/tests/test_tools/test_guards/test_pipeline.py
backend/tests/test_tools/test_guards/test_registry.py
backend/tests/test_tools/test_guards/test_run_tool_safely_integration.py
backend/tests/test_tools/test_guards/test_telemetry.py
```

Query-engine docs now describe platform hooks instead of `hook_executor`.

## Files Retargeted

Runtime files retargeted from `hook_executor` or direct tool execution to the
new hook-aware execution primitive:

```text
backend/src/tools/core/tool_execution.py
backend/src/tools/core/base.py
backend/src/engine/core/query.py
backend/src/engine/core/streaming_executor.py
backend/src/engine/runtime/background_dispatch.py
backend/src/external_trigger/runner.py
backend/src/engine/runtime/agent.py
```

Configuration files to retarget:

```text
backend/src/config/settings.py
```

`settings.py` dropped the `hooks` field and the import of legacy hook schemas.
`pyproject.toml` still keeps `httpx` because other runtime/provider paths depend
on it directly or transitively.

Existing hook registrations moved:

```text
backend/src/tools/daytona_toolkit/guards.py
backend/src/tools/daytona_toolkit/__init__.py
backend/src/tools/daytona_toolkit/toolkit.py
```

These now register platform hooks through `tools.core.hooks`, preserving the
current Daytona write-scope and daytona_shell behavior. The monolithic `guards.py`
file was replaced by the per-hook module layout under
`tools.daytona_toolkit.hooks`.

Documentation files to update:

```text
docs/architecture/query-engine.md
docs/query-engine-diagram.html
```

The architecture design note itself is `docs/architecture/platform-tool-hooks.md`.

## Implemented Migration Plan

### Phase 0: Confirm Behavior and Freeze Scope

Deliverables:

- Confirm this design is the source of truth for platform tool hooks.
- Confirm legacy user-configurable subprocess hooks are removed, not migrated.
- Confirm advisories are user-only `SystemNotification` events.
- Confirm foreground tool calls remain concurrent.
- Confirm post-hook denial result-selection policy.

Exit criteria:

- `docs/architecture/platform-tool-hooks.md` reflects the accepted policy.
- Open decisions that block implementation are resolved or explicitly deferred.

### Phase 1: Introduce Platform Hook Package

Deliverables:

- Added `tools.core.hooks` package.
- Added `PreHookOutcome`, `PostHookOutcome`, and pipeline result types.
- Added registry and pipeline tests.
- Removed the old `tools.core.guards` package after migration.

Exit criteria:

- Empty registry is a no-op.
- Pre-hook mutation is threaded through later pre-hooks.
- Pre-hook denial short-circuits.
- Pre-hook advisories emit immediately through the provided `emit` callback.
- Post-hook advisories emit immediately through the provided `emit` callback.
- Post-hook denial stops the post-hook chain.

Suggested tests:

```text
backend/tests/test_tools/test_hooks/test_pipeline.py
backend/tests/test_tools/test_hooks/test_execution.py
```

### Phase 2: Extract Validated Tool Execution Helpers

Deliverables:

- Split validation, direct execution, exception normalization, and output
  validation helpers out of `run_tool_safely()`.
- Keep `run_tool_safely()` temporarily as a non-streaming wrapper for tests and
  legacy call sites.
- Ensure helper behavior matches current input and output validation messages.

Exit criteria:

- Existing tool validation tests remain green.
- `run_tool_safely()` behavior is unchanged for callers that still use it.
- New helpers can execute a tool that already has parsed arguments.

Suggested tests:

```text
backend/tests/test_tools/test_hooks/test_execution.py
backend/tests/test_engine/test_tool_call_loop.py
```

### Phase 3: Add Hook-Aware Streaming Execution Primitive

Deliverables:

- Add `execute_tool_call_streaming()`.
- Wire pre-hooks, tool execution, post-hooks, result selection, runtime metadata
  merge, and final `ToolResultBlock` creation into that primitive.
- Emit hook advisories directly through the active stream callback.
- Emit tool completion after post-hooks select the final result.

Exit criteria:

- One final `ToolResultBlock` is returned for every tool use.
- Pre-hook advisory is yielded before the tool starts.
- Post-hook advisory is yielded after the tool result exists and before final
  completion.
- Pre-hook denial returns one failed result and skips tool execution.
- Post-hook denial returns one failed result and records that post-hook policy
  replaced the original result.

Suggested tests:

```text
backend/tests/test_tools/test_hooks/test_execution.py
backend/tests/test_engine/test_tool_hook_concurrency.py
```

### Phase 4: Wire Foreground Query Execution

Deliverables:

- Replace foreground `execute_tool_call()` use in the query loop with the new
  stream-aware primitive.
- Preserve concurrent execution for multiple foreground tool calls.
- Multiplex per-tool events through the query loop.
- Preserve batch validation and budget-limit behavior.

Exit criteria:

- Single foreground tool calls produce the same final tool result as before.
- Multiple foreground tool calls still run concurrently.
- Interleaved hook advisories are yielded with tool names.
- Tool-result collection still appends exactly one result per tool use.

Suggested tests:

```text
backend/tests/test_engine/test_tool_call_loop.py
backend/tests/test_engine/test_streaming_executor.py
backend/tests/test_engine/test_tool_hook_concurrency.py
```

### Phase 5: Wire Streaming Executor Path

Deliverables:

- Retarget `StreamingToolExecutor` so mid-stream tool execution uses the same
  hook-aware primitive or the same lower-level execution components.
- Ensure progress and cancellation semantics remain correct.
- Ensure hook advisories emitted by mid-stream tools reach subscribers without
  entering API history.

Exit criteria:

- Mid-stream tool execution still starts promptly when allowed.
- Cancellation behavior remains unchanged.
- Hook advisories can interleave with normal tool progress.
- Hook denial produces the required failed tool result.

Suggested tests:

```text
backend/tests/test_engine/test_streaming_executor.py
backend/tests/test_engine/test_tool_hook_concurrency.py
```

### Phase 6: Wire Background and External Trigger Paths

Deliverables:

- Ensure background task execution uses the hook-aware primitive for the actual
  background tool body.
- Preserve background launch acknowledgement semantics.
- Retarget external trigger tool execution to the shared hook-aware primitive.
- Define how external-trigger advisory notifications are exposed when no live
  stream subscriber exists.

Exit criteria:

- Background tool advisories are user-visible when a stream exists.
- Background post-hook denial can replace the background task result.
- External triggers do not call tool bodies directly for hooked tools.
- External trigger hook denial returns a failed trigger-local tool result.

Suggested tests:

```text
backend/tests/test_engine/test_background_tasks.py
backend/tests/test_engine/test_background_e2e.py
backend/tests/test_external_trigger/test_runner.py
```

### Phase 7: Migrate Existing Policy Registrations

Deliverables:

- Move Daytona write-scope and daytona_shell policy registrations to
  `tools.core.hooks`.
- Split the current monolithic Daytona guard module into one file per hook under
  `tools.daytona_toolkit.hooks.prehook` and `tools.daytona_toolkit.hooks.posthook`.
- Add idempotent package-level registration for Daytona hooks.
- Preserve existing policy messages and golden outputs.
- Remove the old Daytona guard module.

Exit criteria:

- Existing Daytona write-scope behavior is unchanged.
- daytona_shell destructive command and file-edit side-channel policy hooks still run.
- Re-importing Daytona hook packages does not duplicate registry entries.
- Existing coordination-warning side effects remain correct where they are still
  part of product behavior.

Suggested tests:

```text
backend/tests/test_tools/test_daytona_toolkit/test_edit_tool.py
backend/tests/test_tools/test_daytona_toolkit/test_tools_execution.py
backend/tests/test_tools/test_daytona_toolkit/test_delete_move_tool.py
backend/tests/test_tools/test_daytona_toolkit/test_shell_tool.py
backend/tests/test_tools/test_daytona_toolkit/test_write_scope_advisory.py
```

### Phase 8: Remove Legacy Hook Bus

Deliverables:

- Delete top-level legacy `hooks` package behavior.
- Remove `Settings.hooks`.
- Remove `make_hook_executor()` wiring from runtime agent setup.
- Remove `QueryContext.hook_executor`.
- Remove legacy docs and tests.
- Remove `httpx` dependency only if no remaining first-party runtime code needs
  it.

Exit criteria:

- No imports of top-level `hooks` remain in backend runtime code.
- Settings load still tolerates old config files if needed, or the breaking
  change is documented.
- Query-engine docs describe platform hooks, not `hook_executor`.

Suggested checks:

```text
rg -n "hook_executor|HookEvent|load_hook_registry|make_hook_executor|hooks\\.schemas|hooks\\.executor" backend/src backend/tests docs
uv run pytest backend/tests/test_tools/test_hooks backend/tests/test_engine/test_tool_call_loop.py -q
```

### Phase 9: Final Verification

Deliverables:

- Run targeted hook, engine, Daytona, background, and external-trigger tests.
- Run broader backend tests if the change touches shared execution semantics.
- Update architecture docs and README references.

Suggested commands:

```text
uv run pytest backend/tests/test_tools/test_hooks -q
uv run pytest backend/tests/test_engine/test_tool_call_loop.py backend/tests/test_engine/test_streaming_executor.py -q
uv run pytest backend/tests/test_engine/test_background_tasks.py backend/tests/test_external_trigger/test_runner.py -q
uv run pytest backend/tests/test_tools/test_daytona_toolkit -q
uv run ruff check backend/src backend/tests
uv run mypy --config-file backend/mypy.ini backend/src/team backend/src/agents
```

## Open Decisions

- Whether advisory notification categories should be standardized by phase,
  policy area, or both. Today only the phase category (`pre_hook_advisory`,
  `post_hook_advisory`) is contractually fixed; a policy-area subcategory is
  still open.
- Whether hook failures caused by unexpected exceptions should expose raw
  exception text to the model or use a sanitized message while preserving raw
  details in telemetry.

Resolved (see §Result Selection Rules): post-hook denial always replaces the
tool result, preserving the original `is_error` under
`metadata.original_tool_is_error` and setting `metadata.blocked_by =
"post_hook"`. Annotation-only post-hooks must emit an advisory and return a
non-error `PostHookOutcome`; they must not use `has_error` to annotate.
