from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Protocol, runtime_checkable


@dataclass(frozen=True)
class ProviderStartupInput:
    agent_id: str
    provider: str
    model: str | None
    workspace: Path
    system_prompt: str
    mcp_config: dict[str, Any] = field(default_factory=dict)
    profile: dict[str, Any] = field(default_factory=dict)
    permission_mode: str | None = None
    runtime_flags: dict[str, Any] = field(default_factory=dict)
    previous_session: dict[str, Any] = field(default_factory=dict)


class ProviderCapabilityError(RuntimeError):
    def __init__(self, provider: str, capability: str, reason: str) -> None:
        self.provider = provider
        self.capability = capability
        self.reason = reason
        super().__init__(f"{provider} provider does not support {capability}: {reason}")


@runtime_checkable
class ProviderCliSocket(Protocol):
    provider: str
    command_name: str

    def build_command(self, startup: ProviderStartupInput) -> list[str]:
        ...

    def build_resume_command(self, startup: ProviderStartupInput) -> list[str]:
        ...

    def build_fork_command(self, startup: ProviderStartupInput, source_session_id: str) -> list[str]:
        ...

    def capture_session_id(self, startup: ProviderStartupInput, timeout_s: float = 3.0) -> dict[str, Any] | None:
        ...

    def cleanup(self, startup: ProviderStartupInput) -> None:
        ...
