"""CLI orchestration for the sandbox-side overlay runtime."""

from __future__ import annotations

import argparse
import base64
import os
import stat
import sys
import time

from .classifier import Classifier
from .command import run_user_command
from .direct_routes import (
    DirectRouteApplier,
    direct_merge_factory,
    narrow_prune_opaque_factory,
)
from .git_adapters import (
    _record_timing,
    build_live_snapshot_in_namespace,
    check_ignore_factory,
    git_show_base_factory,
)
from .namespace import (
    _NS_LOWER,
    _NS_UPPER,
    OverlayMountError,
    setup_mounts,
)
from .ndjson import write_diff_ndjson, write_reject_ndjson
from .policy import reject_exit_code
from .types import PolicyRejectOutcome
from .upperdir import walk_upperdir


def _parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace-root", required=True)
    parser.add_argument("--run-dir", required=True)
    parser.add_argument("--snap", default="")
    parser.add_argument("--upper-size-mb", type=int, required=True)
    parser.add_argument(
        "--user-cmd-b64",
        required=True,
        help="Base64-encoded bash command to run inside the overlay.",
    )
    parser.add_argument(
        "--stdin-b64",
        default="",
        help="Optional base64-encoded stdin payload for the user command.",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:  # pragma: no cover - e2e path
    total_started = time.perf_counter()
    args = _parse_args(argv if argv is not None else sys.argv[1:])
    workspace_root = args.workspace_root.rstrip("/")
    run_dir = args.run_dir.rstrip("/")
    os.makedirs(run_dir, exist_ok=True)

    snap = str(args.snap or "").strip()
    snapshot_timings: dict[str, float] = {}
    run_timings: dict[str, float] = {}
    if not snap:
        try:
            snap, snapshot_timings = build_live_snapshot_in_namespace(workspace_root)
        except Exception as exc:
            print(f"snapshot failed: {exc}", file=sys.stderr)
            return 254

    try:
        setup_started = time.perf_counter()
        setup_mounts(live_root=workspace_root, upper_size_mb=args.upper_size_mb)
        _record_timing(run_timings, "setup_mounts", setup_started)
    except OverlayMountError as exc:
        print(str(exc), file=sys.stderr)
        return 255

    decode_started = time.perf_counter()
    user_cmd = base64.b64decode(args.user_cmd_b64).decode("utf-8")
    stdin_bytes = base64.b64decode(args.stdin_b64) if args.stdin_b64 else None
    _record_timing(run_timings, "decode_command", decode_started)

    user_started = time.perf_counter()
    stdout_path = os.path.join(run_dir, "stdout.bin")
    _stdout_bytes, exit_code = run_user_command(
        user_cmd=user_cmd,
        stdin_bytes=stdin_bytes,
        cwd=workspace_root,
        stdout_path=stdout_path,
    )
    _record_timing(run_timings, "user_command", user_started)

    walk_started = time.perf_counter()
    upper_entries = list(walk_upperdir(_NS_UPPER))
    upper_files = len(upper_entries)
    upper_bytes = sum(
        entry.st.st_size for entry in upper_entries if stat.S_ISREG(entry.st.st_mode)
    )
    _record_timing(run_timings, "walk_upperdir", walk_started)

    classifier_started = time.perf_counter()
    classifier = Classifier(
        read_upper_bytes=lambda rel: open(os.path.join(_NS_UPPER, rel), "rb").read(),
        git_show_base=git_show_base_factory(repo_root=_NS_LOWER, snap=snap),
        check_ignore=check_ignore_factory(repo_root=_NS_LOWER),
    )
    _record_timing(run_timings, "build_classifier", classifier_started)

    classify_started = time.perf_counter()
    result = classifier.classify_plan(upper_entries)
    _record_timing(run_timings, "classify", classify_started)
    if isinstance(result, PolicyRejectOutcome):
        run_timings["total"] = round(time.perf_counter() - total_started, 6)
        write_reject_ndjson(
            run_dir=run_dir,
            snap=snap,
            reject=result,
            snapshot_timings=snapshot_timings,
            run_timings=run_timings,
        )
        return reject_exit_code(result.reason)

    direct_apply_started = time.perf_counter()
    direct_merged_bytes = DirectRouteApplier(
        direct_merge=direct_merge_factory(live_root=_NS_LOWER),
        prune_opaque_narrow=narrow_prune_opaque_factory(live_root=_NS_LOWER),
    ).apply(result)
    _record_timing(run_timings, "apply_direct_routes", direct_apply_started)
    run_timings["total"] = round(time.perf_counter() - total_started, 6)

    write_diff_ndjson(
        run_dir=run_dir,
        snap=snap,
        exit_code=exit_code,
        outcome=result.to_outcome(direct_merged_bytes=direct_merged_bytes),
        upper_bytes=upper_bytes,
        upper_files=upper_files,
        snapshot_timings=snapshot_timings,
        run_timings=run_timings,
    )
    return exit_code


__all__ = ["_parse_args", "main"]
