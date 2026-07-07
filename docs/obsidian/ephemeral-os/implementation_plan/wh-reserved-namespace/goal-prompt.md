/goal Implement the reserved `.wh.` namespace fix in ephemeral-os, then build and pass its live e2e catalog.

Design truth — read both first, follow exactly:
- docs/obsidian/ephemeral-os/implementation_plan/wh-reserved-namespace/spec.md
- docs/obsidian/ephemeral-os/implementation_plan/wh-reserved-namespace/test-case.md

Bug: capture turns user file `.wh.foo` into `Delete{foo}` and `.wh..wh..opq` into `OpaqueDir{parent}`; sessionless `file_write` can plant literal `.wh.` names in layer dirs, which every reader treats as whiteout markers. Silent data loss. Fix: reserve `.wh.` path components, fail closed at publish admission.

Phase 1 — implementation (on main, commit directly; touch only what this task needs):
1. D1 gate: layerstack stack/publish/route.rs `is_protected()` — any path COMPONENT starting with `.wh.` is protected (covers bare `.wh.`, `.wh..wh..opq`, every `.wh.X`; NOT `.wh`, `.whx`, `x.wh.y`). Keep the existing set. All validated routes (finalize, amend_path file ops, command publishes) inherit it; reject class stays `protected_path`.
2. D2 capture: in capture.rs delete the two marker consts, the `OPAQUE_MARKER` and `is_whiteout_marker` branches, `is_whiteout_marker()`, `whiteout_target()`. Keep `is_overlay_whiteout` (char-dev 0:0 + user.overlay.whiteout xattr) and `has_overlay_opaque_xattr` unchanged. `.wh.` names then flow as ordinary writes and reject at publish. Add NO new drop reason.
3. D3 guard: stack/projection/apply.rs:104 — logical-whiteout classification requires `name.len() > prefix.len()` (parity with flatten.rs:392) so a bare `.wh.` layer entry projects as a file, never `remove_path(parent)`.
4. D4 docs: ephemeral-os.md `protected_path` bullet (add `.wh.*` + reason), capture.rs module doc, `is_protected` doc comment.
5. Unit tests per spec §Unit test plan, tests/ only (never src/): overlay_capture.rs — `.wh.foo` with content → WriteFile never Delete{foo}, `dir/.wh..wh..opq` → never OpaqueDir, bare `.wh.` → write, old logical-marker fixture reworked to Linux-gated overlay-xattr fixtures; publish.rs — every change kind at reserved paths rejects ProtectedPath, manifest unchanged, lookalikes publish; stack.rs — bare-`.wh.` projection.
Gates: cargo build && cargo test && cargo clippy --all-targets && cargo fmt. No inline comments in production code; prefer less; keep crate boundaries.

Phase 2 — live e2e (only after Phase 1 gates are green):
1. Implement the 16-case catalog exactly as test-case.md defines, under cli-operation-e2e-live-test/runtime/reserved_paths/: test_wh_reserved_easy.py (EZ-01..06), _medium.py (MED-01..06), _complex.py (CX-01..04); markers `whreserved and easy|medium|complex`; reuse runtime/file/helpers.py + runtime/workspace_session/helpers.py; local helpers.py owns verdict.json writing (catalog §2 schema) and preconditions P1-P3 (hard-fail, never skip).
2. Bring-up: export PATH="$PWD/bin:$PATH"; bin/start-sandbox-docker-gateway --rebuild-binary.
3. Run in catalog §5 order: preconditions → EZ → MED → CX-01/02/04 → CX-03 storm. Each case writes test-reports/<RUN_ID>/<CASE_ID>/verdict.json; suite writes SUMMARY.md even on abort (note red-first waiver).
4. Assert on structured JSON only — `publish_rejected`, `publish_reject_class == "protected_path"`, assert_manifest_delta, _assert_stack_unchanged, file_read byte-equality — never log scraping. Data-safety is load-bearing: every base file a marker would have masked reads byte-equal after each reject; no `.wh.*` name ever visible in merged views or listings.
5. On failure, diagnose against the spec invariant it traces to (catalog §4) and fix the code, not the assertion — unless the catalog is provably wrong; then correct it and record why in SUMMARY.md.

Done when: all cargo gates green; 16/16 cases pass on all three axes plus teardown with P1-P3 asserted; docs updated; committed to main citing the spec.
