"""config family fixtures: gateway custody + baseline restore.

The gateway binds one fixed socket and writes fixed ``/tmp/eos-gateway.*``
paths, so this family cannot coexist with the baseline gateway other families
reuse. A package-scoped finalizer restores a baseline (``config/prd.yml``)
gateway after the family's last test — even on failure — mirroring the
``_session_sandbox_cleanup`` guarantee. The restored gateway recovers every
container labeled with the shared ``gateway_instance_id``, so the session
cleanup net still destroys anything a config test leaked.
"""

import pytest

from config import helpers


@pytest.fixture(scope="package", autouse=True)
def config_family_custody(gateway_up):
    """Own the gateway while the family runs; restore the baseline afterwards."""
    yield
    helpers.restore_baseline_gateway()


@pytest.fixture(scope="module")
def lane_a_daemon_yaml(tmp_path_factory, config_family_custody):
    """One family gateway per module, its ``manager.docker.daemon_config_yaml_path``
    pointed at a generated daemon YAML under pytest tmp.

    Returns that daemon YAML path. Lane A tests rewrite it per test (always a
    full regenerate from the baseline, never incremental) and create fresh
    sandboxes to observe the change.
    """
    root = tmp_path_factory.mktemp("config-family")
    daemon_yaml = helpers.make_config({}, root / "daemon.yml")
    gateway_yaml = helpers.make_config(
        {"manager": {"docker": {"daemon_config_yaml_path": str(daemon_yaml)}}},
        root / "gateway.yml",
    )
    helpers.start_gateway(gateway_yaml)
    return daemon_yaml
