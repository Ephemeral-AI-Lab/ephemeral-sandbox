# Runtime Tool Surface

Use the runtime tool-surface overlay for the exact helper names available in the
current run.

## Rules

- The live runtime overlay is authoritative over examples in `SKILL.md`.
- Use only helpers that are explicitly exposed in the current run.
- If a skill example mentions a helper that is not exposed, translate the intent
  onto the closest available helper instead of inventing names.
- Prefer synthesis from the reports already collected. Do not assume extra repo
  discovery tools are available unless the runtime overlay exposes them.

## Synthesis Discipline

- Treat explorer reports as the primary evidence.
- Use runtime helpers only to clarify the tool contract, not to restart broad
  exploration from the synthesizer stage.
- If a report is partial or failed, surface that as an exploration gap instead
  of speculating about hidden structure.
