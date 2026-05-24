from __future__ import annotations

from typing import Any

from team_agent.provider_cli.base import ProviderCapabilityError, ProviderStartupInput


class UnsupportedCliPlug:
    provider = ""
    command_name = ""

    def build_command(self, startup: ProviderStartupInput) -> list[str]:
        _ = startup
        self._unsupported("start")

    def build_resume_command(self, startup: ProviderStartupInput) -> list[str]:
        _ = startup
        self._unsupported("resume")

    def build_fork_command(self, startup: ProviderStartupInput, source_session_id: str) -> list[str]:
        _ = startup, source_session_id
        self._unsupported("fork_or_branch")

    def capture_session_id(self, startup: ProviderStartupInput, timeout_s: float = 3.0) -> dict[str, Any] | None:
        _ = startup, timeout_s
        return None

    def cleanup(self, startup: ProviderStartupInput) -> None:
        _ = startup

    def _unsupported(self, capability: str) -> Any:
        raise ProviderCapabilityError(self.provider, capability, "provider plug exists but is not implemented yet")
