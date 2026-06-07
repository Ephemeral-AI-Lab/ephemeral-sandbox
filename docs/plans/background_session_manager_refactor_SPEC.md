# Background Session Manager Refactor — SPEC

Status: Implemented
Date: 2026-06-08
Owner: agent-core engine
Scope: `agent-core/crates/eos-engine`, `agent-core/crates/eos-tools`,
`agent-core/crates/eos-workflow`
Related:
- `docs/plans/agent_run_local_background_supervisor_SPEC.md`
- `docs/plans/uniform_recursive_cancellation_SPEC.md`

## 1. Problem

`agent-core/crates/eos-engine/src/background` currently mixes several concepts:

- per-agent-run background session accounting,
- subagent launch policy and model-facing progress rendering,
- delegated workflow bookkeeping,
- command-session completion recovery,
- parent-exit finalization,
- notification rendering,
- implementation names inherited from the old "supervisor/lane/record" model.

That makes the background module look broader than its intended ownership.
Background should not own model-tool semantics, workflow creation, or command
execution behavior. It should own the engine-local lifecycle accounting for
background sessions: count live sessions, observe completion, emit completion
notifications, and cancel live background sessions when requested.

## 2. Goals

- Replace `lane`, `record`, `supervisor`, and `handle` vocabulary with explicit
  background-session manager vocabulary.
- Keep one per-agent-run background runtime.
- Keep one session manager per background family: subagent, workflow, command.
- Give the three session-manager folders the same file pattern:
  `manager.rs`, `session.rs`, and `monitor.rs`.
- Keep the shared interfaces for those three files in
  `session_managers/mod.rs`.
- Rename folders under `session_managers/` to `subagent`, `workflow`, and
  `command`; do not use `_session` in the folder names.
- Make `BackgroundSessionRuntime` an aggregate root only. It should expose the three
  concrete managers, aggregate counts, and aggregate cancellation.
- Keep subagent/workflow/command-specific lifecycle details encapsulated inside
  the relevant session manager.
- Shift tool-specific behavior out of `background`.

## 3. Non-Goals

- No workflow creation inside `background`; workflow creation stays in
  `eos-workflow` behind `WorkflowPort::start`.
- No command execution inside `background`; command execution stays in the
  sandbox tool/daemon path.
- No model-facing prompt wording, rejection wording, or progress rendering inside
  `background`.
- No broad service bag for manager dependencies.
- No inheritance-style hierarchy or public generic abstraction.
- No `core.rs`, `status.rs`, `snapshot.rs`, or shared top-level
  `session_managers/monitor.rs` layer unless later code growth proves it needed.
- No `cancel_all` method on concrete session managers. Use one `cancel(reason)`
  method per manager and one aggregate `BackgroundSessionRuntime::cancel(reason)`.

## 4. Target Ownership

```text
Agent run
  owns BackgroundSessionRuntime
    ├─ agent_run_id
    ├─ SubagentSessionManager
    │    ├─ live subagent sessions
    │    └─ SubagentSessionMonitor
    ├─ WorkflowSessionManager
    │    ├─ live workflow sessions
    │    └─ WorkflowSessionMonitor
    └─ CommandSessionManager
         ├─ live command sessions
         └─ CommandSessionMonitor
```

Each concrete manager owns:

- its live session map,
- its own completion polling logic through the shared manager interface,
- notification emission on finish,
- its own cancellation mechanics.

The monitors do not call `NotificationService` directly. They use the manager
interface:

```text
monitor loop -> manager.poll() -> manager.finish(completion)
manager.finish(completion) -> BackgroundNotificationEmitter.emit(...)
```

## 5. Target File and Folder Structure

```text
agent-core/crates/eos-engine/src/background/
  mod.rs
  factory.rs
  session_runtime.rs
  notification.rs
  session_managers/
    mod.rs
    subagent/
      mod.rs
      manager.rs
      session.rs
      monitor.rs
    workflow/
      mod.rs
      manager.rs
      session.rs
      monitor.rs
    command/
      mod.rs
      manager.rs
      session.rs
      monitor.rs
```

Removed/replaced concepts:

| Current | Target |
| --- | --- |
| `background/handle.rs` | fold cloneable service/runtime wrapper into `session_runtime.rs` or rename to service explicitly if needed |
| `background/supervisor.rs` | remove; state lives in concrete managers |
| `background/parent_exit.rs` | fold finalizer into `session_runtime.rs` or a narrowly named `finalizer.rs` if it remains separate |
| `background/lanes/*` | replace with `session_managers/{subagent,workflow,command}` |
| `BackgroundSupervisorRuntime` | `BackgroundSessionRuntime` |
| `BackgroundTaskSupervisor` | remove |
| `BackgroundSupervisorHandle` | replace with a port-facing service name if a cloneable wrapper remains |
| `SubagentLane` / `WorkflowLane` / `CommandSessionLane` | `SubagentSessionManager` / `WorkflowSessionManager` / `CommandSessionManager` |
| `*Record` | session structs in `session.rs` |

## 6. Shared Interfaces

`session_managers/mod.rs` is the shared contract layer for the three local file
roles: manager, session, and monitor.

```rust
pub(super) mod command;
pub(super) mod subagent;
pub(super) mod workflow;

pub(super) trait BackgroundSession {
    type Id: Eq + std::hash::Hash + Clone + Send + Sync + 'static;

    fn id(&self) -> &Self::Id;
}

#[async_trait::async_trait]
pub(super) trait BackgroundSessionManager {
    type Session: BackgroundSession + Send + 'static;
    type Completion: Send + 'static;

    async fn insert(&self, session: Self::Session);
    async fn count(&self) -> usize;
    async fn poll(&self) -> Vec<Self::Completion>;
    async fn finish(&self, completion: Self::Completion);
    async fn cancel(&self, reason: &str);
}

pub(super) trait BackgroundSessionMonitor {
    type Manager: BackgroundSessionManager + Clone + Send + Sync + 'static;

    fn spawn(manager: Self::Manager, interval: std::time::Duration) -> Self;
}

pub(super) fn spawn_monitor_loop<M>(
    manager: M,
    interval: std::time::Duration,
) -> tokio::task::JoinHandle<()>
where
    M: BackgroundSessionManager + Clone + Send + Sync + 'static,
    M::Completion: Send + 'static,
{
    tokio::spawn(async move {
        loop {
            for completion in manager.poll().await {
                manager.finish(completion).await;
            }
            tokio::time::sleep(interval).await;
        }
    })
}
```

This shared layer is private to `eos-engine`. It is not a public abstraction.
The concrete managers keep domain-specific fields and behavior.

## 7. BackgroundSessionRuntime API

`BackgroundSessionRuntime` is the aggregate root. It should not forward every possible
operation. It should expose managers and aggregate behavior only.

```rust
pub(super) struct BackgroundSessionRuntime {
    agent_run_id: AgentRunId,
    subagent_session_manager: SubagentSessionManager,
    workflow_session_manager: WorkflowSessionManager,
    command_session_manager: CommandSessionManager,
}

impl BackgroundSessionRuntime {
    pub(super) fn new(...) -> Self;

    pub(super) fn agent_run_id(&self) -> &AgentRunId;

    pub(super) fn subagent_session_manager(&self) -> &SubagentSessionManager;
    pub(super) fn workflow_session_manager(&self) -> &WorkflowSessionManager;
    pub(super) fn command_session_manager(&self) -> &CommandSessionManager;

    pub(super) async fn count(&self) -> BackgroundSessionCounts;

    pub(super) async fn cancel(&self, reason: &str) -> BackgroundSessionCounts;
}
```

`BackgroundSessionRuntime::cancel(reason)` calls:

```rust
self.subagent_session_manager.cancel(reason).await;
self.workflow_session_manager.cancel(reason).await;
self.command_session_manager.cancel(reason).await;
self.count().await
```

`BackgroundSessionRuntime::cancel` must not accept `SubagentPort`, `WorkflowPort`,
`CommandPort`, or other manager-specific dependencies. Those dependencies belong
inside the manager that uses them.

## 8. Concrete Managers

Each concrete manager uses the same dependency naming convention:
`subagent_port`, `workflow_port`, and `command_port`. These are
manager-facing ports for background tracking, polling, and cancellation. They
may wrap broader existing crate ports during migration, but the background
module should depend on the narrow role names.

### 8.1 Subagent

```rust
#[derive(Clone)]
pub(super) struct SubagentSessionManager {
    sessions: Arc<Mutex<HashMap<SubagentSessionId, SubagentSession>>>,
    subagent_port: Arc<dyn SubagentPort>,
    notification: BackgroundNotificationEmitter,
}
```

`subagent/session.rs`:

```rust
pub(super) struct SubagentSession {
    id: SubagentSessionId,
    agent_run_id: AgentRunId,
    driver: JoinHandle<AgentRunResult>,
}

impl BackgroundSession for SubagentSession {
    type Id = SubagentSessionId;

    fn id(&self) -> &Self::Id {
        &self.id
    }
}
```

Subagent polling uses the same loop pattern as workflow and command. The
manager's `poll()` checks live subagent sessions for completed local driver
tasks, converts completed drivers into `SubagentCompletion`, and returns those
completions. `finish()` removes the completed session and emits the background
notification.

### 8.2 Workflow

```rust
#[derive(Clone)]
pub(super) struct WorkflowSessionManager {
    sessions: Arc<Mutex<HashMap<WorkflowSessionId, WorkflowSession>>>,
    workflow_port: Arc<OnceLock<Arc<dyn WorkflowPort>>>,
    notification: BackgroundNotificationEmitter,
}
```

`workflow/session.rs`:

```rust
pub(super) struct WorkflowSession {
    id: WorkflowSessionId,
    workflow_id: WorkflowId,
}

impl BackgroundSession for WorkflowSession {
    type Id = WorkflowSessionId;

    fn id(&self) -> &Self::Id {
        &self.id
    }
}
```

Workflow polling asks `WorkflowPort::status` for live sessions. Terminal
workflow states produce `WorkflowCompletion`; non-terminal or transient failures
remain live and are retried by the next monitor tick.

### 8.3 Command

```rust
#[derive(Clone)]
pub(super) struct CommandSessionManager {
    sessions: Arc<Mutex<HashMap<CommandSessionId, CommandSession>>>,
    agent_run_id: AgentRunId,
    command_port: Arc<dyn CommandPort>,
    notification: BackgroundNotificationEmitter,
}
```

`command/session.rs`:

```rust
pub(super) struct CommandSession {
    id: CommandSessionId,
    sandbox_id: SandboxId,
}

impl BackgroundSession for CommandSession {
    type Id = CommandSessionId;

    fn id(&self) -> &Self::Id {
        &self.id
    }
}
```

Command polling groups live command sessions by sandbox and calls daemon
completion collection once per sandbox. Returned terminal completions produce
`CommandCompletion`; missing completions remain live.

## 9. Monitors

Each agent run owns one monitor per session family:

```text
BackgroundSessionRuntime
  ├─ SubagentSessionManager -> SubagentSessionMonitor
  ├─ WorkflowSessionManager -> WorkflowSessionMonitor
  └─ CommandSessionManager  -> CommandSessionMonitor
```

Each concrete `monitor.rs` uses the same shared loop:

```rust
pub(super) struct SubagentSessionMonitor {
    join: JoinHandle<()>,
}

impl BackgroundSessionMonitor for SubagentSessionMonitor {
    type Manager = SubagentSessionManager;

    fn spawn(manager: Self::Manager, interval: Duration) -> Self {
        Self {
            join: spawn_monitor_loop(manager, interval),
        }
    }
}
```

Workflow and command monitors have the same shape with their own manager type.
The per-family differences live in `manager.poll()`, not in the monitor loop.

## 10. Functionality Shift Out of `background`

The refactor should reduce the responsibility of
`agent-core/crates/eos-engine/src/background`.

Move or keep outside `background`:

| Functionality | Target owner |
| --- | --- |
| `run_subagent` model-facing validation and rejection text | `agent-core/crates/eos-tools/src/tools/subagent` |
| subagent launch prompt shaping | `agent-core/crates/eos-tools/src/tools/subagent` |
| `check_subagent_progress` model-facing rendering | `agent-core/crates/eos-tools/src/tools/subagent` |
| `cancel_subagent` model-facing output text | `agent-core/crates/eos-tools/src/tools/subagent` |
| workflow creation / delegated lifecycle creation | `agent-core/crates/eos-workflow` through `WorkflowPort::start` |
| command execution and stdin/progress tool output rendering | `agent-core/crates/eos-tools/src/tools/sandbox` plus sandbox transport/daemon |

Keep inside `background`:

| Functionality | Reason |
| --- | --- |
| live background session accounting | engine lifecycle ownership |
| per-family completion monitoring | engine owns completion notification delivery |
| completion notification emission | completion notifications target the owning agent run |
| cancellation of live tracked sessions | parent-run cancellation / exit lifecycle |
| aggregate count and aggregate cancel | per-agent-run runtime ownership |

The background module should return typed facts or accept typed sessions and
completions. It should not render model-facing tool output.

## 11. Naming Rules

- Use `agent_run_id`, not `owner_agent_run_id`.
- In `SubagentSession`, use `agent_run_id`, not `child_agent_run_id` or
  `sub_agent_run_id`.
- Use `notification` for a `BackgroundNotificationEmitter` field, not
  `notifications`.
- Use `subagent_session_manager`, `workflow_session_manager`, and
  `command_session_manager` for `BackgroundSessionRuntime` fields and accessors.
- Use `BackgroundSessionRuntime` for the per-agent-run aggregate.
- Use `BackgroundSessionManager` for the private shared trait implemented by the
  three concrete session managers.
- Avoid `lane`, `record`, `supervisor`, and `handle` as new domain vocabulary.

## 12. Acceptance Criteria

- The new folder structure exists under `eos-engine/src/background` with
  `session_managers/{subagent,workflow,command}`.
- No new folder names under `session_managers/` end with `_session`.
- `session_managers/mod.rs` defines the shared manager/session/monitor
  interfaces.
- `BackgroundSessionRuntime` has only aggregate-root methods:
  `new`, `agent_run_id`, concrete-manager accessors, `count`, and `cancel`.
- `BackgroundSessionRuntime::cancel` takes only `reason: &str` and delegates to all
  three managers.
- Workflow-specific dependencies such as `WorkflowPort` are stored in
  `WorkflowSessionManager`, not passed into `BackgroundSessionRuntime::cancel`.
- Command-specific dependencies such as `CommandPort` are stored in
  `CommandSessionManager`, not passed into `BackgroundSessionRuntime::cancel`.
- `BackgroundNotificationEmitter` is referenced through a singular
  `notification` field.
- Tool-specific wording and progress rendering are no longer implemented inside
  `background`.
- `cargo check -p eos-engine --all-targets` passes after implementation.
- Focused tests cover count, finish notification, and cancel behavior for
  subagent, workflow, and command session managers.

## 13. Progress Tracker

| Phase | Status | Work |
| --- | --- | --- |
| 1 | Complete | Introduce new modules and shared interfaces without changing behavior. |
| 2 | Complete | Move subagent tracking to `session_managers/subagent`; move model-facing subagent behavior out of `background`. |
| 3 | Complete | Move workflow tracking/polling to `session_managers/workflow`; keep workflow creation in `eos-workflow`. |
| 4 | Complete | Move command tracking/polling to `session_managers/command`; keep command execution/output rendering outside `background`. |
| 5 | Complete | Replace old `lane`/`record`/`supervisor` exports and remove stale files. |
| 6 | Complete | Run focused checks and update architecture references if implementation lands. |
