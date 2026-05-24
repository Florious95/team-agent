from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.simple_yaml import dumps
from team_agent.spec import load_spec, workspace_from_spec


def init_workspace(workspace: Path, force: bool = False) -> dict[str, Path]:
    from team_agent.runtime import ensure_workspace_dirs
    from team_agent.paths import example_path, template_path

    ensure_workspace_dirs(workspace)
    team_dir = workspace / ".team" / "current"
    team_dir.mkdir(parents=True, exist_ok=True)
    spec_path = team_dir / "team.spec.yaml"
    state_path = workspace / "team_state.md"
    if spec_path.exists() and not force:
        from team_agent.runtime import RuntimeError
        raise RuntimeError(f"{spec_path} already exists; pass --force to overwrite")
    spec_path.write_text(example_path("team.spec.yaml").read_text(encoding="utf-8"), encoding="utf-8")
    if not state_path.exists() or force:
        state_path.write_text(template_path("team_state.md").read_text(encoding="utf-8"), encoding="utf-8")
    EventLog(workspace).write("init", spec_path=str(spec_path), state_path=str(state_path))
    return {"spec": spec_path, "state": state_path}


def validate_file(spec_path: Path) -> dict[str, Any]:
    if spec_path.is_dir():
        from team_agent.compiler import compile_team

        result = compile_team(spec_path)
        spec = result["spec"]
        return {
            "ok": True,
            "type": "team_dir",
            "workspace": str(Path(spec["team"]["workspace"]).resolve()),
            "team": spec["team"]["name"],
            "agents": [agent["id"] for agent in spec.get("agents", [])],
        }
    spec = load_spec(spec_path)
    workspace = workspace_from_spec(spec, spec_path)
    return {"ok": True, "workspace": str(workspace), "team": spec["team"]["name"]}


def tmux_session_conflict_error(session_name: str) -> str:
    return (
        f"tmux session already exists: {session_name}. "
        "Startup will not terminate existing tmux sessions because they may belong to active teams. "
        "Use a different team name or runtime.session_name and start again."
    )


def spec_team_dir(spec_path: Path, workspace: Path) -> Path:
    spec_dir = spec_path.resolve().parent
    if spec_dir.parent.name == ".team":
        return spec_dir
    return workspace.resolve() / ".team" / "current"


def is_team_doc_dir(team_dir: Path) -> bool:
    return (team_dir / "TEAM.md").exists() and (team_dir / "agents").is_dir()


def compile_team_dir_spec(team_dir: Path, workspace: Path) -> dict[str, Any]:
    from team_agent.compiler import compile_team

    spec_path = team_dir / "team.spec.yaml"
    compiled = compile_team(team_dir, spec_path)
    if compiled["spec"].get("context", {}).get("state_file") == "team_state.md":
        state_file = str(team_dir.relative_to(workspace) / "team_state.md") if team_dir.is_relative_to(workspace) else "team_state.md"
        compiled["spec"]["context"]["state_file"] = state_file
        spec_path.write_text(dumps(compiled["spec"]), encoding="utf-8")
    return compiled


def attach_team_profile_dirs(spec: dict[str, Any], spec_path: Path, workspace: Path | None = None, team_dir: Path | None = None) -> None:
    workspace = workspace.resolve() if workspace else workspace_from_spec(spec, spec_path)
    team_dir = team_dir.resolve() if team_dir else spec_team_dir(spec_path, workspace)
    profiles_dir = team_dir / "profiles"
    for agent in spec.get("agents", []):
        if isinstance(agent, dict) and agent.get("profile"):
            agent["_profile_dir"] = str(profiles_dir)
