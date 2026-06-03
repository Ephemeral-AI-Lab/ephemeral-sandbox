from sandbox._shared.command_exec_resource_metrics import (
    _parse_cgroup_io_stat,
    _parse_cgroup_key_values,
)


def test_parse_cgroup_key_values_ignores_malformed_rows() -> None:
    assert _parse_cgroup_key_values(
        "usage_usec 123\n"
        "user_usec 45\n"
        "system_usec 6\n"
        "malformed\n"
        "nr_periods nope\n"
    ) == {
        "usage_usec": 123.0,
        "user_usec": 45.0,
        "system_usec": 6.0,
    }


def test_parse_cgroup_io_stat_aggregates_devices() -> None:
    assert _parse_cgroup_io_stat(
        "8:0 rbytes=100 wbytes=200 rios=3 wios=4\n"
        "8:16 rbytes=50 wbytes=70 rios=1 wios=2 bogus nope=x\n"
    ) == {
        "rbytes": 150.0,
        "wbytes": 270.0,
        "rios": 4.0,
        "wios": 6.0,
    }
