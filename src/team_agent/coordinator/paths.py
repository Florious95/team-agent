from __future__ import annotations

from pathlib import Path

from team_agent.paths import runtime_dir


def coordinator_pid_path(workspace: Path) -> Path:
    return runtime_dir(workspace) / "coordinator.pid"


def coordinator_meta_path(workspace: Path) -> Path:
    return runtime_dir(workspace) / "coordinator.json"


def coordinator_log_path(workspace: Path) -> Path:
    return runtime_dir(workspace) / "coordinator.log"
