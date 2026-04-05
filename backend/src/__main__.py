"""Allow running as ``python -m ephemeralos``."""

import asyncio
import os

from ephemeralos.server.entrypoint import run_web

if __name__ == "__main__":
    dev = os.environ.get("EPHEMERALOS_DEV", "").lower() in ("1", "true")
    asyncio.run(run_web(open_browser=False, reload=dev))
