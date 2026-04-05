# Synthesis Rubric

## Merging Region Reports

When multiple explorer reports cover overlapping areas:

1. **Deduplicate files**: If two reports mention the same file, merge their observations (keep the more detailed one)
2. **Unify symbol references**: Build a single symbol table from all reports — resolve conflicting descriptions by preferring the report that explored the symbol's defining file
3. **Preserve region attribution**: Note which region each observation came from for traceability

## Identifying Cross-Cutting Concerns

Look for patterns that appear in 2+ region reports:

- **Shared imports**: Modules imported by multiple regions (e.g. logging, config, auth middleware)
- **Common patterns**: Design patterns used across regions (factory, observer, repository)
- **Consistent error handling**: Shared exception types, error response formats
- **Configuration access**: How different regions read config values

## Shared Foundations

Identify modules that are foundational to the codebase:

- Modules imported by 3+ other modules
- Base classes or interfaces that multiple regions extend
- Utility modules (helpers, constants, types)
- Infrastructure code (database, caching, messaging)

## Risk Hotspots

Flag areas that pose risk for the planning phase:

- **High complexity**: Files with many dependencies or deep nesting (reported by explorers)
- **Multiple owners**: Files touched by multiple regions — likely merge conflict zones
- **Missing tests**: Core modules with no corresponding test files
- **Stale patterns**: Legacy code with different patterns from the rest of the codebase

## Confidence Scoring Rubric

| Explorer Results | Score Range | Notes |
|---|---|---|
| All succeeded, rich reports | 0.8 - 1.0 | Full coverage |
| Most succeeded, some sparse | 0.6 - 0.8 | Minor gaps acceptable |
| Mixed success/failure | 0.4 - 0.6 | Significant uncertainty |
| Most failed | 0.1 - 0.3 | Map is speculative |
| All failed | 0.0 | Empty map, recommend fallback |

## Example Output Structure

```json
{
  "codebase_map": {
    "modules": [
      {"path": "src/auth", "purpose": "Authentication and authorization", "key_files": ["jwt.py", "middleware.py"]},
      {"path": "src/api", "purpose": "REST API endpoints", "key_files": ["routes.py", "schemas.py"]}
    ],
    "cross_cutting_concerns": [
      "Logging via structlog in all modules",
      "JWT auth middleware applied to all API routes"
    ],
    "shared_foundations": ["src/core/config.py", "src/core/db.py", "src/core/exceptions.py"],
    "risk_hotspots": ["src/api/routes.py (high complexity, 500+ lines)", "src/auth/jwt.py (no tests)"],
    "coverage_gaps": ["src/migrations/ (not explored)", "src/scripts/ (not explored)"]
  },
  "confidence_score": 0.85,
  "report_count": 4,
  "success_count": 4,
  "failed_count": 0
}
```
