"""Bridge + veth + nftables + IP pool for isolated workspaces (Linux only).

State at daemon scope:
  - One bridge ``eos-shared0`` with gateway ``10.244.0.1/24``.
  - One MASQUERADE rule on outbound from ``10.244.0.0/24``.
  - One IMDS drop rule (``169.254.169.254``) on the forward chain.
  - (Opt-in) RFC1918-deny drop rule when ``rfc1918_egress == "deny"``.

Per-workspace state:
  - One veth pair: ``eos-iws-{handle_id[:6]}h`` (host end on bridge) and
    ``eos-iws-{handle_id[:6]}n`` (peer end moved into the netns, renamed
    ``eth0``).
  - One ``/32`` allocation from ``10.244.0.2 - 10.244.0.254``.

The IP pool itself is pure Python; ``ip`` / ``nft`` calls are Linux-only and
raise ``IsolatedNetworkUnavailable`` if the tools are missing.
"""

from __future__ import annotations

import ipaddress
import logging
import shutil
import subprocess
from dataclasses import dataclass
from typing import Literal


logger = logging.getLogger("sandbox.isolated_workspace.network")

BRIDGE_NAME = "eos-shared0"
BRIDGE_CIDR = ipaddress.IPv4Network("10.244.0.0/24")
GATEWAY = ipaddress.IPv4Address("10.244.0.1")
NFT_NAT_TABLE = "eos_iws_nat"
NFT_FILTER_TABLE = "eos_iws_filter"
IMDS_ADDR = "169.254.169.254"
RFC1918_NETS = ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16")
VETH_PREFIX = "eos-iws-"


class IsolatedNetworkUnavailable(RuntimeError):
    """Linux network primitives (ip / nft / CAP_NET_ADMIN) are not available."""


@dataclass(frozen=True)
class VethPair:
    handle_short: str
    host_name: str
    ns_name: str
    ns_ip: ipaddress.IPv4Address


class IPPool:
    """Allocates /32s from ``10.244.0.2 - 10.244.0.254``.

    Pure Python, no Linux deps. Lowest-IP-first O(N) scan; N ≤ 253 so this
    is fine.
    """

    def __init__(self, network: ipaddress.IPv4Network = BRIDGE_CIDR) -> None:
        self._network = network
        first = int(network.network_address) + 2  # skip .0 (network) and .1 (gw)
        last = int(network.broadcast_address) - 1  # skip .255 (broadcast)
        self._range = range(first, last + 1)
        self._allocated: set[ipaddress.IPv4Address] = set()

    @property
    def capacity(self) -> int:
        return len(self._range)

    @property
    def allocated(self) -> frozenset[ipaddress.IPv4Address]:
        return frozenset(self._allocated)

    def reserve(self, ip: ipaddress.IPv4Address) -> None:
        """Mark ``ip`` as in-use (used to rebuild pool state from manager.json)."""
        if int(ip) not in self._range:
            raise ValueError(f"{ip} is outside the pool range")
        self._allocated.add(ip)

    def allocate(self) -> ipaddress.IPv4Address:
        for value in self._range:
            candidate = ipaddress.IPv4Address(value)
            if candidate not in self._allocated:
                self._allocated.add(candidate)
                return candidate
        raise IsolatedNetworkUnavailable("isolated_workspace_ip_pool_exhausted")

    def free(self, ip: ipaddress.IPv4Address) -> None:
        self._allocated.discard(ip)


class IsolatedNetwork:
    """Owns ``eos-shared0`` bridge + static nft rules + per-ws veth wiring."""

    def __init__(
        self,
        *,
        rfc1918_egress: Literal["allow", "deny"] = "allow",
        pool: IPPool | None = None,
    ) -> None:
        self.rfc1918_egress = rfc1918_egress
        self.pool = pool or IPPool()
        self._initialized = False

    @property
    def initialized(self) -> bool:
        return self._initialized

    def initialize(self) -> None:
        """Install bridge + MASQUERADE + IMDS drop. Idempotent.

        Before installing the v2 tables, sweep any v1-named leftovers
        (``eos_pinws_*``) so renaming PRs don't leave stranded rules holding
        a netfilter slot. Pinned by ``test_v1_nft_table_migration_sweep``.
        """
        self._require_tools()
        self._sweep_v1_nft_tables()
        self._ensure_bridge()
        self._install_static_rules()
        self._initialized = True

    def _sweep_v1_nft_tables(self) -> None:
        """Delete any pre-v2 (``eos_pinws_*``) nft tables left over from a renaming PR."""
        for legacy in ("eos_pinws_nat", "eos_pinws_filter"):
            _nft_quiet("delete", "table", "inet", legacy)

    def install_veth(self, *, handle_id: str, root_pid: int) -> VethPair:
        """Create veth pair, attach host end to bridge with port isolation."""
        if not self._initialized:
            raise IsolatedNetworkUnavailable("isolated_network_not_initialized")
        # Linux IFNAMSIZ caps interface names at 15 chars.
        # VETH_PREFIX (8: "eos-iws-") + short (6) + suffix (1) = 15 exactly.
        short = handle_id[:6]
        host = f"{VETH_PREFIX}{short}h"
        ns = f"{VETH_PREFIX}{short}n"
        ns_ip = self.pool.allocate()
        try:
            _ip("link", "add", host, "type", "veth", "peer", "name", ns)
            _ip("link", "set", ns, "netns", str(root_pid))
            _ip("link", "set", host, "master", BRIDGE_NAME)
            _ip("link", "set", host, "type", "bridge_slave", "isolated", "on",
                "mcast_flood", "off")
            _ip("link", "set", host, "up")
        except Exception:
            self.pool.free(ns_ip)
            _ip_quiet("link", "del", host)
            raise
        return VethPair(handle_short=short, host_name=host, ns_name=ns, ns_ip=ns_ip)

    def teardown_veth(self, pair: VethPair) -> None:
        _ip_quiet("link", "del", pair.host_name)
        self.pool.free(pair.ns_ip)

    def reachable_rfc1918_subnets(self) -> list[str]:
        """Best-effort enumerate RFC1918 routes visible from the daemon's netns.

        Surfaces the daemon-host pivot surface (plan §4 step 7 / Scenario 5).
        """
        try:
            result = subprocess.run(
                ["ip", "-o", "route", "show"], capture_output=True, text=True, check=False,
            )
        except FileNotFoundError:
            return []
        hits: list[str] = []
        for line in result.stdout.splitlines():
            dst = line.split(maxsplit=1)[0] if line else ""
            for net in RFC1918_NETS:
                if dst.startswith(net.split("/")[0]):
                    hits.append(dst)
                    break
        return hits

    # ------------------------------------------------------------------
    def _ensure_bridge(self) -> None:
        existing = subprocess.run(
            ["ip", "-o", "link", "show", "dev", BRIDGE_NAME],
            capture_output=True, text=True, check=False,
        )
        if existing.returncode != 0:
            _ip("link", "add", BRIDGE_NAME, "type", "bridge")
            _ip("addr", "add", f"{GATEWAY}/24", "dev", BRIDGE_NAME)
            _ip("link", "set", BRIDGE_NAME, "up")

    def _install_static_rules(self) -> None:
        _nft("add", "table", "inet", NFT_NAT_TABLE)
        _nft("add", "chain", "inet", NFT_NAT_TABLE, "postrouting",
             "{ type nat hook postrouting priority 100 ; }")
        _nft("add", "rule", "inet", NFT_NAT_TABLE, "postrouting",
             f"ip saddr {BRIDGE_CIDR} oifname != \"{BRIDGE_NAME}\" masquerade")
        _nft("add", "table", "inet", NFT_FILTER_TABLE)
        _nft("add", "chain", "inet", NFT_FILTER_TABLE, "forward",
             "{ type filter hook forward priority 0 ; }")
        _nft("add", "rule", "inet", NFT_FILTER_TABLE, "forward",
             f"ip daddr {IMDS_ADDR} drop")
        if self.rfc1918_egress == "deny":
            for net in RFC1918_NETS:
                _nft("add", "rule", "inet", NFT_FILTER_TABLE, "forward",
                     f"ip daddr {net} ip daddr != {BRIDGE_CIDR} drop")

    def _require_tools(self) -> None:
        for tool in ("ip", "nft"):
            if shutil.which(tool) is None:
                raise IsolatedNetworkUnavailable(f"missing required tool: {tool}")


def _ip(*args: str) -> None:
    subprocess.run(["ip", *args], check=True, capture_output=True, text=True)


def _ip_quiet(*args: str) -> None:
    subprocess.run(
        ["ip", *args], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )


def _nft(*args: str) -> None:
    """Run `nft` and ignore EEXIST (`File exists`) so initialize() stays idempotent."""
    result = subprocess.run(
        ["nft", *args], capture_output=True, text=True, check=False,
    )
    if result.returncode == 0:
        return
    err = result.stderr.lower()
    if "file exists" in err or "already exists" in err:
        return
    raise subprocess.CalledProcessError(
        result.returncode, result.args, output=result.stdout, stderr=result.stderr,
    )


def _nft_quiet(*args: str) -> None:
    """Run ``nft`` ignoring all errors (used by the v1 migration sweep)."""
    subprocess.run(
        ["nft", *args], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False,
    )


__all__ = [
    "BRIDGE_CIDR",
    "BRIDGE_NAME",
    "GATEWAY",
    "IMDS_ADDR",
    "IPPool",
    "IsolatedNetwork",
    "IsolatedNetworkUnavailable",
    "NFT_FILTER_TABLE",
    "NFT_NAT_TABLE",
    "RFC1918_NETS",
    "VethPair",
    "VETH_PREFIX",
]
