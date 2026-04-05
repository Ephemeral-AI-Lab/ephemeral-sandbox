"""Web server entry points."""

from __future__ import annotations


async def run_web(
    *,
    host: str = "127.0.0.1",
    port: int = 8420,
    cwd: str | None = None,
    model: str | None = None,
    base_url: str | None = None,
    system_prompt: str | None = None,
    api_key: str | None = None,
    api_format: str | None = None,
    open_browser: bool = True,
    restore_messages: list[dict] | None = None,
    reload: bool = False,
) -> None:
    """Start the web frontend server."""
    import os

    from ephemeralos.server.app_factory import WebServer

    if cwd:
        os.chdir(cwd)

    server = WebServer(
        host=host,
        port=port,
        model=model,
        base_url=base_url,
        system_prompt=system_prompt,
        api_key=api_key,
        api_format=api_format,
        restore_messages=restore_messages,
    )

    url = f"http://{host}:{port}"
    print(f"EphemeralOS web UI: {url}")

    if open_browser:
        import webbrowser

        webbrowser.open(url)

    await server.start(reload=reload)

