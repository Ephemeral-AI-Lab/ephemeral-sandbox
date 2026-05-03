"""Entrypoint for ``python -m sandbox.code_intelligence.daemon``."""

from __future__ import annotations

import argparse
import asyncio
import logging
import sys

from sandbox.code_intelligence.daemon.server import (
    DaemonAlreadyRunning,
    run_daemon,
)
from sandbox.code_intelligence.daemon.storage import StorageUnavailable


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="sandbox.code_intelligence.daemon")
    parser.add_argument("--workspace-root", required=True)
    parser.add_argument("--log-level", default="INFO")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    logging.basicConfig(
        level=getattr(logging, str(args.log_level).upper(), logging.INFO)
    )
    try:
        asyncio.run(run_daemon(args.workspace_root))
    except StorageUnavailable as exc:
        logging.error(
            "storage unavailable: errno=%s path=%s message=%s",
            exc.errno,
            exc.path,
            exc.message,
        )
        return 13
    except DaemonAlreadyRunning as exc:
        logging.error("%s", exc)
        return exc.exit_code
    except KeyboardInterrupt:
        return 0
    return 0


if __name__ == "__main__":  # pragma: no cover - exercised by subprocess/live
    sys.exit(main())
