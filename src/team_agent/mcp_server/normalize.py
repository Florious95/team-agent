from __future__ import annotations

from typing import Any


def _compact_tool_result(result: dict[str, Any]) -> dict[str, Any]:
    if result.get("ok") is False:
        keys = [
            "ok",
            "status",
            "reason",
            "error",
            "message_id",
            "agent_id",
            "new_agent_id",
            "source_agent_id",
            "role_file_sha",
            "session_id",
            "to",
            "targets",
            "delivered_count",
            "failed_count",
            "fallback_path",
            "suggestion",
        ]
        return {key: result[key] for key in keys if key in result}
    keys = [
        "ok",
        "status",
        "message_id",
        "to",
        "targets",
        "delivered_count",
        "failed_count",
        "submitted",
        "visible",
        "result_id",
        "task_id",
        "agent_id",
        "new_agent_id",
        "source_agent_id",
        "role_file_sha",
        "session_id",
        "leader_notified",
        "notification_message_id",
        "notification_status",
        "notification_channel",
        "notification_event_id",
    ]
    compact = {key: result[key] for key in keys if key in result}
    if "acknowledged_messages" in result:
        compact["acknowledged_count"] = len(result.get("acknowledged_messages") or [])
    return compact or {"ok": True}


def _normalize_report_envelope(env: dict[str, Any]) -> dict[str, Any]:
    summary = _text(env.get("summary")) or "completed"
    return {
        "schema_version": "result_envelope_v1",
        "task_id": _text(env.get("task_id")) or "manual",
        "agent_id": _text(env.get("agent_id")) or "unknown",
        "status": _normalize_result_status(env.get("status")),
        "summary": summary,
        "changes": _normalize_changes(env.get("changes"), summary),
        "tests": _normalize_tests(env.get("tests")),
        "risks": _normalize_risks(env.get("risks")),
        "artifacts": _normalize_artifacts(env.get("artifacts")),
        "next_actions": _normalize_next_actions(env.get("next_actions")),
    }


def _items(value: Any) -> list[Any]:
    if value is None:
        return []
    if isinstance(value, list):
        return value
    return [value]


def _text(value: Any) -> str | None:
    if value is None:
        return None
    text = str(value).strip()
    return text or None


def _first_text(item: dict[str, Any], *keys: str) -> str | None:
    for key in keys:
        text = _text(item.get(key))
        if text:
            return text
    return None


def _normalize_result_status(value: Any) -> str:
    text = (_text(value) or "success").lower().replace("-", "_").replace(" ", "_")
    mapping = {
        "ok": "success",
        "done": "success",
        "complete": "success",
        "completed": "success",
        "passed": "success",
        "pass": "success",
        "blocked": "blocked",
        "block": "blocked",
        "failed": "failed",
        "fail": "failed",
        "error": "failed",
        "partial": "partial",
        "partially_done": "partial",
    }
    return mapping.get(text, text if text in {"success", "blocked", "failed", "partial"} else "success")


def _normalize_changes(value: Any, fallback_summary: str) -> list[dict[str, str]]:
    changes: list[dict[str, str]] = []
    for item in _items(value):
        if not isinstance(item, dict):
            continue
        path = _first_text(item, "path", "file", "filepath", "filename")
        if not path:
            continue
        description = _first_text(item, "description", "summary", "detail", "details", "message") or fallback_summary
        changes.append(
            {
                "path": path,
                "kind": _normalize_change_kind(_first_text(item, "kind", "type", "action"), description),
                "description": description,
            }
        )
    return changes


def _normalize_change_kind(value: str | None, description: str) -> str:
    text = (value or "").strip().lower().replace("-", "_").replace(" ", "_")
    if text in {"created", "modified", "deleted", "observed"}:
        return text
    mapping = {
        "create": "created",
        "added": "created",
        "add": "created",
        "new": "created",
        "changed": "modified",
        "change": "modified",
        "updated": "modified",
        "update": "modified",
        "edited": "modified",
        "edit": "modified",
        "removed": "deleted",
        "remove": "deleted",
        "delete": "deleted",
        "observe": "observed",
        "observed": "observed",
        "inspected": "observed",
        "inspect": "observed",
    }
    if text in mapping:
        return mapping[text]
    description_text = description.lower()
    if any(word in description_text for word in ["created", "added", "new file"]):
        return "created"
    if any(word in description_text for word in ["deleted", "removed"]):
        return "deleted"
    if any(word in description_text for word in ["observed", "inspected", "verified"]):
        return "observed"
    return "modified"


def _normalize_tests(value: Any) -> list[dict[str, str]]:
    tests: list[dict[str, str]] = []
    for item in _items(value):
        if not isinstance(item, dict):
            command = _text(item)
            if command:
                tests.append({"command": command, "status": "not_run"})
            continue
        command = _first_text(item, "command", "cmd", "name", "test")
        if not command:
            continue
        test = {"command": command, "status": _normalize_test_status(item.get("status"))}
        detail = _first_text(item, "detail", "output", "stdout", "stderr", "summary", "message")
        if detail:
            test["detail"] = detail
        tests.append(test)
    return tests


def _normalize_test_status(value: Any) -> str:
    text = (_text(value) or "not_run").lower().replace("-", "_").replace(" ", "_")
    mapping = {
        "pass": "passed",
        "ok": "passed",
        "success": "passed",
        "fail": "failed",
        "error": "failed",
        "notrun": "not_run",
        "not_run": "not_run",
        "notrun_yet": "not_run",
        "skip": "skipped",
    }
    return mapping.get(text, text if text in {"passed", "failed", "not_run", "skipped"} else "not_run")


def _normalize_risks(value: Any) -> list[dict[str, str]]:
    risks: list[dict[str, str]] = []
    for item in _items(value):
        if not isinstance(item, dict):
            description = _text(item)
            if description:
                risks.append({"severity": "low", "description": description})
            continue
        description = _first_text(item, "description", "summary", "detail", "message")
        if not description:
            continue
        severity = (_first_text(item, "severity", "level") or "low").lower()
        if severity not in {"low", "medium", "high"}:
            severity = "low"
        risks.append({"severity": severity, "description": description})
    return risks


def _normalize_artifacts(value: Any) -> list[dict[str, str]]:
    artifacts: list[dict[str, str]] = []
    for item in _items(value):
        if not isinstance(item, dict):
            path = _text(item)
            if path:
                artifacts.append({"path": path, "description": path})
            continue
        path = _first_text(item, "path", "file", "filepath", "filename")
        if not path:
            continue
        artifacts.append({"path": path, "description": _first_text(item, "description", "summary", "detail") or path})
    return artifacts


def _normalize_next_actions(value: Any) -> list[dict[str, str]]:
    actions: list[dict[str, str]] = []
    for item in _items(value):
        if isinstance(item, dict):
            description = _first_text(item, "description", "summary", "action", "todo", "message")
        else:
            description = _text(item)
        if description:
            actions.append({"description": description})
    return actions
