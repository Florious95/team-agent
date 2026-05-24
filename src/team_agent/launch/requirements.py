from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.diagnose import (
    compact_model_checks,
    format_model_check_failures,
    format_profile_check_failures,
    format_profile_smoke_failures,
    model_checks_for_agents,
    profile_checks_for_agents,
    profile_smoke_checks_for_agents,
)
from team_agent.events import EventLog
from team_agent.profiles import compact_profile_check


def ensure_agent_start_requirements(
    workspace: Path,
    agents: list[dict[str, Any]],
    event_log: EventLog,
    event_prefix: str,
    skip_profile_smoke: bool = False,
) -> None:
    from team_agent.runtime import RuntimeError, get_adapter
    active_agents = [agent for agent in agents if not agent.get("paused")]
    for agent in active_agents:
        adapter = get_adapter(agent["provider"])
        if not adapter.is_installed():
            event_log.write(
                f"{event_prefix}.provider_missing",
                agent_id=agent["id"],
                provider=agent["provider"],
                command=adapter.command_name,
            )
            raise RuntimeError(
                f"Provider {agent['provider']} command {adapter.command_name!r} not found for agent {agent['id']}"
            )
    profile_checks = profile_checks_for_agents(workspace, active_agents)
    profile_failures = [item for item in profile_checks if item.get("ok") is False]
    event_log.write(f"{event_prefix}.profile_check", ok=not profile_failures, checks=[compact_profile_check(item) for item in profile_checks])
    if profile_failures:
        raise RuntimeError(format_profile_check_failures(profile_failures))
    if skip_profile_smoke:
        event_log.write(f"{event_prefix}.profile_smoke_check", ok=True, skipped=True, reason="already_checked")
    else:
        smoke_checks = profile_smoke_checks_for_agents(workspace, active_agents)
        smoke_failures = [item for item in smoke_checks if item.get("ok") is False]
        event_log.write(f"{event_prefix}.profile_smoke_check", ok=not smoke_failures, checks=[compact_profile_check(item) for item in smoke_checks])
        if smoke_failures:
            raise RuntimeError(format_profile_smoke_failures(smoke_failures))
    checks = model_checks_for_agents(active_agents, workspace)
    failures = [item for item in checks if item.get("ok") is False]
    event_log.write(f"{event_prefix}.model_check", ok=not failures, checks=compact_model_checks(checks))
    if failures:
        raise RuntimeError(format_model_check_failures(failures))
