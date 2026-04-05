# Infrastructure and DevOps — Decomposition Reference

Use when the goal involves environment setup, Docker containerization, CI/CD pipelines, cloud infrastructure, or operational tooling.

---

## Standard Macro Graph Shapes

### Application Containerization
```
depth 0 (atomic, run first):
  X0  audit-current-setup           [atomic] devops-engineer
      # read existing package files, identify runtime deps, note Python/Node versions

depth 1 (parallel service Dockerfiles):
  D1  backend-dockerfile            [expandable] devops-engineer      deps: [X0]
  D2  frontend-dockerfile           [expandable] devops-engineer      deps: [X0]
  D3  worker-dockerfile             [expandable] devops-engineer      deps: [X0]

depth 2 (compose + wiring):
  C1  docker-compose-dev            [expandable] devops-engineer      deps: [D1, D2, D3]
  C2  docker-compose-prod           [expandable] devops-engineer      deps: [D1, D2, D3]

depth 3 (validation):
  V1  container-smoke-tests         [expandable] test-engineer        deps: [C1]
```

### CI/CD Pipeline
```
depth 0 (atomic):
  X0  repository-setup              [atomic] devops-engineer
      # .gitignore, branch protection rules, secrets config

depth 1 (parallel pipeline stages):
  P1  lint-and-typecheck-job        [expandable] devops-engineer      deps: [X0]
  P2  unit-test-job                 [expandable] devops-engineer      deps: [X0]
  P3  integration-test-job          [expandable] devops-engineer      deps: [X0]
  P4  build-and-push-job            [expandable] devops-engineer      deps: [X0]

depth 2 (deployment):
  R1  staging-deploy-job            [expandable] devops-engineer      deps: [P1, P2, P3, P4]
  R2  production-deploy-job         [expandable] devops-engineer      deps: [R1]
```

### Cloud Infrastructure (IaC)
```
depth 1 (foundation):
  F1  networking-foundation         [expandable] devops-engineer
      # VPC, subnets, security groups, IAM roles

depth 2 (parallel service infra):
  I1  database-infra                [expandable] devops-engineer      deps: [F1]
  I2  compute-infra                 [expandable] devops-engineer      deps: [F1]
  I3  storage-infra                 [expandable] devops-engineer      deps: [F1]
  I4  cdn-and-dns                   [expandable] devops-engineer      deps: [F1]

depth 3 (application layer):
  A1  app-deployment                [expandable] devops-engineer      deps: [I1, I2, I3]
  A2  monitoring-and-alerts         [expandable] devops-engineer      deps: [I2]
```

---

## Expansion Hints by Layer

### Dockerfile hint (backend Python)
```
"(1) base image selection + Python version pin; (2) dependency layer: COPY requirements.txt → pip install (cached layer); (3) app layer: COPY src + set WORKDIR + ENV; (4) non-root user setup; (5) health check instruction + CMD/ENTRYPOINT; use multi-stage build: builder stage installs dev deps, production stage copies only runtime artifacts"
```

### Dockerfile hint (frontend Node)
```
"multi-stage: (1) builder stage: node:lts-alpine, COPY package*.json, npm ci, COPY src, npm run build; (2) production stage: nginx:alpine, COPY --from=builder dist/ to nginx html dir; (3) custom nginx.conf for SPA routing (try_files $uri /index.html); (4) EXPOSE 80, health check"
```

### CI job hint (GitHub Actions)
```
"(1) trigger conditions (push to main, PR to main); (2) job matrix if multi-platform; (3) cache action for pip/npm deps using hash of lockfile; (4) job steps: checkout → cache restore → install → run command → cache save; (5) artifact upload for build outputs or test reports"
```

### Docker Compose hint
```
"(1) service definitions: one block per container (app, db, redis, worker); (2) network definition with named bridge network; (3) volume definitions for persistent data (postgres-data, redis-data); (4) environment variable injection via .env file; (5) healthcheck + depends_on with condition: service_healthy for startup ordering; (6) resource limits for local dev"
```

---

## Key Patterns

**Atomic setup tasks are fine at root level.** Creating .gitignore, writing .env.example, or installing base deps does not need a sub-coordinator. Make it atomic and dispatch to devops-engineer.

**Parallel Dockerfiles.** Backend and frontend Dockerfiles are fully independent. Always parallel.

**Cache layer order matters.** In Dockerfiles, always COPY dependency manifests (requirements.txt, package.json) before COPY source code. This ensures the expensive install layer is cached on source-only changes.

**Infrastructure before application.** Networking + IAM must exist before compute or storage resources. Foundation blast radius here can be large — keep it minimal.

**Never hardcode secrets.** All secret injection happens via environment variables from .env files or secret managers. Write .env.example with placeholder values; never commit .env.

---

## Common Mistakes

**One task for "set up Docker":** Split into per-service Dockerfiles + compose. Each Dockerfile is an independent deliverable. Parallel.

**CI pipeline as one expandable:** Each CI job stage (lint, test, build, deploy) is an independent task with its own failure domain. Split into parallel jobs with explicit sequential deps only at deployment gates.

**Infrastructure and application config together:** VPC/networking/IAM is depth-1 foundation. Application-level config (env vars, secrets, feature flags) is depth-2. Keep them in separate tasks.
