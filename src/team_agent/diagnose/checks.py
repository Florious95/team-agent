from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.profiles import effective_model, smoke_check_agent_profile, validate_agent_profile


def profile_checks_for_agents(workspace: Path, agents: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [validate_agent_profile(workspace, agent) for agent in agents if not agent.get("paused")]


def profile_smoke_checks_for_agents(workspace: Path, agents: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [smoke_check_agent_profile(workspace, agent) for agent in agents if not agent.get("paused")]


def model_checks_for_agents(agents: list[dict[str, Any]], workspace: Path | None = None) -> list[dict[str, Any]]:
    from team_agent.runtime import get_adapter
    checks: list[dict[str, Any]] = []
    for agent in agents:
        if agent.get("paused"):
            continue
        if agent.get("auth_mode") == "compatible_api" and agent.get("provider") == "codex":
            checks.append(
                {
                    "ok": True,
                    "status": "profile_model_deferred_to_smoke",
                    "provider": agent["provider"],
                    "model": effective_model(agent, workspace),
                    "agent_id": agent["id"],
                }
            )
            continue
        adapter = get_adapter(agent["provider"])
        validator = getattr(adapter, "validate_model", None)
        model = effective_model(agent, workspace)
        if not callable(validator):
            result = {"ok": True, "status": "not_checked", "provider": agent["provider"], "model": model}
        else:
            result = validator(model)
            if not isinstance(result, dict):
                result = {"ok": True, "status": "not_checked", "provider": agent["provider"], "model": model}
        result = dict(result)
        result.setdefault("provider", agent["provider"])
        result.setdefault("model", model)
        result["agent_id"] = agent["id"]
        checks.append(result)
    return checks


def compact_model_checks(checks: list[dict[str, Any]]) -> list[dict[str, Any]]:
    compact: list[dict[str, Any]] = []
    for item in checks:
        compact.append(
            {
                key: item.get(key)
                for key in ["agent_id", "provider", "model", "ok", "status", "reason", "suggested_model", "command"]
                if key in item
            }
        )
    return compact


def format_model_check_failures(failures: list[dict[str, Any]]) -> str:
    lines = ["model validation failed before starting worker windows:"]
    for item in failures:
        message = f"{item.get('agent_id')}: provider={item.get('provider')} model={item.get('model')!r}"
        if item.get("suggested_model"):
            message += f" is not an exact model id; use {item['suggested_model']!r}"
        else:
            message += f" is unsupported ({item.get('reason') or item.get('status')})"
        lines.append(message)
    return "\n".join(lines)


def format_profile_check_failures(failures: list[dict[str, Any]]) -> str:
    lines = ["profile validation failed before starting worker windows:"]
    for item in failures:
        message = f"{item.get('agent_id')}: profile={item.get('profile')!r} auth_mode={item.get('auth_mode')}"
        if item.get("missing_required"):
            message += f" missing {', '.join(item['missing_required'])}"
        else:
            message += f" failed ({item.get('reason') or item.get('status')})"
        if item.get("suggestion"):
            message += f"; {item['suggestion']}"
        lines.append(message)
    return "\n".join(lines)


def format_profile_smoke_failures(failures: list[dict[str, Any]]) -> str:
    lines = ["provider profile smoke check failed before starting worker windows:"]
    for item in failures:
        message = f"{item.get('agent_id')}: provider={item.get('provider')} profile={item.get('profile')!r}"
        message += f" status={item.get('status')} reason={item.get('reason') or 'unknown'}"
        if item.get("http_status"):
            message += f" http_status={item['http_status']}"
        if item.get("error"):
            message += f"; {item['error']}"
        message += "; fix the local profile file or model id, then start again"
        lines.append(message)
    return "\n".join(lines)
