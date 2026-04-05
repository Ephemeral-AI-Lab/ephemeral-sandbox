# Fullstack Web Application — Decomposition Reference

Use when the goal involves both a backend API and a frontend UI (React + FastAPI, Next.js + Django, etc.).

---

## Standard Macro Graph Shape

```
depth 0 (optional atomic):
  X0  scaffold + install deps      [atomic] devops-engineer
      # create directory structure, install npm + pip packages, write .env.example

depth 1 (foundation — shared contracts):
  F1  shared-foundation            [expandable] fullstack-developer
      deps: [X0]
      hint: "parallel: (1) Python Pydantic domain schemas, (2) TypeScript interfaces mirroring schemas, (3) DB session factory + base migration; all three can run concurrently"

depth 2 (domain spines — run in parallel):
  B1  <domain>-api-spine           [expandable] backend-developer    deps: [F1]
  B2  <domain>-api-spine           [expandable] backend-developer    deps: [F1]
  U1  <domain>-ui-spine            [expandable] frontend-developer   deps: [F1]
  U2  <domain>-ui-spine            [expandable] frontend-developer   deps: [F1]
  A1  auth-wiring                  [expandable] fullstack-developer  deps: [F1]

depth 3 (integration + verification — only what actually needs everything):
  X1  api-client-wiring            [expandable] fullstack-developer  deps: [B1, B2, U1, U2]
  V1  backend-tests                [expandable] test-engineer        deps: [B1, B2]
      hint: "parallel subsets by domain: (1) auth tests; (2) catalog/product tests; (3) cart/order tests; each subset is one atomic task"
  V2  frontend-tests               [expandable] test-engineer        deps: [U1, U2]
      hint: "parallel subsets by page: (1) auth page tests; (2) catalog page tests; (3) cart/checkout tests; each subset is one atomic task"
  V3  e2e-tests                    [expandable] test-engineer        deps: [X1]
```

---

## Expansion Hints by Layer

### Foundation macro hint
```
"parallel wave: (1) Python Pydantic schemas for all shared domain types; (2) TypeScript interfaces that mirror the Python schemas; (3) DB session factory with connection pooling; then one task for baseline Alembic migration depending on DB session factory"
```

### Backend API spine hint (per domain)
```
"(1) ORM model + Alembic migration — collapsed, one task; (2) repository + service layer — collapsed, service is thin orchestration; (3) parallel: one task per endpoint group (list, detail, create, update, delete); (4) router wiring + registration in main app — depends on all endpoints"
```

### Frontend UI spine hint (per domain)
```
"parallel wave: (1) typed API client function for this domain; (2) primary data hook (useXxx) managing fetch + state; (3) leaf display components (Card, Row, Badge) with no deps on each other; then container Page component depending on hook + components; then route registration depending on Page"
```

### Auth wiring hint
```
"(1) backend: JWT token generation + validation middleware; (2) backend: /auth/login and /auth/me endpoints; (3) frontend: AuthContext provider with login/logout/session; (4) frontend: ProtectedRoute wrapper; (5) wire ProtectedRoute into app router — depends on (3) and (4)"
```

### API client wiring hint
```
"parallel: one task per domain to configure typed API base URL and inject auth headers; then one task to wire global error boundary and loading states; then smoke test task hitting each endpoint"
```

---

## Common Mistakes

**Combining auth into foundation:**
Auth is its own failure domain. Foundation should only contain shared types + DB session. Auth wiring that fails should not block catalog or cart.

**One UI spine for all domains:**
Separate domains = separate spines. Cart UI and Catalog UI do not share components. Combining them creates a blast radius of "all frontend".

**Integration macro depending on everything:**
`deps: [B1, B2, U1, U2, A1]` for an API client wiring task that only touches B1 and U1 is wasteful. Depend only on what you need.

**Atomic test tasks that span multiple domains:**
A single "write all backend tests" atomic task risks blowing the worker tool budget when the test surface covers auth + products + cart + orders. Always make cross-domain verification tasks expandable and split by domain subset so each atomic subtask stays comfortably within the 100-call Agno ceiling.

---

## Sizing Guide

| App size | Backend spines | Frontend spines | Total macros |
|---|---|---|---|
| Small (1-2 domains) | 1–2 | 1–2 | 5–7 |
| Medium (3-4 domains) | 3–4 | 3–4 | 8–12 |
| Large (5+ domains) | 5–8 | 5–8 | 12–18 |

For large apps, split into phases: Phase 1 = foundation + core domains, Phase 2 = secondary domains + integration.
