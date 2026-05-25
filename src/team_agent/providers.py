from __future__ import annotations

import os
import shlex
from pathlib import Path
from typing import Any

from team_agent.paths import repo_root
from team_agent.profiles import ensure_compatible_claude_mcp_config, prepare_agent_profile_launch
from team_agent.provider_cli.adapter import (
    ProviderAdapter,
    ResumeUnavailable,
    agent_model,
    parse_time,
    read_json_object,
)
from team_agent.provider_cli.claude import ClaudeCodeAdapter
from team_agent.provider_cli.codex import CodexAdapter
from team_agent.provider_cli.fake import FakeAdapter
from team_agent.provider_cli.gemini import GeminiCliAdapter
from team_agent.provider_cli.prompt import TEAMMATE_SYSTEM_PROMPT, compile_system_prompt

# Import-time assertions: every re-exported symbol must resolve at import,
# so a refactor that drops one of these provider_cli APIs fails loudly here
# instead of silently breaking callers that still import from providers.
assert issubclass(ClaudeCodeAdapter, ProviderAdapter)
assert issubclass(CodexAdapter, ProviderAdapter)
assert issubclass(GeminiCliAdapter, ProviderAdapter)
assert issubclass(FakeAdapter, ProviderAdapter)
assert callable(compile_system_prompt)
assert isinstance(TEAMMATE_SYSTEM_PROMPT, str)
assert callable(agent_model)
assert callable(parse_time)
assert callable(read_json_object)
assert issubclass(ResumeUnavailable, RuntimeError)


ADAPTERS: dict[str, ProviderAdapter] = {
    "claude": ClaudeCodeAdapter(),
    "claude_code": ClaudeCodeAdapter(),
    "codex": CodexAdapter(),
    "gemini_cli": GeminiCliAdapter(),
    "fake": FakeAdapter(),
}


def get_adapter(provider: str) -> ProviderAdapter:
    try:
        return ADAPTERS[provider]
    except KeyError as exc:
        raise KeyError(f"Unsupported provider: {provider}") from exc


def shell_command_for_agent(agent: dict[str, Any], workspace: Path, mcp_config: dict[str, Any]) -> str:
    adapter = get_adapter(agent["provider"])
    command_agent = dict(agent)
    profile_launch = command_agent.get("_provider_profile") or prepare_agent_profile_launch(workspace, command_agent)
    if profile_launch:
        command_agent["_provider_profile"] = profile_launch
        agent["_provider_profile"] = profile_launch
    if (
        agent.get("provider") in {"claude", "claude_code"}
        and profile_launch
        and profile_launch.get("auth_mode") == "compatible_api"
        and profile_launch.get("claude_projects_root")
    ):
        ensure_compatible_claude_mcp_config(workspace, agent["id"], mcp_config)
    cmd = adapter.build_command(command_agent, workspace, mcp_config)
    if command_agent.get("_session_id"):
        agent["_session_id"] = command_agent["_session_id"]
    return shell_command(cmd, agent["id"], workspace, profile_launch)


def shell_resume_command_for_agent(
    agent: dict[str, Any],
    agent_state: dict[str, Any],
    workspace: Path,
    mcp_config: dict[str, Any],
) -> str:
    adapter = get_adapter(agent["provider"])
    command_agent = dict(agent)
    profile_launch = command_agent.get("_provider_profile") or prepare_agent_profile_launch(workspace, command_agent)
    if profile_launch:
        command_agent["_provider_profile"] = profile_launch
        agent["_provider_profile"] = profile_launch
    if (
        agent.get("provider") in {"claude", "claude_code"}
        and profile_launch
        and profile_launch.get("auth_mode") == "compatible_api"
        and profile_launch.get("claude_projects_root")
    ):
        ensure_compatible_claude_mcp_config(workspace, agent["id"], mcp_config)
    resume_state = dict(agent_state)
    resume_state["_agent_spec"] = command_agent
    cmd = adapter.build_resume_command(resume_state, workspace, mcp_config)
    return shell_command(cmd, agent["id"], workspace, profile_launch)


def shell_fork_command_for_agent(
    agent: dict[str, Any],
    source_session_id: str,
    workspace: Path,
    mcp_config: dict[str, Any],
) -> str:
    adapter = get_adapter(agent["provider"])
    command_agent = dict(agent)
    profile_launch = command_agent.get("_provider_profile") or prepare_agent_profile_launch(workspace, command_agent)
    if profile_launch:
        command_agent["_provider_profile"] = profile_launch
        agent["_provider_profile"] = profile_launch
    if (
        agent.get("provider") in {"claude", "claude_code"}
        and profile_launch
        and profile_launch.get("auth_mode") == "compatible_api"
        and profile_launch.get("claude_projects_root")
    ):
        ensure_compatible_claude_mcp_config(workspace, agent["id"], mcp_config)
    cmd = adapter.build_fork_command(command_agent, source_session_id, workspace, mcp_config)
    if command_agent.get("_session_id"):
        agent["_session_id"] = command_agent["_session_id"]
    return shell_command(cmd, agent["id"], workspace, profile_launch)


def shell_command(
    cmd: list[str],
    agent_id: str,
    workspace: Path,
    profile_launch: dict[str, Any] | None = None,
) -> str:
    env = {
        "TEAM_AGENT_ID": agent_id,
        "TEAM_AGENT_WORKSPACE": str(workspace),
        "PYTHONPATH": str(repo_root() / "src"),
    }
    if os.environ.get("PATH"):
        # tmux commands inherit the tmux server's old environment, not the
        # current Codex shell. Preserve PATH so local wrappers such as
        # ~/.local/bin/codex remain effective without logging proxy secrets.
        env["PATH"] = os.environ["PATH"]
    exports = " ".join(f"{key}={shlex.quote(value)}" for key, value in env.items())
    source_profile = ""
    env_file = profile_launch.get("env_file") if profile_launch else None
    if env_file:
        source_profile = f". {shlex.quote(str(env_file))} && "
    return f"cd {shlex.quote(str(workspace))} && export {exports} && {source_profile}exec {shlex.join(cmd)}"


__all__ = [
    "ADAPTERS",
    "ClaudeCodeAdapter",
    "CodexAdapter",
    "FakeAdapter",
    "GeminiCliAdapter",
    "ProviderAdapter",
    "ResumeUnavailable",
    "TEAMMATE_SYSTEM_PROMPT",
    "compile_system_prompt",
    "get_adapter",
    "shell_command",
    "shell_command_for_agent",
    "shell_fork_command_for_agent",
    "shell_resume_command_for_agent",
]
