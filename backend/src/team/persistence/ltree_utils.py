"""Utilities for converting file paths to PostgreSQL ltree labels.

See Section 14.5 of the coordination redesign doc for specification.
"""

from __future__ import annotations

import re

_LTREE_UNSAFE = re.compile(r'[^a-zA-Z0-9_]')


def _escape_char(ch: str) -> str:
    """Escape a non-alphanumeric character to a reversible representation.

    Dots -> 'D', hyphens -> 'H', others -> 'X' + 2-digit hex ordinal.
    Distinct inputs always produce distinct ltree labels.
    """
    if ch == '.':
        return 'D'
    if ch == '-':
        return 'H'
    return f'X{ord(ch):02x}'


def path_to_ltree(path: str) -> str:
    """Convert a file path to an ltree label path.

    Rules:
      1. Strip leading/trailing slashes.
      2. Split on '/'.
      3. For each component, replace unsafe chars via _escape_char.
      4. ltree labels must be [a-zA-Z0-9_], max 256 chars.
      5. Drop empty labels.

    Examples:
      "src/auth/"               -> "src.auth"
      "src/auth/session.py"     -> "src.auth.sessionDpy"
      "src/my-module/foo.py"    -> "src.myHmodule.fooDpy"
      "src/my_module/foo.py"    -> "src.my_module.fooDpy"
    """
    parts = path.strip('/').split('/')
    labels = []
    for part in parts:
        label = _LTREE_UNSAFE.sub(lambda m: _escape_char(m.group()), part)
        if label:
            labels.append(label)
    return '.'.join(labels)
