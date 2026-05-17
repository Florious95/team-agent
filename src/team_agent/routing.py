from __future__ import annotations

import fnmatch
import re
from typing import Any


def route_task(spec: dict[str, Any], task: dict[str, Any]) -> dict[str, Any]:
    leader_id = spec.get("leader", {}).get("id", "leader")
    valid_agents = {leader_id, *(a["id"] for a in spec.get("agents", []))}
    explicit = task.get("assignee")
    if explicit:
        if explicit in valid_agents:
            return {"agent_id": explicit, "reason": "explicit assignee on task"}
        return {"agent_id": leader_id, "reason": f"unknown explicit assignee {explicit!r}"}

    rules = sorted(
        spec.get("routing", {}).get("rules", []),
        key=lambda rule: rule.get("priority", 0),
        reverse=True,
    )
    for rule in rules:
        if _rule_matches(rule, task):
            return {
                "agent_id": rule["assign_to"],
                "reason": f"matched routing rule {rule.get('id', '<unnamed>')}",
            }

    default = spec.get("routing", {}).get("default_assignee", leader_id)
    return {"agent_id": default, "reason": "no routing rule matched"}


def _rule_matches(rule: dict[str, Any], task: dict[str, Any]) -> bool:
    match = rule.get("match")
    if isinstance(match, dict) and not _structured_match(match, task):
        return False
    when = rule.get("when")
    if when and not _when_match(str(when), task):
        return False
    return True


def _structured_match(match: dict[str, Any], task: dict[str, Any]) -> bool:
    if "type" in match and task.get("type") not in _as_list(match["type"]):
        return False
    if "risk" in match and task.get("risk") not in _as_list(match["risk"]):
        return False
    if "requires_tools" in match:
        required = set(_as_list(match["requires_tools"]))
        actual = set(task.get("requires_tools", []))
        if not required.issubset(actual):
            return False
    if "files" in match:
        patterns = _as_list(match["files"])
        files = task.get("files", [])
        if not any(fnmatch.fnmatch(path, pattern) for path in files for pattern in patterns):
            return False
    return True


def _when_match(expr: str, task: dict[str, Any]) -> bool:
    type_match = re.fullmatch(r"task\.type\s+in\s+\[(.*)\]", expr.strip())
    if type_match:
        values = _quoted_values(type_match.group(1))
        return task.get("type") in values

    eq_match = re.fullmatch(r"task\.(type|risk)\s*==\s*['\"]([^'\"]+)['\"]", expr.strip())
    if eq_match:
        return task.get(eq_match.group(1)) == eq_match.group(2)

    files_match = re.fullmatch(r"task\.files\s+matches\s+\[(.*)\]", expr.strip())
    if files_match:
        patterns = _quoted_values(files_match.group(1))
        return any(fnmatch.fnmatch(path, pattern) for path in task.get("files", []) for pattern in patterns)

    return False


def _quoted_values(raw: str) -> list[str]:
    return re.findall(r"['\"]([^'\"]+)['\"]", raw)


def _as_list(value: Any) -> list[Any]:
    return value if isinstance(value, list) else [value]
