"""Eval-sandbox file fixtures and remote populate helper."""

from __future__ import annotations

import base64
import shlex
from pathlib import Path

from config.defaults import DEFAULT_SANDBOX_CI_ROOT
from sandbox.bash import wrap_bash_command


EVAL_SANDBOX_FILES: dict[str, str] = {
    "src/__init__.py": "",
    "src/main.py": '''"""Main application module."""
import os
from typing import Optional

DEBUG = False
VERSION = "1.0.0"


def get_config() -> dict:
    """Get application configuration."""
    return {
        "debug": DEBUG,
        "version": VERSION,
        "env": os.getenv("APP_ENV", "development"),
    }


def initialize() -> bool:
    """Initialize the application."""
    if DEBUG:
        print("Running in debug mode")
    return True


class App:
    """Main application class."""

    def __init__(self, name: str):
        self.name = name
        self.running = False

    def start(self) -> None:
        """Start the application."""
        self.running = True

    def stop(self) -> None:
        """Stop the application."""
        self.running = False


def main() -> None:
    """Entry point."""
    app = App("MyApp")
    app.start()
    print(f"Started {app.name}")
''',
    "src/utils.py": '''"""Utility functions."""
import json
import hashlib
from typing import Any


def sha256(data: str) -> str:
    """Compute SHA-256 hash of data."""
    return hashlib.sha256(data.encode()).hexdigest()


def format_json(data: Any) -> str:
    """Format data as JSON string."""
    return json.dumps(data, indent=2)


def parse_json(text: str) -> Any:
    """Parse JSON text into Python object."""
    return json.loads(text)


def truncate(text: str, max_length: int = 100) -> str:
    """Truncate text to max length."""
    if len(text) <= max_length:
        return text
    return text[:max_length] + "..."


def validate_email(email: str) -> bool:
    """Validate email address format."""
    return "@" in email and "." in email.split("@")[1]


def generate_id(prefix: str = "") -> str:
    """Generate a unique ID."""
    import time
    import random
    return f"{prefix}{int(time.time())}{random.randint(1000, 9999)}"
''',
    "src/app.py": '''"""Application module with routes and handlers."""
from flask import Flask, request, jsonify
from typing import Dict, Any


app = Flask(__name__)


@app.route("/")
def index() -> str:
    return "Hello, World!"


@app.route("/api/data", methods=["GET"])
def get_data() -> Dict[str, Any]:
    return jsonify({"status": "ok", "data": []})


@app.route("/api/data", methods=["POST"])
def post_data() -> Dict[str, Any]:
    payload = request.get_json()
    return jsonify({"status": "created", "data": payload}), 201


def create_app() -> app:
    return app
''',
    "src/models.py": '''"""Data models for the application."""
from dataclasses import dataclass
from typing import Optional, List
from datetime import datetime


@dataclass
class User:
    id: int
    username: str
    email: str
    created_at: datetime


@dataclass
class Post:
    id: int
    title: str
    content: str
    author_id: int
    published_at: Optional[datetime] = None


@dataclass
class Comment:
    id: int
    post_id: int
    user_id: int
    body: str
    created_at: datetime
''',
    "src/auth.py": '''"""Authentication module."""
from typing import Optional, Dict
import secrets


class AuthService:
    """Handle user authentication."""

    def __init__(self):
        self._tokens: Dict[str, int] = {}

    def login(self, username: str, password: str) -> Optional[str]:
        """Authenticate user and return token."""
        if not username or not password:
            return None
        token = secrets.token_urlsafe(32)
        self._tokens[token] = hash(username)
        return token

    def logout(self, token: str) -> bool:
        """Invalidate token."""
        if token in self._tokens:
            del self._tokens[token]
            return True
        return False

    def verify(self, token: str) -> Optional[int]:
        """Verify token and return user ID."""
        return self._tokens.get(token)
''',
}


def populate_sandbox_files(sandbox_id: str) -> None:
    from sandbox.testing.fixtures import get_sandbox_service

    svc = get_sandbox_service()
    raw_sandbox = svc.get_sandbox_object(sandbox_id)

    home_resp = raw_sandbox.process.exec("pwd", timeout=10)
    home = home_resp.result.strip() if home_resp.result else DEFAULT_SANDBOX_CI_ROOT

    resolved_files: dict[str, str] = {}
    for fp, content in EVAL_SANDBOX_FILES.items():
        abs_path = fp if fp.startswith("/") else f"{home}/{fp}"
        resolved_files[abs_path] = content

    dirs = {str(Path(fp).parent) for fp in resolved_files}
    for d in sorted(dirs):
        try:
            raw_sandbox.process.exec(f"mkdir -p {d}", timeout=10)
        except Exception as exc:
            print(f"Warning: mkdir -p {d} failed: {exc}")

    for file_path, content in resolved_files.items():
        try:
            raw_sandbox.process.exec(
                wrap_bash_command(_build_write_text_file_command(file_path, content)),
                timeout=10,
            )
        except Exception:
            try:
                escaped = shlex.quote(content)
                raw_sandbox.process.exec(f"printf %s {escaped} > {file_path}", timeout=10)
            except Exception as exc:
                print(f"Warning: Failed to write {file_path}: {exc}")


def _build_write_text_file_command(file_path: str, content: str) -> str:
    payload = base64.b64encode(content.encode("utf-8")).decode("ascii")
    script = """
import base64
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text(base64.b64decode(sys.argv[2]).decode("utf-8"), encoding="utf-8")
"""
    return (
        f"python3 -c {shlex.quote(script)} "
        f"{shlex.quote(file_path)} {shlex.quote(payload)}"
    )


__all__ = ["EVAL_SANDBOX_FILES", "populate_sandbox_files"]
