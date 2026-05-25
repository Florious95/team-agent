from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from team_agent import simple_yaml

_CONDITION_RE = re.compile(
    r"^\s*report_result\.(\w+)\s*==\s*['\"]([^'\"]+)['\"]\s*$"
)


class InvalidPlanError(ValueError):
    def __init__(self, reason: str, *, plan_id: str | None = None, stage_id: str | None = None, expr: str | None = None) -> None:
        self.reason = reason
        self.plan_id = plan_id
        self.stage_id = stage_id
        self.expr = expr
        parts = [reason]
        if plan_id:
            parts.append(f"plan_id={plan_id}")
        if stage_id:
            parts.append(f"stage_id={stage_id}")
        if expr is not None:
            parts.append(f"expr={expr!r}")
        super().__init__(" ".join(parts))


def load_plan(plan_path: Path) -> dict[str, Any]:
    text = Path(plan_path).read_text(encoding="utf-8")
    data = simple_yaml.loads(text)
    if not isinstance(data, dict):
        raise InvalidPlanError("invalid_plan: top-level must be a YAML mapping")
    plan_id = str(data.get("id") or "").strip()
    if not plan_id:
        raise InvalidPlanError("invalid_plan: missing 'id'")
    stages = data.get("stages")
    if not isinstance(stages, list) or not stages:
        raise InvalidPlanError("invalid_plan: stages must be a non-empty list", plan_id=plan_id)
    for stage in stages:
        if not isinstance(stage, dict):
            raise InvalidPlanError("invalid_plan: each stage must be a mapping", plan_id=plan_id)
        stage_id = str(stage.get("id") or "").strip() or None
        for key in ("advance_on", "halt_on"):
            expr = stage.get(key)
            if expr is None:
                continue
            text_expr = str(expr).strip()
            if not _is_supported_condition(text_expr):
                raise InvalidPlanError(
                    f"invalid_condition: {key} grammar must be \"any\" or "
                    "\"report_result.<field> == '<value>'\"",
                    plan_id=plan_id,
                    stage_id=stage_id,
                    expr=text_expr,
                )
    return data


def stage_matches_result(
    stage: dict[str, Any],
    envelope: dict[str, Any],
    *,
    current_dispatch: dict[str, Any] | None = None,
) -> bool:
    agent_id = str(envelope.get("agent_id") or "").strip()
    task_id = str(envelope.get("task_id") or "").strip()
    if current_dispatch:
        expected_agent = str(current_dispatch.get("to") or "").strip()
        expected_task = str(current_dispatch.get("task_id") or "").strip()
        if expected_agent and expected_agent != agent_id:
            return False
        if expected_task:
            return bool(task_id) and expected_task == task_id
        if expected_agent:
            stage_id = str(stage.get("id") or "").strip()
            if stage_id and task_id and stage_id != task_id:
                return False
            return True
        return False
    dispatch = stage.get("dispatch") or {}
    expected_to = str(dispatch.get("to") or "").strip()
    stage_id = str(stage.get("id") or "").strip()
    if expected_to and agent_id and expected_to == agent_id:
        return True
    if stage_id and task_id and stage_id == task_id:
        return True
    return False


def _is_supported_condition(expr: str) -> bool:
    text = (expr or "").strip()
    if not text:
        return False
    if text.lower() == "any":
        return True
    return bool(_CONDITION_RE.match(text))


def evaluate_condition(expr: str, envelope: dict[str, Any]) -> bool:
    text = (expr or "").strip()
    if not text:
        return False
    if text.lower() == "any":
        return True
    match = _CONDITION_RE.match(text)
    if not match:
        raise InvalidPlanError("invalid_condition", expr=text)
    field, expected = match.group(1), match.group(2)
    actual = envelope.get(field)
    if actual is None:
        return False
    return str(actual) == expected


__all__ = [
    "InvalidPlanError",
    "evaluate_condition",
    "load_plan",
    "stage_matches_result",
]
