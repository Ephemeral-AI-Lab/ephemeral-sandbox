# Runtime Tool Surface

The current run may expose only a subset of the coordination helpers described
across the skills.

## Rules

- The live runtime overlay is authoritative over every static skill example.
- Use only the exact helper names exposed in the current run.
- If a planning reference mentions a missing helper, translate the intent onto
  the available helpers instead of inventing tool names.
- If the surface is planning-only, rely on the injected project context,
  synthesized codebase map, and scoped expansion facts already attached to the
  run.

## Coordinator Mapping

- Repo discovery intent: use the exposed repo-analysis helpers only when they
  are actually available in the current run.
- Worker selection intent: use the exposed roster helper if present; otherwise
  trust the injected roster context.
- Planning intent: call the exposed planning helper exactly once.

## Safety

- Do not perform implementation work from the coordinator.
- Do not ask clarifying questions when the planning surface can support a
  narrower reasonable assumption.
