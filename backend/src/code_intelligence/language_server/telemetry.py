"""Telemetry counters for language-server clients."""

from __future__ import annotations

import threading

from code_intelligence.language_server.models import LspTelemetry


class LspTelemetryMixin:
    _counter_lock: threading.Lock
    _telemetry: LspTelemetry

    def _record_query(self) -> None:
        with self._counter_lock:
            self._telemetry.queries += 1

    def _record_success(self) -> None:
        with self._counter_lock:
            self._telemetry.successes += 1

    def _record_error(self) -> None:
        with self._counter_lock:
            self._telemetry.errors += 1

    def _record_script_run(self) -> None:
        with self._counter_lock:
            self._telemetry.script_runs += 1

    def _record_script_success(self) -> None:
        with self._counter_lock:
            self._telemetry.script_successes += 1

    def _record_script_error(self) -> None:
        with self._counter_lock:
            self._telemetry.script_errors += 1
