"""Collaboration toolkit — multi-agent and user interaction tools."""

from ephemeralos.tools.agent_tool import AgentTool
from ephemeralos.tools.ask_user_question_tool import AskUserQuestionTool
from ephemeralos.tools.base import BaseToolkit
from ephemeralos.tools.send_message_tool import SendMessageTool
from ephemeralos.tools.team_create_tool import TeamCreateTool
from ephemeralos.tools.team_delete_tool import TeamDeleteTool


class CollaborationToolkit(BaseToolkit):
    """Multi-agent collaboration and user interaction."""

    def __init__(self) -> None:
        super().__init__(
            name="collaboration",
            description="Multi-agent collaboration and user interaction",
            tools=[
                AgentTool(),
                SendMessageTool(),
                TeamCreateTool(),
                TeamDeleteTool(),
                AskUserQuestionTool(),
            ],
        )
