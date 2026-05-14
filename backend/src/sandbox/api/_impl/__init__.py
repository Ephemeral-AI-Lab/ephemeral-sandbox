"""Internal implementations for public sandbox API tool verbs.

CONVENTION: each verb module (read/write/edit/shell) deliberately repeats the
``selected_transport = ... -> _call closure -> audited_operation(...)`` scaffold.
A consolidated dispatcher (``_VerbSpec`` / ``_run_verb``) was tried in W7a and
removed in 805d4b10 because the abstraction cost more LOC than the duplication
it eliminated, and ``shell`` never fit the spec shape. Do not reintroduce.
"""

from __future__ import annotations
