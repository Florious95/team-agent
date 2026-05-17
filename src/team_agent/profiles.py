from __future__ import annotations

from contextlib import contextmanager
import json
import os
import re
import shlex
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

from team_agent.rust_core import redact_text

AUTH_MODES = {"subscription", "official_api", "compatible_api"}
PROFILE_KEY_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
SECRET_KEYS = {"API_KEY", "AUTH_TOKEN", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "OPENAI_API_KEY", "GEMINI_API_KEY"}
PROXY_ENV_KEYS = ("HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY", "https_proxy", "http_proxy", "all_proxy", "NO_PROXY", "no_proxy")
CA_ENV_KEYS = ("NODE_EXTRA_CA_CERTS", "SSL_CERT_FILE", "REQUESTS_CA_BUNDLE")
COMPATIBLE_API_NETWORK_ENV_KEYS = PROXY_ENV_KEYS + CA_ENV_KEYS
PROFILE_SECRET_BOUNDARY_TEXT = """# Team Agent Profile Secret Boundary

Do not read, print, grep, cat, sed, copy, summarize, or open raw `*.env` files in this directory.
These files may contain API keys or auth tokens and must stay out of agent context.

Use `team-agent profile show <name> --workspace . --json` or `team-agent profile doctor <name> --workspace . --json`
for redacted status. If a required value is missing, ask the human user to edit the local profile file.
"""


def profile_dir(workspace: Path) -> Path:
    return workspace / ".team" / "current" / "profiles"


def _profile_lookup_dir(workspace: Path, profiles_dir: Path | str | None = None) -> Path:
    return Path(profiles_dir).resolve() if profiles_dir else profile_dir(workspace)


def _agent_profile_dir(agent: dict[str, Any]) -> Path | None:
    value = agent.get("_profile_dir")
    return Path(str(value)).resolve() if value else None


def _safe_inspection_command(name: str, profiles_dir: Path | str | None = None) -> str:
    if profiles_dir:
        return f"team-agent profile show {name} --team {Path(profiles_dir).resolve().parent} --json"
    return f"team-agent profile show {name} --workspace . --json"


def _safe_init_command(name: str, auth_mode: str, profiles_dir: Path | str | None = None) -> str:
    if profiles_dir:
        return f"team-agent profile init {name} --auth-mode {auth_mode} --team {Path(profiles_dir).resolve().parent}"
    return f"team-agent profile init {name} --auth-mode {auth_mode}"


def ensure_profile_secret_boundary_dir(directory: Path) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    first_path = directory / "AGENTS.md"
    for filename in ("AGENTS.md", "CLAUDE.md"):
        path = directory / filename
        if not path.exists():
            path.write_text(PROFILE_SECRET_BOUNDARY_TEXT, encoding="utf-8")
    return first_path


def ensure_profile_secret_boundary(workspace: Path) -> Path:
    directory = profile_dir(workspace)
    return ensure_profile_secret_boundary_dir(directory)


def init_profile(workspace: Path, name: str, auth_mode: str, profiles_dir: Path | str | None = None) -> dict[str, Any]:
    if auth_mode not in AUTH_MODES:
        raise ValueError(f"unknown auth_mode: {auth_mode}")
    directory = _profile_lookup_dir(workspace, profiles_dir)
    directory.mkdir(parents=True, exist_ok=True)
    ensure_profile_secret_boundary_dir(directory)
    path = directory / f"{name}.env"
    template_path = directory / f"{name}.example.env"
    if auth_mode == "subscription":
        body = f"AUTH_MODE=subscription\nPROFILE_NAME={name}\n"
    elif auth_mode == "official_api":
        body = f"AUTH_MODE=official_api\nPROFILE_NAME={name}\nAPI_KEY=\nMODEL=\n"
    else:
        body = f"AUTH_MODE=compatible_api\nPROFILE_NAME={name}\nBASE_URL=\nAPI_KEY=\nMODEL=\n"
    created_profile = False
    created_template = False
    if not path.exists():
        path.write_text(body, encoding="utf-8")
        try:
            path.chmod(0o600)
        except OSError:
            pass
        created_profile = True
    if not template_path.exists():
        template_path.write_text(body, encoding="utf-8")
        created_template = True
    safe_inspection = _safe_inspection_command(name, directory if profiles_dir else None)
    return {
        "ok": True,
        "profile": name,
        "auth_mode": auth_mode,
        "path": str(path),
        "template_path": str(template_path),
        "created_profile": created_profile,
        "created_template": created_template,
        "secret_written": False,
        "safe_inspection_command": safe_inspection,
        "raw_file_read_allowed_for_agents": False,
        "instruction": (
            f"Fill {path} locally; do not paste API keys into chat or role docs. "
            f"Agents must inspect this profile only with `{safe_inspection}`."
        ),
    }


def doctor_profile(workspace: Path, name: str, profiles_dir: Path | str | None = None) -> dict[str, Any]:
    directory = _profile_lookup_dir(workspace, profiles_dir)
    loaded = load_profile(workspace, name, directory)
    real_path = directory / f"{name}.env"
    example_path = directory / f"{name}.example.env"
    exists = bool(loaded.get("exists"))
    values = loaded.get("values", {})
    keys_present = [key for key, value in values.items() if value]
    auth_mode = values.get("AUTH_MODE")
    return {
        "ok": exists,
        "profile": name,
        "path": str(loaded.get("path") or real_path),
        "template_path": str(example_path),
        "credential_present": real_path.exists(),
        "auth_mode": auth_mode,
        "keys_present": sorted(keys_present),
        "secret_keys_present": sorted(key for key in keys_present if _is_secret_key(key)),
        "redaction_engine": redact_text(" ".join(f"{key}=present" for key in keys_present)).get("engine"),
        "secret_values_printed": False,
        "safe_for_agent_context": True,
        "raw_file_read_allowed_for_agents": False,
        "safe_inspection_command": _safe_inspection_command(name, directory if profiles_dir else None),
        "suggestion": None if exists else f"Run {_safe_init_command(name, 'subscription', directory if profiles_dir else None)}.",
    }


def show_profile(workspace: Path, name: str, profiles_dir: Path | str | None = None) -> dict[str, Any]:
    loaded = load_profile(workspace, name, profiles_dir)
    values: dict[str, str] = loaded.get("values", {})
    auth_mode = values.get("AUTH_MODE")
    safe_values = {
        key: _safe_profile_value(key, value)
        for key, value in sorted(values.items())
    }
    missing = _common_missing_values(auth_mode, values)
    return {
        "ok": bool(loaded.get("exists")),
        "profile": name,
        "credential_present": bool(loaded.get("credential_present")),
        "auth_mode": auth_mode,
        "values": safe_values,
        "keys_present": sorted(key for key, value in values.items() if value),
        "secret_keys_present": sorted(key for key, value in values.items() if value and _is_secret_key(key)),
        "missing_common": missing,
        "safe_for_agent_context": True,
        "secret_values_printed": False,
        "raw_file_read_allowed_for_agents": False,
        "instruction": (
            "Use this redacted output for diagnostics. Do not read .team/*/profiles/*.env "
            "or .team/runtime/provider-env/*.env into agent context."
        ),
    }


def known_profiles(team_dir: Path) -> set[str]:
    directory = team_dir / "profiles"
    if not directory.exists():
        return set()
    names = set()
    for path in directory.glob("*.env"):
        names.add(path.name.removesuffix(".env").removesuffix(".example"))
    for path in directory.glob("*.example.env"):
        names.add(path.name.removesuffix(".example.env"))
    return names


def gitignore_patterns() -> list[str]:
    return [".team/*/profiles/*.env", "!.team/*/profiles/*.example.env", ".team/runtime/provider-env/*.env"]


def load_profile(workspace: Path, name: str, profiles_dir: Path | str | None = None) -> dict[str, Any]:
    directory = _profile_lookup_dir(workspace, profiles_dir)
    real_path = directory / f"{name}.env"
    example_path = directory / f"{name}.example.env"
    path = real_path if real_path.exists() else example_path
    if not path.exists():
        return {
            "exists": False,
            "profile": name,
            "path": str(real_path),
            "template_path": str(example_path),
            "credential_present": False,
            "values": {},
        }
    values = parse_env_file(path)
    return {
        "exists": True,
        "profile": name,
        "path": str(path),
        "template_path": str(example_path),
        "credential_present": real_path.exists(),
        "values": values,
    }


def parse_env_file(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export ") :].strip()
        if "=" not in line:
            continue
        key, value = line.split("=", 1)
        key = key.strip()
        if not PROFILE_KEY_RE.fullmatch(key):
            continue
        values[key] = _strip_env_value(value.strip())
    return values


def validate_agent_profile(workspace: Path, agent: dict[str, Any]) -> dict[str, Any]:
    profile = agent.get("profile")
    auth_mode = str(agent.get("auth_mode") or "subscription")
    profiles_dir = _agent_profile_dir(agent)
    result = {
        "ok": True,
        "agent_id": agent.get("id"),
        "provider": agent.get("provider"),
        "profile": profile,
        "auth_mode": auth_mode,
        "path": None,
        "credential_present": False,
        "keys_present": [],
        "missing_required": [],
        "secret_values_printed": False,
    }
    if not profile:
        return result
    loaded = load_profile(workspace, str(profile), profiles_dir)
    result["path"] = loaded.get("path")
    result["credential_present"] = bool(loaded.get("credential_present"))
    if not loaded.get("exists"):
        result["ok"] = False
        result["reason"] = "profile_missing"
        result["suggestion"] = (
            f"Run {_safe_init_command(str(profile), auth_mode, profiles_dir)}, then ask the human user "
            "to fill the generated local profile file."
        )
        return result
    values: dict[str, str] = loaded.get("values", {})
    file_auth_mode = values.get("AUTH_MODE")
    if file_auth_mode and file_auth_mode != auth_mode:
        result["ok"] = False
        result["reason"] = "auth_mode_mismatch"
        result["file_auth_mode"] = file_auth_mode
        result["suggestion"] = (
            f"Set AUTH_MODE={auth_mode} in the local profile or update the role doc. "
            f"Inspect safely with `{_safe_inspection_command(str(profile), profiles_dir)}`."
        )
        return result
    keys_present = sorted(key for key, value in values.items() if value)
    result["keys_present"] = keys_present
    role_model = str(agent.get("model") or "") or None
    profile_model = values.get("MODEL") or values.get("ANTHROPIC_MODEL")
    effective = role_model or profile_model
    result["effective_model"] = effective
    result["model_source"] = "role" if role_model else ("profile" if profile_model else None)
    if auth_mode == "compatible_api" and role_model and profile_model and role_model != profile_model:
        result["ok"] = False
        result["reason"] = "model_mismatch"
        result["suggestion"] = (
            f"Role model {role_model!r} does not match the profile MODEL value; "
            f"inspect safely with `{_safe_inspection_command(str(profile), profiles_dir)}`, "
            "then remove model from the role doc or make both values identical."
        )
        return result
    required = required_profile_keys(str(agent.get("provider") or ""), auth_mode)
    missing = [key for key in required if not values.get(key) and not _alternate_value(values, key)]
    if auth_mode == "compatible_api" and not effective:
        missing.append("MODEL")
    result["missing_required"] = missing
    if missing:
        result["ok"] = False
        result["reason"] = "profile_required_values_missing"
        result["suggestion"] = (
            f"Ask the human user to fill {', '.join(missing)} in local profile {profile}; "
            f"inspect safely with `{_safe_inspection_command(str(profile), profiles_dir)}`. "
            "Do not paste API keys into chat."
        )
    return result


def smoke_check_agent_profile(workspace: Path, agent: dict[str, Any], timeout: float = 8.0) -> dict[str, Any]:
    auth_mode = str(agent.get("auth_mode") or "subscription")
    profile = agent.get("profile")
    result = {
        "ok": True,
        "agent_id": agent.get("id"),
        "provider": agent.get("provider"),
        "profile": profile,
        "auth_mode": auth_mode,
        "status": "not_required",
        "secret_values_printed": False,
    }
    if auth_mode != "compatible_api" or not profile:
        return result
    loaded = load_profile(workspace, str(profile), _agent_profile_dir(agent))
    values: dict[str, str] = loaded.get("values", {})
    if str(values.get("PROFILE_SMOKE", "true")).lower() in {"0", "false", "no", "off"}:
        result["status"] = "skipped_by_profile"
        return result
    validation = validate_agent_profile(workspace, agent)
    if not validation.get("ok"):
        return {**result, **validation, "ok": False, "status": "profile_invalid"}
    provider = str(agent.get("provider") or "")
    model = effective_model(agent, workspace)
    if provider in {"claude", "claude_code"}:
        return _anthropic_compatible_smoke(values, model, result, timeout)
    if provider == "codex":
        return _openai_compatible_smoke(values, model, result, timeout)
    result["status"] = "unsupported_provider_smoke_skipped"
    return result


def required_profile_keys(provider: str, auth_mode: str) -> list[str]:
    if auth_mode == "subscription":
        return []
    if provider in {"claude", "claude_code"}:
        if auth_mode == "official_api":
            return ["API_KEY"]
        if auth_mode == "compatible_api":
            return ["BASE_URL", "API_KEY"]
    if provider == "codex":
        if auth_mode == "official_api":
            return ["API_KEY"]
        if auth_mode == "compatible_api":
            return ["BASE_URL", "API_KEY"]
    if provider == "gemini_cli" and auth_mode in {"official_api", "compatible_api"}:
        return ["API_KEY"]
    return []


def compact_profile_check(check: dict[str, Any]) -> dict[str, Any]:
    keys = [
        "agent_id",
        "provider",
        "profile",
        "auth_mode",
        "ok",
        "status",
        "reason",
        "credential_present",
        "keys_present",
        "missing_required",
        "effective_model",
        "model_source",
        "suggestion",
        "http_status",
        "endpoint",
        "error",
        "proxy_configured",
        "proxy_scheme",
        "proxy_url",
        "proxy_source",
        "proxy_mode",
    ]
    return {key: check.get(key) for key in keys if key in check}


def prepare_agent_profile_launch(workspace: Path, agent: dict[str, Any]) -> dict[str, Any] | None:
    profile = agent.get("profile")
    if not profile:
        return None
    check = validate_agent_profile(workspace, agent)
    if not check.get("ok"):
        raise ValueError(_format_profile_check_failure(check))
    loaded = load_profile(workspace, str(profile), _agent_profile_dir(agent))
    values: dict[str, str] = loaded.get("values", {})
    auth_mode = str(agent.get("auth_mode") or values.get("AUTH_MODE") or "subscription")
    provider = str(agent.get("provider") or "")
    exports = _provider_env_exports(provider, auth_mode, values)
    claude_projects_root = None
    if provider in {"claude", "claude_code"} and auth_mode == "compatible_api":
        claude_config_dir = _compatible_claude_config_dir(workspace, str(agent["id"]))
        exports["CLAUDE_CONFIG_DIR"] = str(claude_config_dir)
        claude_projects_root = str(claude_config_dir / "projects")
    exports.update(_compatible_api_network_exports(auth_mode, values))
    unsets = _provider_env_unsets(provider, auth_mode)
    if auth_mode == "compatible_api" and _profile_proxy_mode(values) == "direct":
        unsets.extend(COMPATIBLE_API_NETWORK_ENV_KEYS)
        exports = {key: value for key, value in exports.items() if key not in COMPATIBLE_API_NETWORK_ENV_KEYS}
    overrides = _provider_command_overrides(str(agent.get("provider") or ""), auth_mode, values, agent)
    env_file = None
    if exports or unsets:
        env_file = _write_runtime_env_file(workspace, str(agent["id"]), exports, unsets)
    return {
        "profile": str(profile),
        "auth_mode": auth_mode,
        "path": loaded.get("path"),
        "credential_present": loaded.get("credential_present"),
        "env_file": str(env_file) if env_file else None,
        "env_keys": sorted(exports),
        "env_unset": sorted(unsets),
        "claude_projects_root": claude_projects_root,
        "command_overrides": overrides,
        "secret_values_printed": False,
    }


def effective_model(agent: dict[str, Any], workspace: Path | None = None) -> str | None:
    if agent.get("model"):
        return str(agent["model"])
    if workspace is None or not agent.get("profile"):
        return None
    loaded = load_profile(workspace, str(agent["profile"]), _agent_profile_dir(agent))
    values: dict[str, str] = loaded.get("values", {})
    return values.get("MODEL") or values.get("ANTHROPIC_MODEL")


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


def _anthropic_compatible_smoke(
    values: dict[str, str],
    model: str | None,
    base_result: dict[str, Any],
    timeout: float,
) -> dict[str, Any]:
    base_url = values.get("ANTHROPIC_BASE_URL") or values.get("BASE_URL")
    api_key = values.get("ANTHROPIC_API_KEY") or values.get("API_KEY")
    auth_token = values.get("ANTHROPIC_AUTH_TOKEN") or values.get("AUTH_TOKEN")
    if not base_url or not (api_key or auth_token) or not model:
        return {**base_result, "ok": False, "status": "smoke_failed", "reason": "missing_base_url_api_key_or_model"}
    endpoint = _anthropic_messages_url(base_url)
    payload = {
        "model": model,
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "ping"}],
    }
    headers = {
        "content-type": "application/json",
        "anthropic-version": values.get("ANTHROPIC_VERSION") or "2023-06-01",
    }
    headers["authorization"] = f"Bearer {auth_token or api_key}"
    return _http_json_smoke(endpoint, payload, headers, values, base_result, timeout)


def _openai_compatible_smoke(
    values: dict[str, str],
    model: str | None,
    base_result: dict[str, Any],
    timeout: float,
) -> dict[str, Any]:
    base_url = values.get("OPENAI_BASE_URL") or values.get("BASE_URL")
    api_key = values.get("OPENAI_API_KEY") or values.get("API_KEY")
    if not base_url or not api_key or not model:
        return {**base_result, "ok": False, "status": "smoke_failed", "reason": "missing_base_url_api_key_or_model"}
    endpoint = _openai_chat_url(base_url)
    payload = {
        "model": model,
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "ping"}],
    }
    headers = {"content-type": "application/json", "authorization": f"Bearer {api_key}"}
    return _http_json_smoke(endpoint, payload, headers, values, base_result, timeout)


def _http_json_smoke(
    endpoint: str,
    payload: dict[str, Any],
    headers: dict[str, str],
    values: dict[str, str],
    base_result: dict[str, Any],
    timeout: float,
) -> dict[str, Any]:
    proxy_info = _proxy_info_for_endpoint(endpoint, values)
    request = urllib.request.Request(
        endpoint,
        data=json.dumps(payload).encode("utf-8"),
        headers=headers,
        method="POST",
    )
    try:
        with _temporary_profile_network_env(values):
            response_ctx = urllib.request.urlopen(request, timeout=timeout)
        with response_ctx as response:
            status = int(getattr(response, "status", 200))
            body = response.read(1024).decode("utf-8", errors="replace")
    except urllib.error.HTTPError as exc:
        body = exc.read(4096).decode("utf-8", errors="replace")
        return {
            **base_result,
            **proxy_info,
            "ok": False,
            "status": "smoke_failed",
            "reason": "http_error",
            "http_status": exc.code,
            "endpoint": _redacted_endpoint(endpoint),
            "error": redact_text(body or str(exc)).get("text"),
        }
    except Exception as exc:
        reason = "proxy_connectivity_failed" if proxy_info.get("proxy_configured") else "request_failed"
        return {
            **base_result,
            **proxy_info,
            "ok": False,
            "status": "smoke_failed",
            "reason": reason,
            "endpoint": _redacted_endpoint(endpoint),
            "error": redact_text(str(exc)).get("text"),
            "suggestion": (
                "Proxy is configured for this request; allow the profile BASE_URL through the proxy or disable the proxy for Team Agent startup."
                if proxy_info.get("proxy_configured")
                else "Check BASE_URL network connectivity from this machine."
            ),
        }
    if 200 <= status < 300:
        return {
            **base_result,
            **proxy_info,
            "ok": True,
            "status": "smoke_passed",
            "http_status": status,
            "endpoint": _redacted_endpoint(endpoint),
        }
    return {
        **base_result,
        **proxy_info,
        "ok": False,
        "status": "smoke_failed",
        "reason": "unexpected_status",
        "http_status": status,
        "endpoint": _redacted_endpoint(endpoint),
        "error": redact_text(body).get("text"),
    }


def _anthropic_messages_url(base_url: str) -> str:
    base = base_url.rstrip("/")
    if base.endswith("/messages"):
        return base
    if base.endswith("/v1"):
        return f"{base}/messages"
    return f"{base}/v1/messages"


def _openai_chat_url(base_url: str) -> str:
    base = base_url.rstrip("/")
    if base.endswith("/chat/completions"):
        return base
    if base.endswith("/v1"):
        return f"{base}/chat/completions"
    return f"{base}/v1/chat/completions"


def _redacted_endpoint(endpoint: str) -> str:
    return endpoint.split("?", 1)[0]


def _proxy_info_for_endpoint(endpoint: str, values: dict[str, str]) -> dict[str, Any]:
    parsed = urllib.parse.urlparse(endpoint)
    if _profile_proxy_mode(values) == "direct":
        return {"proxy_configured": False, "proxy_mode": "direct"}
    profile_env = _compatible_api_network_exports("compatible_api", values)
    proxy_url = _proxy_url_from_env(parsed.scheme, profile_env)
    if proxy_url:
        return {
            "proxy_configured": True,
            "proxy_scheme": parsed.scheme,
            "proxy_url": _redact_proxy_url(proxy_url),
            "proxy_source": "profile",
        }
    ambient_proxy_url = _proxy_url_from_env(parsed.scheme, os.environ)
    if ambient_proxy_url:
        return {
            "proxy_configured": True,
            "proxy_scheme": parsed.scheme,
            "proxy_url": _redact_proxy_url(ambient_proxy_url),
            "proxy_source": "ambient",
        }
    return {"proxy_configured": False}


def _proxy_url_from_env(scheme: str, env: Any) -> str | None:
    upper = f"{scheme.upper()}_PROXY"
    lower = f"{scheme.lower()}_proxy"
    return env.get(upper) or env.get(lower) or env.get("ALL_PROXY") or env.get("all_proxy")


def _compatible_api_network_exports(auth_mode: str, values: dict[str, str]) -> dict[str, str]:
    if auth_mode != "compatible_api" or _profile_proxy_mode(values) == "direct":
        return {}
    return {key: values[key] for key in COMPATIBLE_API_NETWORK_ENV_KEYS if values.get(key)}


@contextmanager
def _temporary_profile_network_env(values: dict[str, str]) -> Any:
    profile_env = _compatible_api_network_exports("compatible_api", values)
    direct = _profile_proxy_mode(values) == "direct"
    touched_keys = COMPATIBLE_API_NETWORK_ENV_KEYS if direct else tuple(profile_env)
    saved = {key: os.environ.get(key) for key in touched_keys}
    try:
        if direct:
            for key in COMPATIBLE_API_NETWORK_ENV_KEYS:
                os.environ.pop(key, None)
        os.environ.update(profile_env)
        yield
    finally:
        for key, value in saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


def _profile_proxy_mode(values: dict[str, str]) -> str:
    return str(values.get("PROXY_MODE") or values.get("NETWORK_MODE") or "inherit").strip().lower()


def _redact_proxy_url(proxy_url: str) -> str:
    parsed = urllib.parse.urlparse(proxy_url)
    if not parsed.netloc:
        return proxy_url
    host = parsed.hostname or ""
    port = f":{parsed.port}" if parsed.port else ""
    auth = "[redacted]@" if parsed.username or parsed.password else ""
    return urllib.parse.urlunparse((parsed.scheme, f"{auth}{host}{port}", parsed.path, "", "", ""))


def _format_profile_check_failure(check: dict[str, Any]) -> str:
    agent_id = check.get("agent_id") or "unknown"
    profile = check.get("profile") or "-"
    reason = check.get("reason") or "profile_invalid"
    suggestion = check.get("suggestion") or f"Inspect safely with `team-agent profile show {profile} --workspace . --json`."
    return f"profile validation failed for {agent_id} profile {profile}: {reason}. {suggestion}"


def _alternate_value(values: dict[str, str], key: str) -> str | None:
    alternates = {
        "BASE_URL": ["ANTHROPIC_BASE_URL", "OPENAI_BASE_URL"],
        "API_KEY": ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "AUTH_TOKEN", "OPENAI_API_KEY", "GEMINI_API_KEY"],
    }
    for candidate in alternates.get(key, []):
        if values.get(candidate):
            return values[candidate]
    return None


def _strip_env_value(value: str) -> str:
    if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
        return value[1:-1]
    return value


def _safe_codex_provider_id(value: str) -> bool:
    return re.fullmatch(r"[A-Za-z0-9_-]+", value) is not None


def _is_secret_key(key: str) -> bool:
    upper = key.upper()
    return upper in SECRET_KEYS or "KEY" in upper or "TOKEN" in upper or "SECRET" in upper


def _safe_profile_value(key: str, value: str) -> dict[str, Any]:
    if _is_secret_key(key):
        return {"present": bool(value), "redacted": True}
    return {"present": bool(value), "redacted": False, "value": _safe_plain_profile_value(value)}


def _common_missing_values(auth_mode: str | None, values: dict[str, str]) -> list[str]:
    if auth_mode == "compatible_api":
        required = ["BASE_URL", "API_KEY", "MODEL"]
    elif auth_mode == "official_api":
        required = ["API_KEY"]
    else:
        required = []
    missing = []
    for key in required:
        if key == "MODEL":
            if not (values.get("MODEL") or values.get("ANTHROPIC_MODEL")):
                missing.append(key)
            continue
        if not values.get(key) and not _alternate_value(values, key):
            missing.append(key)
    return missing


def _safe_plain_profile_value(value: str) -> str:
    parsed = urllib.parse.urlparse(value)
    if parsed.scheme and parsed.netloc:
        host = parsed.hostname or ""
        port = f":{parsed.port}" if parsed.port else ""
        auth = "[redacted]@" if parsed.username or parsed.password else ""
        value = urllib.parse.urlunparse((parsed.scheme, f"{auth}{host}{port}", parsed.path, "", "", ""))
    return str(redact_text(value).get("text") or "")
