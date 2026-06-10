# Agent Collaboration and Implementation Notes

This codebase is edited across multiple agent sessions at the same time. A dirty
worktree is usually expected and should be treated as parallel agent activity,
not as a reason to stop.

## Parallel Agent Work

- Do not revert, overwrite, or discard another agent's work unless the user
  explicitly asks for that.
- If existing changes are outside the current plan, infer the likely intent from
  file names, diffs, tests, and surrounding code, then adjust your own plan
  around that work instead of blocking. Ask only when ambiguity makes safe
  progress impossible.
- Keep your edits scoped to your task, but integrate with concurrent changes
  when needed for correctness.
- Feel free to launch dynamic workflows and subagents for parallel execution,
  exploration, and redundancy checks when the scopes can stay disjoint; reconcile
  their findings before acting on or reporting results.
- If tests fail because of another agent's in-progress work, it is acceptable to
  help fix those failures when the fix is clear and compatible with your task;
  then continue your own work.
- Before committing or staging, distinguish your intended changes from unrelated
  concurrent work unless the user explicitly asked to include everything.

## Before Coding

- State material assumptions before acting when the task or ownership boundary is
  ambiguous.
- If a request has multiple plausible interpretations, name the options and pick
  the smallest safe interpretation, or ask when guessing would risk the user's
  work.
- Push back on unnecessary complexity. Prefer the direct implementation that
  solves the stated problem.

## Implementation Style

- Make the touched file's final implementation as small as the request allows.
  Prefer net-negative changes when existing code can be simplified or deleted,
  and use aggressive, transformative rewrites when they improve extensibility or
  implementation feasibility. Do not add speculative features, configuration,
  extension points, or abstractions.
- Treat LOC guidance as a final-code standard, not a net-diff target: if a file
  implements something in 200 lines that can be expressed clearly in 50, rewrite
  it toward the smaller shape.
- Treat 800-1000+ LOC implementation files as a review smell: split when the
  file mixes multiple concepts, lifecycle phases, backends, DTOs, tests, or
  helper layers. Very large files are acceptable only when mechanically
  cohesive, such as generated code, big enum/table definitions, or tightly
  coupled parser/state-machine code.
- Do not enforce a hard file-size cap in this repo. The better standard is final
  files as small as the request allows, with splits following real ownership
  boundaries rather than arbitrary LOC.
- If the solution is growing large and a smaller design would solve the same
  problem, simplify before continuing.
- Prefer typed IDs, enums, DTOs, explicit state transitions, and explicit
  dependency edges over stringly or ad hoc compatibility shims.
- Design public APIs as narrow owned contracts. Keep internal details private
  until another package or module genuinely needs them, and document public
  invariants where the project enables documentation linting.
- Put dependency injection at real resource, provider, plugin, or cross-package
  boundaries. Prefer concrete types inside a module; introduce substitution
  seams only when they are load-bearing for tests, alternate backends, or runtime
  selection.
- Avoid defensive branches for impossible states unless the surrounding codebase
  already requires that style.
- Match the existing code's style and ownership boundaries even when you would
  design greenfield code differently.

## Surgical Scope

- Touch only the files and lines needed for the user's request.
- Do not opportunistically refactor adjacent code, reformat unrelated files, or
  delete pre-existing dead code.
- Clean up imports, variables, functions, and files that your own changes made
  unused, but leave unrelated cleanup as a note unless asked.
- Every changed line should have a clear reason tied to the task, a test fix, or
  compatibility with parallel work.

## Demonstration Guidance

- When explaining or demonstrating architecture, workflows, contracts, or
  migration plans, prefer a workflow diagram plus a comparison or grouping table
  over pure prose. Use prose to call out key caveats and evidence, not to replace
  the structured view.

## Verification

- Convert the request into concrete success criteria before or while
  implementing.
- For bugs, prefer a failing test or focused reproduction before the fix when
  practical.
- For refactors, preserve behavior and run the narrowest convincing checks before
  and after risky changes when practical.
- Use the narrowest convincing verification from the owning workspace or package
  first. Broaden checks only when the change crosses package, runtime, or
  dependency boundaries.
- Report pre-existing verification noise instead of hiding it with broad lint,
  test, or type-check suppressions.
- For multi-step tasks, keep a short plan with a verification step for each
  meaningful phase, then iterate until the criteria are met.
