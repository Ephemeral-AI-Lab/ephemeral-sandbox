# Explore Batching

Use `scripts/batch_phase_items.py` when the analyze phase returns more items than
the allowed concurrent worker budget.

## Purpose

- preserve original analyze ordering within the same priority tier
- move `critical` and `high` items earlier
- emit batches that never exceed the configured batch size

## Input

Pass a JSON array on stdin. Each item may be:

- a string path
- an object with at least `path`

Optional object fields:

- `priority`: `critical`, `high`, `normal`, or `low`

## Output

The script returns a JSON object with:

- `ordered_items`
- `ordered_paths`
- `batch_count`
- `batches`

Each entry in `batches` is ready to use as the `items` argument for one
`run_parallel_agents` call.

## Example

```bash
printf '%s\n' '[{"path":"src/api","priority":"high"},{"path":"docs","priority":"normal"},{"path":"src/auth","priority":"critical"}]' \
  | python scripts/batch_phase_items.py --max-batch-size 2
```
