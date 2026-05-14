"""Configuration defaults used by public sandbox APIs."""

from __future__ import annotations


def configured_sandbox_defaults() -> tuple[str | None, str | None]:
    from config import load_settings

    sandbox = load_settings().sandbox
    snapshot = sandbox.default_snapshot.strip()
    image = sandbox.default_image.strip()
    return snapshot or None, image or None


__all__ = ["configured_sandbox_defaults"]
