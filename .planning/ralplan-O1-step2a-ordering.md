# Step 2a Finding: lowerdir+ priority ordering

**Result:** FIRST `fsconfig(SET_STRING, "lowerdir+", path)` call = TOP priority (highest priority layer).

**Empirical proof:** Three layers A, B, C mounted via `lowerdir+` in alphabetical order (A first). `merged/marker.txt` returned `"A"`. Confirmed in Docker (`python:3.11-slim`, `--cap-add=SYS_ADMIN`).

**Primary source:** Linux kernel `Documentation/filesystems/overlayfs.rst` ("Multiple lower layers"): *"The specified lower directories will be stacked beginning from the rightmost one and going left. In the above example lower1 will be the top, lower2 the middle and lower3 the bottom layer."* For `lowerdir+` fsconfig calls, first call = leftmost = top.

**Implication for Steps 3 and 6:** `manifest.layers` is already ordered newest-first (per `view.py:214` comment). Therefore `kernel_mount.py` must iterate `manifest.layers` in natural order (no reversal) when calling `lowerdir+` — the first element (newest layer) must be the first `lowerdir+` call so it wins file conflicts.

The bisect script's `list(reversed(lowers))` was correct: it reversed a bottom-to-top list so newest called first.

**Permanent CI assertion:** `backend/tests/live_e2e_test/sandbox/overlay/native/test_lowerdir_priority_ordering.py` — skips on non-Linux, fails if kernel changes ordering.
