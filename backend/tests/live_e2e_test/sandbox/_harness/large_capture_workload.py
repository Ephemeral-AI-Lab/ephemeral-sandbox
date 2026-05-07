"""Helpers for the Phase 06 / 07 large-capture benchmarks.

All builders emit a ``python3 -c`` driver. The previous bash for-loop
(``for i in $(seq 1 10000)``) silently truncates at K=10 000 inside the
daytona sandbox; a single Python process with ``os.write`` avoids the
per-iteration subshell fork and runs the workload to completion.
"""

from __future__ import annotations

_MIN_HEAD_BYTES = 32  # safe floor for "head + pad" builders (longest head ≤ 25 B)


def _py_driver(body: str) -> str:
    """Wrap a Python source body in a ``python3 - <<'PY'`` heredoc.

    Heredoc avoids argv quoting issues and removes shell parsing of the
    source.
    """
    return "python3 - <<'PY'\n" + body + "\nPY"


def _require_k(k: int) -> None:
    if k < 1:
        raise ValueError(f"k must be >= 1, got {k}")


def _require_prefix(prefix: str, *, name: str = "prefix") -> None:
    if not prefix:
        raise ValueError(f"{name} must be non-empty")


def _require_min_size(file_size_bytes: int, *, minimum: int = _MIN_HEAD_BYTES) -> None:
    if file_size_bytes < minimum:
        raise ValueError(
            f"file_size_bytes must be >= {minimum} (header room), got {file_size_bytes}"
        )


def build_k_capture_command(prefix: str, k: int) -> str:
    """Create K small files under ``prefix``.

    Same shape as the original bash builder (one tiny file per iteration)
    but driven from a single Python process. Approximates the side-effect
    of ``pip install`` / ``npm install`` without depending on network or
    a specific package layout.
    """
    _require_k(k)
    _require_prefix(prefix)
    body = (
        "import os\n"
        f"prefix = {prefix!r}\n"
        f"k = {int(k)}\n"
        "os.makedirs(prefix, exist_ok=True)\n"
        "for i in range(1, k + 1):\n"
        "    path = f'{prefix}/file_{i:06d}.bin'\n"
        "    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)\n"
        "    try:\n"
        "        os.write(fd, f'k={k} i={i}\\n'.encode())\n"
        "    finally:\n"
        "        os.close(fd)\n"
    )
    return _py_driver(body)


def build_sized_capture(prefix: str, k: int, file_size_bytes: int) -> str:
    """Create K files of exactly ``file_size_bytes`` each under ``prefix``.

    Each file is ``b'x' * (size-16) + b'i=...\\n'`` (16-byte tail) so two
    runs produce byte-identical content — useful for separating stager
    byte-copy cost (bytes/s) from filesystem syscall cost (calls/file).
    """
    _require_k(k)
    _require_prefix(prefix)
    _require_min_size(file_size_bytes, minimum=16)
    body = (
        "import os\n"
        f"prefix = {prefix!r}\n"
        f"k = {int(k)}\n"
        f"size = {int(file_size_bytes)}\n"
        "os.makedirs(prefix, exist_ok=True)\n"
        "filler = b'x' * (size - 16)\n"
        "for i in range(1, k + 1):\n"
        "    path = f'{prefix}/file_{i:06d}.bin'\n"
        "    tail = f'i={i:013d}\\n'.encode()\n"
        "    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)\n"
        "    try:\n"
        "        os.write(fd, filler)\n"
        "        os.write(fd, tail)\n"
        "    finally:\n"
        "        os.close(fd)\n"
    )
    return _py_driver(body)


def build_seed_capture(prefix: str, k: int, file_size_bytes: int = 64) -> str:
    """Seed K pre-existing files under ``prefix`` with a 'baseline' marker.

    Untimed setup step before ``build_modify_capture`` /
    ``build_delete_capture``. Contents start with ``'baseline '`` so the
    modify scenarios can verify the capture replaced the byte stream.
    """
    _require_k(k)
    _require_prefix(prefix)
    _require_min_size(file_size_bytes)
    body = (
        "import os\n"
        f"prefix = {prefix!r}\n"
        f"k = {int(k)}\n"
        f"size = {int(file_size_bytes)}\n"
        "os.makedirs(prefix, exist_ok=True)\n"
        "for i in range(1, k + 1):\n"
        "    path = f'{prefix}/file_{i:06d}.bin'\n"
        "    head = f'baseline i={i:013d}\\n'.encode()\n"
        "    pad = b'b' * (size - len(head))\n"
        "    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)\n"
        "    try:\n"
        "        os.write(fd, head + pad)\n"
        "    finally:\n"
        "        os.close(fd)\n"
    )
    return _py_driver(body)


def build_modify_capture(prefix: str, k: int, file_size_bytes: int = 64) -> str:
    """Overwrite K pre-existing files under ``prefix`` with 'modified' content.

    Pair with ``build_seed_capture(prefix, k)`` as the (untimed) setup —
    the OCC commit then sees K *modified* paths instead of K *new* paths,
    which exercises the read-current validate path against an existing
    layer-stack entry.
    """
    _require_k(k)
    _require_prefix(prefix)
    _require_min_size(file_size_bytes)
    body = (
        "import os\n"
        f"prefix = {prefix!r}\n"
        f"k = {int(k)}\n"
        f"size = {int(file_size_bytes)}\n"
        "for i in range(1, k + 1):\n"
        "    path = f'{prefix}/file_{i:06d}.bin'\n"
        "    head = f'modified i={i:013d}\\n'.encode()\n"
        "    pad = b'm' * (size - len(head))\n"
        "    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)\n"
        "    try:\n"
        "        os.write(fd, head + pad)\n"
        "    finally:\n"
        "        os.close(fd)\n"
    )
    return _py_driver(body)


def build_delete_capture(prefix: str, k: int) -> str:
    """Delete K pre-existing files under ``prefix``.

    Pair with ``build_seed_capture(prefix, k)`` as the (untimed) setup —
    the OCC commit then sees K *whiteout* paths, which exercises the
    publish-layer whiteout path independent of stager byte traffic.
    """
    _require_k(k)
    _require_prefix(prefix)
    body = (
        "import os\n"
        f"prefix = {prefix!r}\n"
        f"k = {int(k)}\n"
        "for i in range(1, k + 1):\n"
        "    path = f'{prefix}/file_{i:06d}.bin'\n"
        "    try:\n"
        "        os.unlink(path)\n"
        "    except FileNotFoundError:\n"
        "        pass\n"
    )
    return _py_driver(body)


def build_mixed_kinds_capture(
    prefix: str,
    *,
    k_new: int,
    k_modify: int,
    k_delete: int,
    file_size_bytes: int = 64,
) -> str:
    """Mix new + modify + delete in one capture under ``prefix``.

    The seed convention (untimed setup) is::

        build_seed_capture(prefix, k=k_modify + k_delete)

    so files ``file_000001..file_{k_modify}.bin`` and
    ``file_{k_modify+1}..file_{k_modify+k_delete}.bin`` already exist.
    The timed call modifies the first range, deletes the second range,
    and creates ``k_new`` brand-new files at indices starting from
    ``k_modify + k_delete + 1``.
    """
    _require_prefix(prefix)
    _require_min_size(file_size_bytes)
    if min(k_new, k_modify, k_delete) < 0:
        raise ValueError("k_new/k_modify/k_delete must be >= 0")
    if k_new + k_modify + k_delete < 1:
        raise ValueError("at least one of k_new/k_modify/k_delete must be > 0")
    body = (
        "import os\n"
        f"prefix = {prefix!r}\n"
        f"k_new = {int(k_new)}\n"
        f"k_modify = {int(k_modify)}\n"
        f"k_delete = {int(k_delete)}\n"
        f"size = {int(file_size_bytes)}\n"
        "os.makedirs(prefix, exist_ok=True)\n"
        "for i in range(1, k_modify + 1):\n"
        "    path = f'{prefix}/file_{i:06d}.bin'\n"
        "    head = f'modified i={i:013d}\\n'.encode()\n"
        "    pad = b'm' * (size - len(head))\n"
        "    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)\n"
        "    try:\n"
        "        os.write(fd, head + pad)\n"
        "    finally:\n"
        "        os.close(fd)\n"
        "for i in range(k_modify + 1, k_modify + k_delete + 1):\n"
        "    path = f'{prefix}/file_{i:06d}.bin'\n"
        "    try:\n"
        "        os.unlink(path)\n"
        "    except FileNotFoundError:\n"
        "        pass\n"
        "start = k_modify + k_delete + 1\n"
        "for i in range(start, start + k_new):\n"
        "    path = f'{prefix}/file_{i:06d}.bin'\n"
        "    head = f'new i={i:013d}\\n'.encode()\n"
        "    pad = b'n' * (size - len(head))\n"
        "    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)\n"
        "    try:\n"
        "        os.write(fd, head + pad)\n"
        "    finally:\n"
        "        os.close(fd)\n"
    )
    return _py_driver(body)


def build_mixed_routing_capture(
    *,
    gated_prefix: str,
    dist_prefix: str,
    k_gated: int,
    k_dist: int,
    file_size_bytes: int = 64,
) -> str:
    """Create files under BOTH a gated and a gitignored prefix in one call.

    Forces ``OccCommitTransaction`` to populate both ``gated_path_count``
    and ``direct_path_count`` from a single shell invocation — the
    routing-decision codepath that the K-scaling matrix never exercised.
    """
    _require_k(k_gated)
    _require_k(k_dist)
    _require_prefix(gated_prefix, name="gated_prefix")
    _require_prefix(dist_prefix, name="dist_prefix")
    _require_min_size(file_size_bytes)
    body = (
        "import os\n"
        f"gated = {gated_prefix!r}\n"
        f"dist = {dist_prefix!r}\n"
        f"k_gated = {int(k_gated)}\n"
        f"k_dist = {int(k_dist)}\n"
        f"size = {int(file_size_bytes)}\n"
        "os.makedirs(gated, exist_ok=True)\n"
        "os.makedirs(dist, exist_ok=True)\n"
        "for i in range(1, k_gated + 1):\n"
        "    path = f'{gated}/file_{i:06d}.bin'\n"
        "    head = f'gated i={i:013d}\\n'.encode()\n"
        "    pad = b'x' * (size - len(head))\n"
        "    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)\n"
        "    try:\n"
        "        os.write(fd, head + pad)\n"
        "    finally:\n"
        "        os.close(fd)\n"
        "for i in range(1, k_dist + 1):\n"
        "    path = f'{dist}/file_{i:06d}.bin'\n"
        "    head = f'dist  i={i:013d}\\n'.encode()\n"
        "    pad = b'x' * (size - len(head))\n"
        "    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)\n"
        "    try:\n"
        "        os.write(fd, head + pad)\n"
        "    finally:\n"
        "        os.close(fd)\n"
    )
    return _py_driver(body)


def build_deep_path_workload(prefix: str, depth: int = 20) -> str:
    """Phase 09 §4A.4 adversarial — single file at depth ``depth``.

    Stresses ``LayerIndex`` key length and ``_join_rel`` path-segment
    handling. Each level uses a 12-char segment so the total path
    length is well over 200 chars.
    """
    _require_prefix(prefix)
    if depth < 2:
        raise ValueError(f"depth must be >= 2, got {depth}")
    body = (
        "import os\n"
        f"prefix = {prefix!r}\n"
        f"depth = {int(depth)}\n"
        "segments = [f'lvl_{i:08d}' for i in range(depth)]\n"
        "deep_dir = os.path.join(prefix, *segments)\n"
        "os.makedirs(deep_dir, exist_ok=True)\n"
        "leaf = os.path.join(deep_dir, 'leaf.txt')\n"
        "fd = os.open(leaf, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)\n"
        "try:\n"
        "    os.write(fd, b'deep_leaf_content_marker_v1')\n"
        "finally:\n"
        "    os.close(fd)\n"
    )
    return _py_driver(body)


def build_symlink_workload(
    prefix: str,
    *,
    link_name: str,
    target: str,
) -> str:
    """Phase 09 §4A.4 adversarial — create a symlink with chosen target.

    Used for the `absolute-inside-workspace` and
    `absolute-outside-workspace` adversarial cells. Does NOT follow the
    target — the symlink is captured as-is by the overlay walker.
    """
    _require_prefix(prefix)
    _require_prefix(link_name, name="link_name")
    body = (
        "import os\n"
        f"prefix = {prefix!r}\n"
        f"link_name = {link_name!r}\n"
        f"target = {target!r}\n"
        "os.makedirs(prefix, exist_ok=True)\n"
        "link_path = os.path.join(prefix, link_name)\n"
        "if os.path.lexists(link_path):\n"
        "    os.unlink(link_path)\n"
        "os.symlink(target, link_path)\n"
    )
    return _py_driver(body)


def build_whiteout_collision_workload(prefix: str, *, name: str = "collide.txt") -> str:
    """Phase 09 §4A.4 adversarial — delete then re-create same path.

    The OCC commit must produce ONE entry (a write), not delete+write,
    because the workload reaches the daemon via a single capture.
    Pair with a (untimed) seed of the same path so the delete is
    non-trivial.
    """
    _require_prefix(prefix)
    body = (
        "import os\n"
        f"prefix = {prefix!r}\n"
        f"name = {name!r}\n"
        "path = os.path.join(prefix, name)\n"
        "os.makedirs(prefix, exist_ok=True)\n"
        "if os.path.exists(path):\n"
        "    os.unlink(path)\n"
        "fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)\n"
        "try:\n"
        "    os.write(fd, b'recreated_after_delete_v1')\n"
        "finally:\n"
        "    os.close(fd)\n"
    )
    return _py_driver(body)


def build_special_chars_workload(prefix: str) -> str:
    """Phase 09 §4A.4 adversarial — filename with bash-special chars.

    Filename contains ``$`` and a backtick to exercise the python
    driver's heredoc quoting. Also includes a space which is the most
    common shell-quoting failure mode.
    """
    _require_prefix(prefix)
    body = (
        "import os\n"
        f"prefix = {prefix!r}\n"
        "os.makedirs(prefix, exist_ok=True)\n"
        # Triple-escape the filename so the heredoc's repr() round-trips.
        "name = 'with $var `cmd` and space.txt'\n"
        "path = os.path.join(prefix, name)\n"
        "fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)\n"
        "try:\n"
        "    os.write(fd, b'special_chars_marker_v1')\n"
        "finally:\n"
        "    os.close(fd)\n"
    )
    return _py_driver(body)


def build_long_filename_workload(prefix: str, name_length: int = 250) -> str:
    """Phase 09 §4A.4 adversarial — filename near the 255-char filesystem cap.

    250 < 255 keeps a 5-char safety margin under POSIX NAME_MAX so
    different filesystems agree. Stresses the path-key length in
    ``LayerIndex`` and the publisher's hardlink target.
    """
    _require_prefix(prefix)
    if name_length < 50 or name_length > 254:
        raise ValueError(f"name_length must be 50..254, got {name_length}")
    body = (
        "import os\n"
        f"prefix = {prefix!r}\n"
        f"name = 'l' * {int(name_length - 4)} + '.bin'\n"
        "os.makedirs(prefix, exist_ok=True)\n"
        "path = os.path.join(prefix, name)\n"
        "fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)\n"
        "try:\n"
        "    os.write(fd, b'long_filename_marker_v1')\n"
        "finally:\n"
        "    os.close(fd)\n"
    )
    return _py_driver(body)


def build_count_files_command(prefix: str) -> str:
    """Print the number of regular files under ``prefix`` (recursive).

    Correctness probe: stdout is a single integer line.
    """
    _require_prefix(prefix)
    body = (
        "import os, sys\n"
        f"prefix = {prefix!r}\n"
        "n = 0\n"
        "for root, _dirs, files in os.walk(prefix):\n"
        "    for name in files:\n"
        "        n += 1\n"
        "sys.stdout.write(str(n) + '\\n')\n"
    )
    return _py_driver(body)


__all__ = [
    "build_count_files_command",
    "build_deep_path_workload",
    "build_delete_capture",
    "build_k_capture_command",
    "build_long_filename_workload",
    "build_mixed_kinds_capture",
    "build_mixed_routing_capture",
    "build_modify_capture",
    "build_seed_capture",
    "build_sized_capture",
    "build_special_chars_workload",
    "build_symlink_workload",
    "build_whiteout_collision_workload",
]
