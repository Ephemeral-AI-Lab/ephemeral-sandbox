"""Filesystem-adjacent tests for the overlay runtime facade."""

from __future__ import annotations

import base64
import json
from pathlib import Path

from sandbox.overlay.runtime.cli import parse_args
from sandbox.overlay.runtime.ndjson import write_diff_ndjson, write_reject_ndjson
from sandbox.overlay.runtime.types import PolicyRejectOutcome, UpperChange
from sandbox.overlay.types import OverlayCapture, OverlayPolicyReject
from sandbox.overlay.wire import parse_diff_ndjson


def test_write_diff_ndjson_emits_base64_upper_changes(tmp_path: Path) -> None:
    path = write_diff_ndjson(
        run_dir=str(tmp_path),
        exit_code=0,
        upper_changes=(
            UpperChange(
                rel="src/app.py",
                kind="regular",
                base_bytes=b"old\n",
                upper_bytes=b"new\n",
                base_existed=True,
            ),
            UpperChange(
                rel=".venv/a.pyc",
                kind="regular",
                base_bytes=None,
                upper_bytes=b"\x00\xff",
                base_existed=False,
            ),
        ),
        upper_bytes=6,
        upper_files=2,
        run_timings={"walk_upperdir": 0.1},
    )

    raw = Path(path).read_text(encoding="utf-8")
    parsed = parse_diff_ndjson(raw)

    assert isinstance(parsed, OverlayCapture)
    assert parsed.upper_files == 2
    assert parsed.upper_changes[0].base_bytes == b"old\n"
    assert parsed.upper_changes[1].upper_bytes == b"\x00\xff"


def test_write_reject_ndjson_emits_reject_block(tmp_path: Path) -> None:
    path = write_reject_ndjson(
        run_dir=str(tmp_path),
        reject=PolicyRejectOutcome(reason="overlay_upper_full", paths=()),
        run_timings={"walk_upperdir": 0.1},
    )

    parsed = parse_diff_ndjson(Path(path).read_text(encoding="utf-8"))

    assert isinstance(parsed, OverlayPolicyReject)
    assert parsed.reason == "overlay_upper_full"
    assert parsed.run_timings == {"walk_upperdir": 0.1}


def test_parse_args_decodes_required_shape() -> None:
    ns = parse_args(
        [
            "--workspace-root",
            "/workspace",
            "--run-dir",
            "/tmp/run",
            "--upper-size-mb",
            "256",
            "--user-cmd-b64",
            base64.b64encode(b"echo hi").decode("ascii"),
        ]
    )

    assert ns.workspace_root == "/workspace"
    assert ns.run_dir == "/tmp/run"
    assert ns.upper_size_mb == 256


def test_ndjson_wire_uses_base64_fields(tmp_path: Path) -> None:
    path = write_diff_ndjson(
        run_dir=str(tmp_path),
        exit_code=0,
        upper_changes=(
            UpperChange(
                rel="bin.dat",
                kind="regular",
                base_bytes=None,
                upper_bytes=b"\x00\xff",
                base_existed=False,
            ),
        ),
        upper_bytes=2,
        upper_files=1,
    )
    lines = Path(path).read_text(encoding="utf-8").splitlines()
    payload = json.loads(lines[1])

    assert payload["upper_bytes_b64"] == base64.b64encode(b"\x00\xff").decode("ascii")
    assert payload["base_bytes_b64"] is None
