"""Append-only prompt/message report helpers for live benchmark runs."""

from __future__ import annotations

import json
import os
import time
from pathlib import Path
from typing import Any, Mapping


def _json_default(value: Any) -> Any:
    model_dump = getattr(value, "model_dump", None)
    if callable(model_dump):
        try:
            return model_dump(mode="json")
        except TypeError:
            return model_dump()
    if isinstance(value, Path):
        return str(value)
    return str(value)


def append_prompt_report_event(
    path: str | Path | None,
    event: Mapping[str, Any],
) -> None:
    """Append one untruncated JSON event to a prompt report JSONL file."""
    if not path:
        return
    output_path = Path(path)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    payload = {"ts": time.time(), **dict(event)}
    data = json.dumps(payload, default=_json_default, ensure_ascii=False) + "\n"
    fd = os.open(output_path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
    try:
        os.write(fd, data.encode("utf-8"))
    finally:
        os.close(fd)
