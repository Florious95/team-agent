from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.profiles.constants import AUTH_MODES, PROFILE_KEY_RE, PROFILE_SECRET_BOUNDARY_TEXT
from team_agent.profiles.helpers import (
    _alternate_value,
    _common_missing_values,
    _format_profile_check_failure,
    _is_secret_key,
    _safe_profile_value,
    _strip_env_value,
)
from team_agent.rust_core import redact_text


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
    from team_agent.profiles.smoke import _anthropic_compatible_smoke, _openai_compatible_smoke

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
    from team_agent.profiles.provider_env import (
        _compatible_api_network_exports,
        _compatible_claude_config_dir,
        _profile_proxy_mode,
        _provider_command_overrides,
        _provider_env_exports,
        _provider_env_unsets,
        _write_runtime_env_file,
    )
    from team_agent.profiles.constants import COMPATIBLE_API_NETWORK_ENV_KEYS

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
