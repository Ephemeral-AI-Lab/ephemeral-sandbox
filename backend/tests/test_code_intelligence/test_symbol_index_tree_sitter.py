"""Tests that SymbolIndex consumes tree-cache parses for non-Python files."""

from __future__ import annotations

from types import SimpleNamespace

from code_intelligence.analysis.symbol_index import SymbolIndex
from code_intelligence.types import SymbolKind


class FakeNode:
    def __init__(
        self,
        node_type: str,
        text: str,
        content: str,
        *,
        start: int | None = None,
        end: int | None = None,
        children: list["FakeNode"] | None = None,
        fields: dict[str, "FakeNode"] | None = None,
    ) -> None:
        self.type = node_type
        self._content = content
        self.children = children or []
        self._fields = fields or {}
        self.parent: FakeNode | None = None

        if start is None:
            start = content.index(text)
        if end is None:
            end = start + len(text)
        self.start_byte = start
        self.end_byte = end
        self.start_point = _offset_to_point(content, start)
        self.end_point = _offset_to_point(content, end)

        for child in self.children:
            child.parent = self
        for child in self._fields.values():
            child.parent = self
            if child not in self.children:
                self.children.append(child)

    @property
    def text(self) -> str:
        return self._content[self.start_byte:self.end_byte]

    def child_by_field_name(self, name: str) -> "FakeNode" | None:
        return self._fields.get(name)


class FakeTreeCache:
    def __init__(self, root: FakeNode) -> None:
        self._root = root
        self.calls: list[tuple[str, str]] = []

    def get_tree(self, file_path: str, content: str | None = None, mtime=None):
        self.calls.append((file_path, content or ""))
        return SimpleNamespace(tree=SimpleNamespace(root_node=self._root))


def _offset_to_point(content: str, offset: int) -> tuple[int, int]:
    prefix = content[:offset]
    row = prefix.count("\n")
    last_newline = prefix.rfind("\n")
    col = offset if last_newline < 0 else offset - last_newline - 1
    return row, col


def test_symbol_index_uses_tree_cache_for_typescript_symbols(tmp_path) -> None:
    content = (
        "class Example {\n"
        "  render() {}\n"
        "}\n"
        "interface Props {}\n"
        "const answer = 42;\n"
    )
    file_path = tmp_path / "sample.ts"
    file_path.write_text(content, encoding="utf-8")

    class_name = FakeNode("type_identifier", "Example", content)
    method_name = FakeNode("property_identifier", "render", content)
    interface_name = FakeNode("type_identifier", "Props", content)
    const_token = FakeNode("const", "const", content)
    const_name = FakeNode("identifier", "answer", content)

    method = FakeNode(
        "method_definition",
        "render() {}",
        content,
        children=[method_name],
        fields={"name": method_name},
    )
    klass = FakeNode(
        "class_declaration",
        "class Example {\n  render() {}\n}",
        content,
        children=[method],
        fields={"name": class_name},
    )
    interface = FakeNode(
        "interface_declaration",
        "interface Props {}",
        content,
        fields={"name": interface_name},
    )
    declarator = FakeNode(
        "variable_declarator",
        "answer = 42",
        content,
        fields={"name": const_name},
    )
    lexical = FakeNode(
        "lexical_declaration",
        "const answer = 42;",
        content,
        children=[const_token, declarator],
    )
    root = FakeNode("program", content, content, start=0, end=len(content), children=[klass, interface, lexical])

    tree_cache = FakeTreeCache(root)
    index = SymbolIndex(str(tmp_path), tree_cache=tree_cache)

    generation = index.refresh(str(file_path), content)
    assert generation > 0
    assert tree_cache.calls == [(str(file_path), content)]

    symbols = index.file_symbols(str(file_path))
    names_to_kinds = {symbol.name: symbol.kind for symbol in symbols}

    assert names_to_kinds["Example"] == SymbolKind.CLASS
    assert names_to_kinds["Example.render"] == SymbolKind.METHOD
    assert names_to_kinds["Props"] == SymbolKind.INTERFACE
    assert names_to_kinds["answer"] == SymbolKind.CONSTANT
