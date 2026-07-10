from pathlib import Path


_MARKERS = ("Cargo.toml", "CLAUDE.md")


def find_repo_root(start):
    resolved = Path(start).resolve()
    for candidate in (resolved, *resolved.parents):
        if all((candidate / marker).is_file() for marker in _MARKERS):
            return candidate
    raise FileNotFoundError(f"repository root not found from {resolved}")


REPO_ROOT = find_repo_root(__file__)
