from __future__ import annotations

import urllib

from team_agent.profiles.constants import *
from team_agent.profiles.core import (
    compact_profile_check,
    doctor_profile,
    effective_model,
    ensure_profile_secret_boundary,
    ensure_profile_secret_boundary_dir,
    gitignore_patterns,
    init_profile,
    known_profiles,
    load_profile,
    parse_env_file,
    prepare_agent_profile_launch,
    profile_dir,
    required_profile_keys,
    show_profile,
    smoke_check_agent_profile,
    validate_agent_profile,
)
from team_agent.profiles.helpers import (
    _alternate_value,
    _common_missing_values,
    _format_profile_check_failure,
    _is_secret_key,
    _safe_codex_provider_id,
    _safe_plain_profile_value,
    _safe_profile_value,
    _strip_env_value,
)
from team_agent.profiles.provider_env import (
    _compatible_api_network_exports,
    _compatible_claude_config_dir,
    _ensure_compatible_claude_config,
    _profile_proxy_mode,
    _provider_command_overrides,
    _provider_env_exports,
    _provider_env_unsets,
    _read_json_object,
    _write_json,
    _write_runtime_env_file,
    ensure_compatible_claude_mcp_config,
)
from team_agent.profiles.smoke import (
    _anthropic_compatible_smoke,
    _anthropic_messages_url,
    _http_json_smoke,
    _openai_chat_url,
    _openai_compatible_smoke,
    _proxy_info_for_endpoint,
    _proxy_url_from_env,
    _redact_proxy_url,
    _redacted_endpoint,
    _temporary_profile_network_env,
)

_REQUIRED_EXPORTS = (
    "profile_dir",
    "init_profile",
    "doctor_profile",
    "show_profile",
    "load_profile",
    "parse_env_file",
    "validate_agent_profile",
    "smoke_check_agent_profile",
    "prepare_agent_profile_launch",
    "effective_model",
    "ensure_compatible_claude_mcp_config",
)
for _name in _REQUIRED_EXPORTS:
    if _name not in globals():
        raise ImportError(f"team_agent.profiles missing export: {_name}")

__all__ = [name for name in globals() if not name.startswith("__")]
