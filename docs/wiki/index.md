# Wiki Index

> 10 pages | Last updated: 2026-05-13T00:00:00.000Z

## architecture

- [Context Engine Recipes](context-engine-recipes.md) — Per-role recipe mechanics, scope, block kinds, priority, renderer.
- [Engine + Query Loop + LLM Seam](engine-query-loop-llm-seam.md) — Agent run internals, query loop, provider seams.
- [Role Context Example: E-commerce Fullstack Build](role-context-ecommerce-example.md) — Concrete planner/generator/evaluator context packets for a fullstack checkout task.
- [Role Context Next Phase Report](role-context-next-phase-report.md) — Follow-up plan for harness gates, summary provenance, and evaluator context bounds.
- [Role: Planner](role-planner.md) — Rubric author; designs the DAG and evaluation criteria for one attempt.
- [Role: Generator (Executor + Verifier)](role-generator.md) — DAG-node worker; two profiles share one recipe but expose different terminals.
- [Role: Evaluator](role-evaluator.md) — Singleton attempt-level judge; binary verdict against the planner's criteria.
- [Sandbox Subsystem](sandbox-subsystem.md) — Daytona-backed ephemeral sandboxes, overlay runtime, lifecycle.
- [Task Center Pipeline](task-center-pipeline.md) — Mission/Episode/Attempt state machine; submission tools and lifecycle.
- [Tools, Hooks, Guardrails, Agents, Notifications, Messages](tools-hooks-guardrails-agents-notifications-messages.md) — Tool envelope, pre/post hooks, agent definitions, message log.
