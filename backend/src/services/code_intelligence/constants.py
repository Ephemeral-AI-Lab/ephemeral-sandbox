"""Configuration constants for the code intelligence service."""

from __future__ import annotations

# Tree cache
TREE_CACHE_MAX_FILES = 500
TREE_CACHE_MAX_FILE_SIZE = 1_000_000  # 1 MB

# Symbol index
SYMBOL_INDEX_MAX_FILES = 10_000
SYMBOL_INDEX_BATCH_SIZE = 50
SYMBOL_INDEX_REFRESH_INTERVAL = 5.0  # seconds

# Arbiter (OCC)
ARBITER_LOCK_TIMEOUT = 30.0  # seconds
ARBITER_MAX_CONCURRENT_EDITS = 10

# LSP client
LSP_QUERY_TIMEOUT = 10.0  # seconds
LSP_CACHE_TTL = 60.0  # seconds
LSP_CACHE_MAX_ENTRIES = 200

# Query router priorities (higher = preferred)
BACKEND_PRIORITY_LSP = 100
BACKEND_PRIORITY_SYMBOL_INDEX = 50

# Ledger
LEDGER_MAX_ENTRIES = 10_000

# Patcher
PATCHER_MAX_DIFF_SIZE = 100_000  # characters

# File scanning
SKIP_DIRECTORIES = frozenset({
    ".git", ".hg", ".svn", ".venv", "venv",
    "__pycache__", "node_modules", ".tox",
    "dist", "build", ".mypy_cache", ".pytest_cache",
    ".ruff_cache", "egg-info",
})

SUPPORTED_EXTENSIONS = frozenset({
    ".py", ".js", ".ts", ".jsx", ".tsx",
    ".java", ".go", ".rs", ".rb", ".php",
    ".c", ".cpp", ".h", ".hpp", ".cs",
    ".swift", ".kt", ".scala", ".sh",
    ".json", ".yaml", ".yml", ".toml",
    ".md", ".txt", ".sql",
})
