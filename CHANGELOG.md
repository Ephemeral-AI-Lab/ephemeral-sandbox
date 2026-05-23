# Changelog

All notable changes to EphemeralOS should be recorded in this file.

The format is based on Keep a Changelog, and this project currently tracks changes in a lightweight, repository-oriented way.

## [Unreleased]

### Added

- `diagnose` skill: trace agent run failures and regressions using structured evidence from run artifacts.
- OpenAI-compatible API client (`--api-format openai`) supporting any provider that implements the OpenAI `/v1/chat/completions` format, including Alibaba DashScope, DeepSeek, GitHub Models, Groq, Together AI, Ollama, and more.
- `EPHEMERALOS_API_FORMAT` environment variable for selecting the API format.
- `OPENAI_API_KEY` fallback when using OpenAI-format providers.
- GitHub Actions CI workflow for Python linting, tests, and frontend TypeScript checks.
- `CONTRIBUTING.md` with local setup, validation commands, and PR expectations.
- `docs/SHOWCASE.md` with concrete EphemeralOS usage patterns and demo commands.
- GitHub issue templates and a pull request template.

### Fixed

- Memory scanner now parses YAML frontmatter (`name`, `description`, `type`) instead of returning raw `---` as description.
- Memory search matches against body content in addition to metadata, with metadata weighted higher for relevance.
- Memory search tokenizer handles Han characters for multilingual queries.
- Fixed duplicate response in React TUI caused by double Enter key submission in the input handler.

### Changed

- README now links to contribution docs, changelog, showcase material, and provider compatibility guidance.
- README quick start now includes a one-command demo and clearer provider compatibility notes.
- **Sandbox API verb rename (PR-0 of `unify_sandbox_tool_api_PLAN.md`):** `search_content` → `grep` and `glob_files` → `glob` across the entire stack. Renamed `SearchContentRequest`/`SearchContentResult` → `GrepRequest`/`GrepResult`; `DAEMON_OP_SEARCH_CONTENT`/`DAEMON_OP_FIND_FILES` → `DAEMON_OP_GREP`/`DAEMON_OP_GLOB` with new wire ops `api.v1.grep` / `api.v1.glob`; `SEARCH_CONTENT_TIMEOUT_S`/`FIND_FILES_TIMEOUT_S` → `GREP_TIMEOUT_S`/`GLOB_TIMEOUT_S`; renamed `SearchContentResult.mode` → `output_mode` (freeing the `mode` slot for the future workspace discriminator). Split `sandbox/daemon/handler/search.py` into sibling modules `grep.py` (regex-scan) and `glob.py` (pattern enumeration). The iws op `api.isolated_workspace.search_content` is renamed to `api.isolated_workspace.grep` (full iws tool-op surface is deleted in PR-A). No behavior change.
- README provider compatibility section updated to include OpenAI-format providers.

## [0.1.0] - 2026-04-01

### Added

- Initial public release of EphemeralOS.
- Core agent loop, tool registry, permission system, hooks, skills, plugins, MCP support, and terminal UI.
