from __future__ import annotations

from team_agent.provider_cli.adapter import (
    ProviderAdapter,
    ResumeUnavailable,
    agent_model,
    parse_time,
    read_json_object,
)
from team_agent.provider_cli.base import (
    ProviderCapabilityError,
    ProviderCliSocket,
    ProviderStartupInput,
)
from team_agent.provider_cli.claude import ClaudeCodeAdapter
from team_agent.provider_cli.codex import CodexAdapter
from team_agent.provider_cli.copilot import CopilotCliPlug
from team_agent.provider_cli.fake import FakeAdapter
from team_agent.provider_cli.gemini import GeminiCliAdapter
from team_agent.provider_cli.opencode import OpenCodeCliPlug
from team_agent.provider_cli.prompt import TEAMMATE_SYSTEM_PROMPT, compile_system_prompt
from team_agent.provider_cli.registry import PLUG_TYPES, build_plug

__all__ = [
    "ClaudeCodeAdapter",
    "CodexAdapter",
    "CopilotCliPlug",
    "FakeAdapter",
    "GeminiCliAdapter",
    "OpenCodeCliPlug",
    "PLUG_TYPES",
    "ProviderAdapter",
    "ProviderCapabilityError",
    "ProviderCliSocket",
    "ProviderStartupInput",
    "ResumeUnavailable",
    "TEAMMATE_SYSTEM_PROMPT",
    "agent_model",
    "build_plug",
    "compile_system_prompt",
    "parse_time",
    "read_json_object",
]
