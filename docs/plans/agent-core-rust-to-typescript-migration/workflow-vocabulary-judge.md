# Workflow Vocabulary Evaluation - LLM-as-Judge

Status: Draft
Date: 2026-06-12
Owner: eos-agent-core
Related: `phase-05.1-workflow-context-redesign_SPEC.md`,
`phase-05.2-workflow-outcome-context-rendering_SPEC.md`

## Purpose

The orchestration system launches ephemeral planner and worker agents whose
only knowledge of the system is the context the harness composes for them.
Entity names therefore are not branding: they appear in composed directives,
context paths, and DTO fields, and a misread name becomes a misplanned run.
This document describes the flow in neutral vocabulary, lists the candidate
naming patterns, and provides a judge prompt to evaluate which pattern an LLM
agent will understand and act on best.

Section 1 deliberately uses placeholder labels (L1/L2/L3) instead of any
candidate's terms, so the judge is not biased toward an incumbent vocabulary.

## 1. Background: The Flow Under Evaluation

### 1.1 Structure

The system pursues a delegated goal through a three-level hierarchy:

```text
L1 (the delegated pursuit)        holds the immutable goal
  L2 #1 -> L2 #2 -> L2 #3 ...     vertical lane: each advances the goal
    L3 #1 -> L3 #2 ...            horizontal lane: tries at the L2's goal
      plan + work items           one planning act, fan-out execution
```

- **L1** is created when an agent judges a goal too complex to carry in one
  run and delegates it. L1 holds the goal, unchanged for its whole life, and
  ends `Success`, `Failed`, or `Cancelled`.
- **L2** units are ordered and never repeat scope. The first L2's goal is the
  L1 goal; every later L2's goal is the transfer payload declared by its
  successful predecessor. An L2 boundary only exists because the predecessor
  closed successfully.
- **L3** units are ordered tries at their L2's goal, bounded by a retry
  budget. Each L3 starts with a planner agent that declares scope and emits
  work items (with dependency edges, executed by parallel worker agents).
  The planning act itself is execution state, not a context entity: it
  renders as a single summary file owned by its L3, never as a child folder.

### 1.2 Mechanism: Delegate

An agent calls `delegate(goal)` when the goal is too complex for one run.
From that point no single agent is load-bearing: every planner and worker is
ephemeral, sees only harness-composed context from the durable store, and the
pursuit survives any individual agent's death.

### 1.3 Mechanism: Defer / Transfer

The planner of an L3 declares a **focus**: the slice of its L2's goal it can
safely plan *now*. The remainder may be declared as a **transfer payload**.
Deferral is epistemic, not capacity-driven: the planner defers work whose
planning would be unsafe before the current slice's outcomes exist. The
transfer payload is therefore a goal to be planned later, never a plan, and
never a description of delivered artifacts. When the L2 closes successfully
with a transfer payload, the system creates the next L2 with that payload as
its goal; the new L2's planner reads it as inherited scope from a predecessor
that finished its own slice.

The L2 layer is also the **continuation gate**. L2 completion is the point
where the system decides whether the delegated pursuit is done or whether a
successor L2 must be spawned. If the successful L2 has no transfer payload,
the L1 can close successfully. If the successful L2 has a transfer payload
(`deferred_goal`, `handoff_goal`, `successor_goal`, `next_leg_goal`, etc.),
the system spawns a new L2 for that payload as its goal. That new L2 is
vertical continuation, not a retry of the previous L2 and not a continuation
of the same L2.

An L2 that closes successfully with a transfer payload is therefore just one
completed slice cut through the L1 goal: it finished the slice it owned and
declared the next slice boundary, rather than failing, pausing, or leaving
unfinished work inside the same L2.

### 1.4 Mechanism: Retry and Overrule

When an L3 fails and budget remains, the system creates the next L3. Its
planner receives the L2 goal, the standing focus, and the failed L3s
consistent with that focus. Two submission paths exist:

- **Submit without a focus**: the planner endorses the previous planner's
  framing and re-plans work items within the standing focus.
- **Submit with a new focus**: the planner overrules the previous planner.
  The standing focus and transfer payload reset, and prior L3s are archived
  as superseded judgments. The retry budget counts all L3s, endorsed or
  overruled, so disagreement is never free.

An L3 therefore targets its L2's *goal*; the focus is one planner's framing
of how to achieve it. An overrule is the one event that changes context
paths: superseded L3s relocate whole under the L2's `archived/` folder, with
the superseded declaration files riding the L3 whose planner declared them.
Archived subtrees are excluded from context search by default, so abandoned
directions stop surfacing to current-focus agents.

### 1.5 Context Store and Outcome Rendering

Every fact the durable store holds projects as one file at one path; an
absent fact is an absent path, and status is never a file (it rides listings
and DTOs). The skeleton, in neutral labels:

```text
l1_<id>/
  goal.md                          the delegated goal
  outcome.md                       L1 Success/Failed; Cancelled marker only

  l2_<id>/
    focus.md                       latest declaration; absent before first declaration
    transfer_goal.md               latest declaration's transfer payload; absent if none
    outcome.md                     Success or final Failed only

    l3_<id>/                       consistent with the standing focus only
      plan_summary.md              accepted planner summary; absent on planner death
      fail_reason.md               failed L3s only
      outcome.md                   successful or failed L3s only
      work_item_<id>/
        description.md             accepted planner work-item description
        spec.md                    accepted planner work-item spec
        summary.md                 worker submitted summary
        outcome.md                 worker submitted outcome

    archived/
      l3_<id>/                     overruled L3, relocated whole
        focus.md                   only if this L3 declared the superseded focus
        transfer_goal.md           only if that declaration carried one
        plan_summary.md            same L3-owned files as the live shape
        fail_reason.md
        outcome.md
        work_item_<id>/
          description.md
          spec.md
          summary.md
          outcome.md
```

Outcomes are derived at render time, bottom-up:

- **L3 outcome**: every work item in planner order with its status and
  worker summary; `(no work items)` when a planner died before materializing
  any. Fail reasons stay in `fail_reason.md`, never embedded.
- **L2 outcome**: the closing (last) L3's outcome, created only when the L2
  closes `Success` or exhausts its budget and closes `Failed`. A failed L3
  with budget remaining produces no L2 outcome.
- **L1 outcome**: the ordered ledger of every closed L2's outcome - a
  successful pursuit shows all slices' outcomes, and a failed pursuit shows
  the successful predecessors plus the final failed L2.
- **Cancellation is status, not business outcome**: cancelled L2s/L3s get no
  business outcome file; a cancelled L1 renders at most a cancellation
  marker plus already-closed L2 outcomes.

These files are what future agents read: the next L2's planner, a retry
planner, and any context search all consume this tree. Entity names appear
verbatim in every directory segment.

This tree is the under-the-hood context-sharing surface for a swarm-style
multiagent run. Planner and worker agents do not share memory, chat state, or
live process identity; they coordinate through the durable projection above.
The names must therefore make ownership and lifecycle readable from paths:
which entity owns the immutable delegated goal, which entity gates successor
spawning, which entity is the retry boundary, which files belong to the active
standing focus, and which archived subtrees are superseded history rather
than current instructions.

### 1.6 Why Names Carry Load

Four decision points depend on a naive agent decoding the names correctly:

1. **Initial planning**: peel a safely-plannable focus off the L2 goal and
   phrase the transfer payload as an actionable goal.
2. **Retry planning**: understand that the prior L2s succeeded, that this L3
   retries the same L2 goal, and that declaring a focus means overruling a
   peer.
3. **Inherited reading**: the next L2's planner must read the transferred
   goal as "my predecessor succeeded and this is the remaining scope," never
   as "something failed upstream."
4. **Continuation gate**: when an L2 closes successfully, the system must
   read absence of a transfer payload as "close the L1" and presence of a
   transfer payload as "spawn the next L2 for that goal."

## 2. Candidate Vocabularies

All candidates keep **attempt** for L3 and **refocus** for the overrule act;
the judge is evaluating the L1/L2 terms and the transfer vocabulary around
them. Pattern A is the incumbent. Pattern H is a modified pursuit candidate
that keeps F's L1 term while replacing `segment`/`successor_goal` with
`leg`/`next_leg_goal`. Pattern I is a quest candidate that keeps H's
`leg`/`next_leg_goal` while replacing `pursuit` with `quest` as the L1 term.

| | A | B | C | D | E | F | G | H | I |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| L1 | workflow | relay | ascent | expedition | mission | pursuit | endeavor | pursuit | quest |
| L2 | iteration | leg | pitch | stage | milestone | segment | leg | leg | leg |
| L3 | attempt | attempt | attempt | attempt | attempt | attempt | attempt | attempt | attempt |
| Delegate tool | `delegate_workflow` | `delegate_relay` | `delegate_ascent` | `delegate_expedition` | `delegate_mission` | `delegate_pursuit` | `delegate_endeavor` | `delegate_pursuit` | `delegate_quest` |
| Focus field | `iteration_focus` | `leg_focus` | `pitch_focus` | `stage_focus` | `milestone_focus` | `segment_focus` | `leg_focus` | `leg_focus` | `leg_focus` |
| Transfer field | `deferred_goal` | `handoff_goal` | `onward_goal` | `carryforward_goal` | `rollover_goal` | `successor_goal` | `remaining_goal` | `next_leg_goal` | `next_leg_goal` |
| Transfer verb | defer | hand off | route onward | carry forward | roll over | continue with | pass forward | spawn next leg for | spawn next leg for |
| Inherited-goal phrase | "deferred by iteration 1" | "handed off by leg 1" | "routed onward by pitch 1" | "carried forward by stage 1" | "rolled over from milestone 1" | "successor goal after segment 1" | "remaining goal from successful leg 1" | "next leg goal from successful leg 1" | "next leg goal from successful leg 1" |
| Budget phrase | "attempt 2 of 2" | "attempt 2 of 2" | "attempt 2 of 2" | "attempt 2 of 2" | "attempt 2 of 2" | "attempt 2 of 2" | "attempt 2 of 2" | "attempt 2 of 2" | "attempt 2 of 2" |
| Context path | `workflow_<id>/iteration_<id>/attempt_<id>/` | `relay_<id>/leg_<id>/attempt_<id>/` | `ascent_<id>/pitch_<id>/attempt_<id>/` | `expedition_<id>/stage_<id>/attempt_<id>/` | `mission_<id>/milestone_<id>/attempt_<id>/` | `pursuit_<id>/segment_<id>/attempt_<id>/` | `endeavor_<id>/leg_<id>/attempt_<id>/` | `pursuit_<id>/leg_<id>/attempt_<id>/` | `quest_<id>/leg_<id>/attempt_<id>/` |
| Transfer file | `deferred_goal.md` | `handoff_goal.md` | `onward_goal.md` | `carryforward_goal.md` | `rollover_goal.md` | `successor_goal.md` | `remaining_goal.md` | `next_leg_goal.md` | `next_leg_goal.md` |

## 3. Judge Prompt

Usage: provide sections 1 and 2 verbatim as context, then the prompt below.
Run across several models and several samples per model; shuffle the A-I
labels between samples to remove position and label bias, and de-shuffle
before aggregating.

```text
You are judging entity vocabularies for an agent-orchestration system. The
system description (Section 1) uses neutral labels L1/L2/L3; nine candidate
vocabularies (Section 2, Patterns A-I) propose real names for those labels
and their actions. The names will be read by small LLM agents that receive
NO documentation - only composed context messages and context-tree paths
using these words. Your job is to predict which vocabulary those agents will
decode and act on correctly.

Judge each pattern against these criteria, scoring 1-5 with a one-sentence
justification anchored in a concrete predicted agent behavior (not in
aesthetics):

1. Delegation story - does the L1 term convey "a goal too complex for one
   run, handed over for autonomous pursuit" rather than "a predefined
   process to execute"?
2. Vertical advancement - does the L2 term read as forward progress through
   new scope, with no risk of being read as repetition, looping,
   re-execution, or a pre-planned schedule?
3. Epistemic replanning - does the vocabulary support "defer the PLANNING of
   what cannot be safely planned yet," and will a planner phrase the
   transfer payload as an actionable goal rather than a delivered-artifact
   description?
4. Transfer semantics - reading the inherited-goal phrase, will a naive
   agent correctly conclude the predecessor SUCCEEDED and this is new scope?
   State the misreading risk if any.
5. Retry and overrule - does the L3 term support "retry of the L2 goal under
   a budget" while staying compatible with refocus-as-disagreement, and does
   the L2 term avoid absorbing the overrule semantics (a new L2 must never
   read as "a different strategy after failure")?
6. Zero-documentation legibility - given only the bare hierarchy and the
   Section 1.5 context paths, would a small model infer which boundary is
   retry and which is advancement? Name any polysemy that could contaminate
   the reading (other senses of each word) and how likely it is to win.
7. Ecosystem collisions - name conflicts with established software concepts
   (orchestration tools, design patterns, frameworks, project-management
   vocabulary) that would import wrong expectations for engineers or models.
8. Ergonomics - the tool name, field names, transfer file, and context path
   from the Section 2 table remain short, readable, and unambiguous; an L2
   term must also read sensibly as a directory that CONTAINS attempts and
   work items.
9. Continuation gate semantics - does the L2 term plus transfer vocabulary
   make it obvious that L2 completion gates whether the L1 closes or a new L2
   is spawned for the transferred goal? State any risk that an agent would
   treat the transfer as retrying the old L2, continuing the same L2, or
   handing work to a worker rather than creating successor L2 scope.
10. LLM-understandable semantics - under compressed prompts and no hidden
   documentation, would a smaller LLM reliably decode the literal words into
   the intended lifecycle, or would metaphor, jargon, or overloaded project
   language dominate?
11. Swarm context-sharing fit - given the Section 1.5 file tree as the only
   durable coordination surface for ephemeral planner and worker agents, do
   the names help agents infer shared context ownership, active versus
   archived scope, and L1/L2/L3 lifecycle boundaries from paths alone?

Then, for each pattern, simulate the four decision points from Section 1.6
in one sentence each: what does a naive planner most plausibly do at initial
planning, retry planning, inherited reading, and the L2 continuation gate
under this vocabulary? Name each pattern's single worst failure mode.

Output JSON only:

{
  "scores": { "A": {"1": n, ..., "11": n}, "B": {...}, "C": {...},
              "D": {...}, "E": {...}, "F": {...}, "G": {...},
              "H": {...}, "I": {...} },
  "justifications": { "A": {"1": "...", ...}, ... },
  "simulations": {
    "A": {"initial": "...", "retry": "...", "inherited": "...",
          "gate": "..."},
    ...
  },
  "worst_failure_mode": { "A": "...", "B": "...", "C": "...",
                          "D": "...", "E": "...", "F": "...",
                          "G": "...", "H": "...", "I": "..." },
  "ranking": ["X", "Y", "Z", "...", "...", "...", "...", "...", "..."],
  "winner": "X",
  "amendments": "term-level swaps or directive sentences that would improve
                 the winning pattern, if any"
}
```
