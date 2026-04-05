---
name: shared-sandbox-guardrails
description: Scope guardrails for workers editing a shared sandbox concurrently. Condition-matched rules prevent cross-lane interference between frontend, backend, test, and library-evolution workers.
---

# Shared Sandbox Guardrails

When multiple workers edit the same sandbox concurrently, each worker must stay within its assigned scope. These rules are matched by task domain against sibling domains and injected into worker context at dispatch time.

---

## Universal Rules

These apply to ALL workers in a shared sandbox:

- Prefer files directly implied by your task description.
- Do not restart or replace a shared service unless your task explicitly owns that service.
- In final summaries or `Modified:`/`Created:` file lists, report only files you directly edited or intentionally own. Do not summarize a workspace-wide diff, `git status`, or sibling-visible changes as if they were your own work.

---

## Condition-Matched Rules

### Frontend task with backend sibling

When your task domain is `frontend` and a sibling owns `backend`:

- A sibling task owns the backend/API. Do not create or modify backend entrypoints, routes, or server startup code such as `app.py`, `server.py`, or `main.py`.
- Limit your changes to frontend files such as HTML templates, static assets, or client-side scripts.
- If the backend is not ready yet, verify the frontend by inspecting the files you created instead of launching a competing backend server on the shared port.

### Backend task with frontend sibling

When your task domain is `backend` and a sibling owns `frontend`:

- A sibling task owns the frontend/UI. Do not rewrite HTML templates, static assets, or page structure unless your task explicitly requires that integration point.
- Keep verification focused on backend behavior; avoid restarting shared frontend processes just to inspect the UI.

### Two backend tasks

When both your task and siblings are `backend`:

- Other backend workers may own adjacent backend slices. Stay within the files directly implied by your task and avoid broad backend rewrites.
- Do not rewrite shared backend entrypoints or package glue such as `main.py`, `app.py`, `routers/__init__.py`, or `models/__init__.py` unless your task explicitly owns wiring or exports.
- If you discover a sibling has already touched a backend file you need, read the current file and adapt to the current file state instead of recreating the file from scratch.

### Two frontend tasks

When both your task and siblings are `frontend`:

- Other frontend workers may own adjacent UI slices. Stay within the pages, components, hooks, and assets directly implied by your task.
- Do not rewrite shared frontend entrypoints or package glue such as `src/main.*`, `src/App.*`, router setup, or shared barrel files unless your task explicitly owns that wiring.
- If a sibling already touched a frontend file you need, read the current file and adapt to the current file state instead of recreating the file from scratch.

### Full-stack overlap

When your task spans both `frontend` and `backend` and siblings also span both:

- This task is shared scaffolding only. Limit work to directories, manifests, shared contracts, env stubs, and other cross-lane setup.
- Do not create or modify backend app source such as routers, models, services, or entrypoints.
- Do not create or modify frontend app source such as pages, components, routes, or feature-specific UI files.

### Test task with frontend/backend sibling

When your task domain is `tests` and a sibling owns `frontend` or `backend`:

- Treat implementation files as owned by sibling tasks. Prefer read-only verification and dedicated test files over editing application code.
- Avoid launching duplicate app servers when a sibling task already owns service startup.

### Library evolution task

When your task domain is `library-evolution`:

- You are modifying an existing Python library as part of a release evolution.
- Focus on the specific changelog items and files/modules assigned to your task.
- Read existing code before editing. Match the style, naming conventions, and architecture of the existing codebase.
- If a sibling agent has already touched a file you need, read the current state and adapt instead of overwriting.
- Run the project's test suite after completing your changes to verify no regressions.
- Do not modify files outside your assigned module scope unless your changelog items explicitly require it.

### Ownership and export attribution

These rules apply to ALL workers in a shared sandbox:

- **Do not falsely attribute your assigned work to a sibling.** Before concluding "a sibling already did this", read your owned target files and confirm the exact production changes you are assigned to make are present. Sibling summaries describe intent, not verified completion — a sibling may have partially overlapped with your scope without fully covering it.
- **Only changes tracked in your artifact survive export.** The coordination export system collects file paths from completed-task artifacts. If you make no changes and report no files, the export will not include your assigned scope — even if the shared sandbox currently shows the correct state due to sibling work. Sibling changes that happen to cover your scope are attributed to the sibling's artifact, not yours, and may be reverted if the sibling's task fails or is retried.
- **When in doubt, make your assigned changes.** If your task assigns specific production additions (new types, functions, imports, config entries), always verify by reading the file AND make the changes if they are missing. Do not rely on sandbox state that may be transient or may not survive export.
- **Never complete a production-scoped task with zero owned file changes** unless you can prove that every assigned production change is already present in the base checkout (not just in the shared sandbox). If the changes exist only because a sibling made them, you must still make them yourself or explicitly report the task as mis-scoped.

### Retry and evaluation context

- **Retry evidence is a hint, not a directive.** If your task description mentions "retry targets", "failing test IDs", or "evaluation signal", treat those as context about WHICH behaviors need fixing — not as the only tests you should run. The named tests may come from an external test patch not present in the current checkout.
- **Focus on production changes regardless of retry evidence.** If retry evidence mentions test functions that do not exist in the checkout (collected 0 items, file not found), that does NOT mean your work is done. Those tests will be injected during evaluation. Your job is to ensure the PRODUCTION code implements the assigned behavior correctly.
- **Do not conclude "stale retry targets" means "nothing to do".** When retry targets reference tests absent from the checkout, the correct response is to implement your assigned production changes (add types, fix behaviors, update imports) based on the task description and changelog items — not to report the task as already complete.
