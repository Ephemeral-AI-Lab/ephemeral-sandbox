"""Provider-neutral ``ensure_git`` operation.

Body lifted from ``SandboxProxy.ensure_git`` and rewritten to use
the registered provider adapter instead of the SDK's ``process.exec``. No SDK
or daytona-package imports.
"""

from __future__ import annotations

import logging

logger = logging.getLogger(__name__)


_GIT_BOOTSTRAP = r"""
set -e
if command -v git >/dev/null 2>&1; then exit 0; fi
echo "[sandbox] Installing git..."
as_root() {
    if [ "$(id -u)" = "0" ]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo -n "$@"
    else
        return 1
    fi
}
if command -v apt-get >/dev/null 2>&1; then
    as_root mkdir -p /var/lib/apt/lists/partial
    as_root apt-get update -qq && as_root apt-get install -y -qq git
elif command -v apk >/dev/null 2>&1; then
    as_root apk add --no-cache git
elif command -v microdnf >/dev/null 2>&1; then
    as_root microdnf install -y git
elif command -v dnf >/dev/null 2>&1; then
    as_root dnf install -y git
elif command -v yum >/dev/null 2>&1; then
    as_root yum install -y git
else
    echo "[sandbox] No package manager found — git not installed" >&2
    exit 1
fi
echo "[sandbox] git installed"
"""


def ensure_git(sandbox_id: str) -> None:
    """Install git in the sandbox if missing.

    Best-effort: failures are logged but not raised — most code paths can
    still operate without git, and a hard failure here would block sandbox
    creation entirely.
    """
    if not sandbox_id:
        return
    try:
        from sandbox.async_bridge import run_sync
        from sandbox.provider.registry import get_adapter

        adapter = get_adapter(sandbox_id)
        logger.info("ensure_git(%s): probe starting", sandbox_id)
        resp = run_sync(
            adapter.exec(
                sandbox_id,
                "command -v git >/dev/null 2>&1 && echo ok || echo missing",
                timeout=10,
            )
        )
        if "ok" in (resp.stdout or ""):
            logger.info("ensure_git(%s): git already available", sandbox_id)
            return
        logger.info("ensure_git(%s): installing git", sandbox_id)
        install = run_sync(adapter.exec(sandbox_id, _GIT_BOOTSTRAP, timeout=120))
        if getattr(install, "exit_code", 1) not in (0, None):
            raise RuntimeError(
                getattr(install, "stderr", "")
                or getattr(install, "stdout", "")
                or "git install failed"
            )
        logger.info("ensure_git(%s): install completed", sandbox_id)
    except RuntimeError as exc:
        # Expected "best-effort" failure: git is unavailable and no
        # package manager can install it. Most code paths cope without
        # git; we keep going. Log loudly enough for diagnostics.
        logger.warning(
            "Git bootstrap failed for sandbox %s: %s", sandbox_id, exc
        )
    except Exception:
        # WR-04: anything else (adapter unreachable, import failure,
        # provider config issue) is NOT a "git is best-effort missing"
        # condition — it indicates the sandbox itself is broken and
        # downstream setup will fail confusingly. Surface the original
        # error to the caller rather than silently swallowing.
        logger.exception(
            "Git bootstrap unexpectedly failed for sandbox %s; "
            "propagating to caller", sandbox_id,
        )
        raise


__all__ = ["ensure_git"]
