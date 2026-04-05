# Runtime Tool Surface

Use the runtime tool-surface overlay for the exact helper names available in the
current run.

## Rules

- The live runtime overlay is authoritative over examples in `SKILL.md`.
- Use only helpers that are explicitly exposed in the current run.
- If a skill example mentions a helper that is not exposed, translate the intent
  onto the closest available helper instead of inventing names.
- Favor bounded exploration. One broad survey is enough; spend the remaining
  budget on narrower reads inside the owned region.

## Explorer-Style Mapping

- Structure survey intent: use the workspace-structure helper exposed in the run.
- Symbol discovery intent: use the symbol query helper exposed in the run.
- Concrete file read intent: use the sandbox file reader exposed in the run.
- Cross-reference tracing intent: use the symbol-reference helper exposed in the run.

## Failure Handling

- If a helper is unavailable or times out, record the gap and continue with the
  narrower evidence you already have.
- Do not leave the owned region just because a cross-reference points outward.
