"""Owns the Pyright language server subprocess and exposes typed query helpers."""

from __future__ import annotations

import ast
import asyncio
import contextlib
import hashlib
import json
import logging
import os
import shutil
import sys
import time
from pathlib import Path
from typing import Any
from urllib.parse import quote, unquote, urlparse

from sandbox.layer_stack.changes import normalize_layer_path
from sandbox.layer_stack.layer_index import build_layer_index, has_ancestor_in
from sandbox.layer_stack.paths import join_layer_path

from plugins.catalog.lsp.runtime.lsp_jsonrpc import (
    JsonRpcError,
    LspJsonRpcClient,
)

__all__ = [
    "PyrightSession",
    "PyrightOverlayRefreshError",
    "PyrightSpawnError",
]


logger = logging.getLogger(__name__)

_CONDA_HOOK = "/opt/miniconda3/etc/profile.d/conda.sh"
_DEFAULT_INIT_TIMEOUT_S = 30.0
_DEFAULT_REQUEST_TIMEOUT_S = 30.0
_REFERENCES_TIMEOUT_S = 5.0
_DIAGNOSTICS_WAIT_S = 5.0
_DIAGNOSTICS_POLL_S = 0.05
_RUNTIME_BUNDLE_ROOT = "/tmp/eos-sandbox-runtime"


class PyrightSpawnError(RuntimeError):
    """Raised when the Pyright language-server subprocess fails to start."""


class PyrightOverlayRefreshError(RuntimeError):
    """Raised when a live Pyright overlay cannot be refreshed in place."""


class PyrightSession:
    """Long-lived Pyright session rooted at a leased workspace overlay."""

    def __init__(
        self,
        *,
        manifest_key: str,
        workspace_root: str,
        overlay_handle: Any | None = None,
    ) -> None:
        self.manifest_key = manifest_key
        self.workspace_root = str(workspace_root or "/testbed").rstrip("/") or "/"
        self._overlay_handle = overlay_handle
        self._uses_private_overlay_namespace = bool(
            getattr(overlay_handle, "layer_paths", None)
        )
        self._overlay_layer_paths = tuple(
            str(path) for path in getattr(overlay_handle, "layer_paths", ()) or ()
        )
        self._layer_index_cache: dict[str, Any] = {}
        self._proc: asyncio.subprocess.Process | None = None
        self._client: LspJsonRpcClient | None = None
        self._opened: set[str] = set()
        self._lock = asyncio.Lock()
        self._started = False
        self._document_versions: dict[str, int] = {}
        self._document_hashes: dict[str, str] = {}
        self._diagnostic_cache: dict[str, list[dict[str, Any]]] = {}
        self.audit_start_count = 0
        self.audit_refresh_count = 0
        self.audit_remount_count = 0
        self.audit_last_start_s = 0.0
        self.audit_last_remount_s = 0.0

    async def refresh_manifest(
        self,
        *,
        manifest_key: str,
        overlay_handle: Any | None = None,
        workspace_root: str | None = None,
    ) -> None:
        """Mark the daemon overlay as refreshed and resync open documents."""
        if manifest_key == self.manifest_key and overlay_handle is None:
            return

        async with self._lock:
            if manifest_key == self.manifest_key and overlay_handle is None:
                return
            if workspace_root is not None:
                normalized_root = str(workspace_root or "/testbed").rstrip("/") or "/"
                if normalized_root != self.workspace_root:
                    raise PyrightOverlayRefreshError(
                        "cannot refresh Pyright session across workspace roots"
                    )
            old_handle = self._overlay_handle
            if overlay_handle is not None:
                await self._refresh_overlay_handle(overlay_handle)
            self.manifest_key = manifest_key
            self._layer_index_cache.clear()
            self._diagnostic_cache.clear()
            if overlay_handle is not None and old_handle is not overlay_handle:
                _release_handle(old_handle)
            self.audit_refresh_count += 1
            await self._notify_workspace_refreshed()

    async def start(self) -> None:
        if self._started:
            return
        async with self._lock:
            if self._started:
                return
            start_s = time.monotonic()
            try:
                await self._spawn()
                await self._initialize()
            except BaseException:
                await self._cleanup_failed_start()
                raise
            self._started = True
            self.audit_start_count += 1
            self.audit_last_start_s = time.monotonic() - start_s

    async def hover(self, args: dict[str, Any]) -> dict[str, Any]:
        await self.start()
        uri = await self._open_document(args["file_path"])
        params = {
            "textDocument": {"uri": uri},
            "position": {
                "line": int(args["line"]),
                "character": int(args["character"]),
            },
        }
        result = await self._send_request("textDocument/hover", params)
        return {"hover": result}

    async def find_definitions(self, args: dict[str, Any]) -> dict[str, Any]:
        await self.start()
        return {
            "definitions": await self._point_query(
                "textDocument/definition", args
            )
        }

    async def find_references(self, args: dict[str, Any]) -> dict[str, Any]:
        await self.start()
        uri = await self._open_document(args["file_path"])
        params = {
            "textDocument": {"uri": uri},
            "position": {
                "line": int(args["line"]),
                "character": int(args["character"]),
            },
            "context": {
                "includeDeclaration": bool(args.get("include_declaration", True))
            },
        }
        timeout_s = _optional_positive_float(
            args.get("timeout_s"),
            default=_REFERENCES_TIMEOUT_S,
        )
        try:
            raw = await asyncio.wait_for(
                self._send_request("textDocument/references", params),
                timeout=timeout_s,
            )
        except TimeoutError:
            await self.evict()
            return {"references": [], "timeout": True}
        return {"references": self._normalize_locations(raw)}

    async def diagnostics(self, args: dict[str, Any]) -> dict[str, Any]:
        await self.start()
        uri = await self._open_document(args["file_path"])
        wait_for_diagnostics = bool(args.get("wait_for_diagnostics"))
        if not wait_for_diagnostics:
            return await self._pull_diagnostics(uri)

        deadline = asyncio.get_running_loop().time() + _DIAGNOSTICS_WAIT_S
        while True:
            result = await self._pull_diagnostics(uri)
            if result.get("diagnostics") or result.get("error"):
                return result
            if asyncio.get_running_loop().time() >= deadline:
                return result
            await asyncio.sleep(_DIAGNOSTICS_POLL_S)

    async def query_symbols(self, args: dict[str, Any]) -> dict[str, Any]:
        await self.start()
        query = str(args.get("query", "")).strip()
        file_path = str(args["file_path"]) if args.get("file_path") else None
        if file_path:
            uri = await self._open_document(file_path)
            params = {"textDocument": {"uri": uri}}
            raw = await self._send_request(
                "textDocument/documentSymbol", params
            )
        else:
            params = {"query": query}
            raw = await self._send_request("workspace/symbol", params)
        if isinstance(raw, list):
            symbols = raw
        else:
            symbols = []
        if query and isinstance(symbols, list):
            symbols = [
                s
                for s in symbols
                if isinstance(s, dict)
                and query.lower() in str(s.get("name", "")).lower()
            ]
        if file_path and not symbols:
            symbols = self._fallback_document_symbols(file_path, query)
        return {"symbols": symbols}

    async def rename(self, args: dict[str, Any]) -> dict[str, Any]:
        await self.start()
        uri = await self._open_document(args["file_path"])
        params = {
            "textDocument": {"uri": uri},
            "position": {
                "line": int(args["line"]),
                "character": int(args["character"]),
            },
            "newName": str(args["new_name"]),
        }
        raw = await self._send_request("textDocument/rename", params)
        return raw if isinstance(raw, dict) else {}

    async def format_document(self, args: dict[str, Any]) -> dict[str, Any]:
        await self.start()
        uri = await self._open_document(args["file_path"])
        raw = await self._send_request(
            "textDocument/formatting",
            {
                "textDocument": {"uri": uri},
                "options": args.get("options") or {"tabSize": 4, "insertSpaces": True},
            },
        )
        if not isinstance(raw, list):
            return {"changes": {}}
        return {"changes": {uri: raw}}

    async def code_actions(self, args: dict[str, Any]) -> dict[str, Any]:
        await self.start()
        uri = await self._open_document(args["file_path"])
        raw_range = args.get("range")
        if isinstance(raw_range, dict):
            range_obj = raw_range
        else:
            line = int(args.get("line", 0))
            character = int(args.get("character", 0))
            range_obj = {
                "start": {"line": line, "character": character},
                "end": {"line": line, "character": character},
            }
        raw = await self._send_request(
            "textDocument/codeAction",
            {
                "textDocument": {"uri": uri},
                "range": range_obj,
                "context": {
                    "diagnostics": args.get("diagnostics") or [],
                    **(
                        {"only": args["only"]}
                        if isinstance(args.get("only"), list)
                        else {}
                    ),
                },
            },
        )
        return {"code_actions": raw if isinstance(raw, list) else []}

    async def evict(self) -> None:
        client = self._client
        proc = self._proc
        self._client = None
        self._proc = None
        self._started = False
        self._diagnostic_cache.clear()
        if client is not None:
            try:
                await client.close()
            except Exception:
                logger.debug("pyright client close error", exc_info=True)
        if proc is not None:
            try:
                proc.terminate()
                await asyncio.wait_for(proc.wait(), timeout=5.0)
            except Exception:
                logger.debug("pyright proc terminate error", exc_info=True)
        self._release_overlay_handle()

    async def _cleanup_failed_start(self) -> None:
        proc = self._proc
        self._client = None
        self._proc = None
        if proc is not None:
            with contextlib.suppress(Exception):
                proc.terminate()
                await asyncio.wait_for(proc.wait(), timeout=5.0)
        self._release_overlay_handle()

    async def _point_query(
        self, method: str, args: dict[str, Any]
    ) -> list[dict[str, Any]]:
        await self.start()
        uri = await self._open_document(args["file_path"])
        params = {
            "textDocument": {"uri": uri},
            "position": {
                "line": int(args["line"]),
                "character": int(args["character"]),
            },
        }
        raw = await self._send_request(method, params)
        return self._normalize_locations(raw)

    def _normalize_locations(self, raw: Any) -> list[dict[str, Any]]:
        if not isinstance(raw, list):
            if isinstance(raw, dict):
                raw = [raw]
            else:
                return []
        out: list[dict[str, Any]] = []
        for entry in raw:
            if not isinstance(entry, dict):
                continue
            uri = entry.get("uri") or entry.get("targetUri") or ""
            try:
                file_path = self._from_uri(str(uri))
            except Exception:
                file_path = str(uri)
            range_obj = entry.get("range") or entry.get("targetRange")
            out.append({"file_path": file_path, "range": range_obj})
        return out

    async def _open_document(self, file_path: str) -> str:
        uri = self._to_uri(file_path)
        notify = self._client
        if notify is None:
            return uri
        if uri in self._opened:
            await self._sync_open_document(uri, file_path)
            return uri
        text = self._read_document_text(file_path)
        await notify.notify(
            "textDocument/didOpen",
            {
                "textDocument": {
                    "uri": uri,
                    "languageId": "python",
                    "version": self._next_document_version(uri),
                    "text": text,
                }
            },
        )
        self._opened.add(uri)
        self._document_hashes[uri] = _text_hash(text)
        return uri

    async def _notify_workspace_refreshed(self) -> None:
        client = self._client
        if client is None or not self._started:
            return
        await client.notify(
            "workspace/didChangeWatchedFiles",
            {"changes": [{"uri": self._workspace_uri(), "type": 2}]},
        )
        for uri in tuple(self._opened):
            try:
                file_path = self._from_uri(uri)
            except Exception:
                self._forget_document(uri)
                continue
            if not self._document_exists(file_path):
                await client.notify(
                    "textDocument/didClose",
                    {"textDocument": {"uri": uri}},
                )
                self._forget_document(uri)
                continue
            await self._sync_open_document(uri, file_path)

    async def _sync_open_document(self, uri: str, file_path: str) -> None:
        client = self._client
        if client is None:
            return
        text = self._read_document_text(file_path)
        text_hash = _text_hash(text)
        if self._document_hashes.get(uri) == text_hash:
            return
        await client.notify(
            "textDocument/didChange",
            {
                "textDocument": {
                    "uri": uri,
                    "version": self._next_document_version(uri),
                },
                "contentChanges": [{"text": text}],
            },
        )
        self._document_hashes[uri] = text_hash

    async def _send_request(self, method: str, params: dict[str, Any]) -> Any:
        client = self._client
        if client is None:
            raise RuntimeError("pyright client is not started")
        try:
            return await client.request(method, params)
        except JsonRpcError as exc:
            return {"error": {"code": exc.code, "message": exc.message}}

    async def _pull_diagnostics(self, uri: str) -> dict[str, Any]:
        raw = await self._send_request(
            "textDocument/diagnostic",
            {"textDocument": {"uri": uri}},
        )
        if isinstance(raw, dict) and "error" in raw:
            return {"diagnostics": [], "error": raw["error"]}
        if not isinstance(raw, dict):
            return {
                "diagnostics": [],
                "error": {
                    "message": (
                        "unexpected Pyright diagnostic response type: "
                        f"{type(raw).__name__}"
                    )
                },
            }

        items = raw.get("items")
        if not isinstance(items, list):
            if raw.get("kind") == "unchanged":
                return self._diagnostic_result(
                    list(self._diagnostic_cache.get(uri, [])),
                    raw,
                )
            return {
                "diagnostics": [],
                "error": {
                    "message": "unexpected Pyright diagnostic response: missing items"
                },
            }

        diagnostics = list(items)
        self._diagnostic_cache[uri] = diagnostics
        return self._diagnostic_result(diagnostics, raw)

    def _fallback_document_symbols(
        self,
        file_path: str,
        query: str,
    ) -> list[dict[str, Any]]:
        """Return AST-derived symbols for one valid Python file.

        Pyright can transiently return an empty ``documentSymbol`` result while
        the live overlay is changing. For file-scoped queries, the tool can
        still answer from the current file contents without changing
        workspace-wide LSP behavior.
        """
        text = self._read_document_text(file_path)
        try:
            module = ast.parse(text)
        except SyntaxError:
            return []
        return _ast_document_symbols(
            module.body,
            uri=self._to_uri(file_path),
            query=query,
        )

    def _diagnostic_result(
        self,
        diagnostics: list[dict[str, Any]],
        raw: dict[str, Any],
    ) -> dict[str, Any]:
        result: dict[str, Any] = {"diagnostics": diagnostics}
        if "kind" in raw:
            result["kind"] = raw["kind"]
        if "resultId" in raw:
            result["result_id"] = raw["resultId"]
        return result

    async def _spawn(self) -> None:
        argv = self._build_argv()
        try:
            proc = await asyncio.create_subprocess_exec(
                *argv,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.DEVNULL,
                env=_runtime_subprocess_env(),
                cwd=_runtime_subprocess_cwd(),
            )
        except FileNotFoundError as exc:
            raise PyrightSpawnError(
                f"failed to spawn pyright-langserver: {exc}"
            ) from exc
        if proc.stdin is None or proc.stdout is None:
            raise PyrightSpawnError(
                "pyright-langserver subprocess streams are unavailable"
            )
        self._proc = proc
        self._client = LspJsonRpcClient(
            proc.stdin,
            proc.stdout,
            request_timeout_s=_DEFAULT_REQUEST_TIMEOUT_S,
            server_request_handler=self._handle_server_request,
        )
        self._client.add_notification_handler(self._on_notification)
        self._client.start()

    def _build_argv(self) -> list[str]:
        overlay_argv = self._build_overlay_argv()
        if overlay_argv is not None:
            return overlay_argv
        return self._build_pyright_argv()

    def _build_overlay_argv(self) -> list[str] | None:
        handle = self._overlay_handle
        layer_paths = getattr(handle, "layer_paths", None)
        if not layer_paths:
            return None
        unshare = shutil.which("unshare")
        if not unshare:
            return None
        run_dir = Path(str(getattr(handle, "run_dir", "")))
        if not str(run_dir):
            return None
        payload_ref = run_dir / "lsp-namespace-request.json"
        payload_ref.parent.mkdir(parents=True, exist_ok=True)
        payload_ref.write_text(
            json.dumps(
                {
                    "workspace_root": self.workspace_root,
                    "layer_paths": list(layer_paths),
                    "upperdir": str(getattr(handle, "upperdir", "")),
                    "workdir": str(getattr(handle, "workdir", "")),
                    "argv": self._build_pyright_argv(),
                    "env": os.environ.copy(),
                },
                separators=(",", ":"),
                sort_keys=True,
            ),
            encoding="utf-8",
        )
        return [
            unshare,
            "-Urm",
            sys.executable,
            "-m",
            "plugins.catalog.lsp.runtime.namespace_entrypoint",
            str(payload_ref),
        ]

    async def _refresh_overlay_handle(self, overlay_handle: Any) -> None:
        if self._started:
            await self._remount_private_overlay(overlay_handle)
        self._install_overlay_handle(overlay_handle)

    def _install_overlay_handle(self, overlay_handle: Any) -> None:
        self._overlay_handle = overlay_handle
        self._uses_private_overlay_namespace = bool(
            getattr(overlay_handle, "layer_paths", None)
        )
        self._overlay_layer_paths = tuple(
            str(path) for path in getattr(overlay_handle, "layer_paths", ()) or ()
        )

    async def _remount_private_overlay(self, overlay_handle: Any) -> None:
        if not self._uses_private_overlay_namespace:
            raise PyrightOverlayRefreshError(
                "running Pyright session does not own a private overlay namespace"
            )
        proc = self._proc
        if proc is None or proc.pid is None:
            raise PyrightOverlayRefreshError("running Pyright process is unavailable")
        layer_paths = getattr(overlay_handle, "layer_paths", None)
        if not layer_paths:
            raise PyrightOverlayRefreshError(
                "fresh overlay handle does not expose layer paths"
            )
        nsenter = shutil.which("nsenter")
        if not nsenter:
            raise PyrightOverlayRefreshError("nsenter is unavailable")
        run_dir = Path(str(getattr(overlay_handle, "run_dir", "")))
        if not str(run_dir):
            raise PyrightOverlayRefreshError("fresh overlay handle has no run dir")
        payload_ref = run_dir / "lsp-namespace-remount.json"
        payload_ref.parent.mkdir(parents=True, exist_ok=True)
        payload_ref.write_text(
            json.dumps(
                {
                    "workspace_root": self.workspace_root,
                    "layer_paths": list(layer_paths),
                    "upperdir": str(getattr(overlay_handle, "upperdir", "")),
                    "workdir": str(getattr(overlay_handle, "workdir", "")),
                },
                separators=(",", ":"),
                sort_keys=True,
            ),
            encoding="utf-8",
        )
        start_s = time.monotonic()
        helper = await asyncio.create_subprocess_exec(
            nsenter,
            "-t",
            str(proc.pid),
            "-U",
            "-m",
            "--preserve-credentials",
            "--",
            sys.executable,
            "-m",
            "plugins.catalog.lsp.runtime.namespace_remount",
            str(payload_ref),
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env=_runtime_subprocess_env(),
            cwd=_runtime_subprocess_cwd(),
        )
        stdout, stderr = await asyncio.wait_for(helper.communicate(), timeout=10.0)
        if helper.returncode != 0:
            message = stderr.decode("utf-8", errors="replace").strip()
            if not message:
                message = stdout.decode("utf-8", errors="replace").strip()
            raise PyrightOverlayRefreshError(
                f"pyright overlay remount failed with exit {helper.returncode}: "
                f"{message}"
            )
        self.audit_remount_count += 1
        self.audit_last_remount_s = time.monotonic() - start_s
        logger.info(
            "pyright session overlay remounted",
            extra={"duration_s": self.audit_last_remount_s},
        )

    def _build_pyright_argv(self) -> list[str]:
        if os.path.exists(_CONDA_HOOK):
            return [
                "bash",
                "-lc",
                (
                    f". {_CONDA_HOOK} && conda activate testbed "
                    "&& export PATH=/tmp/eos-node22/bin:$PATH "
                    "&& exec pyright-langserver --stdio"
                ),
            ]
        binary = shutil.which("pyright-langserver")
        if binary:
            return [binary, "--stdio"]
        return [
            "bash",
            "-lc",
            "export PATH=/tmp/eos-node22/bin:$PATH && exec pyright-langserver --stdio",
        ]

    def _release_overlay_handle(self) -> None:
        handle = self._overlay_handle
        if handle is None:
            return
        self._overlay_handle = None
        _release_handle(handle)

    async def _initialize(self) -> None:
        client = self._client
        if client is None:
            return
        await asyncio.wait_for(
            client.request(
                "initialize",
                {
                    "processId": os.getpid(),
                    "rootUri": self._workspace_uri(),
                    "workspaceFolders": [
                        {"uri": self._workspace_uri(), "name": "testbed"}
                    ],
                    "capabilities": {
                        "workspace": {
                            "workspaceFolders": True,
                            "didChangeWatchedFiles": {"dynamicRegistration": False},
                        },
                        "textDocument": {
                            "diagnostic": {
                                "dynamicRegistration": False,
                                "relatedDocumentSupport": True,
                            },
                            "definition": {"linkSupport": True},
                            "hover": {"contentFormat": ["markdown", "plaintext"]},
                        },
                    },
                    "initializationOptions": {},
                },
            ),
            timeout=_DEFAULT_INIT_TIMEOUT_S,
        )
        await client.notify("initialized", {})

    async def _on_notification(self, message: dict[str, Any]) -> None:
        del message

    def _handle_server_request(self, message: dict[str, Any]) -> Any:
        method = message.get("method")
        params = message.get("params") or {}
        if method == "workspace/configuration":
            items = params.get("items") if isinstance(params, dict) else []
            return [{} for _ in items] if isinstance(items, list) else []
        if method == "workspace/workspaceFolders":
            return [{"uri": self._workspace_uri(), "name": "testbed"}]
        return None

    def _next_document_version(self, uri: str) -> int:
        version = self._document_versions.get(uri, 0) + 1
        self._document_versions[uri] = version
        return version

    def _read_document_text(self, file_path: str) -> str:
        if self._uses_private_overlay_namespace:
            return self._read_document_text_from_layers(file_path)
        try:
            with open(self._to_full_path(file_path), encoding="utf-8") as fh:
                return fh.read()
        except OSError:
            return ""

    def _read_document_text_from_layers(self, file_path: str) -> str:
        rel = self._to_layer_relative_path(file_path)
        if not rel:
            return ""
        for layer_path in self._overlay_layer_paths:
            layer = Path(layer_path)
            index = self._layer_index(layer)
            if rel in index.whiteouts:
                return ""
            if rel in index.files:
                candidate = join_layer_path(layer, rel)
                try:
                    if candidate.is_symlink():
                        return os.readlink(candidate)
                    if candidate.is_file():
                        return candidate.read_text(encoding="utf-8")
                except OSError:
                    return ""
                return ""
            if has_ancestor_in(rel, index.files) or has_ancestor_in(
                rel,
                index.opaque_dirs,
            ):
                return ""
        return ""

    def _document_exists(self, file_path: str) -> bool:
        if self._uses_private_overlay_namespace:
            return self._document_exists_in_layers(file_path)
        return os.path.exists(self._to_full_path(file_path))

    def _document_exists_in_layers(self, file_path: str) -> bool:
        rel = self._to_layer_relative_path(file_path)
        if not rel:
            return True
        for layer_path in self._overlay_layer_paths:
            index = self._layer_index(Path(layer_path))
            if rel in index.whiteouts:
                return False
            if rel in index.files:
                return True
            if has_ancestor_in(rel, index.files) or has_ancestor_in(
                rel,
                index.opaque_dirs,
            ):
                return False
        return False

    def _layer_index(self, layer: Path) -> Any:
        key = layer.as_posix()
        cached = self._layer_index_cache.get(key)
        if cached is not None:
            return cached
        index = build_layer_index(layer)
        self._layer_index_cache[key] = index
        return index

    def _to_layer_relative_path(self, file_path: str) -> str:
        full = self._to_full_path(file_path)
        root = self.workspace_root
        if full == root:
            return ""
        if full.startswith(f"{root}/"):
            return normalize_layer_path(full[len(root) + 1 :])
        return normalize_layer_path(str(file_path).lstrip("/"))

    def _forget_document(self, uri: str) -> None:
        self._opened.discard(uri)
        self._document_hashes.pop(uri, None)
        self._document_versions.pop(uri, None)
        self._diagnostic_cache.pop(uri, None)

    def _workspace_uri(self) -> str:
        return self._path_uri(self.workspace_root)

    def _to_uri(self, file_path: str) -> str:
        return self._path_uri(self._to_full_path(file_path))

    def _from_uri(self, uri: str) -> str:
        parsed = urlparse(uri)
        if parsed.scheme != "file":
            raise ValueError(f"unsupported uri scheme: {uri}")
        path = unquote(parsed.path)
        return self._to_agent_path(path)

    def _to_agent_path(self, path: str) -> str:
        full = os.path.normpath(path)
        root = self.workspace_root
        if full == root:
            return root
        if full.startswith(f"{root}/"):
            return full
        return full

    def _to_full_path(self, file_path: str) -> str:
        raw = str(file_path or "").strip()
        if raw.startswith("file://"):
            return self._to_full_path(self._from_uri(raw))
        if not raw:
            return self.workspace_root
        if os.path.isabs(raw):
            normalized = os.path.normpath(raw)
            if normalized == self.workspace_root or normalized.startswith(
                f"{self.workspace_root}/"
            ):
                return normalized
            return os.path.normpath(os.path.join(self.workspace_root, raw.lstrip("/")))
        return os.path.normpath(os.path.join(self.workspace_root, raw))

    def _path_uri(self, path: str) -> str:
        normalized = os.path.normpath(path)
        return "file://" + quote(normalized)


def _text_hash(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _runtime_subprocess_env() -> dict[str, str]:
    env = os.environ.copy()
    existing = env.get("PYTHONPATH", "")
    parts = [_RUNTIME_BUNDLE_ROOT]
    parts.extend(
        part
        for part in existing.split(os.pathsep)
        if part and part != _RUNTIME_BUNDLE_ROOT
    )
    env["PYTHONPATH"] = os.pathsep.join(parts)
    return env


def _runtime_subprocess_cwd() -> str | None:
    return _RUNTIME_BUNDLE_ROOT if os.path.isdir(_RUNTIME_BUNDLE_ROOT) else None


def _ast_document_symbols(
    body: list[ast.stmt],
    *,
    uri: str,
    query: str,
) -> list[dict[str, Any]]:
    query_lower = query.lower()
    symbols: list[dict[str, Any]] = []
    for node in body:
        if not isinstance(
            node,
            (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef),
        ):
            continue
        children = _ast_document_symbols(node.body, uri=uri, query=query)
        symbol = _ast_symbol(node, uri=uri)
        if children:
            symbol["children"] = children
        if not query_lower or query_lower in symbol["name"].lower() or children:
            symbols.append(symbol)
    return symbols


def _ast_symbol(
    node: ast.ClassDef | ast.FunctionDef | ast.AsyncFunctionDef,
    *,
    uri: str,
) -> dict[str, Any]:
    range_obj = {
        "start": {
            "line": max(0, int(getattr(node, "lineno", 1)) - 1),
            "character": int(getattr(node, "col_offset", 0)),
        },
        "end": {
            "line": max(
                0,
                int(getattr(node, "end_lineno", getattr(node, "lineno", 1))) - 1,
            ),
            "character": int(
                getattr(
                    node,
                    "end_col_offset",
                    getattr(node, "col_offset", 0) + len(node.name),
                )
            ),
        },
    }
    return {
        "name": node.name,
        "kind": 5 if isinstance(node, ast.ClassDef) else 12,
        "uri": uri,
        "location": {
            "uri": uri,
            "range": range_obj,
        },
    }


def _optional_positive_float(value: object, *, default: float) -> float:
    try:
        parsed = float(value) if value is not None else default
    except (TypeError, ValueError):
        return default
    return parsed if parsed > 0 else default


def _release_handle(handle: Any | None) -> None:
    if handle is None:
        return
    release = getattr(handle, "release", None)
    if callable(release):
        release()
    run_dir = getattr(handle, "run_dir", None)
    if run_dir:
        shutil.rmtree(run_dir, ignore_errors=True)
