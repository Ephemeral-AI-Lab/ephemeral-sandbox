"""Bridge + veth + nftables + IP pool for isolated workspaces (Linux only).

State at daemon scope:
  - One bridge ``eos-shared0`` with gateway ``10.244.0.1/24``.
  - One MASQUERADE rule on outbound from ``10.244.0.0/24``.
  - One IMDS drop rule (``169.254.169.254``) on the forward chain.
  - (Opt-in) RFC1918-deny drop rule when ``rfc1918_egress == "deny"``.

Per-workspace state:
  - One veth pair: ``eos-iws-{handle_id[:6]}h`` (host end on bridge) and
    ``eos-iws-{handle_id[:6]}n`` (peer end moved into the netns).
  - One ``/32`` allocation from ``10.244.0.2 - 10.244.0.254``.

The IP pool itself is pure Python; ``ip`` / ``nft`` calls are Linux-only and
raise ``IsolatedNetworkUnavailable`` if the tools are missing.
"""

from __future__ import annotations

import ipaddress
import shutil
import subprocess
from dataclasses import dataclass
from typing import Literal


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
class VethAllocation:
    host_name: str
    ns_ip: ipaddress.IPv4Address


class BridgeAddressPool:
    """Allocates /32s from ``10.244.0.2 - 10.244.0.254``.

    Pure Python, no Linux deps. Lowest-IP-first O(N) scan; N ≤ 253 so this
    is fine.
    """

    def __init__(self, network: ipaddress.IPv4Network = BRIDGE_CIDR) -> None:
        first = int(network.network_address) + 2  # skip .0 (network) and .1 (gw)
        last = int(network.broadcast_address) - 1  # skip .255 (broadcast)
        self._range = range(first, last + 1)
        self._allocated: set[ipaddress.IPv4Address] = set()

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
        pool: BridgeAddressPool | None = None,
    ) -> None:
        self.rfc1918_egress = rfc1918_egress
        self.pool = pool or BridgeAddressPool()
        self._initialized = False

    @property
    def initialized(self) -> bool:
        return self._initialized

    def initialize(self) -> None:
        """Install bridge + MASQUERADE + IMDS drop. Idempotent."""
        self._require_tools()
        self._ensure_bridge()
        self._install_static_rules()
        self._initialized = True

    def install_veth(self, *, handle_id: str, root_pid: int) -> VethAllocation:
        """Create veth pair, attach host end to bridge with port isolation.

        Also configures the ns-side end inside the iws's net namespace:
        brings it up, assigns ``ns_ip`` with the /24 prefix, and adds a
        default route via the bridge gateway. Without this the iws's veth
        is a bare interface — no IP, no link up, no route — and outbound
        traffic (ping/curl/DNS) fails with "Network unreachable". The
        ns-side config is intentionally minimal (single /32 in the bridge
        /24, default via gateway) so the design's MASQUERADE postrouting
        rule is the sole egress path.
        """
        if not self._initialized:
            raise IsolatedNetworkUnavailable("isolated_network_not_initialized")
        host, ns = _veth_names(handle_id)
        ns_ip = self.pool.allocate()
        try:
            _ip("link", "add", host, "type", "veth", "peer", "name", ns)
            _ip("link", "set", ns, "netns", str(root_pid))
            _ip("link", "set", host, "master", BRIDGE_NAME)
            _ip(
                "link",
                "set",
                host,
                "type",
                "bridge_slave",
                "isolated",
                "on",
                "mcast_flood",
                "off",
            )
            _ip("link", "set", host, "up")
            # Configure the ns-side inside the iws net namespace via nsenter.
            # The iws-side ns_holder.py is the holder of the new net ns but it
            # only brings up ``lo`` and purges IPv6; veth-side IP+route are
            # left to the daemon (this code) so the ns_holder stays minimal.
            prefix = BRIDGE_CIDR.prefixlen
            _ip_ns(root_pid, "link", "set", ns, "up")
            _ip_ns(root_pid, "addr", "add", f"{ns_ip}/{prefix}", "dev", ns)
            _ip_ns(root_pid, "route", "add", "default", "via", str(GATEWAY))
        except Exception:
            self.pool.free(ns_ip)
            _ip_quiet("link", "del", host)
            raise
        return VethAllocation(host_name=host, ns_ip=ns_ip)

    def teardown_veth(self, allocation: VethAllocation) -> None:
        _ip_quiet("link", "del", allocation.host_name)
        self.pool.free(allocation.ns_ip)

    def daemon_private_routes(self) -> list[str]:
        """Best-effort enumerate private routes visible from the daemon's netns.

        Surfaces the daemon-host pivot surface (plan §4 step 7 / Scenario 5).
        """
        try:
            result = subprocess.run(
                ["ip", "-o", "route", "show"],
                capture_output=True,
                text=True,
                check=False,
            )
        except FileNotFoundError:
            return []
        routes: list[str] = []
        for line in result.stdout.splitlines():
            dst = line.split(maxsplit=1)[0] if line else ""
            if _is_host_private_route(dst):
                routes.append(dst)
        return routes

    # ------------------------------------------------------------------
    def _ensure_bridge(self) -> None:
        existing = subprocess.run(
            ["ip", "-o", "link", "show", "dev", BRIDGE_NAME],
            capture_output=True,
            text=True,
            check=False,
        )
        if existing.returncode != 0:
            _ip("link", "add", BRIDGE_NAME, "type", "bridge")
            _ip("addr", "add", f"{GATEWAY}/24", "dev", BRIDGE_NAME)
            _ip("link", "set", BRIDGE_NAME, "up")

    def _install_static_rules(self) -> None:
        _nft("add", "table", "inet", NFT_NAT_TABLE)
        _nft(
            "add",
            "chain",
            "inet",
            NFT_NAT_TABLE,
            "postrouting",
            "{ type nat hook postrouting priority 100 ; }",
        )
        _nft(
            "add",
            "rule",
            "inet",
            NFT_NAT_TABLE,
            "postrouting",
            f'ip saddr {BRIDGE_CIDR} oifname != "{BRIDGE_NAME}" masquerade',
        )
        _nft("add", "table", "inet", NFT_FILTER_TABLE)
        _nft(
            "add",
            "chain",
            "inet",
            NFT_FILTER_TABLE,
            "forward",
            "{ type filter hook forward priority 0 ; }",
        )
        _nft("add", "rule", "inet", NFT_FILTER_TABLE, "forward", f"ip daddr {IMDS_ADDR} drop")
        if self.rfc1918_egress == "deny":
            for net in RFC1918_NETS:
                _nft(
                    "add",
                    "rule",
                    "inet",
                    NFT_FILTER_TABLE,
                    "forward",
                    f"ip daddr {net} ip daddr != {BRIDGE_CIDR} drop",
                )

    def _require_tools(self) -> None:
        for tool in ("ip", "nft"):
            if shutil.which(tool) is None:
                raise IsolatedNetworkUnavailable(f"missing required tool: {tool}")


def _veth_names(handle_id: str) -> tuple[str, str]:
    # Linux IFNAMSIZ caps interface names at 15 chars.
    # VETH_PREFIX (8: "eos-iws-") + short (6) + suffix (1) = 15 exactly.
    short = handle_id[:6]
    return f"{VETH_PREFIX}{short}h", f"{VETH_PREFIX}{short}n"


def _is_host_private_route(destination: str) -> bool:
    try:
        route = ipaddress.ip_network(destination, strict=False)
    except ValueError:
        return False
    if not isinstance(route, ipaddress.IPv4Network):
        return False
    if route.subnet_of(BRIDGE_CIDR):
        return False
    return any(route.overlaps(ipaddress.IPv4Network(net)) for net in RFC1918_NETS)


def _ip(*args: str) -> None:
    result = subprocess.run(
        ["ip", *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        # Surface stderr in the exception so debugging cascading network
        # failures doesn't require re-running with a custom logger. The
        # default ``check=True`` swallows it and only shows the cmd + rc.
        raise RuntimeError(
            f"ip {' '.join(args)} failed rc={result.returncode}: stderr={result.stderr.strip()!r}"
        )


def _ip_quiet(*args: str) -> None:
    subprocess.run(
        ["ip", *args],
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def _ip_ns(root_pid: int, *args: str) -> None:
    """Run ``ip <args>`` inside the net namespace owned by ``root_pid``.

    Uses nsenter against ``/proc/<pid>/ns/net`` rather than ``setns(2)`` from
    Python so the call stays single-step and inherits the daemon's existing
    capabilities. The iws ns_holder's user_ns is the parent's (mapped via
    --map-root-user), so root in the daemon's user_ns is root inside the
    iws net_ns — no extra capability dance needed for ip address/route ops.
    """
    subprocess.run(
        ["nsenter", "-t", str(root_pid), "-n", "--", "ip", *args],
        check=True,
        capture_output=True,
        text=True,
    )


def _nft(*args: str) -> None:
    """Run `nft` and ignore EEXIST (`File exists`) so initialize() stays idempotent."""
    result = subprocess.run(
        ["nft", *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode == 0:
        return
    err = result.stderr.lower()
    if "file exists" in err or "already exists" in err:
        return
    raise subprocess.CalledProcessError(
        result.returncode,
        result.args,
        output=result.stdout,
        stderr=result.stderr,
    )
