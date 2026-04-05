"""Tests for prompts.system_prompt."""

from __future__ import annotations

from prompts.environment import EnvironmentInfo
from prompts.system_prompt import build_system_prompt


def _make_env(**overrides) -> EnvironmentInfo:
    defaults = dict(
        os_name="Linux",
        os_version="5.15.0",
        platform_machine="x86_64",
        shell="bash",
        cwd="/home/user/project",
        home_dir="/home/user",
        date="2026-04-01",
        python_version="3.10.17",
        is_git_repo=True,
        git_branch="main",
        hostname="testhost",
    )
    defaults.update(overrides)
    return EnvironmentInfo(**defaults)


def test_build_system_prompt_contains_environment():
    env = _make_env()
    prompt = build_system_prompt(env=env)
    assert "Linux 5.15.0" in prompt
    assert "x86_64" in prompt
    assert "bash" in prompt
    assert "/home/user/project" in prompt
    assert "2026-04-01" in prompt
    assert "3.10.17" in prompt
    assert "branch: main" in prompt


def test_build_system_prompt_no_git():
    env = _make_env(is_git_repo=False, git_branch=None)
    prompt = build_system_prompt(env=env)
    assert "Git:" not in prompt


def test_build_system_prompt_git_no_branch():
    env = _make_env(is_git_repo=True, git_branch=None)
    prompt = build_system_prompt(env=env)
    assert "Git: yes" in prompt
    assert "branch:" not in prompt


def test_build_system_prompt_custom_prompt():
    env = _make_env()
    prompt = build_system_prompt(agent_system_prompt="You are a helpful bot.", env=env)
    assert prompt.startswith("You are a helpful bot.")
    assert "Linux 5.15.0" in prompt


def test_build_system_prompt_default_returns_env_only():
    env = _make_env()
    prompt = build_system_prompt(env=env)
    assert "# Environment" in prompt
    assert "Linux 5.15.0" in prompt
