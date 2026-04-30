"""Per-file Python symbol extraction for the workspace index."""

from __future__ import annotations

import ast
from pathlib import Path

from sandbox.code_intelligence.core.types import SymbolInfo, SymbolKind


def extract_symbols(file_path: str, content: str) -> list[SymbolInfo]:
    """Extract Python symbols from *content*."""
    if Path(file_path).suffix.lower() != ".py":
        return []
    return _extract_python(file_path, content)


# -- Python ast ---------------------------------------------------------------


def _extract_python(file_path: str, content: str) -> list[SymbolInfo]:
    try:
        tree = ast.parse(content, filename=file_path)
    except SyntaxError:
        return []
    symbols: list[SymbolInfo] = []
    _walk_python_ast(tree, file_path, symbols, container="")
    return symbols


def _walk_python_ast(
    node: ast.AST,
    file_path: str,
    bucket: list[SymbolInfo],
    container: str,
) -> None:
    for child in ast.iter_child_nodes(node):
        if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)):
            full_name = f"{container}.{child.name}" if container else child.name
            args = [arg.arg for arg in child.args.args]
            bucket.append(
                _python_symbol(
                    file_path,
                    child,
                    name=full_name,
                    kind=SymbolKind.METHOD if container else SymbolKind.FUNCTION,
                    signature=f"def {child.name}({', '.join(args)})",
                    docstring=ast.get_docstring(child) or "",
                    container=container,
                )
            )
            _walk_python_ast(child, file_path, bucket, full_name)
        elif isinstance(child, ast.ClassDef):
            full_name = f"{container}.{child.name}" if container else child.name
            bucket.append(
                _python_symbol(
                    file_path,
                    child,
                    name=full_name,
                    kind=SymbolKind.CLASS,
                    signature=f"class {child.name}",
                    docstring=ast.get_docstring(child) or "",
                    container=container,
                )
            )
            _walk_python_ast(child, file_path, bucket, full_name)
        elif isinstance(child, ast.Assign):
            for target in child.targets:
                if isinstance(target, ast.Name):
                    full_name = f"{container}.{target.id}" if container else target.id
                    bucket.append(
                        _python_symbol(
                            file_path,
                            target,
                            name=full_name,
                            kind=SymbolKind.VARIABLE,
                            signature=f"{target.id} = ...",
                            container=container,
                        )
                    )
        else:
            _walk_python_ast(child, file_path, bucket, container)


def _python_symbol(
    file_path: str,
    node: ast.AST,
    *,
    name: str,
    kind: SymbolKind,
    signature: str,
    docstring: str = "",
    container: str = "",
) -> SymbolInfo:
    return SymbolInfo(
        name=name,
        kind=kind,
        file_path=file_path,
        line=getattr(node, "lineno", 0),
        end_line=getattr(node, "end_lineno", getattr(node, "lineno", 0)),
        character=getattr(node, "col_offset", 0),
        signature=signature,
        docstring=docstring,
        container=container,
    )
