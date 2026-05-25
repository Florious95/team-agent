from __future__ import annotations

from typing import Any

from team_agent.coordinator.lifecycle import (
    coordinator_health,
    coordinator_tick,
    message_store_schema_health,
    start_coordinator,
    stop_coordinator,
)
from team_agent.coordinator.metadata import (
    COORDINATOR_PROTOCOL_VERSION,
    coordinator_metadata_ok,
    pid_is_running,
    read_coordinator_metadata,
    write_coordinator_metadata,
)
from team_agent.coordinator.paths import (
    coordinator_log_path,
    coordinator_meta_path,
    coordinator_pid_path,
)

__all__ = [
    "COORDINATOR_PROTOCOL_VERSION",
    "coordinator_health",
    "coordinator_log_path",
    "coordinator_meta_path",
    "coordinator_metadata_ok",
    "coordinator_pid_path",
    "coordinator_tick",
    "main",
    "message_store_schema_health",
    "pid_is_running",
    "read_coordinator_metadata",
    "start_coordinator",
    "stop_coordinator",
    "write_coordinator_metadata",
]


def __getattr__(name: str) -> Any:
    # Lazy re-export of the daemon entry so the pyproject console_script
    # `team-agent-coordinator = "team_agent.coordinator:main"` keeps
    # resolving after the package split, without triggering the runtime
    # <-> coordinator import cycle that an eager top-level
    # `from team_agent.coordinator.__main__ import main` would cause
    # (runtime imports coordinator/__init__ at module load).
    if name == "main":
        from team_agent.coordinator.__main__ import main as _main
        return _main
    raise AttributeError(f"module 'team_agent.coordinator' has no attribute {name!r}")
