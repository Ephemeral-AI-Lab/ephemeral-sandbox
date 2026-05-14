"""Native in-sandbox probe renderer for phase-01 workspace-base live tests."""

from __future__ import annotations

import json
from collections.abc import Mapping
from typing import Any

from .native_cases import NATIVE_CASE_PRELUDE
from .native_probe import render, shell_command
from .sandbox_fixture import SandboxHandle


WORKSPACE_BASE_PROBE_PRELUDE = (
    NATIVE_CASE_PRELUDE
    + r"""
from sandbox.layer_stack.layer_change import LayerChange
from sandbox.layer_stack.manifest import manifest_path, read_manifest
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.layer_stack.workspace_binding import workspace_binding_path
from sandbox.layer_stack.workspace_base import build_workspace_base

WORKSPACE_ROOT = Path("/testbed")
PHASE01_ROOT = Path("/tmp/eos-sandbox-runtime/layer-stack-phase01-native")


def _phase01_root(label, suffix=""):
    safe_label = "".join(ch if ch.isalnum() or ch in ("-", "_") else "-" for ch in label)
    safe_suffix = "".join(ch if ch.isalnum() or ch in ("-", "_") else "-" for ch in str(suffix))
    root = PHASE01_ROOT / ("%s%s" % (safe_label, ("-" + safe_suffix) if safe_suffix else ""))
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True, exist_ok=True)
    return root


def _reset_stack(stack_root):
    stack_root = Path(stack_root)
    shutil.rmtree(stack_root, ignore_errors=True)
    stack_root.mkdir(parents=True, exist_ok=True)
    return stack_root


def _build_base(stack_root, workspace_root=WORKSPACE_ROOT):
    timings = {}
    started = time.perf_counter()
    binding = build_workspace_base(
        workspace_root=workspace_root,
        layer_stack_root=stack_root,
        timings=timings,
    )
    timings.setdefault("api.workspace_base.total_s", time.perf_counter() - started)
    return binding.to_dict(), timings


def _inventory(root):
    root = Path(root)
    files = 0
    dirs = 0
    symlinks = 0
    total_bytes = 0
    sample_hashes = {}
    symlink_targets = {}
    empty_dirs = []
    for current_root, dirnames, filenames in os.walk(root, topdown=True, followlinks=False):
        current = Path(current_root)
        dirnames.sort()
        filenames.sort()
        kept_dirs = []
        child_count = len(filenames)
        for dirname in dirnames:
            path = current / dirname
            child_count += 1
            rel = path.relative_to(root).as_posix()
            if path.is_symlink():
                symlinks += 1
                symlink_targets[rel] = os.readlink(path)
                continue
            dirs += 1
            kept_dirs.append(dirname)
        dirnames[:] = kept_dirs
        if current != root and child_count == 0:
            empty_dirs.append(current.relative_to(root).as_posix())

        for filename in filenames:
            path = current / filename
            rel = path.relative_to(root).as_posix()
            if path.is_symlink():
                symlinks += 1
                symlink_targets[rel] = os.readlink(path)
                continue
            if not path.is_file():
                continue
            files += 1
            total_bytes += path.stat().st_size
            if len(sample_hashes) < 32:
                sample_hashes[rel] = _file_sha(path)
    return {
        "files": files,
        "dirs": dirs,
        "symlinks": symlinks,
        "bytes": total_bytes,
        "sample_hashes": sample_hashes,
        "symlink_targets": symlink_targets,
        "empty_dirs": empty_dirs,
        "repo_commit": _repo_commit(root),
    }


def _repo_commit(root):
    try:
        return subprocess.check_output(
            ["git", "-C", str(root), "rev-parse", "HEAD"],
            stderr=subprocess.DEVNULL,
            text=True,
        ).strip()
    except Exception:
        return ""


def _file_sha(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _tree_digest(root):
    inv = _inventory(root)
    digest = hashlib.sha256()
    for path, value in sorted(inv["sample_hashes"].items()):
        digest.update(path.encode("utf-8"))
        digest.update(b"\0")
        digest.update(value.encode("ascii"))
        digest.update(b"\0")
    for path, target in sorted(inv["symlink_targets"].items()):
        digest.update(path.encode("utf-8"))
        digest.update(b"\0symlink\0")
        digest.update(target.encode("utf-8"))
        digest.update(b"\0")
    digest.update(str(inv["files"]).encode("ascii"))
    digest.update(str(inv["dirs"]).encode("ascii"))
    digest.update(str(inv["symlinks"]).encode("ascii"))
    digest.update(str(inv["bytes"]).encode("ascii"))
    return digest.hexdigest(), inv


def _materialize_digest(manager, destination, manifest=None):
    destination = Path(destination)
    materialize_start = time.perf_counter()
    manager.materialize(destination, manifest)
    elapsed = time.perf_counter() - materialize_start
    digest, inventory = _tree_digest(destination)
    return digest, inventory, elapsed


def _publish_changes(manager, changes):
    timings = {}
    started = time.perf_counter()
    with manager.commit_transaction() as transaction:
        manifest = transaction.publish_layer(changes, timings=timings)
        timings["layer_stack.publish.lock_wait_s"] = transaction.lock_wait_s
        timings["layer_stack.publish.lock_held_s"] = transaction.lock_held_s
    timings.setdefault("layer_stack.publish.total_s", time.perf_counter() - started)
    return manifest, timings


def _storage_bytes(root):
    total = 0
    for entry in Path(root).rglob("*"):
        if entry.is_file() or entry.is_symlink():
            total += entry.lstat().st_size
    return total


def _seed_workspace_files(prefix, *, files=0, deletes=0, overwrites=0):
    root = WORKSPACE_ROOT / prefix
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True, exist_ok=True)
    for index in range(files):
        (root / "files" / ("%03d.txt" % index)).parent.mkdir(parents=True, exist_ok=True)
        (root / "files" / ("%03d.txt" % index)).write_text(
            "base-file-%03d\n" % index,
            encoding="utf-8",
        )
    for index in range(deletes):
        (root / "deletes" / ("%03d.txt" % index)).parent.mkdir(parents=True, exist_ok=True)
        (root / "deletes" / ("%03d.txt" % index)).write_text(
            "delete-me-%03d\n" % index,
            encoding="utf-8",
        )
    for index in range(overwrites):
        (root / "overwrites" / ("%03d.txt" % index)).parent.mkdir(parents=True, exist_ok=True)
        (root / "overwrites" / ("%03d.txt" % index)).write_text(
            "overwrite-base-%03d\n" % index,
            encoding="utf-8",
        )
    return root


def _call_row(case, label, success, started_at, timings=None, extra=None):
    row = {
        "schema": "sandbox.live_e2e.phase01_workspace_base.v1",
        "kind": "call",
        "case": case,
        "label": label,
        "success": bool(success),
        "wall_ms": (time.perf_counter() - started_at) * 1000.0,
        "runtime_ms": 0.0,
        "timings": dict(timings or {}),
        "resource": {},
    }
    if extra:
        row.update(extra)
    return row


def _base_summary(case, binding, inventory, timings=None, pass_bars=None):
    return {
        "schema": "sandbox.live_e2e.phase01_workspace_base.v1",
        "kind": "summary",
        "case": case,
        "workspace_root": binding["workspace_root"],
        "layer_stack_root": binding["layer_stack_root"],
        "base_manifest_version": binding["base_manifest_version"],
        "base_root_hash": binding["base_root_hash"],
        "active_manifest_version": binding["active_manifest_version"],
        "active_root_hash": binding["active_root_hash"],
        "repo_commit": inventory.get("repo_commit", ""),
        "workspace_inventory": {
            "files": inventory["files"],
            "dirs": inventory["dirs"],
            "symlinks": inventory["symlinks"],
            "bytes": inventory["bytes"],
            "sample_hashes": inventory.get("sample_hashes", {}),
        },
        "timings": dict(timings or {}),
        "timings_ms": {
            key: value * 1000.0
            for key, value in dict(timings or {}).items()
            if key.endswith("_s")
        },
        "pass_bars": dict(pass_bars or {}),
    }


def _emit_workspace_payload(label, started_at, summary, rows, extra=None):
    before = sample_resource()
    payload = {
        "label": label,
        "duration_ms": (time.perf_counter() - started_at) * 1000.0,
        "resource_before": before,
        "resource_after": sample_resource(),
        "summary": summary,
        "rows": rows,
    }
    if extra:
        payload.update(extra)
    print(json.dumps(payload, separators=(",", ":"), sort_keys=True))
"""
)


async def run_workspace_base_probe(
    handle: SandboxHandle,
    body: str,
    *,
    label: str,
    cfg: Mapping[str, Any] | None = None,
    timeout: float = 180,
) -> dict[str, Any]:
    """Run a phase-01 native probe and return its trailing JSON payload."""
    cmd = shell_command(render(WORKSPACE_BASE_PROBE_PRELUDE + body, cfg=dict(cfg or {})))
    result = await handle.raw_exec(handle.sandbox_id, cmd, timeout=timeout)
    assert result.exit_code == 0, (
        f"{label} workspace-base probe failed (rc={result.exit_code}): "
        f"stderr={result.stderr!r} stdout={result.stdout!r}"
    )
    payload = json.loads(result.stdout.strip().splitlines()[-1])
    print(f"\n[{label}] {json.dumps(payload, separators=(',', ':'), sort_keys=True)}")
    return payload


__all__ = [
    "WORKSPACE_BASE_PROBE_PRELUDE",
    "run_workspace_base_probe",
]
