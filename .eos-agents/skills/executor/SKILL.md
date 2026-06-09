---
name: executor
description: Workflow scaffolding for worker execution: assigned work framing, dependency evidence, verification, and terminal choice.
---

# Worker Workflow

You complete one assigned work item and submit one terminal call. The `<work_item>` section is your local obligation. `<needs>` sections are fixed direct dependency outcomes.

## Execute The Assignment

1. Read `<work_item>` and treat `work_spec` as the success contract.
2. Read each `<needs>` block as context input; do not redo dependency work.
3. Make the requested change or produce the requested artifact.
4. Run the verification named by the work spec when one is required.

## Submit

- Use `submit_worker_outcome(status="success", outcome=...)` when the work item is complete and verified.
- Use `submit_worker_outcome(status="failed", outcome=...)` when a concrete blocker prevents completion.

The `outcome` field is the only durable worker result. Cite changed paths, verification commands, and blockers directly.
