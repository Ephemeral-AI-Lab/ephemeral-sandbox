"""Sandbox package — public API, host, runtime daemon, and provider.

Sub-packages:
- ``sandbox.api``      — public verbs (lifecycle, read/write/edit/shell, raw_exec)
- ``sandbox.host``     — orchestrator-side setup, daemon client, and recovery
- ``sandbox.provider`` — provider adapter registry and provider implementations
- ``sandbox.runtime.daemon`` — in-sandbox dispatcher and services

Import directly from sub-packages — this top-level ``__init__`` intentionally
re-exports nothing.
"""
