"""Codifies the verifier/evaluator inline-edit policy from remove-ask-resolver.

After the ask_resolver helper was removed, verifier and evaluator profiles
inherit `edit_file` / `write_file` directly so they can apply trivial
inline fixes (typos, single-line obvious bugs) without spawning a helper
agent. The scope boundary is enforced by profile prose plus the advisor
pre-terminal gate (which sees full edit-input transcripts).

This test pins the policy at the profile-MD level so a future edit that
strips the write tools or re-introduces a resolver_limit reminder fails
loudly.
"""

from __future__ import annotations

from pathlib import Path

from agents import load_agents_dir


_BACKEND_SRC = Path(__file__).resolve().parents[3] / "src"
_MAIN_PROFILE_DIR = _BACKEND_SRC / "agents" / "profile" / "main"


def _load_named(name: str):
    loaded = load_agents_dir(_MAIN_PROFILE_DIR)
    by_name = {a.name: a for a in loaded}
    assert name in by_name, f"agent {name!r} not found in {_MAIN_PROFILE_DIR}"
    return by_name[name]


def test_verifier_has_inline_edit_tools_and_no_resolver_residue():
    verifier = _load_named("verifier")
    assert "edit_file" in verifier.allowed_tools
    assert "write_file" in verifier.allowed_tools
    assert "ask_resolver" not in verifier.allowed_tools
    assert verifier.notification_triggers == []


def test_evaluator_has_inline_edit_tools_and_no_resolver_residue():
    evaluator = _load_named("evaluator")
    assert "edit_file" in evaluator.allowed_tools
    assert "write_file" in evaluator.allowed_tools
    assert "ask_resolver" not in evaluator.allowed_tools
    assert evaluator.notification_triggers == []
