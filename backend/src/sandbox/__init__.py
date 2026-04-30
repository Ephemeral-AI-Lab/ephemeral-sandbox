"""Sandbox package — Daytona transport, lifecycle, code-intelligence, testing.

Sub-packages:
- ``sandbox.client``           — Daytona sync/async client lifecycle and shutdown
- ``sandbox.lifecycle``        — :class:`SandboxService`, :class:`SandboxProxy`,
                                  context preparation, workspace discovery, and
                                  shell/commit submission helpers
- ``sandbox.daytona``          — bash/exec/path/recovery primitives
- ``sandbox.code_intelligence``— per-sandbox CI service
- ``sandbox.testing``          — sandbox factories and eval-file fixtures
- ``sandbox.errors``           — :class:`DaytonaUnavailableError`,
                                  :class:`AsyncDaytonaUnavailableError`

Import directly from sub-packages — this top-level ``__init__`` intentionally
re-exports nothing.
"""
