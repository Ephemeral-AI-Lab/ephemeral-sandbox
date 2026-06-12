# Side Note: Workflow and Background Supervisor Dependency Direction

Status: Observed
Date: 2026-06-12
Owner: eos-agent-core
Related: `phase-05-workflow-orchestration_SPEC.md`,
`phase-05.1-workflow-context-redesign_SPEC.md`

## Point

`@eos/workflow` does not depend on `@eos/background` and should not import the
background supervisor. The dependency direction is deliberately inverted:
workflow exposes a delegated-workflow handle, and the runtime/tool adapter
registers that handle with the delegating run's `BackgroundSessionSupervisor`.

The live shape is:

```text
@eos/workflow
  WorkflowService.delegate(...)
    -> DelegatedWorkflow {
         workflowId,
         terminal: Promise<WorkflowTerminal>,
         cancel(reason),
         describe()
       }

@eos/tool + @eos/agent-runtime
  delegate_workflow adapter
    -> supervisor.registerBackgroundSession(
         { type: "workflow", id: workflowId },
         {
           settled: workflow.terminal.then(mapWorkflowTerminal),
           cancel: workflow.cancel,
           describe: workflow.describe
         }
       )

@eos/background
  BackgroundSessionSupervisor
    -> observes the registered handle's settled promise
    -> publishes one session_settled notification
    -> evicts the session after notification drain
```

## Completion Flow

Workflow completion is DB-first and supervisor-second:

1. A planner or worker run submits through its entity-bound terminal tool.
2. `WorkflowService` reconciles the workflow tree in `@eos/workflow`.
3. When the root workflow becomes `Success`, `Failed`, or `Cancelled`,
   `WorkflowService` resolves the delegated workflow's `terminal` promise.
4. The parent run's `BackgroundSessionSupervisor` observes that promise through
   the handle registered by `delegate_workflow`.
5. The supervisor records the background session as `completed`, `failed`, or
   `cancelled`, publishes `session_settled`, and later removes the session after
   the notification is drained.

This means the background supervisor never owns workflow state, workflow rows,
planner/worker scheduling, or workflow terminal derivation. It owns only the
parent run's background-session lifecycle and user-visible completion
notification.

## Handle Contract

Every registered background session must provide a `BackgroundSessionHandle`
with two lifecycle capabilities:

```ts
{
  settled: Promise<BackgroundSessionOutcome>;
  cancel(reason: string): Promise<void>;
}
```

`settled` is the one push-completion surface the supervisor observes.
`cancel` is the teardown callback the supervisor invokes for
`cancel_background_session` and run-disposal cascades. Both are required for
every session kind because the supervisor is intentionally generic: it never
switches on `workflow`, `subagent`, or `command` to find behavior.

`describe` is optional:

```ts
{
  describe?(): string;
}
```

It is only display metadata for `list_background_sessions`. When a handle
provides `describe`, the supervisor copies its current return value into the
row's `description` field. It does not affect scheduling, cancellation,
settlement, notification delivery, or workflow terminal derivation.

The live workflow adapter provides `describe` as the first line of the delegated
goal so `list_background_sessions` can show a useful label for the open
workflow. Subagent sessions currently register without `describe`; their list
rows still work because `description` is optional. Command sessions may provide
a command-line description, but that remains a spawn-site decision, not a
supervisor requirement.

## Boundary Rule

Keep the package boundary this way:

- `@eos/workflow` may expose a narrow terminal/cancel/optional-describe handle.
- `@eos/tool` may adapt that handle into a background session.
- `@eos/agent-runtime` may wire both packages for each parent run.
- `@eos/background` must stay generic over background session handles and should
  not learn workflow DB or scheduler concepts.
