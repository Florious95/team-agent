from __future__ import annotations

import json
import shlex
from pathlib import Path
from typing import Any

from team_agent.profiles.constants import COMPATIBLE_API_NETWORK_ENV_KEYS
from team_agent.profiles.helpers import _safe_codex_provider_id


def _provider_env_exports(provider: str, auth_mode: str, values: dict[str, str]) -> dict[str, str]:
    if auth_mode == "subscription":
        return {}
    if provider in {"claude", "claude_code"}:
        exports: dict[str, str] = {}
        base_url = values.get("ANTHROPIC_BASE_URL") or values.get("BASE_URL")
        api_key = values.get("ANTHROPIC_API_KEY") or values.get("API_KEY")
        auth_token = values.get("ANTHROPIC_AUTH_TOKEN") or values.get("AUTH_TOKEN")
        model = values.get("ANTHROPIC_MODEL") or values.get("MODEL")
        if base_url:
            exports["ANTHROPIC_BASE_URL"] = base_url
        if auth_mode == "official_api" and api_key:
            exports["ANTHROPIC_API_KEY"] = api_key
        if auth_token or (auth_mode == "compatible_api" and api_key):
            exports["ANTHROPIC_AUTH_TOKEN"] = auth_token or api_key
        if model:
            exports["ANTHROPIC_MODEL"] = model
        return exports
    if provider == "codex":
        exports = {}
        api_key = values.get("OPENAI_API_KEY") or values.get("API_KEY")
        if api_key:
            exports["TEAM_AGENT_PROVIDER_API_KEY"] = api_key
            exports.setdefault("OPENAI_API_KEY", api_key)
        if values.get("BASE_URL"):
            exports["OPENAI_BASE_URL"] = values["BASE_URL"]
        return exports
    if provider == "gemini_cli":
        api_key = values.get("GEMINI_API_KEY") or values.get("API_KEY")
        return {"GEMINI_API_KEY": api_key} if api_key else {}
    return {}

def _provider_env_unsets(provider: str, auth_mode: str) -> list[str]:
    unsets: list[str] = []
    if provider in {"claude", "claude_code"}:
        if auth_mode == "compatible_api":
            unsets.append("ANTHROPIC_API_KEY")
        if auth_mode == "official_api":
            unsets.append("ANTHROPIC_AUTH_TOKEN")
    if provider == "codex" and auth_mode == "compatible_api":
        unsets.extend(["OPENAI_API_KEY", "OPENAI_BASE_URL"])
    if provider == "gemini_cli" and auth_mode == "compatible_api":
        unsets.append("GEMINI_API_KEY")
    return sorted(set(unsets))

def _provider_command_overrides(provider: str, auth_mode: str, values: dict[str, str], agent: dict[str, Any]) -> dict[str, Any]:
    overrides: dict[str, Any] = {}
    model = agent.get("model") or values.get("MODEL") or values.get("ANTHROPIC_MODEL")
    if model:
        overrides["model"] = str(model)
    if provider == "codex":
        codex_profile = values.get("CODEX_PROFILE") or values.get("NATIVE_PROFILE")
        if codex_profile:
            overrides["codex_profile"] = codex_profile
        configs: list[str] = []
        model_provider = values.get("MODEL_PROVIDER")
        base_url = values.get("BASE_URL")
        if auth_mode == "compatible_api" and model_provider and base_url and _safe_codex_provider_id(model_provider):
            configs.append(f'model_provider="{model_provider}"')
            prefix = f"model_providers.{model_provider}"
            configs.append(f'{prefix}.base_url="{base_url}"')
            configs.append(f'{prefix}.env_key="TEAM_AGENT_PROVIDER_API_KEY"')
            if values.get("WIRE_API"):
                configs.append(f'{prefix}.wire_api="{values["WIRE_API"]}"')
            if values.get("PROVIDER_NAME"):
                configs.append(f'{prefix}.name="{values["PROVIDER_NAME"]}"')
        if configs:
            overrides["codex_config"] = configs
    return overrides

def _write_runtime_env_file(workspace: Path, agent_id: str, exports: dict[str, str], unsets: list[str]) -> Path:
    directory = workspace / ".team" / "runtime" / "provider-env"
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / f"{agent_id}.env"
    lines = [f"unset {key}" for key in sorted(unsets)]
    lines.extend(f"export {key}={shlex.quote(value)}" for key, value in sorted(exports.items()))
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    try:
        path.chmod(0o600)
    except OSError:
        pass
    return path

def _compatible_claude_config_dir(workspace: Path, agent_id: str) -> Path:
    directory = workspace / ".team" / "runtime" / "provider-config" / agent_id / "claude"
    directory.mkdir(parents=True, exist_ok=True)
    _ensure_compatible_claude_config(directory, workspace)
    return directory

def ensure_compatible_claude_mcp_config(workspace: Path, agent_id: str, mcp_config: dict[str, Any]) -> None:
    if not mcp_config:
        return
    directory = _compatible_claude_config_dir(workspace, agent_id)
    state_path = directory / ".claude.json"
    state = _read_json_object(state_path)
    projects = state.setdefault("projects", {})
    if not isinstance(projects, dict):
        projects = {}
        state["projects"] = projects
    for project_key in _claude_project_keys(workspace):
        project = projects.setdefault(project_key, {})
        if not isinstance(project, dict):
            project = {}
            projects[project_key] = project
        project["hasTrustDialogAccepted"] = True
        project.setdefault("projectOnboardingSeenCount", 1)
        project.setdefault("allowedTools", [])
        project.setdefault("mcpContextUris", [])
        project.setdefault("enabledMcpjsonServers", [])
        project.setdefault("disabledMcpjsonServers", [])
        project.setdefault("hasClaudeMdExternalIncludesApproved", False)
        project.setdefault("hasClaudeMdExternalIncludesWarningShown", False)
        servers = project.setdefault("mcpServers", {})
        if not isinstance(servers, dict):
            servers = {}
            project["mcpServers"] = servers
        servers.update(mcp_config)
    _write_json(state_path, state)

def _ensure_compatible_claude_config(directory: Path, workspace: Path) -> None:
    settings_path = directory / "settings.json"
    settings = _read_json_object(settings_path)
    settings.setdefault("theme", "auto")
    settings.setdefault("skipDangerousModePermissionPrompt", True)
    _write_json(settings_path, settings)

    state_path = directory / ".claude.json"
    state = _read_json_object(state_path)
    state["hasCompletedOnboarding"] = True
    state.setdefault("lastOnboardingVersion", "2.1.0")
    state.setdefault("firstStartTime", "1970-01-01T00:00:00.000Z")
    state.setdefault("numStartups", 0)
    projects = state.get("projects")
    if not isinstance(projects, dict):
        projects = {}
        state["projects"] = projects
    for project_key in _claude_project_keys(workspace):
        project = projects.get(project_key)
        if not isinstance(project, dict):
            project = {}
            projects[project_key] = project
        project["hasTrustDialogAccepted"] = True
        project.setdefault("projectOnboardingSeenCount", 1)
    _write_json(state_path, state)

def _claude_project_keys(workspace: Path) -> list[str]:
    keys = [str(workspace)]
    try:
        resolved = str(workspace.resolve())
    except OSError:
        resolved = None
    if resolved and resolved not in keys:
        keys.append(resolved)
    return keys

def _read_json_object(path: Path) -> dict[str, Any]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}
    return data if isinstance(data, dict) else {}

def _write_json(path: Path, data: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    try:
        path.chmod(0o600)
    except OSError:
        pass

def _compatible_api_network_exports(auth_mode: str, values: dict[str, str]) -> dict[str, str]:
    if auth_mode != "compatible_api" or _profile_proxy_mode(values) == "direct":
        return {}
    return {key: values[key] for key in COMPATIBLE_API_NETWORK_ENV_KEYS if values.get(key)}

def _profile_proxy_mode(values: dict[str, str]) -> str:
    return str(values.get("PROXY_MODE") or values.get("NETWORK_MODE") or "inherit").strip().lower()
