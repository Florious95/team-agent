from __future__ import annotations

from team_agent.provider_cli.unsupported import UnsupportedCliPlug


class CopilotCliPlug(UnsupportedCliPlug):
    provider = "copilot"
    command_name = "copilot"
