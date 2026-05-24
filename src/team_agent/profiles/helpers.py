from __future__ import annotations

import re
import urllib.parse
from typing import Any

from team_agent.rust_core import redact_text
from team_agent.profiles.constants import SECRET_KEYS


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
