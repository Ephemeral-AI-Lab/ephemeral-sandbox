"""System toolkit — configuration and utility tools."""

from ephemeralos.tools.base import BaseToolkit
from ephemeralos.tools.brief_tool import BriefTool
from ephemeralos.tools.config_tool import ConfigTool
from ephemeralos.tools.remote_trigger_tool import RemoteTriggerTool
from ephemeralos.tools.sleep_tool import SleepTool


class SystemToolkit(BaseToolkit):
    """System utilities: config, brief mode, sleep, remote triggers."""

    def __init__(self) -> None:
        super().__init__(
            name="system",
            description="System utilities: config, brief mode, sleep, remote triggers",
            tools=[ConfigTool(), BriefTool(), SleepTool(), RemoteTriggerTool()],
        )
