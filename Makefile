.PHONY: install test lint clean

install:
	uv sync --extra dev

test:
	uv run pytest -q

lint:
	uv run ruff check backend/src backend/tests
	uv run python backend/tools/lint_dispatch_callsites.py

clean:
	rm -rf .pytest_cache .ruff_cache .mypy_cache backend/.pytest_cache
