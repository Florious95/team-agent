from __future__ import annotations

from team_agent.diagnose.checks import (
    compact_model_checks,
    format_model_check_failures,
    format_profile_check_failures,
    format_profile_smoke_failures,
    model_checks_for_agents,
    profile_checks_for_agents,
    profile_smoke_checks_for_agents,
)
from team_agent.diagnose.health import diagnose, doctor
from team_agent.diagnose.preflight import (
    ensure_profiles_for_roles,
    preflight,
    preflight_blockers,
    preflight_next_actions,
    start,
)
from team_agent.diagnose.quick_start import (
    prepare_quick_start_team,
    quick_start,
    repair_state,
    settle,
    wait_ready,
)

__all__ = [
    "compact_model_checks",
    "diagnose",
    "doctor",
    "ensure_profiles_for_roles",
    "format_model_check_failures",
    "format_profile_check_failures",
    "format_profile_smoke_failures",
    "model_checks_for_agents",
    "prepare_quick_start_team",
    "preflight",
    "preflight_blockers",
    "preflight_next_actions",
    "profile_checks_for_agents",
    "profile_smoke_checks_for_agents",
    "quick_start",
    "repair_state",
    "settle",
    "start",
    "wait_ready",
]
