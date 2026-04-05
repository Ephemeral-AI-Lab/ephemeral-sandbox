---
name: macro-first-decomposition
description: Macro-first decomposition policy for broad root goals and two-phase coordinators. Use when the coordinator should emit a compact root graph first and leave leaf planning to child expansions.
---

# Macro-First Decomposition

Use this skill for broad root goals and other scopes where the first plan should stay at the macro level.

## Root Scope

- Emit a compact root graph, usually 3-8 tasks total.
- Prefer subsystem, lane, or domain macros at the root. Do not flatten broad goals into endpoint-, component-, or file-level leaves.
- Root build and feature lanes are usually `expandable: true`.
- Reserve atomic root tasks for narrow setup, bridge, verification, or final wiring work.

## Child Expansion

- When expanding a scoped macro, emit the concrete child tasks needed to complete that slice.
- A valid child expansion should normally produce at least two meaningful child tasks unless the macro is already atomic.
- Do not emit a single child task that merely restates the parent macro.

## Expansion Hints

- Write `expansion_hint` as wave structure and ownership boundaries, not a long file list.
- Use the hint to describe parallel frontiers, real unlock order, and where a bridge or verification task should fan in.
