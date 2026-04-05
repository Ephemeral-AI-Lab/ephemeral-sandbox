#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import sys


PRIORITY_ORDER = {
    "critical": 0,
    "high": 1,
    "normal": 2,
    "low": 3,
}


def _normalize_item(item: object) -> dict[str, object]:
    if isinstance(item, str):
        return {"path": item, "priority": "normal"}
    if isinstance(item, dict):
        path = item.get("path")
        if not isinstance(path, str) or not path.strip():
            raise ValueError("Each object item must include a non-empty string 'path'.")
        priority = item.get("priority", "normal")
        if not isinstance(priority, str):
            priority = "normal"
        normalized = dict(item)
        normalized["path"] = path
        normalized["priority"] = priority
        return normalized
    raise ValueError("Each item must be either a string path or an object with a 'path'.")


def _priority_rank(priority: object) -> int:
    if not isinstance(priority, str):
        return PRIORITY_ORDER["normal"]
    return PRIORITY_ORDER.get(priority.strip().lower(), PRIORITY_ORDER["normal"])


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--max-batch-size", type=int, required=True)
    args = parser.parse_args()

    if args.max_batch_size <= 0:
        raise SystemExit("--max-batch-size must be > 0")

    raw = sys.stdin.read()
    payload = json.loads(raw or "[]")
    if not isinstance(payload, list):
        raise SystemExit("Expected a JSON array on stdin.")

    normalized = [_normalize_item(item) for item in payload]
    ordered_items = sorted(
        normalized,
        key=lambda item: (_priority_rank(item.get("priority")), normalized.index(item)),
    )
    ordered_paths = [str(item["path"]) for item in ordered_items]
    batches = [
        ordered_paths[i : i + args.max_batch_size]
        for i in range(0, len(ordered_paths), args.max_batch_size)
    ]

    print(
        json.dumps(
            {
                "ordered_items": ordered_items,
                "ordered_paths": ordered_paths,
                "batch_count": len(batches),
                "batches": batches,
            }
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
