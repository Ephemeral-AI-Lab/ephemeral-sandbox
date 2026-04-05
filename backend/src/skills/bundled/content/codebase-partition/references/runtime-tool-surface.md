# Runtime Tool Surface

Use the runtime tool-surface overlay for the exact helper names available in the
current run.

## Rules

- The live runtime overlay is authoritative over examples in `SKILL.md`.
- Use only helpers that are explicitly exposed in the current run.
- If a skill example mentions a helper that is not exposed, translate the intent
  onto the closest available helper instead of inventing names.
- Prefer one broad structure survey, then narrow quickly. Do not repeat the same
  repo-wide call after you already know the relevant directory prefixes.

## Explorer-Style Mapping

- Structure survey intent: use the workspace-structure helper exposed in the run.
- Symbol discovery intent: use the symbol query helper exposed in the run.
- Concrete file read intent: use the sandbox file reader exposed in the run.
- Cross-reference tracing intent: use the symbol-reference helper exposed in the run.
- Planning intent: only use the planning helper if the current run exposes it.

## Failure Handling

- If a helper call fails because the tool is unavailable, unsupported, or
  times out, narrow the scope rather than escalating to a larger repo sweep.
- Do not fabricate missing runtime capabilities in the output.
