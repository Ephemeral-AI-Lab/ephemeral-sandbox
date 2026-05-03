"""Sandbox package — Daytona transport, lifecycle, runtime, testing.

Sub-packages:
- ``sandbox.client``           — Daytona sync/async client lifecycle and shutdown
- ``sandbox.lifecycle``        — :class:`SandboxService`, :class:`SandboxProxy`,
                                  context preparation, workspace discovery, and
                                  shell/commit submission helpers
- ``sandbox.daytona``          — Daytona transport/provider primitives
- ``sandbox.runtime``          — in-sandbox runtime bundle and service adapters
- ``sandbox.testing``          — sandbox factories and eval-file fixtures
- ``sandbox.errors``           — :class:`DaytonaUnavailableError`,
                                  :class:`AsyncDaytonaUnavailableError`

Import directly from sub-packages — this top-level ``__init__`` intentionally
re-exports nothing.
"""
