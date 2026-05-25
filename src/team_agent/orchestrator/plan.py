from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from team_agent import simple_yaml

_CONDITION_RE = re.compile(
    r"^\s*report_result\.(\w+)\s*==\s*['\"]([^'\"]+)['\"]\s*$"
)


def load_plan(plan_path: Path) -> dict[str, Any]:
    text = Path(plan_path).read_text(encoding="utf-8")
    data = simple_yaml.loads(text)
    if not isinstance(data, dict):
        raise ValueError(f"plan at {plan_path} must be a YAML mapping")
    return data


def stage_matches_result(stage: dict[str, Any], envelope: dict[str, Any]) -> bool:
    dispatch = stage.get("dispatch") or {}
    expected_to = str(dispatch.get("to") or "").strip()
    stage_id = str(stage.get("id") or "").strip()
    agent_id = str(envelope.get("agent_id") or "").strip()
    task_id = str(envelope.get("task_id") or "").strip()
    if expected_to and agent_id and expected_to == agent_id:
        return True
    if stage_id and task_id and stage_id == task_id:
        return True
    return False


def evaluate_condition(expr: str, envelope: dict[str, Any]) -> bool:
    text = (expr or "").strip()
    if not text:
        return False
    if text.lower() == "any":
        return True
    match = _CONDITION_RE.match(text)
    if not match:
        return False
    field, expected = match.group(1), match.group(2)
    actual = envelope.get(field)
    if actual is None:
        return False
    return str(actual) == expected


__all__ = ["evaluate_condition", "load_plan", "stage_matches_result"]
