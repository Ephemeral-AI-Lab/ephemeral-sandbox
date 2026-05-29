"""Workflow package.

Workflow DTOs/enums live in :mod:`task_center.workflow.state`; lifecycle, ancestry,
closure-report routing, and goal-start sequencing live in their dedicated
submodules (``lifecycle``, ``ancestry``, ``closure_report_router``,
``starter``). Callers import from the canonical submodule path; the package
root deliberately re-exports nothing.
"""
