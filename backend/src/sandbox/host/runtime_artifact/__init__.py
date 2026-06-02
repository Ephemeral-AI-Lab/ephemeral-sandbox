"""Pinned ``eosd`` runtime-artifact coupling surface (consumer side).

The ENTIRE coupling between the Python host and the external ``/sandbox`` Rust
runtime is: the wire protocol, the data-type contract (see
``sandbox/_contract_fixtures/``), and THIS pin. The host fetches + verifies the
``eosd-linux-{arch}`` binary against the SHA256 recorded here before upload/exec
(verify logic lands in a later phase — this module is data only).

Phase 0 local-upload closeout: the pins are host-local static-musl builds
packaged by ``xtask`` and uploaded with provider ``put_archive``. Minisign stays
empty until the later release-grade provenance gate.
"""

from __future__ import annotations

# eosd artifact this backend is pinned to. Bumped on a coordinated protocol or
# artifact release per CONTRACT.md.
EOSD_VERSION = "0.1.0-local.20260602"

# Per-arch SHA256 of the binary. Keys = container arch tokens the host maps to
# (amd64 / arm64).
EOSD_SHA256: dict[str, str] = {
    "amd64": "62e6d703964fb5525874629cb39c522e1aa96e25fd80f9619c3da205ef98b83f",
    "arm64": "e07a59546cecf931922386a91bf08a8ee5e1fa08747cbc45ee56462eeac4417b",
}

# Minisign trust-anchor public key (the release signing key). Empty for the
# Phase 0 local-upload pin; AV-8 fail-closed signature verification lands later.
MINISIGN_PUBLIC_KEY = ""

# Wire protocol version the pinned eosd speaks. MUST stay in lockstep with
# host.daemon_client.DAEMON_PROTOCOL_VERSION; a bump is a coordinated release
# event (CONTRACT.md).
PROTOCOL_VERSION = 1

__all__ = [
    "EOSD_VERSION",
    "EOSD_SHA256",
    "MINISIGN_PUBLIC_KEY",
    "PROTOCOL_VERSION",
]
