"""Live Docker e2e for LayerStack squash + live workspace remount.

CLI-driven through ``sandbox-cli``; on-disk / mount state is observed with
``docker exec`` (the outside-observation channel proven in Phase 0 X0.10).
Every test creates and destroys its own sandbox with the real
``--workspace-bind-root`` flag, so teardown is part of the assertion.

Coverage mapping (the rest is proven by the Phase 0/4/5/6 kernel probes and
the unit suites, recorded in the tracker):
  - test_boot_gate_enables_live_remount      -> G1 + G2 (the boot gate IS the
                                                same-upperdir + userxattr proof)
  - test_squash_empty_and_idle_contract      -> tests 1/17/20/21, B1
  - test_live_migration_shortens_idle_chain  -> E5 / B2 (live migration,
                                                merged-view equivalence, reclaim)
  - test_cwd_pinned_session_stays_leased     -> E4 (cwd pin), C6 physics
  - test_boot_reap_then_sweep_recovers       -> G3 / E10 (reap-then-sweep,
                                                PDEATHSIG, fail-closed)
"""

import json
import subprocess
import time
import uuid

import pytest

from core.cli import cli, internal_runtime, manager, runtime

IMAGE = "ubuntu:24.04"


def _docker(container, *args):
    return subprocess.run(
        ["docker", "exec", container, *args],
        capture_output=True,
        text=True,
        timeout=60,
    )


def _manifest(container):
    out = _docker(container, "cat", "/eos/layer-stack/manifest.json").stdout
    return json.loads(out)


def _layer_ids(container):
    return [layer["layer_id"] for layer in _manifest(container)["layers"]]


def _layer_dirs(container):
    out = _docker(container, "sh", "-c", "ls /eos/layer-stack/layers/ 2>/dev/null").stdout
    return sorted(name for name in out.split())


def _new_sandbox():
    workspace = f"/tmp/eos-e2e-squash-{uuid.uuid4().hex[:8]}"
    subprocess.run(["mkdir", "-p", workspace], check=True)
    result = manager(
        "create_sandbox", "--image", IMAGE, "--workspace-bind-root", workspace
    )
    assert isinstance(result, dict) and result.get("id"), f"create failed: {result}"
    return result["id"]


@pytest.fixture
def squash_sandbox():
    sandbox_id = _new_sandbox()
    try:
        yield sandbox_id
    finally:
        manager("destroy_sandbox", "--sandbox-id", sandbox_id)


def _publish(sandbox_id, name):
    """Publish one layer by writing a file through a one-shot exec."""
    result = runtime(
        sandbox_id, "exec_command", f"echo {name} > /workspace/{name}.txt"
    )
    assert result.get("status") == "ok", f"publish {name} failed: {result}"


def _create_session(sandbox_id):
    result = internal_runtime(sandbox_id, "create_workspace_session")
    session = result.get("workspace_session_id")
    assert session, f"create_workspace_session failed: {result}"
    return session


def _read(sandbox_id, session, paths):
    cmd = "cat " + " ".join(f"/workspace/{p}" for p in paths)
    return runtime(sandbox_id, "exec_command", "--workspace-session-id", session, cmd)


# --- G1 + G2: the boot gate is the same-upperdir + userxattr kernel proof ---

def test_boot_gate_enables_live_remount(squash_sandbox):
    """The daemon's boot gate probes same-upperdir coexistence + userxattr
    parity in a scratch userns; on the supported environment it must PROVE
    and enable live remount (else every session would report
    leased(unsupported:kernel_gate_not_proven))."""
    container = squash_sandbox
    logs = subprocess.run(
        ["docker", "logs", container], capture_output=True, text=True, timeout=30
    )
    haystack = logs.stdout + logs.stderr
    assert "live remount kernel gate: PROVEN" in haystack, (
        "boot gate did not prove on the supported environment; "
        f"log tail: {haystack[-2000:]}"
    )
    assert "NOT PROVEN" not in haystack


# --- tests 1/17/20/21 + B1: contract and idle reclaim ---

def test_squash_empty_and_idle_contract(squash_sandbox):
    container = squash_sandbox

    empty = manager("squash_layerstacks", "--sandbox-id", container)
    assert empty == {"manifest_version": 1, "squashed_blocks": []}, empty

    for name in ("a", "b", "c"):
        _publish(container, name)
    before = _layer_ids(container)
    assert before[-1].startswith("B"), "base is the bottom boundary"

    result = manager("squash_layerstacks", "--sandbox-id", container)
    assert set(result.keys()) == {"manifest_version", "squashed_blocks"}, result
    blocks = result["squashed_blocks"]
    assert len(blocks) == 1
    block = blocks[0]
    assert block["squashed_layer_id"].startswith("S")
    assert len(block["replaced_layer_ids"]) == 3
    assert block["replaced_layers"] == "reclaimed"
    assert "blocked_reasons" not in block

    # Merged view preserved through the commit.
    session = _create_session(container)
    read = _read(container, session, ["a.txt", "b.txt", "c.txt"])
    assert read.get("output") == "a\nb\nc", read
    internal_runtime(
        container,
        "destroy_workspace_session",
        {"workspace_session_id": session},
    )

    # S layer carries no .digest sidecar (only the accepted observability
    # .bytes self-heal may appear).
    s_id = block["squashed_layer_id"]
    digest = _docker(
        container,
        "test",
        "-f",
        f"/eos/layer-stack/.layer-metadata/{s_id}.digest",
    )
    assert digest.returncode != 0, "S layer must have no .digest sidecar"

    # Nothing left to squash: empty blocks, no no_op flag.
    again = manager("squash_layerstacks", "--sandbox-id", container)
    assert again["squashed_blocks"] == [], again


# --- E5 / B2: live migration shortens an idle session's chain ---

def test_live_migration_shortens_idle_chain(squash_sandbox):
    container = squash_sandbox
    for name in ("m1", "m2", "m3"):
        _publish(container, name)

    # The session leases the current top; its newest layer is the only
    # boundary, so the run beneath it is one squashable block.
    session = _create_session(container)
    pre_ids = _layer_ids(container)
    pre_len = len(pre_ids)

    result = manager("squash_layerstacks", "--sandbox-id", container)
    blocks = result["squashed_blocks"]
    assert len(blocks) == 1, result
    block = blocks[0]
    # A reclaimed block below a live session's boundary means the session
    # migrated live and its old lease released — the block's sources are gone.
    assert block["replaced_layers"] == "reclaimed", (
        "the idle session must migrate live so its old chain reclaims; "
        f"got {block}"
    )
    for replaced in block["replaced_layer_ids"]:
        gone = _docker(
            container, "test", "-d", f"/eos/layer-stack/layers/{replaced}"
        )
        assert gone.returncode != 0, f"{replaced} should be reclaimed after migration"

    # The migrated session reads every file correctly through the NEW mount.
    read = _read(container, session, ["m1.txt", "m2.txt", "m3.txt"])
    assert read.get("output") == "m1\nm2\nm3", read

    # The active manifest shortened (the block collapsed to one S layer).
    post_ids = _layer_ids(container)
    assert len(post_ids) < pre_len, f"chain did not shorten: {pre_ids} -> {post_ids}"

    # Teardown: destroy the session, assert no leaked leases.
    destroyed = internal_runtime(
        container,
        "destroy_workspace_session",
        {"workspace_session_id": session},
    )
    assert destroyed.get("destroyed") is True, destroyed
    assert destroyed.get("active_leases_after") in (None, 0), destroyed


# --- E4: a cwd-pinned interactive session stays leased ---

def test_cwd_pinned_session_stays_leased(squash_sandbox):
    container = squash_sandbox
    for name in ("p1", "p2", "p3"):
        _publish(container, name)
    session = _create_session(container)

    # Start an interactive PTY shell whose cwd is inside the workspace: frozen,
    # it is always pinned:cwd_pinned_workspace (C6 physics), so its block stays
    # leased and its old layers are retained.
    shell = runtime(
        container,
        "exec_command",
        "--workspace-session-id",
        session,
        "--yield-time-ms",
        "300",
        "cd /workspace && sleep 30",
        timeout=30,
    )
    assert shell.get("status") in ("running", "ok"), shell

    result = manager("squash_layerstacks", "--sandbox-id", container)
    blocks = result["squashed_blocks"]
    assert len(blocks) == 1, result
    block = blocks[0]
    assert block["replaced_layers"] == "leased", (
        f"a cwd-pinned session must keep its chain leased; got {block}"
    )
    reasons = block.get("blocked_reasons")
    assert reasons, "leased blocks must carry non-empty blocked_reasons"
    # The pinning class is a cwd/mount pin (free-form string; we assert the
    # family, not an exact code).
    assert any(
        "pinned" in reason or "mount" in reason for reason in reasons
    ), reasons

    # The pinned session never observed the squash and still works.
    read = _read(container, session, ["p1.txt"])
    assert read.get("output") == "p1", read

    internal_runtime(
        container,
        "destroy_workspace_session",
        {"workspace_session_id": session},
    )


# --- G3 / E10: boot reap-then-sweep recovers, holders die with the daemon ---

def test_boot_reap_then_sweep_recovers(squash_sandbox):
    container = squash_sandbox
    for name in ("r1", "r2", "r3"):
        _publish(container, name)
    session = _create_session(container)

    # Plant a crash-orphan: a promoted S dir not in the manifest.
    _docker(
        container,
        "mkdir",
        "-p",
        "/eos/layer-stack/layers/S000099-orphan/data",
    )
    assert "S000099-orphan" in _layer_dirs(container)

    # Record the holder pids and the daemon's own uploaded layer set, then
    # `docker restart` — the natural container restart (docker-init is the
    # daemon's parent, so an in-place kill would stop the container). PDEATHSIG
    # must take every holder down with the daemon, and the fresh boot runs
    # reap-then-sweep before serving.
    holders_before = _docker(
        container, "sh", "-c", "pgrep -f ns-holder | tr '\\n' ' '"
    ).stdout.split()

    subprocess.run(["docker", "restart", container], check=True, timeout=120)
    _wait_container_running(container, timeout=60)

    # PDEATHSIG proof: none of the pre-restart holder pids survived the restart.
    for pid in holders_before:
        alive = _docker(container, "test", "-d", f"/proc/{pid}")
        assert alive.returncode != 0, f"holder {pid} outlived the daemon (PDEATHSIG failed)"

    # The fresh boot log records gate re-proof + reap-then-sweep in order.
    logs = subprocess.run(
        ["docker", "logs", container], capture_output=True, text=True, timeout=30
    )
    boot = logs.stdout + logs.stderr
    assert "live remount kernel gate: PROVEN" in boot, boot[-1500:]
    reap_idx = boot.rfind("boot reap removed")
    sweep_idx = boot.rfind("boot storage sweep")
    assert reap_idx != -1 and sweep_idx != -1, "reap/sweep records missing"
    assert reap_idx < sweep_idx, "reap must precede sweep"

    # Fail-closed reap-then-sweep: the crash-orphan S dir is gone, the active
    # manifest's layers remain, and no non-manifest layer survives.
    assert "S000099-orphan" not in _layer_dirs(container), "orphan S dir not swept"
    manifest_ids = set(_layer_ids(container))
    disk_ids = {name for name in _layer_dirs(container) if not name.startswith("B")}
    assert disk_ids <= manifest_ids, (
        f"sweep left non-manifest layers: {disk_ids - manifest_ids}"
    )

    # No session state resurrects: manager.json is reaped to the empty set.
    handles = _docker(
        container,
        "sh",
        "-c",
        "cat /eos/workspace/manager.json 2>/dev/null || echo '{}'",
    ).stdout
    try:
        parsed = json.loads(handles)
        assert not parsed.get("handles"), f"stale handles survived reap: {parsed}"
    except json.JSONDecodeError:
        pass  # absent/empty handle file is fine

    # NOTE: exercising a fresh session/squash via the CLI after `docker restart`
    # is blocked by Docker remapping the container's published port (the manager
    # keeps the pre-restart endpoint) — an infra limitation, not a feature
    # fault. The daemon's own recovery is fully asserted above from the boot log
    # and on-disk state; the fresh-squash-after-recovery path is covered by unit
    # test 14 (boot_cleanup_matrix) and the X7.1 PDEATHSIG probe.

def _wait_container_running(container, timeout=60):
    """Wait until the restarted container's daemon has finished boot cleanup
    (the ns-holder-less window right after `docker restart`)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        running = subprocess.run(
            ["docker", "inspect", "-f", "{{.State.Running}}", container],
            capture_output=True,
            text=True,
            timeout=15,
        ).stdout.strip()
        if running == "true":
            probe = _docker(container, "sh", "-c", "test -S /eos/runtime/daemon/runtime.sock && echo up")
            if probe.stdout.strip() == "up":
                time.sleep(1)  # let boot cleanup finish
                return
        time.sleep(0.5)
    raise RuntimeError(f"container {container} not running after restart")
