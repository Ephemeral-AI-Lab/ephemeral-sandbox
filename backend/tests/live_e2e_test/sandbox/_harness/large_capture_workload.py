"""Helpers for the Phase 06 / 07 large-capture benchmarks.

All builders emit a ``python3 -c`` driver instead of a bash for-loop. The
bash driver hits per-iteration subshell limits at K=10 000 inside the
sandbox tmpfs (`for i in $(seq …)` exits 1 mid-run). A single Python
process with ``os.write`` avoids the fork-per-file cost and the bash
errno surfaces, so K=10K runs to completion on both gated and gitignored
prefixes.
"""

from __future__ import annotations


def _py_driver(body: str) -> str:
    """Wrap a Python source body inside a ``python3 -c`` shell invocation.

    The body is passed via stdin to avoid argv quoting issues — the
    sandbox shell is bash, but we want zero shell parsing of the source.
    """
    return "python3 - <<'PY'\n" + body + "\nPY"


def build_k_capture_command(prefix: str, k: int) -> str:
    """Create K small files under ``prefix``.

    Same shape as the original bash builder (one tiny file per iteration)
    but driven from a single Python process. Approximates the side-effect
    of ``pip install`` / ``npm install`` without depending on network or
    a specific package layout.
    """
    if k < 1:
        raise ValueError(f"k must be >= 1, got {k}")
    if not prefix:
        raise ValueError("prefix must be non-empty")
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

    File contents are deterministic (``'x' * size`` with the iteration
    index encoded in the trailing 16 bytes) so that two runs produce
    identical byte streams — useful when distinguishing capture/stager
    cost (which is bytes-per-second) from filesystem cost (which is
    syscalls-per-file).
    """
    if k < 1:
        raise ValueError(f"k must be >= 1, got {k}")
    if file_size_bytes < 16:
        raise ValueError(
            f"file_size_bytes must be >= 16 (header room), got {file_size_bytes}"
        )
    if not prefix:
        raise ValueError("prefix must be non-empty")
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

    Used as the *untimed* setup step before ``build_modify_capture`` /
    ``build_delete_capture``. Contents start with ``'baseline '`` so that
    modify scenarios can verify the capture replaced the byte stream.
    """
    if k < 1:
        raise ValueError(f"k must be >= 1, got {k}")
    if file_size_bytes < 32:
        raise ValueError(
            f"file_size_bytes must be >= 32 (header room), got {file_size_bytes}"
        )
    if not prefix:
        raise ValueError("prefix must be non-empty")
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
    which exercises the gated read-current path against an existing
    layer-stack entry.
    """
    if k < 1:
        raise ValueError(f"k must be >= 1, got {k}")
    if file_size_bytes < 32:
        raise ValueError(
            f"file_size_bytes must be >= 32 (header room), got {file_size_bytes}"
        )
    if not prefix:
        raise ValueError("prefix must be non-empty")
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
    if k < 1:
        raise ValueError(f"k must be >= 1, got {k}")
    if not prefix:
        raise ValueError("prefix must be non-empty")
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
    if not prefix:
        raise ValueError("prefix must be non-empty")
    if min(k_new, k_modify, k_delete) < 0:
        raise ValueError("k_new/k_modify/k_delete must be >= 0")
    if k_new + k_modify + k_delete < 1:
        raise ValueError("at least one of k_new/k_modify/k_delete must be > 0")
    if file_size_bytes < 32:
        raise ValueError(
            f"file_size_bytes must be >= 32 (header room), got {file_size_bytes}"
        )
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
    if k_gated < 1 or k_dist < 1:
        raise ValueError(
            f"k_gated and k_dist must both be >= 1, got {k_gated}, {k_dist}"
        )
    if not gated_prefix or not dist_prefix:
        raise ValueError("both prefixes must be non-empty")
    if file_size_bytes < 32:
        raise ValueError(
            f"file_size_bytes must be >= 32 (header room), got {file_size_bytes}"
        )
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


def build_count_files_command(prefix: str) -> str:
    """Print the number of regular files directly under ``prefix`` (recursive).

    Used as a correctness probe — count of paths the test expects to be
    present after a workload runs. Stdout is a single integer line.
    """
    if not prefix:
        raise ValueError("prefix must be non-empty")
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
    "build_k_capture_command",
    "build_sized_capture",
    "build_seed_capture",
    "build_modify_capture",
    "build_delete_capture",
    "build_mixed_kinds_capture",
    "build_mixed_routing_capture",
    "build_count_files_command",
]
