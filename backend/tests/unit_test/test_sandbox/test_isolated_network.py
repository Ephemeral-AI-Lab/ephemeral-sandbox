"""Unit contracts for isolated workspace network helpers."""

from __future__ import annotations

import ipaddress
import subprocess
from typing import Any

from sandbox.isolated_workspace import network as network_module


def test_bridge_address_pool_allocates_lowest_available_address() -> None:
    pool = network_module.BridgeAddressPool(ipaddress.IPv4Network("192.0.2.0/29"))

    first = pool.allocate()
    second = pool.allocate()
    pool.free(first)

    assert [first, second, pool.allocate()] == [
        ipaddress.IPv4Address("192.0.2.2"),
        ipaddress.IPv4Address("192.0.2.3"),
        ipaddress.IPv4Address("192.0.2.2"),
    ]


def test_daemon_private_routes_detects_rfc1918_without_bridge_false_positive(
    monkeypatch,
) -> None:
    stdout = "\n".join(
        [
            "default via 172.17.0.1 dev eth0",
            "10.244.0.0/24 dev eos-shared0 proto kernel scope link src 10.244.0.1",
            "10.12.0.0/16 via 172.17.0.1 dev eth0",
            "172.16.5.0/24 dev eth1",
            "192.168.1.1 dev eth2",
            "8.8.8.0/24 dev eth3",
        ]
    )

    def fake_run(args: list[str], **_kwargs: Any) -> subprocess.CompletedProcess[str]:
        assert args == ["ip", "-o", "route", "show"]
        return subprocess.CompletedProcess(args=args, returncode=0, stdout=stdout)

    monkeypatch.setattr(network_module.subprocess, "run", fake_run)

    assert network_module.IsolatedNetwork().daemon_private_routes() == [
        "10.12.0.0/16",
        "172.16.5.0/24",
        "192.168.1.1",
    ]
