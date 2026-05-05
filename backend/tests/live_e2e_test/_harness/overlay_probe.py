"""In-sandbox overlay probe scripts shipped via ``raw_exec``.

Step 5 (overlay/) tests measure direct ``mount(2)`` overlayfs behaviour
inside the Daytona sandbox — the host-side ``OverlayClient`` is registered
on the handle for parity with the migration plan, but real measurements
require a Linux kernel and therefore live inside the sandbox.

Each helper returns a Python source string ready for
``python3 -c <source>`` invocation (after wrapping with
``unshare -Urm`` for the user+mount namespace probes).
"""

from __future__ import annotations

import json
import shlex
from collections.abc import Sequence

OVERLAY_ROOT = "/dev/shm/o"


_PROBE_PRELUDE = r"""
import ctypes, ctypes.util, errno, json, os, shutil, sys, time

libc_name = ctypes.util.find_library("c") or "libc.so.6"
libc = ctypes.CDLL(libc_name, use_errno=True)
libc.mount.argtypes = [
    ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p,
    ctypes.c_ulong, ctypes.c_void_p,
]
libc.mount.restype = ctypes.c_int
libc.umount2.argtypes = [ctypes.c_char_p, ctypes.c_int]
libc.umount2.restype = ctypes.c_int
MNT_DETACH = 2

def fresh(root):
    if os.path.isdir(root):
        try:
            with open("/proc/self/mounts") as fh:
                lines = fh.read().splitlines()
        except OSError:
            lines = []
        for line in lines:
            parts = line.split()
            if len(parts) >= 3 and parts[2] == "overlay" and parts[1].startswith(root):
                libc.umount2(parts[1].encode(), MNT_DETACH)
        shutil.rmtree(root, ignore_errors=True)
    os.makedirs(root, exist_ok=True)

def build_layers(root, depth):
    # cwd into root and use basenames so options stays under PAGE_SIZE at depth 200.
    os.chdir(root)
    lowers = []
    for i in range(depth):
        name = "L%d" % i
        os.makedirs(name, exist_ok=True)
        with open(os.path.join(name, "m_%d.txt" % i), "w") as fh:
            fh.write("layer %d\n" % i)
        lowers.append(name)
    os.makedirs("u", exist_ok=True)
    os.makedirs("w", exist_ok=True)
    os.makedirs("m", exist_ok=True)
    options = "lowerdir=" + ":".join(reversed(lowers)) + ",upperdir=u,workdir=w"
    return "m", options

def mount2(target, options):
    rc = libc.mount(b"overlay", target.encode(), b"overlay", 0, options.encode())
    return rc, ctypes.get_errno()

def umount(target):
    libc.umount2(target.encode(), MNT_DETACH)

def percentile(values, p):
    if not values:
        return 0.0
    s = sorted(values); k = (len(s) - 1) * (p / 100.0)
    lo = int(k); hi = min(lo + 1, len(s) - 1)
    return s[lo] + (s[hi] - s[lo]) * (k - lo)
"""


_MOUNT_DEPTHS_BODY = r"""
cfg = json.loads(__CFG_JSON__)
results = []
for depth in cfg["depths"]:
    root = os.path.join(cfg["overlay_root"], "syscall_d%d" % depth)
    fresh(root)
    merged, options = build_layers(root, depth)
    t0 = time.perf_counter()
    rc, err = mount2(merged, options)
    elapsed = (time.perf_counter() - t0) * 1000.0
    marker_ok = None
    if cfg["write_marker"] and rc == 0:
        try:
            mark = os.path.join(merged, "_probe_marker.txt")
            with open(mark, "w") as fh:
                fh.write("ok\n")
            with open(mark) as fh:
                marker_ok = fh.read().strip() == "ok"
        except OSError:
            marker_ok = False
    if rc == 0:
        umount(merged)
    results.append({
        "depth": depth,
        "rc": rc,
        "errno": err,
        "errno_name": (errno.errorcode.get(err) if err else None),
        "elapsed_ms": elapsed,
        "options_len": len(options),
        "marker_ok": marker_ok,
    })
print(json.dumps({"results": results}, separators=(",", ":")))
"""


_MOUNT8_BODY = r"""
import subprocess
cfg = json.loads(__CFG_JSON__)
results = []
# mount(8) deliberately uses ABSOLUTE paths to mirror agent-shell behaviour
# and surface util-linux's options-buffer overflow at modest depth.
LONG_PREFIX = "/dev/shm/o/mount8-deep-namespaced-overlay-layer-test-"
for depth in cfg["depths"]:
    root = LONG_PREFIX + ("d%d" % depth)
    fresh(root)
    lowers = []
    for i in range(depth):
        d = os.path.join(root, "lower-layer-%05d" % i)
        os.makedirs(d, exist_ok=True)
        lowers.append(d)
    upper = os.path.join(root, "upper-layer-dir"); os.makedirs(upper, exist_ok=True)
    work = os.path.join(root, "work-layer-dir");   os.makedirs(work, exist_ok=True)
    merged = os.path.join(root, "merged-target");  os.makedirs(merged, exist_ok=True)
    options = "lowerdir=" + ":".join(reversed(lowers)) + ",upperdir=" + upper + ",workdir=" + work
    proc = subprocess.run(
        ["mount", "-t", "overlay", "overlay", "-o", options, merged],
        capture_output=True, text=True,
    )
    if proc.returncode == 0:
        umount(merged)
    results.append({
        "depth": depth,
        "rc": proc.returncode,
        "options_len": len(options),
        "stderr": (proc.stderr or "")[:240],
    })
print(json.dumps({"results": results}, separators=(",", ":")))
"""


_LATENCY_BODY = r"""
cfg = json.loads(__CFG_JSON__)
out = []
for depth in cfg["depths"]:
    root = os.path.join(cfg["overlay_root"], "snap_d%d" % depth)
    fresh(root); merged, options = build_layers(root, depth)
    timings = []; failures = 0; first_errno = None
    for _ in range(cfg["iterations"]):
        t0 = time.perf_counter()
        rc, err = mount2(merged, options)
        elapsed = (time.perf_counter() - t0) * 1000.0
        if rc != 0:
            failures += 1
            if first_errno is None:
                first_errno = errno.errorcode.get(err, err)
            continue
        timings.append(elapsed)
        umount(merged)
    out.append({
        "depth": depth,
        "iterations": cfg["iterations"],
        "failures": failures,
        "first_errno": first_errno,
        "options_len": len(options),
        "p50_ms": percentile(timings, 50),
        "p95_ms": percentile(timings, 95),
        "p99_ms": percentile(timings, 99),
        "min_ms": min(timings) if timings else 0.0,
        "max_ms": max(timings) if timings else 0.0,
        "mean_ms": (sum(timings) / len(timings)) if timings else 0.0,
    })
print(json.dumps({"results": out}, separators=(",", ":")))
"""


_READ_BODY = r"""
cfg = json.loads(__CFG_JSON__)

def populate_lower(layers, files, size):
    payload_bytes = ("x" * size).encode("ascii")
    n = len(layers)
    for j in range(files):
        i = j % n
        layer = layers[i]
        sub = os.path.join(layer, "d%02d" % (j % 16))
        os.makedirs(sub, exist_ok=True)
        with open(os.path.join(sub, "f%05d.dat" % j), "wb") as fh:
            fh.write(payload_bytes)

def read_all(merged):
    total = 0; count = 0
    for dirpath, _dirs, files in os.walk(merged):
        for name in files:
            count += 1
            with open(os.path.join(dirpath, name), "rb") as fh:
                total += len(fh.read())
    return total, count

def try_drop_caches():
    try:
        with open("/proc/sys/vm/drop_caches", "w") as fh:
            fh.write("3\n")
        return {"supported": True, "error": None}
    except OSError as exc:
        return {"supported": False, "error": "%s: %s" % (type(exc).__name__, exc)}

depths = cfg["depths"]
files = cfg["files_per_depth"]
size = cfg["bytes_per_file"]
results = []
for depth in depths:
    root = os.path.join(cfg["overlay_root"], "r%d" % depth)
    fresh(root)
    os.chdir(root)
    layers = []
    for i in range(depth):
        name = "L%d" % i; os.makedirs(name, exist_ok=True)
        layers.append(name)
    populate_lower(layers, files, size)
    os.makedirs("u", exist_ok=True); os.makedirs("w", exist_ok=True); os.makedirs("m", exist_ok=True)
    options = "lowerdir=" + ":".join(reversed(layers)) + ",upperdir=u,workdir=w"
    merged = "m"
    rc, err = mount2(merged, options)
    if rc != 0:
        results.append({"depth": depth, "mount_errno": err, "first_read_ms": None,
                        "warm_read_ms": None, "cold_read_ms": None, "files": 0, "bytes": 0,
                        "drop_caches": {"supported": False, "error": "mount failed"}})
        continue
    drop = try_drop_caches()
    t0 = time.perf_counter(); first_bytes, first_count = read_all(merged)
    first_ms = (time.perf_counter() - t0) * 1000.0
    t0 = time.perf_counter(); _, _ = read_all(merged)
    warm_ms = (time.perf_counter() - t0) * 1000.0
    cold_ms = None
    if drop["supported"]:
        try_drop_caches()
        t0 = time.perf_counter(); _, _ = read_all(merged)
        cold_ms = (time.perf_counter() - t0) * 1000.0
    umount(merged)
    results.append({
        "depth": depth, "mount_errno": err, "options_len": len(options),
        "files": first_count, "bytes": first_bytes,
        "first_read_ms": first_ms, "warm_read_ms": warm_ms, "cold_read_ms": cold_ms,
        "drop_caches": drop,
    })
base_warm = None
base_first = None
base_cold = None
for r in results:
    if r["depth"] == depths[0]:
        base_warm = r["warm_read_ms"]; base_first = r["first_read_ms"]; base_cold = r["cold_read_ms"]
        break
for r in results:
    r["warm_vs_depth1"] = (r["warm_read_ms"] / base_warm) if (base_warm and r["warm_read_ms"]) else None
    r["first_vs_depth1"] = (r["first_read_ms"] / base_first) if (base_first and r["first_read_ms"]) else None
    r["cold_vs_depth1"] = (r["cold_read_ms"] / base_cold) if (base_cold and r["cold_read_ms"]) else None

depth100_warm = None
for r in results:
    if r["depth"] == 100:
        depth100_warm = r["warm_vs_depth1"]; break
print(json.dumps({
    "depths": results,
    "depth100_warm_vs_depth1": depth100_warm,
    "cold_cache_available": any(r.get("cold_read_ms") for r in results),
}, separators=(",", ":")))
"""


_CONCURRENT_BODY = r"""
cfg = json.loads(__CFG_JSON__)
depth = cfg["depth"]
out = []
for n in cfg["counts"]:
    root = os.path.join(cfg["overlay_root"], "conc_n%d" % n)
    fresh(root)
    os.chdir(root)
    # Build N independent mount points sharing one stack of N lower layers.
    lowers = []
    for i in range(depth):
        name = "L%d" % i
        os.makedirs(name, exist_ok=True)
        with open(os.path.join(name, "m.txt"), "w") as fh:
            fh.write("layer %d\n" % i)
        lowers.append(name)
    lower_arg = ":".join(reversed(lowers))
    mounted = []
    timings = []
    failures = 0
    first_err = None
    for k in range(n):
        os.makedirs("u%d" % k, exist_ok=True)
        os.makedirs("w%d" % k, exist_ok=True)
        os.makedirs("m%d" % k, exist_ok=True)
        options = "lowerdir=%s,upperdir=u%d,workdir=w%d" % (lower_arg, k, k)
        merged = "m%d" % k
        t0 = time.perf_counter()
        rc, err = mount2(merged, options)
        elapsed = (time.perf_counter() - t0) * 1000.0
        if rc == 0:
            mounted.append(merged)
            timings.append(elapsed)
        else:
            failures += 1
            if first_err is None:
                first_err = errno.errorcode.get(err, err)
    # Read /proc/self/mounts under the steady-state holding pattern
    proc_t0 = time.perf_counter()
    with open("/proc/self/mounts") as fh:
        proc_text = fh.read()
    proc_ms = (time.perf_counter() - proc_t0) * 1000.0
    proc_lines = proc_text.count("\n")
    overlay_lines = sum(1 for line in proc_text.splitlines() if " overlay " in line)
    # Tear down (LIFO).
    umount_timings = []
    for merged in reversed(mounted):
        t0 = time.perf_counter()
        umount(merged)
        umount_timings.append((time.perf_counter() - t0) * 1000.0)
    out.append({
        "n_target": n,
        "n_mounted": len(mounted),
        "failures": failures,
        "first_errno": first_err,
        "options_len": len(options) if mounted else 0,
        "mount_p50_ms": percentile(timings, 50),
        "mount_p95_ms": percentile(timings, 95),
        "mount_p99_ms": percentile(timings, 99),
        "mount_max_ms": max(timings) if timings else 0.0,
        "umount_p99_ms": percentile(umount_timings, 99),
        "proc_mounts_lines": proc_lines,
        "proc_overlay_lines": overlay_lines,
        "proc_mounts_read_ms": proc_ms,
    })
print(json.dumps({"depth": depth, "results": out}, separators=(",", ":")))
"""


_HEAVY_WRITE_BODY = r"""
cfg = json.loads(__CFG_JSON__)
depth = cfg["depth"]
files = cfg["files"]
write_bytes = cfg["write_bytes"]

root = os.path.join(cfg["overlay_root"], "hw_d%d_f%d" % (depth, files))
fresh(root)
os.chdir(root)

# Spread `files` files across `depth` lower layers (round-robin).
# Each file path is "data/L<layer>/f<idx>.txt" and exists exactly once.
file_paths = []
for i in range(files):
    layer = i % depth
    layer_dir = "L%d" % layer
    sub = os.path.join(layer_dir, "data", "L%d" % layer)
    os.makedirs(sub, exist_ok=True)
    rel = os.path.join("data", "L%d" % layer, "f%05d.txt" % i)
    abs_in_layer = os.path.join(layer_dir, rel)
    with open(abs_in_layer, "w") as fh:
        fh.write("seed-%05d\n" % i)
    file_paths.append(rel)

lowers = ["L%d" % i for i in range(depth)]
os.makedirs("u", exist_ok=True)
os.makedirs("w", exist_ok=True)
os.makedirs("m", exist_ok=True)
options = "lowerdir=" + ":".join(reversed(lowers)) + ",upperdir=u,workdir=w"

t0 = time.perf_counter()
rc, err = mount2("m", options)
mount_ms = (time.perf_counter() - t0) * 1000.0
if rc != 0:
    print(json.dumps({
        "depth": depth, "files": files, "mount_errno": err,
        "first_errno": errno.errorcode.get(err, err),
    }, separators=(",", ":")))
    raise SystemExit(1)

payload = ("x" * write_bytes).encode("ascii")
write_timings = []
write_failures = 0
total_t0 = time.perf_counter()
for rel in file_paths:
    target = os.path.join("m", rel)
    t0 = time.perf_counter()
    try:
        with open(target, "wb") as fh:
            fh.write(payload)
        write_timings.append((time.perf_counter() - t0) * 1000.0)
    except OSError:
        write_failures += 1
total_ms = (time.perf_counter() - total_t0) * 1000.0

# Upperdir size after the copy-up storm.
upper_bytes = 0
upper_files = 0
for dirpath, _dirs, names in os.walk("u"):
    for name in names:
        try:
            upper_bytes += os.path.getsize(os.path.join(dirpath, name))
            upper_files += 1
        except OSError:
            pass

umount("m")

print(json.dumps({
    "depth": depth,
    "files": files,
    "write_bytes": write_bytes,
    "mount_ms": mount_ms,
    "total_write_ms": total_ms,
    "writes_per_s": (len(write_timings) / (total_ms / 1000.0)) if total_ms > 0 else 0.0,
    "write_failures": write_failures,
    "p50_ms": percentile(write_timings, 50),
    "p95_ms": percentile(write_timings, 95),
    "p99_ms": percentile(write_timings, 99),
    "max_ms": max(write_timings) if write_timings else 0.0,
    "mean_ms": (sum(write_timings) / len(write_timings)) if write_timings else 0.0,
    "upper_files": upper_files,
    "upper_bytes": upper_bytes,
}, separators=(",", ":")))
"""


_PURGE_BODY = r"""
fresh(__OVERLAY_ROOT__)
print("ok")
"""


def _render(body: str, *, cfg: dict | None = None, overlay_root: str | None = None) -> str:
    rendered = _PROBE_PRELUDE + body
    if cfg is not None:
        rendered = rendered.replace("__CFG_JSON__", repr(json.dumps(cfg)))
    if overlay_root is not None:
        rendered = rendered.replace("__OVERLAY_ROOT__", repr(overlay_root))
    return rendered


def script_mount_depths(
    *,
    overlay_root: str,
    depths: Sequence[int],
    write_marker: bool = True,
) -> str:
    cfg = {
        "depths": list(depths),
        "overlay_root": overlay_root,
        "write_marker": write_marker,
    }
    return _render(_MOUNT_DEPTHS_BODY, cfg=cfg)


def script_mount8_negative_control(
    *, overlay_root: str, depths: Sequence[int]
) -> str:
    cfg = {"depths": list(depths), "overlay_root": overlay_root}
    return _render(_MOUNT8_BODY, cfg=cfg)


def script_snapshot_latency(
    *, overlay_root: str, depths: Sequence[int], iterations: int
) -> str:
    cfg = {
        "depths": list(depths),
        "iterations": int(iterations),
        "overlay_root": overlay_root,
    }
    return _render(_LATENCY_BODY, cfg=cfg)


def script_read_latency(
    *,
    overlay_root: str,
    depths: Sequence[int],
    files_per_depth: int,
    bytes_per_file: int,
) -> str:
    cfg = {
        "depths": list(depths),
        "overlay_root": overlay_root,
        "files_per_depth": int(files_per_depth),
        "bytes_per_file": int(bytes_per_file),
    }
    return _render(_READ_BODY, cfg=cfg)


def script_concurrent_mounts(
    *, overlay_root: str, counts: Sequence[int], depth: int
) -> str:
    """Hold N overlay mounts open simultaneously; report mount/umount + /proc."""
    cfg = {
        "overlay_root": overlay_root,
        "counts": [int(n) for n in counts],
        "depth": int(depth),
    }
    return _render(_CONCURRENT_BODY, cfg=cfg)


def script_heavy_write_copy_up(
    *, overlay_root: str, depth: int, files: int, write_bytes: int
) -> str:
    """Stress copy-up by rewriting every file through a deep overlay stack."""
    cfg = {
        "overlay_root": overlay_root,
        "depth": int(depth),
        "files": int(files),
        "write_bytes": int(write_bytes),
    }
    return _render(_HEAVY_WRITE_BODY, cfg=cfg)


def script_purge_overlay_mounts(*, overlay_root: str) -> str:
    return _render(_PURGE_BODY, overlay_root=overlay_root)


def wrap_unshare(script: str, *, prog: str = "python3") -> str:
    """Run *script* under ``unshare -Urm`` so mount(2) is permitted."""
    return "unshare -Urm {prog} -c {script}".format(
        prog=shlex.quote(prog),
        script=shlex.quote(script),
    )


__all__ = [
    "OVERLAY_ROOT",
    "script_mount_depths",
    "script_mount8_negative_control",
    "script_snapshot_latency",
    "script_read_latency",
    "script_concurrent_mounts",
    "script_heavy_write_copy_up",
    "script_purge_overlay_mounts",
    "wrap_unshare",
]
