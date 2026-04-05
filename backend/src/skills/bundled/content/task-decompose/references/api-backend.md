# API-Only Backend — Decomposition Reference

Use when the goal is a backend API with no frontend component (REST API, GraphQL, microservice, internal service).

---

## Standard Macro Graph Shape

```
depth 0 (atomic setup):
  X0  project-scaffold             [atomic] devops-engineer
      # virtualenv, pip install, alembic init, .env.example

depth 1 (foundation):
  F1  core-foundation              [expandable] backend-developer    deps: [X0]
      # shared types, DB session, base migration, app factory, config

depth 2 (domain spines — parallel):
  D1  <domain>-domain              [expandable] backend-developer    deps: [F1]
  D2  <domain>-domain              [expandable] backend-developer    deps: [F1]
  D3  <domain>-domain              [expandable] backend-developer    deps: [F1]
  Mx  <cross-cutting-concern>      [expandable] backend-developer    deps: [F1]
      # auth middleware, rate limiting, observability

depth 3 (integration + verification):
  I1  api-integration              [expandable] backend-developer    deps: [D1, D2, D3, Mx]
      # mount all routers, openapi docs, health endpoint
  V1  <domain>-tests               [expandable] test-engineer        deps: [D1]
  V2  <domain>-tests               [expandable] test-engineer        deps: [D2]
  V3  integration-tests            [expandable] test-engineer        deps: [I1]
```

---

## Expansion Hints by Layer

### Core foundation hint
```
"parallel: (1) domain type definitions + Pydantic v2 base schemas; (2) DB engine + session factory with connection pool; (3) app factory function + lifespan handler; then baseline Alembic migration depending on (2)"
```

### Single-domain API spine hint
```
"(1) ORM models + relationships + Alembic migration — one collapsed task; (2) repository layer: CRUD operations with pagination and filtering; (3) service layer: business logic, validation, error translation — depends on (2); (4) parallel: one task per logical endpoint group (each group = one router file); (5) mount domain router in app — depends on all endpoint tasks"
```

### Cross-cutting concern hints

**Auth middleware:**
```
"(1) JWT encode/decode utility + token schema; (2) FastAPI dependency: get_current_user + require_roles; (3) /auth/register + /auth/login + /auth/refresh endpoints; (4) wire auth dependency into protected routers"
```

**Observability:**
```
"parallel: (1) structured logging setup with request correlation ID middleware; (2) Prometheus metrics endpoint + request duration histogram; (3) /health and /ready endpoints; then one integration task to wire all three into app lifespan"
```

---

## Domain Spine Expansion (Mid-level)

When a sub-coordinator receives a domain spine macro, it should apply this pattern:

```
Wave 1 (parallel, no deps):     ORM model + migration
Wave 2 (depends on wave 1):     Repository (CRUD + query methods)
Wave 3 (depends on wave 2):     Service layer (business logic)
Wave 4 (parallel, depends on 3): One task per endpoint group
Wave 5 (depends on wave 4):     Router mount + app registration
```

Maximum chain depth: 5. If you need more, the domain is too large — split it into sub-domains.

---

## Microservice vs Monolith

**Monolith:** All domain spines share F1 foundation. Routes mount in one app factory.

**Microservice:** Each service is its own root-level decomposition. No shared foundation. Each service has its own X0 + F1 + domain spines + tests.

---

## Test Decomposition

### Per-domain test hint
```
"parallel: (1) unit tests for service layer (mock repo); (2) integration tests for repository (real DB via pytest-postgresql or sqlite); (3) API tests for each endpoint group (test client, real service + repo); then one coverage report task"
```

### Integration test hint
```
"one test file per user flow (not per endpoint): (1) create-read-update-delete flow; (2) auth + protected endpoint flow; (3) pagination + filtering flow; (4) error handling flow (invalid input, not found, forbidden)"
```
