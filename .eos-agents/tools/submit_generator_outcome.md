---
intent: read_only
terminal: true
hooks: [no_background_sessions, advisor_approval]
---
Terminate your generator run with SUCCESS or FAILED for the current generator task.

## Inputs
- `status`: `"success"` when the assigned task is complete, or `"failed"`
  when it cannot be completed in this attempt.
- `outcome`: 1-3 sentence factual report. For success, include what changed,
  verification evidence, and artifact references. For failure, include the
  concrete blocker and evidence.

## Do Not Use This Tool When
- A delegated workflow you started is still outstanding; use
  `delegate_workflow` only for new delegated work, then inspect or
  cancel outstanding workflow handles before submitting your final outcome.

## Behavior
- Records reducer-visible generator success or failure on the current task.
- The orchestrator advances the DAG after the submission is accepted.

## Success vs Failure Decision

Generator task:
- Treat `<dependencies>` outcomes as context inputs for your `<assigned_task>`.
- Work on the assigned generator task, use `delegate_workflow` only
  when a subtask needs delegated decomposition, then choose exactly one terminal
  tool after all delegated work is resolved.

Call `submit_generator_outcome` with `status="success"` when:
- You completed the assigned task and the deliverable is in place.
- Required verification passed, or the task did not require verification.
- Your `outcome` identifies what downstream tasks or reducers should read,
  including verification and artifact references.

Call `submit_generator_outcome` with `status="failed"` when:
- You attempted the assigned task but cannot complete it in this attempt.
- The blocker is concrete enough for retry or replanning.
- Delegated workflow results still leave the assigned task incomplete.