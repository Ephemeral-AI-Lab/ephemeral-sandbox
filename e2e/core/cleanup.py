"""Session-wide sandbox registry.

Every sandbox the suite creates (via ``manager.management.helpers.create_sandbox``)
is tracked here, so a session-end finalizer can destroy any that a test leaked —
e.g. tests that create inline and fail before their own teardown. Only ids the
suite created are tracked; sandboxes owned by other clients are never touched.
"""

_tracked = set()


def track(sandbox_id):
    if sandbox_id:
        _tracked.add(sandbox_id)


def untrack(sandbox_id):
    _tracked.discard(sandbox_id)


def drain():
    """Return and clear all tracked ids."""
    ids = sorted(_tracked)
    _tracked.clear()
    return ids
