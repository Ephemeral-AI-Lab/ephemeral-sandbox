"""Measurement and report writing for the LayerStack squash live suite."""

from __future__ import annotations

import atexit
import datetime as dt
import json
import os
import statistics
import sys
import threading
import time
import traceback
from pathlib import Path

from core.root import REPO_ROOT

SUITE_DIR = Path(__file__).resolve().parent
DOC_ROOT = (
    REPO_ROOT
    / "docs"
    / "obsidian"
    / "ephemeral-os"
    / "implementation_plan"
    / "squash"
)
RUN_ID = os.environ.get(
    "SQUASH_RUN_ID", dt.datetime.now().strftime("squash-%Y%m%d-%H%M%S")
)
REPORT_ROOT = SUITE_DIR / "test-reports" / RUN_ID
ITERATION_REPORT = DOC_ROOT / "iteration-report.md"

_lock = threading.Lock()
_started_at = time.monotonic()
_iteration_started = False
_finalized = False


def now_iso():
    return dt.datetime.now().astimezone().isoformat(timespec="seconds")


def monotonic_ms(start):
    return round((time.monotonic() - start) * 1000.0, 3)


def ensure_report_root():
    REPORT_ROOT.mkdir(parents=True, exist_ok=True)
    return REPORT_ROOT


def start_iteration_report(scope="squash"):
    global _iteration_started
    with _lock:
        if _iteration_started:
            return
        _iteration_started = True
        ensure_report_root()
        ITERATION_REPORT.parent.mkdir(parents=True, exist_ok=True)
        if not ITERATION_REPORT.exists():
            ITERATION_REPORT.write_text("# Squash Iteration Report\n\n", encoding="utf-8")
        with ITERATION_REPORT.open("a", encoding="utf-8") as fh:
            fh.write(
                f"## {RUN_ID} START — {now_iso()}\n"
                f"- Command/tier: `{' '.join(sys.argv)}` ({scope})\n"
                "- Cases run/passed/failed/skipped: pending\n"
                "- Wall time: pending\n"
                "- Description: live e2e run started; per-case verdicts will be "
                f"written under `{REPORT_ROOT}`.\n\n"
            )


class CaseRecorder:
    def __init__(self, case):
        self.case = dict(case)
        self.case_id = self.case["id"]
        self.case_dir = ensure_report_root() / self.case_id
        self.commands = []
        self.timers = {}
        self.snapshots = {}
        self.axes = {
            "correctness": {"pass": False, "status": "not_run", "details": ""},
            "space": {"pass": False, "status": "not_run", "details": ""},
            "time": {"pass": False, "status": "not_run", "details": ""},
        }
        self.teardown = {"pass": False, "details": "not checked"}
        self.notes = []
        self.errors = []
        self.started = None
        self.verdict = None

    def __enter__(self):
        start_iteration_report(self.case.get("tier", "squash"))
        self.started = time.monotonic()
        self.case_dir.mkdir(parents=True, exist_ok=True)
        self.write_json("case.json", self.case)
        return self

    def __exit__(self, exc_type, exc, tb):
        if exc is not None:
            self.errors.append(
                {
                    "type": exc_type.__name__ if exc_type else "Exception",
                    "message": str(exc),
                    "traceback": "".join(traceback.format_exception(exc_type, exc, tb)),
                }
            )
            if self.axes["correctness"]["status"] == "not_run":
                self.axis("correctness", False, str(exc))
        if self.verdict is None:
            self.write_verdict()
        return False

    def write_json(self, name, payload):
        path = self.case_dir / name
        path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        return path

    def write_text(self, name, text):
        path = self.case_dir / name
        path.write_text(text, encoding="utf-8")
        return path

    def add_command(self, record):
        self.commands.append(record)

    def add_timer(self, name, value_ms, source="harness"):
        self.timers[name] = {"ms": round(float(value_ms), 3), "source": source}
        self.write_json("timers.json", self.timers)

    def add_snapshot(self, label, snapshot):
        self.snapshots[label] = snapshot
        self.write_json(f"snapshot-{label}.json", snapshot)

    def axis(self, name, passed, details="", *, status=None, metrics=None, n_a=False):
        if status is None:
            status = "pass" if passed else "fail"
        self.axes[name] = {
            "pass": bool(passed),
            "status": status,
            "details": details,
            "n_a": bool(n_a),
            "metrics": metrics or {},
        }

    def note(self, message):
        self.notes.append(message)
        self.write_json("notes.json", self.notes)

    def set_teardown(self, passed, details, metrics=None):
        self.teardown = {
            "pass": bool(passed),
            "details": details,
            "metrics": metrics or {},
        }

    def write_verdict(self):
        elapsed = monotonic_ms(self.started or time.monotonic())
        self.add_timer("T_e2e", elapsed, "harness")
        self.write_json("cmd.log.json", self.commands)
        axes_pass = all(axis.get("pass") for axis in self.axes.values())
        slow = any(axis.get("status") == "SLOW" for axis in self.axes.values())
        skipped = any(axis.get("status", "").startswith("skipped:") for axis in self.axes.values())
        status = "PASS" if axes_pass else "FAIL"
        if skipped:
            status = "SKIP"
        elif slow and axes_pass:
            status = "SLOW"
        self.verdict = {
            "schema_version": 1,
            "run_id": RUN_ID,
            "case_id": self.case_id,
            "tier": self.case.get("tier"),
            "title": self.case.get("title"),
            "status": status,
            "axes": self.axes,
            "teardown": self.teardown,
            "timers": self.timers,
            "notes": self.notes,
            "errors": self.errors,
            "artifacts": {
                "case_dir": str(self.case_dir),
                "commands": "cmd.log.json",
                "snapshots": sorted(self.snapshots),
            },
            "generated_at": now_iso(),
        }
        self.write_json("verdict.json", self.verdict)
        return self.verdict


def record_case(case):
    return CaseRecorder(case)


def _read_verdicts():
    verdicts = []
    if not REPORT_ROOT.exists():
        return verdicts
    for path in sorted(REPORT_ROOT.glob("*/verdict.json")):
        try:
            verdicts.append(json.loads(path.read_text(encoding="utf-8")))
        except (OSError, json.JSONDecodeError):
            continue
    return verdicts


def finalize_summary(exitstatus=None):
    global _finalized
    with _lock:
        if _finalized:
            return REPORT_ROOT / "SUMMARY.md"
        _finalized = True
        ensure_report_root()
        verdicts = _read_verdicts()
        counts = {
            "PASS": 0,
            "SLOW": 0,
            "FAIL": 0,
            "SKIP": 0,
        }
        for verdict in verdicts:
            counts[verdict.get("status", "FAIL")] = counts.get(verdict.get("status", "FAIL"), 0) + 1
        timing = _timing_distribution(verdicts)
        summary_path = REPORT_ROOT / "SUMMARY.md"
        summary_path.write_text(
            _summary_markdown(verdicts, counts, timing, exitstatus),
            encoding="utf-8",
        )
        (REPORT_ROOT / "timing-distribution.json").write_text(
            json.dumps(timing, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        _write_soak_baseline(verdicts, timing)
        _append_final_iteration(counts, len(verdicts), exitstatus)
        return summary_path


def _timing_distribution(verdicts):
    grouped = {}
    for verdict in verdicts:
        for name, record in verdict.get("timers", {}).items():
            grouped.setdefault(name, []).append(float(record["ms"]))
    rows = {}
    for name, values in grouped.items():
        values = sorted(values)
        rows[name] = {
            "count": len(values),
            "min_ms": values[0],
            "p50_ms": _percentile(values, 0.50),
            "p95_ms": _percentile(values, 0.95),
            "max_ms": values[-1],
            "mean_ms": round(statistics.mean(values), 3),
        }
    return rows


def _percentile(values, quantile):
    if len(values) == 1:
        return values[0]
    rank = (len(values) - 1) * quantile
    lower = int(rank)
    upper = min(lower + 1, len(values) - 1)
    weight = rank - lower
    return round(values[lower] * (1.0 - weight) + values[upper] * weight, 3)


def _summary_markdown(verdicts, counts, timing, exitstatus):
    rows = [
        "# LayerStack Squash Live-Docker Summary",
        "",
        f"- Run id: `{RUN_ID}`",
        f"- Generated: `{now_iso()}`",
        f"- Pytest exit status: `{exitstatus}`",
        f"- Cases: `{len(verdicts)}` run · `{counts.get('PASS', 0)}` pass · "
        f"`{counts.get('SLOW', 0)}` slow · `{counts.get('FAIL', 0)}` fail · "
        f"`{counts.get('SKIP', 0)}` skipped",
        "",
        "| Case | Tier | Status | Correctness | Space | Time | Notes |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]
    for verdict in verdicts:
        axes = verdict.get("axes", {})
        notes = _notes_cell(verdict.get("notes", []))
        rows.append(
            f"| `{verdict.get('case_id')}` | {verdict.get('tier')} | "
            f"{verdict.get('status')} | {_axis_cell(axes.get('correctness', {}))} | "
            f"{_axis_cell(axes.get('space', {}))} | {_axis_cell(axes.get('time', {}))} | "
            f"{notes} |"
        )
    allowed = [
        (verdict.get("case_id"), note)
        for verdict in verdicts
        for note in verdict.get("notes", [])
        if str(note).startswith("skipped:")
    ]
    if allowed:
        rows.extend(["", "## Allowed Partial Skips", ""])
        for case_id, note in allowed:
            rows.append(f"- `{case_id}`: `{note}`")
    rows.extend(["", "## Timing Distributions", ""])
    rows.append("| Timer | Count | P50 ms | P95 ms | Max ms |")
    rows.append("| --- | ---: | ---: | ---: | ---: |")
    for name, record in sorted(timing.items()):
        rows.append(
            f"| `{name}` | {record['count']} | {record['p50_ms']:.3f} | "
            f"{record['p95_ms']:.3f} | {record['max_ms']:.3f} |"
        )
    rows.append("")
    return "\n".join(rows)


def _axis_cell(axis):
    if axis.get("n_a"):
        return "n/a"
    return "pass" if axis.get("pass") else f"fail: {axis.get('details', '')}"


def _notes_cell(notes):
    selected = [str(note) for note in notes if str(note).startswith("skipped:")]
    if not selected:
        return ""
    return "<br>".join(f"`{note}`" for note in selected)


def _write_soak_baseline(verdicts, timing):
    soak = next((v for v in verdicts if v.get("case_id") == "HRD-20"), None)
    if soak is None:
        return
    baseline = {
        "run_id": RUN_ID,
        "case_id": "HRD-20",
        "status": soak.get("status"),
        "generated_at": now_iso(),
        "timing": timing,
    }
    (REPORT_ROOT / "soak-baseline.json").write_text(
        json.dumps(baseline, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def _append_final_iteration(counts, total, exitstatus):
    if not _iteration_started:
        return
    wall = monotonic_ms(_started_at)
    with ITERATION_REPORT.open("a", encoding="utf-8") as fh:
        fh.write(
            f"## {RUN_ID} FINAL — {now_iso()}\n"
            f"- Command/tier: `{' '.join(sys.argv)}`\n"
            f"- Cases run/passed/failed/skipped: {total}/"
            f"{counts.get('PASS', 0) + counts.get('SLOW', 0)}/"
            f"{counts.get('FAIL', 0)}/{counts.get('SKIP', 0)}\n"
            f"- Wall time: {wall:.3f} ms\n"
            f"- Description: suite completed with pytest exit status `{exitstatus}`; "
            f"artifacts in `{REPORT_ROOT}`.\n\n"
        )


atexit.register(lambda: finalize_summary(exitstatus="atexit"))
