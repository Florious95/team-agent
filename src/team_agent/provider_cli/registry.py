from __future__ import annotations

from team_agent.provider_cli.base import ProviderCliSocket
from team_agent.provider_cli.copilot import CopilotCliPlug
from team_agent.provider_cli.opencode import OpenCodeCliPlug


PLUG_TYPES = {
    "opencode": OpenCodeCliPlug,
    "copilot": CopilotCliPlug,
}


def build_plug(provider: str) -> ProviderCliSocket:
    try:
        return PLUG_TYPES[provider]()
    except KeyError as exc:
        raise KeyError(f"Unsupported provider plug: {provider}") from exc
