"""Persistent Jedi worker process.

Runs as a long-lived subprocess alongside the :class:`LspClient` and
amortises Jedi's cold-import cost across every ``_python_*`` call. It can
communicate with the parent over stdio or, when launched inside a sandbox,
over a Unix-domain socket.

Protocol
--------

Request::

    {"id": "<opaque>", "op": "<name>", "args": {...}}

Response::

    {"id": "<same>", "ok": true|false, "result": <any>, "error": "<msg>|null"}

Supported ops: ``ping``, ``definitions``, ``references``, ``rename``,
``hover``, ``invalidate``, ``shutdown``.

Scope
-----

* Single in-flight request per client (Jedi's ``Project`` cache is not
  safe across concurrent ``Script`` calls rooted at the same project).
* No shadow mode, no generation-keyed cache, no N-request restart ceiling
  yet — those belong to a second increment once this path has broader
  shadow-traffic coverage.
"""

from __future__ import annotations

import json
import os
import socket
import sys
import traceback
from typing import Any


def _safe_import_jedi():
    try:
        import jedi  # type: ignore[import-untyped]
    except Exception as exc:
        return None, str(exc)
    return jedi, None


def _make_script(jedi_mod, project, path: str):
    return jedi_mod.Script(path=path, project=project)


def _op_ping(_jedi, _project, _args: dict[str, Any]) -> Any:
    return {"pong": True}


def _op_definitions(jedi_mod, project, args: dict[str, Any]) -> Any:
    path = str(args["path"])
    line = int(args["line"])
    column = int(args["column"])
    script = _make_script(jedi_mod, project, path)
    out = []
    for name in script.goto(line=line, column=column, follow_imports=True):
        out.append(
            {
                "name": getattr(name, "name", ""),
                "type": getattr(name, "type", ""),
                "module_path": str(getattr(name, "module_path", "") or ""),
                "line": int(getattr(name, "line", 0) or 0),
                "column": int(getattr(name, "column", 0) or 0),
                "description": getattr(name, "description", "") or "",
            }
        )
    return out


def _op_references(jedi_mod, project, args: dict[str, Any]) -> Any:
    path = str(args["path"])
    line = int(args["line"])
    column = int(args["column"])
    script = _make_script(jedi_mod, project, path)
    out = []
    for name in script.get_references(line=line, column=column, include_builtins=False):
        out.append(
            {
                "name": getattr(name, "name", ""),
                "module_path": str(getattr(name, "module_path", "") or ""),
                "line": int(getattr(name, "line", 0) or 0),
                "column": int(getattr(name, "column", 0) or 0),
            }
        )
    return out


def _op_rename(jedi_mod, _project, args: dict[str, Any]) -> Any:
    path = str(args["path"])
    line = int(args["line"])
    column = int(args["column"])
    new_name = str(args["new_name"])
    # Match the subprocess-per-call path for rename. In live Daytona runs,
    # forcing the shared Project here caused sporadic multi-second cold
    # rename plans for otherwise tiny workspaces; Jedi can infer the project
    # from an absolute path and still benefits from the hot worker import.
    script = _make_script(jedi_mod, None, path)
    refactoring = script.rename(line=line, column=column, new_name=new_name)
    out: dict[str, str] = {}
    for p, cf in refactoring.get_changed_files().items():
        try:
            out[str(p)] = cf.get_new_code()
        except Exception:  # pragma: no cover - per-file degradation
            continue
    return out


def _op_hover(jedi_mod, project, args: dict[str, Any]) -> Any:
    path = str(args["path"])
    line = int(args["line"])
    column = int(args["column"])
    script = _make_script(jedi_mod, project, path)
    names = script.help(line=line, column=column)
    if not names:
        return None
    n = names[0]
    sigs = script.get_signatures(line=line, column=column)
    sig = str(sigs[0]) if sigs else ""
    return {
        "name": getattr(n, "name", ""),
        "type": getattr(n, "type", ""),
        "docstring": (n.docstring() or "")[:500],
        "signature": sig,
    }


def _op_invalidate(_jedi, project, args: dict[str, Any]) -> Any:
    path = str(args.get("path", ""))
    if not path or project is None:
        return {"invalidated": False}
    try:
        state = getattr(project, "_inference_state", None)
        module_cache = getattr(state, "module_cache", None) if state else None
        if module_cache is not None and hasattr(module_cache, "delete"):
            module_cache.delete(path)
            return {"invalidated": True}
    except Exception:
        pass
    return {"invalidated": False}


_DISPATCH = {
    "ping": _op_ping,
    "definitions": _op_definitions,
    "references": _op_references,
    "rename": _op_rename,
    "hover": _op_hover,
    "invalidate": _op_invalidate,
}


def _load_backend(workspace_root: str):
    jedi_mod, import_err = _safe_import_jedi()
    project = None
    if jedi_mod is not None and workspace_root:
        try:
            project = jedi_mod.Project(path=workspace_root)
        except Exception as exc:
            jedi_mod = None
            import_err = str(exc)
    return jedi_mod, project, import_err


def _response(req_id: str, *, ok: bool, result: Any = None, error: str | None = None) -> dict[str, Any]:
    return {"id": req_id, "ok": ok, "result": result, "error": error}


def _handle_request(
    req: dict[str, Any],
    *,
    jedi_mod: Any,
    project: Any,
    import_err: str | None,
) -> tuple[dict[str, Any], bool]:
    req_id = str(req.get("id", ""))
    op = str(req.get("op", ""))
    args = req.get("args") or {}
    if op == "shutdown":
        return _response(req_id, ok=True, result={"bye": True}), True
    if jedi_mod is None and op != "ping":
        return _response(req_id, ok=False, error=f"jedi_unavailable: {import_err}"), False
    handler = _DISPATCH.get(op)
    if handler is None:
        return _response(req_id, ok=False, error=f"unknown_op: {op}"), False
    try:
        result = handler(jedi_mod, project, args)
        return _response(req_id, ok=True, result=result), False
    except Exception as exc:
        return (
            _response(
                req_id,
                ok=False,
                error=f"{type(exc).__name__}: {exc}",
                result={"trace": traceback.format_exc(limit=5)},
            ),
            False,
        )


def _write_response(writer: Any, payload: dict[str, Any]) -> None:
    writer.write(json.dumps(payload) + "\n")
    writer.flush()


def _serve_stdio(workspace_root: str) -> int:
    jedi_mod, project, import_err = _load_backend(workspace_root)
    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except Exception as exc:
            _write_response(sys.stdout, _response("", ok=False, error=f"json_decode: {exc}"))
            continue
        response, should_stop = _handle_request(
            req,
            jedi_mod=jedi_mod,
            project=project,
            import_err=import_err,
        )
        _write_response(sys.stdout, response)
        if should_stop:
            break
    return 0


def _serve_socket(socket_path: str, workspace_root: str) -> int:
    """Serve one newline-delimited JSON request per Unix socket connection."""
    os.makedirs(os.path.dirname(socket_path) or ".", exist_ok=True)
    try:
        os.unlink(socket_path)
    except FileNotFoundError:
        pass

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        server.bind(socket_path)
        os.chmod(socket_path, 0o600)
        server.listen(20)
        jedi_mod, project, import_err = _load_backend(workspace_root)
        while True:
            conn, _ = server.accept()
            should_stop = False
            with conn:
                reader = conn.makefile("r", encoding="utf-8")
                writer = conn.makefile("w", encoding="utf-8")
                raw = reader.readline()
                try:
                    req = json.loads(raw.strip() or "{}")
                except Exception as exc:
                    response = _response("", ok=False, error=f"json_decode: {exc}")
                else:
                    response, should_stop = _handle_request(
                        req,
                        jedi_mod=jedi_mod,
                        project=project,
                        import_err=import_err,
                    )
                _write_response(writer, response)
            if should_stop:
                break
    finally:
        try:
            server.close()
        finally:
            try:
                os.unlink(socket_path)
            except FileNotFoundError:
                pass
    return 0


def main(argv: list[str]) -> int:
    if len(argv) >= 4 and argv[1] == "--socket":
        return _serve_socket(argv[2], argv[3])
    workspace_root = argv[1] if len(argv) > 1 else ""
    return _serve_stdio(workspace_root)


if __name__ == "__main__":  # pragma: no cover - executed as subprocess
    sys.exit(main(sys.argv))
