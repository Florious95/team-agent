from __future__ import annotations

from team_agent.provider_cli.unsupported import UnsupportedCliPlug


class OpenCodeCliPlug(UnsupportedCliPlug):
    provider = "opencode"
    command_name = "opencode"
