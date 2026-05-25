from __future__ import annotations

import json
from pathlib import Path
from typing import Any


def _orchestrator_runtime_dir(workspace: Path) -> Path:
    return Path(workspace) / ".team" / "runtime" / "orchestrator"


def _orchestrator_artifact_dir(workspace: Path) -> Path:
    return Path(workspace) / ".team" / "artifacts" / "orchestrator"


def state_path(workspace: Path, plan_id: str) -> Path:
    return _orchestrator_runtime_dir(workspace) / f"plan-{plan_id}.state.json"


def artifact_path(workspace: Path, plan_id: str, ts: str) -> Path:
    return _orchestrator_artifact_dir(workspace) / f"halt-{plan_id}-{ts}.md"


def load_plan_state(workspace: Path, plan_id: str) -> dict[str, Any] | None:
    path = state_path(workspace, plan_id)
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return None


def save_plan_state(workspace: Path, state: dict[str, Any]) -> Path:
    plan_id = str(state.get("plan_id") or "").strip()
    if not plan_id:
        raise ValueError("plan state missing 'plan_id'")
    path = state_path(workspace, plan_id)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(state, indent=2, ensure_ascii=False, sort_keys=True), encoding="utf-8")
    return path


def list_plan_states(workspace: Path) -> list[dict[str, Any]]:
    directory = _orchestrator_runtime_dir(workspace)
    if not directory.exists():
        return []
    out: list[dict[str, Any]] = []
    for path in sorted(directory.glob("plan-*.state.json")):
        try:
            out.append(json.loads(path.read_text(encoding="utf-8")))
        except json.JSONDecodeError:
            continue
    return out


__all__ = [
    "artifact_path",
    "list_plan_states",
    "load_plan_state",
    "save_plan_state",
    "state_path",
]
