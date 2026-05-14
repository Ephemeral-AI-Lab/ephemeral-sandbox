# Sandbox API Review Remediation Plan

Source review: `.planning/code-reviews/sandbox-api-REVIEW.md`

## Goals

- Make the public sandbox API explicitly typed and injectable.
- Remove import-cycle workarounds from the facade.
- Keep the existing `sandbox.api.*` caller surface stable unless a compatibility
  shim is explicitly documented.
- Move repeated verb mechanics into shared helpers.
- Preserve provider/runtime boundaries with focused contract tests.

## Phase 1: API Contracts And Shared Helpers

Issues addressed:

- 1.5: no public API protocol/base contract.
- 2.3: caller envelope is a loose, hand-maintained schema.
- 2.4, 3.4: duplicated cwd and error-message helpers.
- 4.1: no transport seam for daemon calls.
- 4.5: scattered timeout constants.
- 4.6: caller envelope cannot evolve.
- 4.7: no daemon protocol version marker.
- 7.1, 7.2, 7.4: cwd normalization, transient matching, loose int coercion.
- 6.2: no caller-envelope contract tests.

Implementation shape:

- Add `sandbox.api.protocol` for `SandboxAPI`, `SandboxLifecycleAPI`, and
  `SandboxTransport`.
- Add `sandbox.api.transport` for the default daemon transport and protocol
  version constants.
- Add `sandbox.api.timeouts` for public verb timeout policy.
- Move common payload/error/cwd/result helpers into the internal tool helper
  layer.
- Prefer dataclass-driven caller projection over manual field enumeration.

Verification:

- Add helper/protocol tests.
- Run `backend/tests/unit_test/test_sandbox/test_api`.

## Phase 2: Tool Verb Refactor

Issues addressed:

- 1.2, 1.3, 2.1: public-looking `tool/` internal package and module/symbol
  collisions.
- 4.2: copied audit try/result/failure boilerplate.
- 4.3: conflict classification scattered through verbs.
- 4.4, 7.3: edit recovery is bespoke and can false-positive when `new_text`
  already existed.
- 5.2, 5.3, 5.4: request, result, and conflict construction duplicate type
  hierarchy.
- 6.1: verb tests require monkey-patching concrete module globals.

Implementation shape:

- Move verb implementations to `sandbox.api._impl`.
- Keep `sandbox.api.tool` as a compatibility shim for old imports.
- Introduce a shared audited execution helper used by read/write/edit/shell/raw.
- Route daemon verbs through `SandboxTransport`.
- Introduce named conflict classifiers and a stricter edit recovery check.
- Add shared request-base/default-description plumbing and conflict factories.
- Share transient transport recovery between write and edit.

Verification:

- Update direct verb tests to use injected transports.
- Keep shim tests for `sandbox.api.tool`.

## Phase 3: Facade And Default Client

Issues addressed:

- 1.1, 3.1, 5.1: facade class is a namespace with method-local imports and a
  hidden singleton.
- 3.2: model re-export path is ambiguous.
- 6.1: no stable injectable facade for tests.

Implementation shape:

- Make `SandboxClient` eagerly import dependency modules and own injected
  `transport`, `lifecycle`, and `audit_sink` fields.
- Move package-level default callables into `sandbox.api.default`.
- Keep `sandbox.api.read_file(...)` and peers as compatibility wrappers backed
  by `default_client()` rather than import-time bound methods.
- Keep model re-exports for public compatibility but document `sandbox.models`
  as the canonical model owner in package docstrings.

Verification:

- Update facade tests to assert injection and no method-local imports.
- Add contract coverage that package-level wrappers are not frozen bound
  methods.

## Phase 4: Lifecycle/Discovery Split

Issues addressed:

- 2.2: `status.py` owns lifecycle, discovery, URL, and control operations.
- 3.3: status module directly orchestrates host/plugin/provider concerns.

Implementation shape:

- Split `status.py` into `lifecycle.py`, `discovery.py`, and `preview_urls.py`.
- Move create/start/delete orchestration into host lifecycle helpers where
  feasible.
- Keep `sandbox.api.status` as a compatibility re-export module.

Verification:

- Update status tests to cover the new owner modules and compatibility import.
- Update import-boundary tests for the new split.

## Phase 5: Daemon Version And Error Taxonomy

Issues addressed:

- 4.3, 4.7, 7.2: magic daemon operation strings and string-only error
  classification.

Implementation shape:

- Add v1 daemon operation names and register daemon aliases.
- Add a client-side error-code taxonomy while preserving legacy substring
  fallback until the daemon emits stable codes everywhere.

Verification:

- Dispatcher tests for v1 aliases.
- Classifier tests for typed code first, regex fallback second.

## Phase 6: Hygiene And Closeout

Issues addressed:

- 1.4: `.DS_Store` noise.
- Review closeout: all findings either fixed or explicitly resolved with a
  compatibility note.

Implementation shape:

- Verify `.DS_Store` is ignored and not tracked. Do not touch generated/cache
  content unless it is tracked.
- Update the implementation report with each phase result and test evidence.

Verification:

- `git ls-files` for `.DS_Store`.
- Final targeted test run.
