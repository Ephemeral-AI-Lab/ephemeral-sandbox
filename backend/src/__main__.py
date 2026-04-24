"""Entry point for the backend server."""

import asyncio
import os

from server.entrypoint import run_web
from team.definitions import register_all as _register_team_builtins

_register_team_builtins()

if __name__ == "__main__":
    dev = os.environ.get("EPHEMERALOS_DEV", "").lower() in ("1", "true")
    asyncio.run(run_web(open_browser=False, reload=dev))
