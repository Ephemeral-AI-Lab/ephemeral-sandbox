# Decomposition Decision Rubric

Use this reference when a changelog lane feels too broad, too speculative, or
ambiguous between `atomic` and `expandable`.

## Primary Test

Ask one question first:

> Can one worker own this deliverable end-to-end without coordinating with
> another concurrent worker mid-task?

| Answer | Action |
|---|---|
| Yes, clearly | Keep it atomic |
| Yes, but it spans several loosely related deliverables | Split or mark expandable |
| No | Mark expandable |
| Unsure | Apply the tiebreakers below |

## Tiebreaker 1: Owned Production Surface

Keep a lane atomic only if you can name:

- the dominant production file or symbol it owns
- the nearby tests or fixtures it may touch
- why another lane does not need to participate mid-task

If you cannot defend those three points in one short paragraph, the lane is too
broad for atomic ownership.

## Tiebreaker 2: Failure-Cluster Count

| Evidence shape | Guidance |
|---|---|
| One FAIL_TO_PASS cluster, one owning production area | Usually atomic |
| Multiple independent FAIL_TO_PASS clusters in one lane | Expandable |
| No direct failing evidence, but one clear implementation module | Atomic or fold into neighbor |
| No failing evidence and several modules/directories | Downstream expandable follow-up |

## Tiebreaker 3: File and Module Spread

| Spread | Guidance |
|---|---|
| 1-3 closely related files | Atomic |
| 4-6 files in one cohesive module | Often atomic |
| 4+ files across multiple modules or layers | Expandable |
| Broad “compatibility sweep” across a package | Expandable unless evidence proves a single root cause |

## Tiebreaker 4: Root-Frontier Value

For root planning, independent ready tasks should be scarce and high-value.

- First-frontier lanes should have FAIL_TO_PASS evidence, explicit production
  ownership, or strict unblocker status.
- Real but weakly evidenced release bullets should not become speculative
  first-frontier lanes.
- Remaining lower-evidence work should usually collapse into one downstream
  expandable follow-up macro instead of many root tasks.

## Common Expandable Patterns

- “Compatibility sweep” across several modules
- “Implement remaining release follow-ups”
- “Refactor tokenization and move tests”
- “Apply deprecations across dataframe and array”
- Any lane that mixes multiple unrelated PR bullets just to reduce task count

## Common Atomic Patterns

- One deprecation family in one owning module
- One rename with a bounded production surface
- One compatibility fix tied to one production subsystem
- One verification task that depends on all implementation tasks

## Benchmark-Safe Defaults

- Hidden or withheld tests are symptom locators, not deliverables.
- Every implementation lane must still own at least one production file.
- Do not create “investigate benchmark” or “mirror hidden test” tasks.
- If uncertainty remains high, prefer a narrower expandable lane over a broad
  atomic lane.
