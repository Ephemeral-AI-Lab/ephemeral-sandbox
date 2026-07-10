"""Shared fixtures: gateway bring-up and sandbox / workspace-session lifecycle.

Lifecycle fixtures guarantee teardown even when a test fails mid-way — the main
reason this suite is in pytest rather than shell.
"""

import datetime as _dt
import json
import logging
import os
from pathlib import Path
import sys

import pytest

from core import cleanup, gateway
from core.cli import operation_timing_records, operation_timing_summary
from core.root import REPO_ROOT
from manager.management import helpers as mgmt

_timing_log = logging.getLogger("e2e.timing")
_test_seconds = {}


def pytest_runtest_logreport(report):
    """Emit a live per-test total-duration line (setup + call + teardown)."""
    _test_seconds[report.nodeid] = _test_seconds.get(report.nodeid, 0.0) + report.duration
    if report.when == "teardown":
        _timing_log.info(
            "⏱  %s — %.3fs total", report.nodeid, _test_seconds.pop(report.nodeid, 0.0)
        )


def pytest_sessionstart(session):
    if _is_squash_run(session.config):
        from manager.management.squash import measure

        measure.start_iteration_report("squash")


def pytest_terminal_summary(terminalreporter, exitstatus, config):
    if _is_squash_run(config):
        from manager.management.squash import measure

        summary_path = measure.finalize_summary(exitstatus=exitstatus)
        terminalreporter.write_sep("-", "layerstack squash verdict summary")
        terminalreporter.write_line(f"squash verdict summary: {summary_path}")

    if _is_export_run(config):
        from manager.management.export import helpers as export_helpers

        summary_path = export_helpers.finalize_summary(exitstatus=exitstatus)
        if summary_path is not None:
            terminalreporter.write_sep("-", "manager export verdict summary")
            terminalreporter.write_line(f"export verdict summary: {summary_path}")

    records = operation_timing_records()
    if not records:
        return

    summary = operation_timing_summary()
    paths = _write_operation_timing_artifacts(records, summary, exitstatus)
    terminalreporter.write_sep("-", "sandbox-cli operation timing metrics")
    terminalreporter.write_line(
        "durations are client-side sandbox-cli wall time; no timing SLO is enforced"
    )
    for row in summary:
        terminalreporter.write_line(
            f"{row['operation']}: n={row['count']} "
            f"p50={row['p50_ms']:.1f}ms p95={row['p95_ms']:.1f}ms "
            f"max={row['max_ms']:.1f}ms "
            f"sub50={row['sub_50ms_pct']:.1f}% "
            f"sub100={row['sub_100ms_pct']:.1f}% "
            f"sub200={row['sub_200ms_pct']:.1f}%"
        )
    terminalreporter.write_line(f"operation timing metrics: {paths['markdown']}")


def _write_operation_timing_artifacts(records, summary, exitstatus):
    metrics_dir = Path(
        os.environ.get(
            "E2E_OP_METRICS_DIR",
            REPO_ROOT
            / "docs"
            / "obsidian"
            / "ephemeral-os"
            / "testing"
            / "file-operation"
            / "operation-timing",
        )
    )
    metrics_dir.mkdir(parents=True, exist_ok=True)

    generated_at = _dt.datetime.now().astimezone().isoformat(timespec="seconds")
    payload = {
        "schema_version": 2,
        "generated_at": generated_at,
        "argv": sys.argv,
        "exitstatus": exitstatus,
        "environment": {
            name: os.environ[name]
            for name in ("E2E_IMAGE", "E2E_WORKSPACE_ROOT", "E2E_PROGRESS")
            if name in os.environ
        },
        "record_count": len(records),
        "summary": summary,
        "records": records,
    }

    json_path = metrics_dir / "latest.json"
    markdown_path = metrics_dir / "latest.md"
    json_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")
    markdown_path.write_text(_operation_timing_markdown(payload))
    return {"json": json_path, "markdown": markdown_path}


def _operation_timing_markdown(payload):
    rows = [
        "# Sandbox CLI Operation Timing",
        "",
        f"- Generated: `{payload['generated_at']}`",
        f"- Command: `{' '.join(payload['argv'])}`",
        f"- Exit status: `{payload['exitstatus']}`",
        f"- CLI calls measured: `{payload['record_count']}`",
        "- Durations are client-side `sandbox-cli` wall time.",
        "- `sub50`/`sub100`/`sub200` are measurement only; the suite does not enforce a timing SLO.",
        "",
        "| Operation | Count | Min ms | P50 ms | P95 ms | Max ms | Sub50 | Sub100 | Sub200 | CLI errors |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for row in payload["summary"]:
        rows.append(
            f"| `{row['operation']}` | {row['count']} | {row['min_ms']:.1f} | "
            f"{row['p50_ms']:.1f} | {row['p95_ms']:.1f} | {row['max_ms']:.1f} | "
            f"{row['sub_50ms_pct']:.1f}% | {row['sub_100ms_pct']:.1f}% | "
            f"{row['sub_200ms_pct']:.1f}% | {row['cli_error_count']} |"
        )
    rows.append("")
    return "\n".join(rows)


def _is_squash_run(config):
    args = " ".join(map(str, getattr(config, "args", ()) or ()))
    markexpr = getattr(config.option, "markexpr", "") or ""
    return "manager/management/squash" in args or "squash" in markexpr


def _is_export_run(config):
    args = " ".join(map(str, getattr(config, "args", ()) or ()))
    markexpr = getattr(config.option, "markexpr", "") or ""
    return "manager/management/export" in args or "export" in markexpr


@pytest.fixture(scope="session", autouse=True)
def gateway_up():
    """Ensure a gateway is running before any test (reused across the session)."""
    gateway.ensure_up()


@pytest.fixture(scope="session", autouse=True)
def _session_sandbox_cleanup(gateway_up):
    """Safety net: destroy any sandbox the suite created but a test leaked.

    Per-test fixtures already tear down their own sandboxes; this catches inline
    creates that failed before cleanup. Only suite-created ids are touched.
    """
    yield
    for sandbox_id in cleanup.drain():
        try:
            mgmt.destroy_sandbox(sandbox_id)
        except Exception:
            pass


@pytest.fixture
def sandbox():
    """A ready sandbox, destroyed on teardown. Yields the sandbox id."""
    created = mgmt.create_sandbox()
    sandbox_id = created.get("id")
    assert sandbox_id, f"create_sandbox failed: {created}"
    try:
        yield sandbox_id
    finally:
        mgmt.destroy_sandbox(sandbox_id)


@pytest.fixture(scope="session")
def squash_preconditions(gateway_up):
    from manager.management.squash import helpers as squash_helpers

    squash_helpers.assert_preconditions_once()


@pytest.fixture(scope="session")
def export_preconditions(gateway_up):
    from manager.management.export import helpers as export_helpers

    export_helpers.assert_preconditions_once()


@pytest.fixture
def squash_sandbox_factory(tmp_path):
    from manager.management.squash import helpers as squash_helpers

    created = []

    def create(rec):
        workspace = tmp_path / rec.case_id.lower()
        workspace.mkdir(parents=True, exist_ok=True)
        squash_helpers.prepare_workspace_for_case(rec.case, workspace)
        sandbox_id = squash_helpers.create_sandbox(rec, str(workspace))
        created.append((sandbox_id, rec))
        return sandbox_id

    yield create

    for sandbox_id, rec in reversed(created):
        try:
            squash_helpers.harvest_observability(rec, sandbox_id)
        except Exception:
            pass
        try:
            squash_helpers.destroy_sandbox(rec, sandbox_id)
        except Exception:
            pass
