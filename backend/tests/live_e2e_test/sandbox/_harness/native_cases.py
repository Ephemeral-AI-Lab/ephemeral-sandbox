"""Shared host-side helpers for native sandbox runtime probes."""

from __future__ import annotations

import json
from collections.abc import Mapping
from typing import Any

from .native_probe import render, shell_command
from .sandbox_fixture import SandboxHandle


NATIVE_CASE_PRELUDE = r"""
import concurrent.futures
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import threading
import time
import uuid
from pathlib import Path


def _case_root(label):
    safe = "".join(ch if ch.isalnum() or ch in ("-", "_") else "-" for ch in label)
    root = Path("/tmp/eos-sandbox-runtime/layer-stack-test-%s" % os.getpid()) / safe
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True, exist_ok=True)
    return root


def _source(root, name, content):
    path = Path(root) / "sources" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(content, str):
        content = content.encode("utf-8")
    path.write_bytes(content)
    return path


def _sha(content):
    if isinstance(content, str):
        content = content.encode("utf-8")
    return hashlib.sha256(content).hexdigest()


def _status(value):
    return getattr(value, "value", str(value))


def _percentile(values, percentile):
    if not values:
        return 0.0
    ordered = sorted(values)
    k = (len(ordered) - 1) * (percentile / 100.0)
    lo = int(k)
    hi = min(lo + 1, len(ordered) - 1)
    return ordered[lo] + (ordered[hi] - ordered[lo]) * (k - lo)


def _emit(label, started_at, before, details):
    after = sample_resource()
    payload = {
        "label": label,
        "duration_ms": (time.perf_counter() - started_at) * 1000.0,
        "resource_before": before,
        "resource_after": after,
    }
    payload.update(details)
    print(json.dumps(payload, separators=(",", ":"), sort_keys=True))
"""


async def run_native_case(
    native_sandbox: SandboxHandle,
    body: str,
    *,
    label: str,
    cfg: Mapping[str, Any] | None = None,
    timeout: float = 90,
    fd_delta_max: int = 2,
) -> dict[str, Any]:
    """Run a rendered native probe and parse its trailing JSON payload."""
    cmd = shell_command(render(NATIVE_CASE_PRELUDE + body, cfg=dict(cfg or {})))
    result = await native_sandbox.raw_exec(
        native_sandbox.sandbox_id,
        cmd,
        timeout=timeout,
    )
    assert result.exit_code == 0, (
        f"{label} native probe failed (rc={result.exit_code}): "
        f"stderr={result.stderr!r} stdout={result.stdout!r}"
    )

    payload = json.loads(result.stdout.strip().splitlines()[-1])
    print(f"\n[{label}] {json.dumps(payload, separators=(',', ':'), sort_keys=True)}")
    _assert_resource_block(payload, "resource_before")
    _assert_resource_block(payload, "resource_after")
    _assert_resource_not_leaked(payload, fd_delta_max=fd_delta_max)
    return payload


def _assert_resource_block(payload: Mapping[str, Any], key: str) -> None:
    block = payload[key]
    assert isinstance(block, dict), (key, block)
    for metric in (
        "fd_open",
        "rss_kb",
        "rss_peak_kb",
        "threads",
        "mounts",
        "overlay_mounts",
        "wall_ms",
        "cpu_user_ms",
        "cpu_sys_ms",
    ):
        assert metric in block, (key, metric, block)


def _assert_resource_not_leaked(
    payload: Mapping[str, Any],
    *,
    fd_delta_max: int,
) -> None:
    before = payload["resource_before"]
    after = payload["resource_after"]
    assert after["mounts"] == before["mounts"], payload
    assert after["overlay_mounts"] == before["overlay_mounts"], payload
    assert after["fd_open"] <= before["fd_open"] + fd_delta_max, payload


__all__ = ["NATIVE_CASE_PRELUDE", "run_native_case"]
