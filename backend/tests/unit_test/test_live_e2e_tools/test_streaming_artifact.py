"""Unit tests for the shared streaming-artifact helpers."""

from __future__ import annotations

import json
import os

from tests.live_e2e_test.sandbox._harness import streaming_artifact as sa


def test_resolve_run_id_honors_env(monkeypatch):
    monkeypatch.setenv("EOS_TIER_RUN_ID", "tier-run-42")
    assert sa.resolve_run_id() == "tier-run-42"


def test_resolve_run_id_falls_back_to_timestamp_pid(monkeypatch):
    monkeypatch.delenv("EOS_TIER_RUN_ID", raising=False)
    rid = sa.resolve_run_id()
    assert "-" in rid
    assert rid.endswith(f"-{os.getpid()}")
    assert rid[:8].isdigit()  # YYYYMMDD prefix


def test_stream_row_appends_and_fsyncs(tmp_path):
    artifact = tmp_path / "art.jsonl"
    sa.stream_row(artifact, {"schema": "x.v1", "cell": "a", "passed": True})
    sa.stream_row(artifact, {"schema": "x.v1", "cell": "b", "passed": False})
    lines = artifact.read_text().splitlines()
    assert len(lines) == 2
    assert json.loads(lines[0])["cell"] == "a"
    assert json.loads(lines[1])["passed"] is False


def test_load_prior_data_rows_skips_summary(tmp_path):
    artifact = tmp_path / "art.jsonl"
    artifact.write_text(
        "\n".join(
            [
                json.dumps({"schema": "x.v1", "cell": "a", "passed": True}),
                json.dumps({"schema": "x.v1", "cell": "b", "passed": False}),
                json.dumps({"schema": "x.summary.v1", "failed_cells": 1}),
            ]
        )
        + "\n"
    )
    rows = sa.load_prior_data_rows(artifact)
    assert len(rows) == 2
    assert [r["cell"] for r in rows] == ["a", "b"]


def test_load_prior_data_rows_handles_missing_file(tmp_path):
    artifact = tmp_path / "missing.jsonl"
    assert sa.load_prior_data_rows(artifact) == []


def test_load_prior_data_rows_skips_corrupt_lines(tmp_path):
    artifact = tmp_path / "art.jsonl"
    artifact.write_text(
        "\n".join(
            [
                json.dumps({"schema": "x.v1", "cell": "a", "passed": True}),
                "not json at all",
                json.dumps({"schema": "x.v1", "cell": "b", "passed": True}),
            ]
        )
        + "\n"
    )
    rows = sa.load_prior_data_rows(artifact)
    assert [r["cell"] for r in rows] == ["a", "b"]


def test_rewrite_artifact_writes_rows_then_summary(tmp_path):
    artifact = tmp_path / "art.jsonl"
    artifact.write_text("stale content\n")
    sa.rewrite_artifact(
        artifact,
        rows=[{"cell": "a"}, {"cell": "b"}],
        summary_row={"schema": "x.summary.v1", "passed": 2},
    )
    lines = artifact.read_text().splitlines()
    assert len(lines) == 3
    assert json.loads(lines[0]) == {"cell": "a"}
    assert json.loads(lines[2]) == {"schema": "x.summary.v1", "passed": 2}


def test_rewrite_artifact_omits_summary_when_none(tmp_path):
    artifact = tmp_path / "art.jsonl"
    sa.rewrite_artifact(artifact, rows=[{"cell": "a"}], summary_row=None)
    lines = artifact.read_text().splitlines()
    assert len(lines) == 1
    assert json.loads(lines[0]) == {"cell": "a"}
