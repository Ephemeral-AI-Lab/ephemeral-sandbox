# Module `tools` — Class Inventory

> Generated class & field reference. Source of truth is the code under
> `backend/src/tools/`. Field/type/default data is extracted directly from
> the AST; one-line purposes come from class docstrings (or, where absent, a
> reviewer summary). This generated inventory is distinct from the hand-curated
> `docs/architecture/` memory layer.

**65 classes across 44 files.**

The `tools` module is EphemeralOS's tool layer: it defines the abstract tool contract and runtime plumbing under `_framework` — `BaseTool`, the `ToolRegistry` name-to-implementation map, normalized `ToolResult`/`TextToolOutput`, the `ToolExecutionContextService` and `ExecutionMetadata` runtime state, the `ToolPreHook`/`ToolPostHook` protocols driven by `ToolHookExecutionPipeline`, and the `ToolFactoryContext`/`ToolCatalogEntry` construction and introspection surfaces. Concrete tool families implement this contract with paired pydantic input/output models: sandbox file and shell operations (`read_file`, `write_file`, `edit_file`, `multi_edit`, `glob`, `grep`, `shell`), background task control (`cancel`/`check_result`/`wait`), the `enter`/`exit` isolated-workspace lifecycle, `run_subagent` (including the caller-aware `RestrictedRunSubagentTool`), skill-reference loading, and the `ask_helper`/`ask_advisor` helper-messaging tools. The largest group is the role-scoped terminal `submission` family — planner (`submit_plan_closes_goal`/`defers_goal` over a shared schema), executor (success/blocker/handoff), evaluator, verifier, explorer, and advisor-feedback tools, backed by `AttemptSubmissionContext`/`ExecutorSubmissionContext` and the `TerminalToolDescriptor`. Cross-cutting `_hooks` enforce policy across tools — advisor approval, blocking in isolated mode, rejecting destructive git/filesystem shell commands, and requiring no in-flight background tasks.

## Contents

- **`tools/_framework/core/base.py`** — `BaseTool`
- **`tools/_framework/core/context.py`** — `ToolExecutionContextService`
- **`tools/_framework/core/hooks.py`** — `HookResult`, `ToolPreHook`, `ToolPostHook`
- **`tools/_framework/core/registry.py`** — `ToolRegistry`
- **`tools/_framework/core/results.py`** — `ToolResult`, `TextToolOutput`, `ToolInputParseResult`
- **`tools/_framework/core/runtime.py`** — `ExecutionMetadata`
- **`tools/_framework/execution/hook_pipeline.py`** — `ToolHookExecutionPipeline`
- **`tools/_framework/factory.py`** — `ToolFactoryContext`
- **`tools/_framework/introspection/catalog.py`** — `ToolCatalogEntry`
- **`tools/_hooks/advisor_approval.py`** — `AdvisorApprovalPreHook`
- **`tools/_hooks/block_in_isolated_mode.py`** — `BlockInIsolatedMode`
- **`tools/_hooks/destructive_shell.py`** — `DestructiveGitShellPreHook`, `DestructiveShellPreHook`
- **`tools/_hooks/require_no_inflight_background_tasks.py`** — `RequireNoInflightBackgroundTasks`
- **`tools/_terminals/registry.py`** — `TerminalToolDescriptor`
- **`tools/ask_helper/_lib/_compose.py`** — `HelperMessageError`, `HelperMessages`
- **`tools/ask_helper/ask_advisor/ask_advisor.py`** — `AskAdvisorInput`
- **`tools/background/cancel_background_task/cancel_background_task.py`** — `CancelBackgroundTaskInput`, `CancelBackgroundTaskTool`
- **`tools/background/check_background_task_result/check_background_task_result.py`** — `CheckBackgroundTaskResultInput`, `CheckBackgroundTaskResultTool`
- **`tools/background/wait_background_tasks/wait_background_tasks.py`** — `WaitBackgroundTasksInput`, `WaitBackgroundTasksTool`
- **`tools/isolated_workspace/enter_isolated_workspace/definition.py`** — `EnterIsolatedWorkspaceInput`
- **`tools/isolated_workspace/exit_isolated_workspace/definition.py`** — `ExitIsolatedWorkspaceInput`
- **`tools/sandbox/_lib/file_payloads.py`** — `ReadFileInput`, `ReadFileOutput`, `WriteFileInput`, `WriteFileOutput`
- **`tools/sandbox/edit_file/edit_file.py`** — `EditFileInput`, `EditFileOutput`
- **`tools/sandbox/glob/glob.py`** — `GlobInput`, `GlobOutput`
- **`tools/sandbox/grep/grep.py`** — `GrepInput`, `GrepOutput`
- **`tools/sandbox/multi_edit/multi_edit.py`** — `MultiEditOp`, `MultiEditInput`, `MultiEditOutput`
- **`tools/sandbox/shell/shell.py`** — `ShellInput`, `ShellOutput`
- **`tools/skills/load_skill_reference.py`** — `LoadSkillReferenceInput`
- **`tools/subagent/_factory.py`** — `RestrictedRunSubagentTool`
- **`tools/subagent/run_subagent/run_subagent.py`** — `_ValidatedRunSubagentRequest`, `RunSubagentInput`
- **`tools/submission/advisor/submit_advisor_feedback/submit_advisor_feedback.py`** — `SubmitAdvisorFeedbackInput`
- **`tools/submission/context/attempt.py`** — `AttemptSubmissionContextError`, `AttemptSubmissionContext`
- **`tools/submission/context/executor.py`** — `ExecutorSubmissionContext`
- **`tools/submission/evaluator/submit_evaluation_failure/submit_evaluation_failure.py`** — `SubmitEvaluationFailureInput`
- **`tools/submission/evaluator/submit_evaluation_success/submit_evaluation_success.py`** — `SubmitEvaluationSuccessInput`
- **`tools/submission/executor/submit_execution_blocker/submit_execution_blocker.py`** — `SubmitExecutionBlockerInput`
- **`tools/submission/executor/submit_execution_handoff/submit_execution_handoff.py`** — `SubmitExecutionHandoffInput`
- **`tools/submission/executor/submit_execution_success/submit_execution_success.py`** — `SubmitExecutionSuccessInput`
- **`tools/submission/explorer/submit_exploration_result/submit_exploration_result.py`** — `SubmitExplorationResultInput`
- **`tools/submission/planner/_schemas.py`** — `PlanTaskInput`, `SharedPlannerSubmissionInput`
- **`tools/submission/planner/submit_plan_closes_goal/submit_plan_closes_goal.py`** — `SubmitPlanClosesGoalInput`
- **`tools/submission/planner/submit_plan_defers_goal/submit_plan_defers_goal.py`** — `SubmitPlanDefersGoalInput`
- **`tools/submission/verifier/submit_verification_failure/submit_verification_failure.py`** — `SubmitVerificationFailureInput`
- **`tools/submission/verifier/submit_verification_success/submit_verification_success.py`** — `SubmitVerificationSuccessInput`

---

## `tools/_framework/core/base.py`

#### `BaseTool`  ·  _abc_  ·  bases: `ABC`  ·  [L27]

Base class for all EphemeralOS tools.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `description` | `str` |  |
| `short_description` | `str \| None` | `None` |
| `input_model` | `type[BaseModel]` |  |
| `output_model` | `type[BaseModel]` | `TextToolOutput` |
| `intent` | `Intent` |  |
| `background` | `BackgroundMode` | `'forbidden'` |
| `task_type` | `str` | `'agent'` |
| `is_terminal_tool` | `bool` | `False` |
| `pre_hooks` | `tuple[Any, ...]` | `()` |
| `post_hooks` | `tuple[Any, ...]` | `()` |
| `context_requirements` | `tuple[str, ...]` | `()` |

<details><summary>Methods (3)</summary>

`execute`, `output_schema`, `to_api_schema`

</details>

---

## `tools/_framework/core/context.py`

#### `ToolExecutionContextService`  ·  _dataclass_  ·  decorators: `@dataclass(init=False)`  ·  [L15]

Service and runtime state store injected into a tool invocation.

**Fields**

| name | type | default |
|------|------|---------|
| `cwd` | `Path` |  |
| `_metadata` | `ExecutionMetadata` | `field(default_factory=ExecutionMetadata, repr=False)` |

<details><summary>Methods (12)</summary>

`__init__`, `_coerce_services`, `__getattr__`, `__setattr__`, `services_copy`, `services_with_overrides`, `update_services`, `get`, `__getitem__`, `__setitem__`, `__contains__`, `notify_system`

</details>

---

## `tools/_framework/core/hooks.py`

#### `HookResult`  ·  _dataclass_  ·  bases: `Generic[TValue]`  ·  decorators: `@dataclass(frozen=True)`  ·  [L21]

Result returned by a tool hook.

**Fields**

| name | type | default |
|------|------|---------|
| `status` | `HookStatus` |  |
| `value` | `TValue \| None` | `None` |
| `reason` | `str` | `''` |
| `message` | `str` | `''` |
| `metadata` | `dict[str, object]` | `field(default_factory=dict)` |

<details><summary>Methods (2)</summary>

`pass_`, `fail`

</details>

#### `ToolPreHook`  ·  _protocol_  ·  bases: `Protocol[TInput_contra]`  ·  [L66]

Tool-specific hook that may mutate validated tool input.

**Fields**

| name | type | default |
|------|------|---------|
| `target_tool` | `str` |  |

<details><summary>Methods (1)</summary>

`run`

</details>

#### `ToolPostHook`  ·  _protocol_  ·  bases: `Protocol[TInput_contra]`  ·  [L79]

Tool-specific hook that may mutate a tool result.

**Fields**

| name | type | default |
|------|------|---------|
| `target_tool` | `str` |  |

<details><summary>Methods (1)</summary>

`run`

</details>

---

## `tools/_framework/core/registry.py`

#### `ToolRegistry`  ·  _class_  ·  [L12]

Map tool names to implementations.

**Instance attributes**: `_tools`

<details><summary>Methods (8)</summary>

`__init__`, `register`, `register_many`, `get`, `list_tools`, `remove_tools`, `restrict_to_tools`, `to_api_schema`

</details>

---

## `tools/_framework/core/results.py`

#### `ToolResult`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L12]

Normalized tool execution result.

**Fields**

| name | type | default |
|------|------|---------|
| `output` | `str` |  |
| `is_error` | `bool` | `False` |
| `metadata` | `dict[str, Any]` | `field(default_factory=dict)` |
| `is_terminal` | `bool` | `False` |

#### `TextToolOutput`  ·  _class_  ·  bases: `RootModel[str]`  ·  [L25]

Successful output for tools whose true output is plain text.

**Fields**

| name | type | default |
|------|------|---------|
| `root` | `str` | `Field(..., description='Plain text returned by the tool.')` |

#### `ToolInputParseResult`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L32]

Result of validating raw tool input.

**Fields**

| name | type | default |
|------|------|---------|
| `args` | `BaseModel \| None` | `None` |
| `error` | `ToolResult \| None` | `None` |

<details><summary>Methods (3)</summary>

`is_error`, `success`, `failure`

</details>

---

## `tools/_framework/core/runtime.py`

#### `ExecutionMetadata`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L21]

Typed bag of runtime metadata passed to tool executions.

**Fields**

| name | type | default |
|------|------|---------|
| `runtime_config` | `Any \| None` | `None` |
| `sandbox_id` | `str` | `''` |
| `agent_run_id` | `str \| None` | `None` |
| `agent_name` | `str` | `''` |
| `cwd` | `str` | `''` |
| `repo_root` | `str` | `''` |
| `exec_cwd` | `str` | `''` |
| `task_center_run_id` | `str \| None` | `None` |
| `task_center_task_id` | `str \| None` | `None` |
| `task_center_attempt_id` | `str \| None` | `None` |
| `task_center_workflow_id` | `str \| None` | `None` |
| `task_center_request_id` | `str \| None` | `None` |
| `attempt_runtime` | `Any \| None` | `None` |
| `composer` | `Any \| None` | `None` |
| `conversation_messages` | `list[Any]` | `field(default_factory=list)` |
| `tool_registry` | `Any \| None` | `None` |
| `context_preparers` | `list[Any]` | `field(default_factory=list)` |
| `background_task_manager` | `Any \| None` | `None` |
| `background_task_id` | `str \| None` | `None` |
| `sandbox_invocation_id` | `str \| None` | `None` |
| `on_progress_line` | `Callable[[str], None] \| None` | `None` |
| `tool_use_id` | `str \| None` | `None` |
| `system_notification_service` | `Any \| None` | `None` |
| `extras` | `dict[str, Any]` | `field(default_factory=dict)` |
| `_TYPED_FIELDS` | `ClassVar[frozenset[str]]` | `frozenset({'runtime_config', 'sandbox_id', 'agent_run_id', 'agent_name', 'cwd', 'repo_root', 'exec_cwd', 'task_center_run_id', 'task_center_task_id', 'task_center_attempt_id', 'task_center_workflow_id', 'task_center_request_id', 'attempt_runtime', 'composer', 'conversation_messages', 'tool_registry', 'context_preparers', 'background_task_manager', 'background_task_id', 'sandbox_invocation_id', 'on_progress_line', 'tool_use_id', 'system_notification_service'})` |

<details><summary>Methods (12)</summary>

`_has_value`, `get`, `__getitem__`, `__setitem__`, `__contains__`, `__iter__`, `keys`, `items`, `values`, `update`, `copy`, `with_overrides`

</details>

---

## `tools/_framework/execution/hook_pipeline.py`

#### `ToolHookExecutionPipeline`  ·  _class_  ·  [L29]

Coordinates tool-specific hook phases and owns metadata/notification plumbing.

**Instance attributes**: `_tool`, `_context`, `_system_notification_service`, `_hook_trace`

<details><summary>Methods (14)</summary>

`__init__`, `run_pre_hooks`, `run_post_hooks`, `finalize_result`, `_ensure_notification_service`, `_hook_event_name`, `_format_validation_errors`, `_invalid_hook_result`, `_append_trace`, `_metadata_with_hook_details`, `_with_hook_details`, `_build_hook_failure_result`, `_validated_hook_input`, `_validate_hook_output`

</details>

---

## `tools/_framework/factory.py`

#### `ToolFactoryContext`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L17]

Runtime context passed to tool factories during agent construction.

**Fields**

| name | type | default |
|------|------|---------|
| `metadata` | `dict[str, Any]` | `field(default_factory=dict)` |

---

## `tools/_framework/introspection/catalog.py`

#### `ToolCatalogEntry`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True)`  ·  [L13]

UI/API-safe tool metadata.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` |  |
| `description` | `str` |  |

---

## `tools/_hooks/advisor_approval.py`

#### `AdvisorApprovalPreHook`  ·  _class_  ·  [L39]

Per-terminal hook: requires advisor approval for THIS tool.

**Instance attributes**: `target_tool`, `name`

<details><summary>Methods (3)</summary>

`__init__`, `run`, `_classify`

</details>

---

## `tools/_hooks/block_in_isolated_mode.py`

#### `BlockInIsolatedMode`  ·  _class_  ·  [L32]

Per-tool hook: reject when the calling agent is in isolated mode.

**Instance attributes**: `target_tool`, `name`

<details><summary>Methods (2)</summary>

`__init__`, `run`

</details>

---

## `tools/_hooks/destructive_shell.py`

#### `DestructiveGitShellPreHook`  ·  _class_  ·  [L194]

Block git working-tree or metadata mutations before shell execution.

**Class variables**: `name = 'sandbox_shell:destructive_git'`, `target_tool = 'shell'`

<details><summary>Methods (1)</summary>

`run`

</details>

#### `DestructiveShellPreHook`  ·  _class_  ·  [L215]

Block destructive filesystem commands before shell execution.

**Class variables**: `name = 'sandbox_shell:destructive_shell'`, `target_tool = 'shell'`

<details><summary>Methods (1)</summary>

`run`

</details>

---

## `tools/_hooks/require_no_inflight_background_tasks.py`

#### `RequireNoInflightBackgroundTasks`  ·  _class_  ·  [L54]

Per-tool hook: reject when the calling agent has in-flight bg tasks.

**Instance attributes**: `target_tool`, `name`

<details><summary>Methods (5)</summary>

`__init__`, `run`, `_local_count`, `_fail_in_flight`, `_fail_or_bailout`

</details>

---

## `tools/_terminals/registry.py`

#### `TerminalToolDescriptor`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L28]

Two views on one terminal tool: parent-facing + advisor-facing.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` | `Field(..., min_length=1)` |
| `selection_guidance` | `str` | `Field(..., min_length=1)` |
| `advisor_review_focus` | `str` | `Field(..., min_length=1)` |

---

## `tools/ask_helper/_lib/_compose.py`

#### `HelperMessageError`  ·  _dataclass, exception_  ·  bases: `Exception`  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L31]

Raised inline so the caller can wrap as a ToolResult error.

**Fields**

| name | type | default |
|------|------|---------|
| `message` | `str` |  |

<details><summary>Methods (1)</summary>

`to_tool_result`

</details>

#### `HelperMessages`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L41]

Building blocks the helper tool assembles into its two messages.

**Fields**

| name | type | default |
|------|------|---------|
| `helper_agent_def` | `AgentDefinition` |  |
| `parent_agent_def` | `AgentDefinition \| None` |  |
| `parent_active_terminals` | `tuple[str, ...]` |  |
| `parent_user_msg_1` | `str` |  |
| `parent_user_msg_2` | `str` |  |
| `parent_transcript` | `str \| None` |  |

---

## `tools/ask_helper/ask_advisor/ask_advisor.py`

#### `AskAdvisorInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L31]

Input schema naming the terminal tool and payload an advisor reviews before submission.

**Fields**

| name | type | default |
|------|------|---------|
| `tool_name` | `str` | `Field(..., min_length=1, description='The name of the terminal tool you intend to call (e.g. submit_execution_success).')` |
| `tool_payload` | `dict[str, object]` | `Field(default_factory=dict, description='The arguments you intend to pass to the terminal tool. The advisor reviews payload quality against the contract.')` |

---

## `tools/background/cancel_background_task/cancel_background_task.py`

#### `CancelBackgroundTaskInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L18]

Input for cancel_background_task tool.

**Fields**

| name | type | default |
|------|------|---------|
| `task_id` | `str` | `BACKGROUND_TASK_ID_FIELD` |
| `reason` | `str` | `Field(default='', description='Optional reason for cancellation.')` |

#### `CancelBackgroundTaskTool`  ·  _class_  ·  bases: `BaseTool`  ·  [L27]

Cancel a running background task.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` | `'cancel_background_task'` |
| `description` | `str` | `get_cancel_background_task_description()` |
| `short_description` | `str` | `'Cancel a background task.'` |
| `input_model` | `type[BaseModel]` | `CancelBackgroundTaskInput` |
| `output_model` | `type[BaseModel]` | `TextToolOutput` |

<details><summary>Methods (1)</summary>

`execute`

</details>

---

## `tools/background/check_background_task_result/check_background_task_result.py`

#### `CheckBackgroundTaskResultInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L25]

Input for check_background_task_result tool.

**Fields**

| name | type | default |
|------|------|---------|
| `task_id` | `str` | `BACKGROUND_TASK_ID_FIELD` |

#### `CheckBackgroundTaskResultTool`  ·  _class_  ·  bases: `BaseTool`  ·  [L87]

Fetch the current result of a single background task.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` | `'check_background_task_result'` |
| `description` | `str` | `get_check_background_task_result_description()` |
| `short_description` | `str` | `"Check a background task's result."` |
| `input_model` | `type[BaseModel]` | `CheckBackgroundTaskResultInput` |
| `output_model` | `type[BaseModel]` | `TextToolOutput` |

<details><summary>Methods (1)</summary>

`execute`

</details>

---

## `tools/background/wait_background_tasks/wait_background_tasks.py`

#### `WaitBackgroundTasksInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L26]

Input for wait_background_tasks tool.

**Fields**

| name | type | default |
|------|------|---------|
| `timeout` | `float` | `Field(default=30, ge=1, le=300, description='Maximum seconds to block waiting for ALL background tasks to settle. Must be in [1, 300]; values outside this range are rejected by schema validation.')` |

#### `WaitBackgroundTasksTool`  ·  _class_  ·  bases: `BaseTool`  ·  [L57]

Block until all background tasks complete or timeout.

**Fields**

| name | type | default |
|------|------|---------|
| `name` | `str` | `'wait_background_tasks'` |
| `description` | `str` | `get_wait_background_tasks_description()` |
| `short_description` | `str` | `'Wait for all background tasks.'` |
| `input_model` | `type[BaseModel]` | `WaitBackgroundTasksInput` |
| `output_model` | `type[BaseModel]` | `TextToolOutput` |

<details><summary>Methods (1)</summary>

`execute`

</details>

---

## `tools/isolated_workspace/enter_isolated_workspace/definition.py`

#### `EnterIsolatedWorkspaceInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L23]

Input schema for the enter_isolated_workspace tool carrying the optional layer-stack root.

**Fields**

| name | type | default |
|------|------|---------|
| `layer_stack_root` | `str` | `''` |

---

## `tools/isolated_workspace/exit_isolated_workspace/definition.py`

#### `ExitIsolatedWorkspaceInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L23]

Input schema for the exit_isolated_workspace tool carrying the background-drain grace period.

**Fields**

| name | type | default |
|------|------|---------|
| `grace_s` | `float` | `5.0` |

---

## `tools/sandbox/_lib/file_payloads.py`

#### `ReadFileInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L17]

Input schema for the read_file tool specifying path and line window with defaulting validators.

**Fields**

| name | type | default |
|------|------|---------|
| `file_path` | `str` | `Field(..., description='Repo-relative or sandbox-root file path.')` |
| `start_line` | `int` | `Field(default=1, ge=1, description='First line to return. Lines are 1-based.')` |
| `end_line` | `int` | `Field(default=MAX_READ_FILE_LINES, ge=1, description='Last line to return, inclusive. Omit this field to read up to 200 lines from start_line; do not pass null.')` |

<details><summary>Methods (2)</summary>

`default_end_line_to_window`, `validate_line_range`

</details>

#### `ReadFileOutput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L55]

Output schema returning read file content, resolved path, and line-range metadata.

**Fields**

| name | type | default |
|------|------|---------|
| `cwd` | `str` | `Field(..., description='Current sandbox working directory.')` |
| `file_path` | `str` | `Field(..., description='Resolved file path that was read.')` |
| `total_lines` | `int` | `Field(..., description='Total number of lines in the file.')` |
| `start_line` | `int` | `Field(..., description='First line returned.')` |
| `end_line` | `int` | `Field(..., description='Last line returned.')` |
| `content` | `str` | `Field(..., description='Selected file content with line numbers.')` |

#### `WriteFileInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L64]

Input schema for the write_file tool specifying the target path and text content.

**Fields**

| name | type | default |
|------|------|---------|
| `file_path` | `str` | `Field(..., description='Repo-relative or sandbox-root file path.')` |
| `content` | `str` | `Field(..., description='Text to write.')` |

#### `WriteFileOutput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L72]

Output schema reporting the result of a write_file operation including status, changed paths, and conflicts.

**Fields**

| name | type | default |
|------|------|---------|
| `cwd` | `str` | `Field(..., description='Current sandbox working directory.')` |
| `file_path` | `str` | `Field(..., description='Resolved file path that was written.')` |
| `status` | `str` | `Field(..., description='Write result: written, aborted_version, or failed.')` |
| `changed_paths` | `list[str]` | `Field(default_factory=list, description='Files changed by the write.')` |
| `changed_path_kinds` | `dict[str, str]` | `Field(default_factory=dict, description='Changed paths keyed to write/delete/symlink/opaque_dir.')` |
| `mutation_source` | `str` | `Field(default='', description='Mutation source tag.')` |
| `conflict_reason` | `str \| None` | `Field(default=None, description='Conflict reason when write failed.')` |
| `error` | `dict[str, object]` | `Field(default_factory=dict, description='Typed error payload.')` |
| `bytes_written` | `int` | `Field(..., description='Number of UTF-8 bytes written.')` |

---

## `tools/sandbox/edit_file/edit_file.py`

#### `EditFileInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L23]

Input schema for the edit_file tool specifying search/replace text and replace-all option.

**Fields**

| name | type | default |
|------|------|---------|
| `file_path` | `str` | `Field(..., description='Repo-relative or sandbox-root file path.')` |
| `old_text` | `str` | `Field(default='', description='Exact text to replace.')` |
| `new_text` | `str` | `Field(default='', description='Replacement text.')` |
| `replace_all` | `bool` | `Field(default=False, description='Replace every occurrence of `old_text` instead of requiring a unique match.')` |
| `description` | `str` | `Field(default='', description='Optional short note about the edit.')` |

**Class variables**: `model_config = ConfigDict(extra='forbid')`

#### `EditFileOutput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L45]

Output schema reporting the result of an edit_file operation including status, changed paths, and conflicts.

**Fields**

| name | type | default |
|------|------|---------|
| `cwd` | `str` | `Field(..., description='Current sandbox working directory.')` |
| `file_path` | `str` | `Field(..., description='Resolved file path that was edited.')` |
| `status` | `str` | `Field(..., description='Edit result: edited, aborted_version, or failed.')` |
| `changed_paths` | `list[str]` | `Field(default_factory=list, description='Files changed by the edit.')` |
| `changed_path_kinds` | `dict[str, str]` | `Field(default_factory=dict, description='Changed paths keyed to write/delete/symlink/opaque_dir.')` |
| `mutation_source` | `str` | `Field(default='', description='Mutation source tag.')` |
| `conflict_reason` | `str \| None` | `Field(default=None, description='Conflict reason when edit failed.')` |
| `error` | `dict[str, object]` | `Field(default_factory=dict, description='Typed error payload.')` |
| `applied_edits` | `int` | `Field(default=0, description='Number of edits applied (not occurrence count; one replace_all edit counts as 1).')` |

---

## `tools/sandbox/glob/glob.py`

#### `GlobInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L26]

Input schema for the glob tool specifying the match pattern and optional directory scope.

**Fields**

| name | type | default |
|------|------|---------|
| `pattern` | `str` | `Field(..., description="fnmatch-style glob pattern applied against workspace-relative paths (e.g. '*.py' matches every Python file; 'pkg/*.py' restricts to a directory).")` |
| `path` | `str \| None` | `Field(default=None, description='Optional workspace-relative or sandbox-root directory to restrict the search to. Defaults to the entire workspace snapshot.')` |

**Class variables**: `model_config = ConfigDict(extra='forbid')`

#### `GlobOutput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L46]

Output schema returning workspace file paths matched by a glob pattern with truncation info.

**Fields**

| name | type | default |
|------|------|---------|
| `cwd` | `str` | `Field(..., description='Current sandbox working directory.')` |
| `pattern` | `str` | `Field(..., description='Glob pattern that was applied.')` |
| `filenames` | `list[str]` | `Field(default_factory=list, description='Workspace-relative paths that matched the pattern.')` |
| `num_files` | `int` | `Field(default=0, description='Number of matched paths returned (post-cap).')` |
| `truncated` | `bool` | `Field(default=False, description='True when the result set was capped at 100 paths.')` |

---

## `tools/sandbox/grep/grep.py`

#### `GrepInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L27]

Input schema for the sandbox grep tool specifying regex pattern, path, glob filter, and output mode.

**Fields**

| name | type | default |
|------|------|---------|
| `pattern` | `str` | `Field(..., description='Python `re` regex pattern (NOT PCRE2). Possessive quantifiers and recursive groups are unsupported.')` |
| `path` | `str \| None` | `Field(default=None, description='Optional workspace-relative directory to restrict scanning to.')` |
| `glob_filter` | `str \| None` | `Field(default=None, description="Optional fnmatch glob restricting which file paths are scanned (e.g. '*.py' to skip non-Python files).")` |
| `output_mode` | `Literal['content', 'files_with_matches', 'count']` | `Field(default='files_with_matches', description="'files_with_matches' (default): list of files containing matches. 'count': files with per-file match counts. 'content': matched lines formatted 'path:line:body' (or 'path:body' when line_numbers=False).")` |
| `head_limit` | `int` | `Field(default=250, ge=0, description='Truncate result set after this many entries (files in matches/count modes; lines in content mode). Set to 0 for unlimited (subject to 20 KB content cap).')` |
| `offset` | `int` | `Field(default=0, ge=0, description='Skip the first N matches (pagination helper).')` |
| `case_insensitive` | `bool` | `Field(default=False, description='Apply re.IGNORECASE.')` |
| `line_numbers` | `bool` | `Field(default=False, description='In content mode, prefix each line with its line number.')` |
| `multiline` | `bool` | `Field(default=False, description="When true, apply re.MULTILINE \| re.DOTALL — '.' matches newlines and ^/$ match line boundaries.")` |

**Class variables**: `model_config = ConfigDict(extra='forbid')`

#### `GrepOutput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L90]

Output schema reporting grep results including matched files, content, counts, and truncation flags.

**Fields**

| name | type | default |
|------|------|---------|
| `cwd` | `str` | `Field(..., description='Current sandbox working directory.')` |
| `pattern` | `str` | `Field(..., description='Regex pattern that was applied.')` |
| `mode` | `str` | `Field(..., description='Output mode in effect.')` |
| `filenames` | `list[str]` | `Field(default_factory=list, description='Files containing matches, in scan order.')` |
| `content` | `str` | `Field(default='', description="Rendered match content for 'content' mode, or 'path:count' lines for 'count' mode. Empty in 'files_with_matches' mode.")` |
| `num_files` | `int` | `Field(default=0, description='Number of files with matches.')` |
| `num_lines` | `int` | `Field(default=0, description='Number of lines emitted (content mode).')` |
| `num_matches` | `int` | `Field(default=0, description='Total regex matches counted.')` |
| `applied_limit` | `int \| None` | `Field(default=None, description='Head limit actually applied (None when unlimited).')` |
| `applied_offset` | `int` | `Field(default=0, description='Offset actually applied.')` |
| `truncated` | `bool` | `Field(default=False, description='True when head_limit or the 20 KB content cap was hit.')` |

---

## `tools/sandbox/multi_edit/multi_edit.py`

#### `MultiEditOp`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L23]

A single find-and-replace edit operation with old text, replacement, and replace-all flag.

**Fields**

| name | type | default |
|------|------|---------|
| `old_text` | `str` | `Field(..., description='Exact text to find.')` |
| `new_text` | `str` | `Field(default='', description='Replacement text.')` |
| `replace_all` | `bool` | `Field(default=False, description='Replace every occurrence of `old_text` instead of requiring a unique match.')` |

**Class variables**: `model_config = ConfigDict(extra='forbid')`

#### `MultiEditInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L34]

Input schema for the multi_edit tool specifying a target file and an ordered batch of edits.

**Fields**

| name | type | default |
|------|------|---------|
| `file_path` | `str` | `Field(..., description='Repo-relative or sandbox-root file path.')` |
| `edits` | `list[MultiEditOp]` | `Field(..., description="Ordered edits applied sequentially against evolving content (edit N sees edit N-1's result); all-or-nothing.")` |
| `description` | `str` | `Field(default='', description='Optional short note about the edits.')` |

**Class variables**: `model_config = ConfigDict(extra='forbid')`

#### `MultiEditOutput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L51]

Output schema reporting multi_edit results including status, changed paths, conflicts, and applied edit count.

**Fields**

| name | type | default |
|------|------|---------|
| `cwd` | `str` | `Field(..., description='Current sandbox working directory.')` |
| `file_path` | `str` | `Field(..., description='Resolved file path that was edited.')` |
| `status` | `str` | `Field(..., description='Edit result: edited, aborted_version, or failed.')` |
| `changed_paths` | `list[str]` | `Field(default_factory=list, description='Files changed by the edits.')` |
| `changed_path_kinds` | `dict[str, str]` | `Field(default_factory=dict, description='Changed paths keyed to write/delete/symlink/opaque_dir.')` |
| `mutation_source` | `str` | `Field(default='', description='Mutation source tag.')` |
| `conflict_reason` | `str \| None` | `Field(default=None, description='Conflict reason when the edits failed.')` |
| `error` | `dict[str, object]` | `Field(default_factory=dict, description='Typed error payload.')` |
| `applied_edits` | `int` | `Field(default=0, description='Number of edits applied (not occurrence count).')` |

---

## `tools/sandbox/shell/shell.py`

#### `ShellInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L33]

Input for shell.

**Fields**

| name | type | default |
|------|------|---------|
| `command` | `str` | `Field(..., min_length=1, description='Shell command to run for tests, builds, or verification.')` |
| `timeout` | `int` | `Field(default=_SHELL_DEFAULT_TIMEOUT, description='Shell command timeout in seconds.')` |

#### `ShellOutput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L47]

Output schema reporting shell command execution including status, exit code, stdout/stderr, and changed paths.

**Fields**

| name | type | default |
|------|------|---------|
| `cwd` | `str` | `Field(..., description='Current sandbox working directory.')` |
| `status` | `str` | `Field(..., description='Execution status: ok or error.')` |
| `changed_paths` | `list[str]` | `Field(default_factory=list, description='Files changed by the command.')` |
| `changed_path_kinds` | `dict[str, str]` | `Field(default_factory=dict, description='Captured changed paths keyed to write/delete/symlink/opaque_dir.')` |
| `mutation_source` | `str` | `Field(default='', description='Mutation source tag.')` |
| `conflict_reason` | `str \| None` | `Field(default=None, description='Conflict reason when auditing failed.')` |
| `command` | `str` | `Field(..., description='Shell command that was run.')` |
| `exit_code` | `int \| str` | `Field(..., description='Command exit code.')` |
| `stdout` | `str` | `Field(..., description='Captured stdout.')` |
| `stderr` | `str` | `Field(..., description='Captured stderr.')` |
| `error` | `str` | `Field(default='', description='Error detail when status is error.')` |

---

## `tools/skills/load_skill_reference.py`

#### `LoadSkillReferenceInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L15]

Input schema naming the skill and reference document to load via the load_skill_reference tool.

**Fields**

| name | type | default |
|------|------|---------|
| `skill_name` | `str` | `Field(..., description='Name of the skill that owns the reference.')` |
| `reference_name` | `str` | `Field(..., description="Exact reference document name to load. Do not use 'default'; call load_skill(skill_name) for the main skill instructions.")` |

---

## `tools/subagent/_factory.py`

#### `RestrictedRunSubagentTool`  ·  _class_  ·  bases: `BaseTool`  ·  [L53]

Caller-aware wrapper that narrows run_subagent's agent_name schema.

**Class variables**: `__doc__ = run_subagent.__doc__`

**Instance attributes**: `_delegate`, `input_model`

<details><summary>Methods (2)</summary>

`__init__`, `execute`

</details>

---

## `tools/subagent/run_subagent/run_subagent.py`

#### `_ValidatedRunSubagentRequest`  ·  _dataclass_  ·  decorators: `@dataclass`  ·  [L48]

Internal wrapper holding a validated subagent definition for a run_subagent request.

**Fields**

| name | type | default |
|------|------|---------|
| `sub_def` | `Any` |  |

#### `RunSubagentInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L95]

Runtime input model for run_subagent.

**Fields**

| name | type | default |
|------|------|---------|
| `agent_name` | `str` | `Field(..., description='Name of a registered dispatchable subagent.')` |
| `prompt` | `str` | `Field(..., min_length=1, description='Free-form, fully descriptive task prompt. Include any target paths, context, and required actions inline — this is the only channel the subagent receives.')` |

---

## `tools/submission/advisor/submit_advisor_feedback/submit_advisor_feedback.py`

#### `SubmitAdvisorFeedbackInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L18]

Input schema carrying an advisor's approve/reject verdict and summary for the submit_advisor_feedback tool.

**Fields**

| name | type | default |
|------|------|---------|
| `verdict` | `Literal['approve', 'reject']` |  |
| `summary` | `str` | `Field(..., min_length=1)` |

**Class variables**: `model_config = ConfigDict(extra='forbid')`

---

## `tools/submission/context/attempt.py`

#### `AttemptSubmissionContextError`  ·  _exception_  ·  bases: `RuntimeError`  ·  [L19]

User-facing submission context resolution failure.

#### `AttemptSubmissionContext`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L24]

Attempt-bound submission context.

**Fields**

| name | type | default |
|------|------|---------|
| `task_center_task_id` | `str` |  |
| `task` | `dict[str, Any]` |  |
| `attempt` | `Attempt` |  |
| `iteration` | `Iteration` |  |
| `workflow` | `Workflow` |  |
| `runtime` | `AttemptDeps` |  |
| `orchestrator` | `AttemptOrchestrator` |  |

---

## `tools/submission/context/executor.py`

#### `ExecutorSubmissionContext`  ·  _dataclass_  ·  decorators: `@dataclass(frozen=True, slots=True)`  ·  [L21]

Unified context for executor-shaped terminal submissions.

**Fields**

| name | type | default |
|------|------|---------|
| `task_center_task_id` | `str` |  |
| `task` | `dict[str, Any]` |  |
| `runtime` | `AttemptDeps` |  |
| `attempt_ctx` | `AttemptSubmissionContext` |  |

<details><summary>Methods (4)</summary>

`attempt_id`, `submit_executor_success`, `submit_executor_blocker`, `start_delegated_workflow`

</details>

---

## `tools/submission/evaluator/submit_evaluation_failure/submit_evaluation_failure.py`

#### `SubmitEvaluationFailureInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L28]

Input schema carrying an evaluator's failure summary and failed criteria for the submit_evaluation_failure tool.

**Fields**

| name | type | default |
|------|------|---------|
| `summary` | `str` | `Field(..., min_length=1)` |
| `failed_criteria` | `list[str]` | `Field(default_factory=list)` |

---

## `tools/submission/evaluator/submit_evaluation_success/submit_evaluation_success.py`

#### `SubmitEvaluationSuccessInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L28]

Input schema carrying an evaluator's success summary and passed criteria for the submit_evaluation_success tool.

**Fields**

| name | type | default |
|------|------|---------|
| `summary` | `str` | `Field(..., min_length=1)` |
| `passed_criteria` | `list[str]` | `Field(default_factory=list)` |

---

## `tools/submission/executor/submit_execution_blocker/submit_execution_blocker.py`

#### `SubmitExecutionBlockerInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L25]

Input schema carrying an executor's blocker summary for the submit_execution_blocker terminal tool.

**Fields**

| name | type | default |
|------|------|---------|
| `summary` | `str` | `Field(..., min_length=1)` |

---

## `tools/submission/executor/submit_execution_handoff/submit_execution_handoff.py`

#### `SubmitExecutionHandoffInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L39]

Input schema for an executor handing a goal back to the planner for decomposition.

**Fields**

| name | type | default |
|------|------|---------|
| `goal_handoff` | `str` | `Field(..., min_length=1, description='The original goal statement (verbatim or paraphrased without information loss), plus your findings and the reasons it needs to be decomposed by the planner.')` |

<details><summary>Methods (1)</summary>

`_validate_goal_handoff`

</details>

---

## `tools/submission/executor/submit_execution_success/submit_execution_success.py`

#### `SubmitExecutionSuccessInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L25]

Input schema for an executor reporting successful task completion with summary and artifacts.

**Fields**

| name | type | default |
|------|------|---------|
| `summary` | `str` | `Field(..., min_length=1)` |
| `artifacts` | `list[str]` | `Field(default_factory=list)` |

---

## `tools/submission/explorer/submit_exploration_result/submit_exploration_result.py`

#### `SubmitExplorationResultInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L16]

Input schema for an explorer submitting exploration findings, summary, and references.

**Fields**

| name | type | default |
|------|------|---------|
| `summary` | `str` | `Field(..., min_length=1)` |
| `findings` | `list[str]` | `Field(default_factory=list)` |
| `references` | `list[str]` | `Field(default_factory=list)` |

---

## `tools/submission/planner/_schemas.py`

#### `PlanTaskInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L26]

Schema for a single planned task with id, assigned agent, and dependency edges.

**Fields**

| name | type | default |
|------|------|---------|
| `id` | `str` | `Field(..., min_length=1)` |
| `agent_name` | `str` | `Field(..., min_length=1)` |
| `deps` | `list[str]` | `Field(default_factory=list)` |

**Class variables**: `model_config = ConfigDict(extra='forbid')`

<details><summary>Methods (3)</summary>

`_validate_id`, `_validate_agent_name`, `_validate_deps`

</details>

#### `SharedPlannerSubmissionInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L51]

Planner submission boundary schema.

**Fields**

| name | type | default |
|------|------|---------|
| `plan_spec` | `str` | `Field(..., min_length=1)` |
| `evaluation_criteria` | `list[str]` | `Field(..., min_length=1)` |
| `tasks` | `list[PlanTaskInput]` | `Field(..., min_length=1)` |
| `task_specs` | `dict[str, str]` | `Field(..., min_length=1)` |

**Class variables**: `model_config = ConfigDict(extra='forbid')`

<details><summary>Methods (3)</summary>

`_validate_plan_spec`, `_validate_evaluation_criteria`, `_validate_task_specs`

</details>

---

## `tools/submission/planner/submit_plan_closes_goal/submit_plan_closes_goal.py`

#### `SubmitPlanClosesGoalInput`  ·  _class_  ·  bases: `SharedPlannerSubmissionInput`  ·  [L29]

Input schema for a planner submitting a plan that fully closes the goal.

---

## `tools/submission/planner/submit_plan_defers_goal/submit_plan_defers_goal.py`

#### `SubmitPlanDefersGoalInput`  ·  _class_  ·  bases: `SharedPlannerSubmissionInput`  ·  [L32]

Input schema for a planner submitting a plan that defers remaining goal work to the next iteration.

**Fields**

| name | type | default |
|------|------|---------|
| `deferred_goal_for_next_iteration` | `str` | `Field(..., min_length=1)` |

<details><summary>Methods (1)</summary>

`_validate_deferred_goal_for_next_iteration`

</details>

---

## `tools/submission/verifier/submit_verification_failure/submit_verification_failure.py`

#### `SubmitVerificationFailureInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L28]

Input schema for the terminal tool reporting a failed verification attempt with a summary and unresolved issues.

**Fields**

| name | type | default |
|------|------|---------|
| `summary` | `str` | `Field(..., min_length=1)` |
| `unresolved_issues` | `list[str]` | `Field(default_factory=list)` |

---

## `tools/submission/verifier/submit_verification_success/submit_verification_success.py`

#### `SubmitVerificationSuccessInput`  ·  _pydantic_  ·  bases: `BaseModel`  ·  [L28]

Input schema for the terminal tool reporting a successful verification attempt with a summary and performed checks.

**Fields**

| name | type | default |
|------|------|---------|
| `summary` | `str` | `Field(..., min_length=1)` |
| `checks` | `list[str]` | `Field(default_factory=list)` |

