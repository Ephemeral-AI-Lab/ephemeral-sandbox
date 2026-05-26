"""Command-exec resource metric sampling."""

from __future__ import annotations

import os
import resource
import sys
from collections import deque
from collections import defaultdict
from pathlib import Path
from typing import Any

from sandbox._shared.clock import monotonic_now
from sandbox.daemon.audit_schema import (
    OsResourceSection,
    build_os_resource_event,
    safe_emit,
)

_DEFAULT_TREE_ENTRY_LIMIT = 2_000


def collect_command_exec_resource_metrics(
    *,
    storage_root: Path,
    writable_root: Path,
    run_dir: Path,
    upperdir: Path,
    manifest: Any | None,
    changed_path_count: int,
) -> dict[str, float]:
    """Return command-exec resource facts as non-duration timing keys.

    The live E2E performance report already aggregates numeric timing maps.
    Resource keys intentionally use the ``resource.`` prefix and byte/count
    suffixes so they are grouped as observations, not latency.
    """
    started = monotonic_now()
    timings: dict[str, float] = {
        "resource.command_exec.changed_path_count": float(changed_path_count),
    }
    _add_manifest_stats(timings, manifest)
    _add_filesystem_stats(
        timings,
        "resource.layer_stack.storage_filesystem",
        storage_root,
    )
    _add_filesystem_stats(
        timings,
        "resource.command_exec.writable_filesystem",
        writable_root,
    )
    _add_tree_stats(timings, "resource.command_exec.run_dir", run_dir)
    _add_tree_stats(timings, "resource.command_exec.workspace", run_dir / "workspace")
    _add_tree_stats(timings, "resource.command_exec.upperdir", upperdir)
    _add_memory_stats(timings)
    timings["resource.audit.collect_s"] = monotonic_now() - started
    _emit_os_resource_sample(timings)
    return timings


def _emit_os_resource_sample(timings: dict[str, float]) -> None:
    """Push one ``os_resource.sampled`` event from the existing tick.

    Zero new threads — reuses the per-tool-call collection cadence per V3
    cross-cutting contract.
    """
    rss = timings.get("resource.process.rss_bytes")
    user_us = timings.get("resource.process.user_cpu_usec")
    sys_us = timings.get("resource.process.system_cpu_usec")
    cpu_throttled = timings.get("resource.cgroup.cpu_throttled_usec")
    io_rbytes = timings.get("resource.cgroup.io_rbytes")
    io_wbytes = timings.get("resource.cgroup.io_wbytes")
    io_rios = timings.get("resource.cgroup.io_rios")
    io_wios = timings.get("resource.cgroup.io_wios")
    safe_emit(
        build_os_resource_event(
            OsResourceSection(
                sampled_at_monotonic_s=monotonic_now(),
                rss_bytes=int(rss) if rss is not None else None,
                cpu_user_s=user_us / 1_000_000.0 if user_us is not None else None,
                cpu_system_s=sys_us / 1_000_000.0 if sys_us is not None else None,
                cpu_throttled_us=(
                    int(cpu_throttled) if cpu_throttled is not None else None
                ),
                io_read_bytes=int(io_rbytes) if io_rbytes is not None else None,
                io_write_bytes=int(io_wbytes) if io_wbytes is not None else None,
                io_read_ops=int(io_rios) if io_rios is not None else None,
                io_write_ops=int(io_wios) if io_wios is not None else None,
            ),
        ),
        lane="sample",
    )


def _add_manifest_stats(timings: dict[str, float], manifest: Any | None) -> None:
    layers = getattr(manifest, "layers", None)
    if layers is None:
        return
    try:
        layer_refs = tuple(layers)
    except TypeError:
        return
    timings["resource.layer_stack.manifest_depth"] = float(len(layer_refs))
    timings["resource.layer_stack.manifest_path_count"] = float(
        len({str(getattr(layer, "path", "")) for layer in layer_refs})
    )


def _add_filesystem_stats(
    timings: dict[str, float],
    prefix: str,
    path: Path,
) -> None:
    try:
        usage = os.statvfs(path)
    except OSError:
        return
    total = float(usage.f_blocks * usage.f_frsize)
    free = float(usage.f_bavail * usage.f_frsize)
    timings[f"{prefix}_total_bytes"] = total
    timings[f"{prefix}_free_bytes"] = free
    timings[f"{prefix}_used_bytes"] = max(0.0, total - free)


def _add_tree_stats(
    timings: dict[str, float],
    prefix: str,
    path: Path,
) -> None:
    max_entries = _tree_entry_limit()
    stats = _bounded_tree_stats(path, max_entries=max_entries)
    for key, value in stats.items():
        timings[f"{prefix}_{key}"] = value


def _bounded_tree_stats(path: Path, *, max_entries: int) -> dict[str, float]:
    if max_entries <= 0:
        return {"tree_truncated": 1.0}
    try:
        root_stat = path.lstat()
    except OSError:
        return {
            "tree_exists": 0.0,
            "tree_bytes": 0.0,
            "tree_file_count": 0.0,
            "tree_dir_count": 0.0,
            "tree_entry_count": 0.0,
            "tree_truncated": 0.0,
        }

    stats = {
        "tree_exists": 1.0,
        "tree_bytes": float(_allocated_bytes(root_stat)),
        "tree_file_count": 0.0,
        "tree_dir_count": 1.0 if path.is_dir() else 0.0,
        "tree_entry_count": 1.0,
        "tree_truncated": 0.0,
    }
    if not path.is_dir():
        stats["tree_file_count"] = 1.0
        stats["tree_dir_count"] = 0.0
        return stats

    queue: deque[Path] = deque([path])
    while queue and stats["tree_entry_count"] < max_entries:
        current = queue.popleft()
        try:
            with os.scandir(current) as entries:
                for entry in entries:
                    if stats["tree_entry_count"] >= max_entries:
                        stats["tree_truncated"] = 1.0
                        break
                    try:
                        entry_stat = entry.stat(follow_symlinks=False)
                    except OSError:
                        continue
                    stats["tree_entry_count"] += 1.0
                    stats["tree_bytes"] += float(_allocated_bytes(entry_stat))
                    if entry.is_dir(follow_symlinks=False):
                        stats["tree_dir_count"] += 1.0
                        queue.append(Path(entry.path))
                    else:
                        stats["tree_file_count"] += 1.0
        except OSError:
            continue
    if queue:
        stats["tree_truncated"] = 1.0
    return stats


def _allocated_bytes(stat_result: os.stat_result) -> int:
    blocks = getattr(stat_result, "st_blocks", None)
    if blocks is None:
        return int(stat_result.st_size)
    return int(blocks) * 512


def _add_memory_stats(timings: dict[str, float]) -> None:
    rss_bytes = _current_rss_bytes()
    if rss_bytes is not None:
        timings["resource.process.rss_bytes"] = rss_bytes
    max_rss = float(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss)
    if not sys.platform.startswith("darwin"):
        max_rss *= 1024.0
    timings["resource.process.max_rss_bytes"] = max_rss
    for name, key in (
        ("memory.current", "resource.cgroup.memory_current_bytes"),
        ("memory.peak", "resource.cgroup.memory_peak_bytes"),
        ("memory.max", "resource.cgroup.memory_max_bytes"),
    ):
        value = _read_cgroup_number(Path("/sys/fs/cgroup") / name)
        if value is not None:
            timings[key] = value
    _add_process_cpu_stats(timings)
    _add_cgroup_cpu_stats(timings)
    _add_cgroup_io_stats(timings)


def _add_process_cpu_stats(timings: dict[str, float]) -> None:
    self_usage = resource.getrusage(resource.RUSAGE_SELF)
    child_usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    timings["resource.process.user_cpu_usec"] = float(self_usage.ru_utime * 1_000_000)
    timings["resource.process.system_cpu_usec"] = float(
        self_usage.ru_stime * 1_000_000
    )
    timings["resource.process.children_user_cpu_usec"] = float(
        child_usage.ru_utime * 1_000_000
    )
    timings["resource.process.children_system_cpu_usec"] = float(
        child_usage.ru_stime * 1_000_000
    )


def _add_cgroup_cpu_stats(timings: dict[str, float]) -> None:
    for name, value in _parse_cgroup_key_values(
        _read_text(Path("/sys/fs/cgroup") / "cpu.stat") or ""
    ).items():
        timings[f"resource.cgroup.cpu_{name}"] = value


def _add_cgroup_io_stats(timings: dict[str, float]) -> None:
    for name, value in _parse_cgroup_io_stat(
        _read_text(Path("/sys/fs/cgroup") / "io.stat") or ""
    ).items():
        timings[f"resource.cgroup.io_{name}"] = value


def _read_cgroup_number(path: Path) -> float | None:
    raw = _read_text(path)
    if raw is None:
        return None
    raw = raw.strip()
    if raw == "max":
        return 0.0
    try:
        return float(raw)
    except ValueError:
        return None


def _read_text(path: Path) -> str | None:
    try:
        return path.read_text(encoding="utf-8")
    except OSError:
        return None


def _parse_cgroup_key_values(raw: str) -> dict[str, float]:
    values: dict[str, float] = {}
    for line in raw.splitlines():
        fields = line.split()
        if len(fields) != 2:
            continue
        key, value = fields
        try:
            values[key] = float(value)
        except ValueError:
            continue
    return values


def _parse_cgroup_io_stat(raw: str) -> dict[str, float]:
    totals: defaultdict[str, float] = defaultdict(float)
    for line in raw.splitlines():
        fields = line.split()
        for field in fields[1:]:
            key, sep, value = field.partition("=")
            if not sep or not key:
                continue
            try:
                number = float(value)
            except ValueError:
                continue
            totals[key] += number
    return dict(totals)


def _current_rss_bytes() -> float | None:
    if sys.platform.startswith("linux"):
        try:
            rss_pages = int(Path("/proc/self/statm").read_text().split()[1])
        except (OSError, IndexError, ValueError):
            return None
        return float(rss_pages * os.sysconf("SC_PAGE_SIZE"))
    if sys.platform.startswith("darwin"):
        return float(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss)
    return None


def _tree_entry_limit() -> int:
    raw = os.environ.get("EPHEMERALOS_RESOURCE_AUDIT_TREE_ENTRY_LIMIT", "").strip()
    if not raw:
        return _DEFAULT_TREE_ENTRY_LIMIT
    try:
        return max(0, int(raw))
    except ValueError:
        return _DEFAULT_TREE_ENTRY_LIMIT


__all__ = ["collect_command_exec_resource_metrics"]
