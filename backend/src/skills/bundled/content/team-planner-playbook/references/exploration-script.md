# Exploration Script

Use this reference only on fresh benchmark roots or any turn that still lacks clear ownership.

## Task/Goal

- You are at a fresh root or still do not have a live-confirmed owner boundary.

## Avoid

- Never map a benchmark cluster to a production file solely because the names look similar.
- Never keep a guessed exact leaf once live evidence disproves it.
- Never open with root-wide CI queries or a broad first anchor when the prompt already points at a deeper production area.
- Never use more than one scope path as the first anchor or stack multiple first anchors before the wave.
- Never spend first-wave explorers on benchmark test files when a plausible production owner exists.
- Never spend a first-wave explorer entirely on benchmark test files; keep them literal in task prose or broaden to the last confirmed production package.
- Never repair an unresolved benchmark test cluster by pairing that test path with a nearby production file in one scout; keep tests evidence-only and scout only live production paths.
- Never guess missing production files from test names or name an exact production file absent from live CI or explorer notes.
- Never react to one missing guessed leaf by opening a new structure pass mid-wave; delete the leaf, keep the confirmed parent boundary, and wait for note review.
- Never use a later `ci_workspace_structure(...)` sibling listing as proof that a disproved leaf or tests-only directory now belongs to a nearby exact file; keep the last confirmed parent boundary broad until live symbol/import/note evidence says otherwise.
- Never bundle unrelated owner slices or the whole first-wave ledger into one explorer.
- Never start loading decomposition or plan-json references while the first explorer wave still has unlaunched exact-file slices.
- Never create a separate root task just to describe a benchmark mismatch; carry confirmed owners forward and drop disproved leaves.

## Workflow

1. Must keep the first-turn exploration script explicit and live-evidence-first.
2. Start with exactly one narrow `ci_workspace_structure(path=...)` on the deepest shared production boundary already implied by the prompt.
3. Use `ci_query_symbol(...)` or `ci_diagnostics(...)` only to refine likely owners from that anchor.
4. If the first anchor is empty, or `ci_status()` reports `initialized=false`, stop exact-file guessing immediately. This is a cold-CI branch.
5. On cold CI, keep unresolved work on stable directories/packages and let scouts confirm exact files. Failing benchmark tests remain evidence only.
6. If more than one owner slice is still unresolved after the first anchor, the next planning action must be a scout wave, not final DAG synthesis.
7. On repeated work, if one canonical owner is already exact and same-run reuse is empty, call `read_task_note(paths=[...])` before relaunching that scout.
8. If a guessed exact leaf is disproved, delete it and keep the last confirmed parent boundary until note review.
9. After the wave, `read_task_note(paths=[...])` with default scope; if `context_changed_since()` or a scope-change warning says the layer moved, refresh only stale slices.
10. Stop exploring once the current layer can name ready work plus residual boundaries.

## Expected Outcome

- The root or unresolved layer ends with live owner boundaries, one useful scout wave where needed, and no guessed exact owners carried forward.
