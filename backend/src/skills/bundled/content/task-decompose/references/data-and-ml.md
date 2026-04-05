# Data Pipeline and ML Workflow — Decomposition Reference

Use when the goal involves ETL pipelines, data processing, ML model training, feature engineering, or batch jobs.

---

## Standard Macro Graph Shapes

### ETL Pipeline
```
depth 0 (atomic):
  X0  scaffold + dependencies       [atomic] devops-engineer
      # install pandas, sqlalchemy, pydantic, dbt or prefect, .env

depth 1 (foundation):
  F1  pipeline-foundation           [expandable] backend-developer    deps: [X0]
      # DB connections, config, shared schemas, logging

depth 2 (pipeline stages — partially parallel):
  E1  extract-<source-A>            [expandable] backend-developer    deps: [F1]
  E2  extract-<source-B>            [expandable] backend-developer    deps: [F1]
  T1  transform-<domain>            [expandable] backend-developer    deps: [E1, E2]
  T2  transform-<domain>            [expandable] backend-developer    deps: [E1]
  L1  load-<destination>            [expandable] backend-developer    deps: [T1, T2]

depth 3 (validation + orchestration):
  V1  data-quality-checks           [expandable] test-engineer        deps: [L1]
  O1  pipeline-orchestration        [expandable] backend-developer    deps: [L1]
      # Prefect/Airflow DAG, scheduling, error handling, retries
```

### ML Training Pipeline
```
depth 0 (atomic):
  X0  environment-setup             [atomic] devops-engineer

depth 1 (foundation):
  F1  data-foundation               [expandable] backend-developer    deps: [X0]
      # data loading, train/val/test split, feature schemas

depth 2 (parallel tracks):
  P1  feature-engineering           [expandable] backend-developer    deps: [F1]
  P2  model-architecture            [expandable] backend-developer    deps: [F1]
  P3  training-infrastructure       [expandable] backend-developer    deps: [F1]
      # trainer loop, checkpointing, logging (wandb/mlflow)

depth 3 (integration):
  I1  training-pipeline             [expandable] backend-developer    deps: [P1, P2, P3]
  E1  evaluation-suite              [expandable] test-engineer        deps: [I1]
  S1  serving-endpoint              [expandable] backend-developer    deps: [I1]
```

---

## Expansion Hints by Layer

### Extract stage hint
```
"(1) source connection + authentication (env-based config); (2) schema validation for raw records (Pydantic model for source data); (3) extraction function with pagination/cursor handling; (4) incremental extraction logic (watermark or CDC); (5) unit tests with fixture data"
```

### Transform stage hint
```
"(1) input schema → output schema mapping; (2) parallel: one transformation function per business rule; (3) transformation pipeline that chains rules with error collection; (4) data quality assertions (nullability, range, referential integrity); (5) unit tests with golden input/output pairs"
```

### Feature engineering hint
```
"parallel: (1) one task per feature group (temporal features, categorical encodings, numerical normalization, embeddings); then (2) feature store writer that merges all feature groups; then (3) feature validation tests checking distribution and null rates"
```

### Model architecture hint
```
"(1) base model class with forward pass; (2) loss function + metrics; (3) parallel: one task per model variant or hyperparameter configuration class; (4) model factory / registry"
```

---

## Key Patterns

**Fan out on independent data sources.** Two extract stages reading from different sources can always run in parallel.

**Collapse transform steps that share state.** If transform B needs transform A's intermediate result, they are one task (or must be sequential).

**Separate schema from logic.** Data schemas (Pydantic models) belong in foundation. Transform functions belong in domain stages.

**Test with golden datasets.** Each transform stage should have an atomic test task that uses a small fixture dataset with known input/output. These test tasks can run in parallel once the transform tasks complete.

---

## Common Mistakes

**One giant "data processing" macro:** Split by source domain (orders, users, products) and by pipeline stage (extract, transform, load). Independent sources → parallel tasks.

**Training and evaluation in one task:** Training produces a model artifact; evaluation consumes it. Keep them separate — evaluation can be retried without re-training.

**Orchestration too early:** Wire Prefect/Airflow only after individual stages are validated. Orchestration is a depth-3 concern, not depth-1.
