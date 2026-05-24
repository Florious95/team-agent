from __future__ import annotations

import copy
import json
import os
import re
import shutil
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.paths import runtime_dir
from team_agent.spec import load_spec
from team_agent.state import normalize_agent_session_state


def save_team_runtime_snapshot(workspace: Path, state: dict[str, Any]) -> Path | None:
    from team_agent.runtime import _spec_team_dir
    session_name = state.get("session_name")
    if not session_name:
        return None
    snapshot_dir = team_runtime_snapshot_dir(workspace, str(session_name))
    snapshot_dir.mkdir(parents=True, exist_ok=True)
    snapshot_state = copy.deepcopy(state)
    spec_path = Path(str(state.get("spec_path") or ""))
    if spec_path.is_file():
        if not snapshot_state.get("team_dir"):
            snapshot_state["team_dir"] = str(_spec_team_dir(spec_path, workspace))
        snapshot_spec = snapshot_dir / "team.spec.yaml"
        if spec_path.resolve() != snapshot_spec.resolve():
            shutil.copy2(spec_path, snapshot_spec)
        snapshot_state["spec_path"] = str(snapshot_spec)
    snapshot_state["team_snapshot"] = {
        "session_name": session_name,
        "team_name": state_team_name(snapshot_state),
        "snapshot_dir": str(snapshot_dir),
        "updated_at": datetime.now(timezone.utc).isoformat(),
    }
    state_path = snapshot_dir / "state.json"
    tmp_path = state_path.with_suffix(".json.tmp")
    tmp_path.write_text(json.dumps(snapshot_state, indent=2, ensure_ascii=False), encoding="utf-8")
    os.replace(tmp_path, state_path)
    return state_path


def team_runtime_snapshot_dir(workspace: Path, session_name: str) -> Path:
    return runtime_dir(workspace) / "teams" / safe_snapshot_name(session_name)


def safe_snapshot_name(value: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]", "_", value).strip("._-") or "team"


def state_team_name(state: dict[str, Any]) -> str | None:
    spec_path = state.get("spec_path")
    if not spec_path:
        return None
    try:
        return str(load_spec(Path(str(spec_path))).get("team", {}).get("name") or "")
    except Exception:
        return None


def load_snapshot_state(path: Path) -> dict[str, Any] | None:
    try:
        state = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    normalize_agent_session_state(state)
    return state
