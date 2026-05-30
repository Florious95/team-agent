from __future__ import annotations

from pathlib import Path
from typing import Any, Protocol


class CommsSelftestDriver(Protocol):
    """Injectable boundary for contract tests; production uses real tmux/runtime."""


def run_comms_selftest(
    workspace: Path,
    *,
    team: str | None = None,
    gate: str | None = None,
    response_sla_sec: float = 20.0,
    probe_content: str | None = None,
    driver: CommsSelftestDriver | None = None,
) -> dict[str, Any]:
    """Run doctor --comms binding + installed-code contract checks and return stable JSON."""
    raise NotImplementedError("contract stub: implement in team_agent.diagnose.comms")


def evaluate_idle_behavior(
    workspace: Path,
    *,
    agent_id: str,
    claimed_status: str,
    response_sla_sec: float = 20.0,
    token: str | None = None,
    driver: CommsSelftestDriver | None = None,
) -> dict[str, Any]:
    """Challenge an IDLE/WORKING claim with a bounded execution ack probe."""
    raise NotImplementedError("contract stub: implement in team_agent.diagnose.comms")
