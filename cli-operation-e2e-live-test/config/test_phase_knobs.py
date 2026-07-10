"""B — consolidation-phase knobs, keyed to the config consolidation spec.

Skip-marked placeholders that activate per phase: landing a phase includes
unskipping its class and implementing the contracts named in each docstring.
Phase 4 (gateway/console sections) is intentionally absent: gateway bind/PID
knobs are exercised implicitly by this family's own gateway bring-up,
max_concurrent_connections has no deterministic CLI observable, and the
console is outside this suite's sandbox-cli charter.
"""

import hashlib

import pytest

from config import helpers
from core import cli as climod

pytestmark = pytest.mark.config


class TestPhase1:
    """runtime.layerstack, manager.export, daemon.http.export (phase 1).

    Lane A methods run first (definition order): the Lane B arms below them
    replace the module's family gateway, so a Lane A test after them would
    rewrite a daemon YAML no running gateway points at.
    """

    def test_sweep_width_squash_invariance(self, lane_a_daemon_yaml):
        """P1-F1 — remount_sweep_width 1 vs 4: squash succeeds identically in
        both arms (perf knob; correctness invariance is the e2e contract) and
        the retired env smuggle is gone from the flow — the width rides only
        the daemon YAML's runtime.layerstack section."""
        for width in (1, 4):
            generated = helpers.rewrite_daemon_yaml(
                lane_a_daemon_yaml,
                {"runtime": {"layerstack": {"remount_sweep_width": width}}},
            )
            rendered = generated.read_text(encoding="utf-8")
            assert "EOS_" not in rendered, "env side channels must be gone from the flow"
            assert "remount_sweep_width" in rendered
            with helpers.sandbox() as sandbox_id:
                helpers.exec_output(sandbox_id, "printf one > sweep-a.txt")
                helpers.exec_output(sandbox_id, "printf two > sweep-b.txt")
                result = climod.manager(
                    "squash_layerstacks", "--sandbox-id", sandbox_id, timeout=240
                )
                assert isinstance(result, dict) and not climod.is_error(result), (
                    f"squash failed at width {width}: {result}"
                )
                assert helpers.exec_output(sandbox_id, "cat sweep-a.txt").strip() == "one"
                assert helpers.exec_output(sandbox_id, "cat sweep-b.txt").strip() == "two"

    def test_export_chunk_shape_invariance(self, lane_a_daemon_yaml, tmp_path):
        """P1-F4 (adapted) — runtime.layerstack.export_chunk_bytes: 4096
        exports bytes identical to the 2 MiB default arm (checksums).

        Spec drift: the spec named daemon.http.export frame shape, but the
        export stream surface was removed in favor of read_export_chunk RPC
        paging while phase 1 landed, so the transport-shape knob is the
        chunk cap. The narrow arm pages a multi-chunk spool; both arms write
        the same delta with pinned modes and mtimes, so the daemon's
        deterministic spool emit must produce identical archives.
        """
        seed_command = (
            "mkdir -p chunks"
            " && i=0; while [ $i -lt 400 ]; do echo $i | sha256sum; i=$((i+1)); done"
            " > chunks/blob.txt"
            " && chmod 755 chunks && chmod 644 chunks/blob.txt"
            " && touch -t 202601010101.01 chunks/blob.txt chunks"
        )
        checksums = {}
        arms = (
            ("narrow", {"runtime": {"layerstack": {"export_chunk_bytes": 4096}}}),
            ("default", {}),
        )
        for arm, overrides in arms:
            helpers.rewrite_daemon_yaml(lane_a_daemon_yaml, overrides)
            with helpers.sandbox() as sandbox_id:
                helpers.exec_output(sandbox_id, seed_command)
                dest = tmp_path / f"chunks-{arm}.tar.zst"
                result = climod.manager(
                    "export_changes",
                    "--sandbox-id",
                    sandbox_id,
                    "--dest",
                    str(dest),
                    "--format",
                    "tar-zst",
                )
                assert not climod.is_error(result), f"{arm} arm export failed: {result}"
                assert dest.stat().st_size > 4096, (
                    "payload must span several narrow chunks to exercise paging"
                )
                checksums[arm] = hashlib.sha256(dest.read_bytes()).hexdigest()
        assert checksums["narrow"] == checksums["default"], (
            f"chunk shape must not change exported bytes: {checksums}"
        )

    @pytest.mark.slow
    def test_export_stream_cap_error(self, tmp_path, config_family_custody):
        """P1-F2 — manager.export.max_stream_bytes: 4096 fails an export of a
        larger delta with the cap error; a generous-cap (baseline) arm accepts
        the same payload."""
        payload_command = "head -c 65536 /dev/urandom > payload.bin"
        capped_yaml = helpers.make_config(
            {"manager": {"export": {"max_stream_bytes": 4096}}},
            tmp_path / "gateway-stream-cap.yml",
        )
        helpers.start_gateway(capped_yaml)
        with helpers.sandbox() as sandbox_id:
            helpers.exec_output(sandbox_id, payload_command)
            dest = tmp_path / "capped.tar.zst"
            result = climod.manager(
                "export_changes",
                "--sandbox-id",
                sandbox_id,
                "--dest",
                str(dest),
                "--format",
                "tar-zst",
            )
            error = helpers.error_text(result)
            assert "export stream cap exceeded" in error, error
            assert not dest.exists(), "a capped export must not materialize the archive"

        generous_yaml = helpers.make_config({}, tmp_path / "gateway-generous.yml")
        helpers.start_gateway(generous_yaml)
        with helpers.sandbox() as sandbox_id:
            helpers.exec_output(sandbox_id, payload_command)
            dest = tmp_path / "generous.tar.zst"
            result = climod.manager(
                "export_changes",
                "--sandbox-id",
                sandbox_id,
                "--dest",
                str(dest),
                "--format",
                "tar-zst",
            )
            assert not climod.is_error(result), f"generous arm export failed: {result}"
            assert dest.exists() and dest.stat().st_size > 4096

    @pytest.mark.slow
    def test_export_apply_entry_cap_error(self, tmp_path, config_family_custody):
        """P1-F3 — manager.export.max_apply_entries: 1 fails a dir-mode export
        of a two-file delta with the entry-cap error, with zero writes into
        the destination."""
        capped_yaml = helpers.make_config(
            {"manager": {"export": {"max_apply_entries": 1}}},
            tmp_path / "gateway-entry-cap.yml",
        )
        with helpers.gateway_with_config(capped_yaml):
            with helpers.sandbox() as sandbox_id:
                helpers.exec_output(
                    sandbox_id, "printf one > entry-a.txt && printf two > entry-b.txt"
                )
                dest = tmp_path / "entry-capped-dest"
                result = climod.manager(
                    "export_changes",
                    "--sandbox-id",
                    sandbox_id,
                    "--dest",
                    str(dest),
                    "--format",
                    "dir",
                )
                error = helpers.error_text(result)
                assert "entry-count cap exceeded" in error, error
                assert not dest.exists() or not any(dest.iterdir()), (
                    "a capped dir export must not write into dest"
                )


@pytest.mark.skip(reason="config consolidation phase 2 not landed")
class TestPhase2:
    """daemon.server limits, observability.views."""

    def test_request_cap_rejects_oversized_write(self):
        """P2-F1 — daemon.server.max_request_bytes: 65536 rejects a write_file
        payload over 64 KiB with the request-too-large error; the default arm
        accepts it."""
        raise NotImplementedError

    def test_layer_delta_view_honors_default_limit(self):
        """P2-F2 — observability.views.layer_delta_default_limit: 3 returns at
        most 3 deltas for a sandbox with more than 3 published layers."""
        raise NotImplementedError


@pytest.mark.skip(reason="config consolidation phase 3 not landed")
class TestPhase3:
    """runtime.command, runtime.file, runtime.namespace_execution."""

    def test_file_list_truncates_at_cap(self):
        """P3-F1 — runtime.file.max_list_entries: 5 lists exactly 5 of 10
        entries plus the truncation indicator per the operation contract."""
        raise NotImplementedError

    def test_file_read_default_lines(self):
        """P3-F2 — runtime.file.read_lines_default: 10 returns 10 lines of a
        100-line file when --limit is omitted."""
        raise NotImplementedError

    def test_file_edit_size_cap_error(self):
        """P3-F3 — runtime.file.max_edit_bytes: 1024 fails a 2 KiB edit with
        the size-cap error."""
        raise NotImplementedError

    def test_command_admission_cap(self):
        """P3-F4 — runtime.command.max_active: 1 returns the admission error
        naming max_active while one long-running command is active."""
        raise NotImplementedError

    def test_terminal_retention_eviction(self):
        """P3-F5 — runtime.namespace_execution.max_terminal_entries: 2 evicts
        the oldest of three commands (draining it errors; the newest two
        drain fine)."""
        raise NotImplementedError
