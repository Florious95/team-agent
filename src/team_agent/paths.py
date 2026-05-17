from __future__ import annotations

from pathlib import Path


PACKAGE_ROOT = Path(__file__).resolve().parents[2]


def repo_root() -> Path:
    return PACKAGE_ROOT


def schema_path(name: str) -> Path:
    return repo_root() / "schemas" / name


def template_path(name: str) -> Path:
    return repo_root() / "templates" / name


def example_path(name: str) -> Path:
    return repo_root() / "examples" / name


def runtime_dir(workspace: Path) -> Path:
    return workspace / ".team" / "runtime"


def logs_dir(workspace: Path) -> Path:
    return workspace / ".team" / "logs"


def artifacts_dir(workspace: Path) -> Path:
    return workspace / ".team" / "artifacts"


def messages_dir(workspace: Path) -> Path:
    return workspace / ".team" / "messages"


def team_workspace(team_dir: Path) -> Path:
    team_dir = team_dir.resolve()
    if team_dir.parent.name == ".team":
        return team_dir.parent.parent
    return team_dir.parent
