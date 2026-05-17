from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any

from team_agent import runtime
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.state import load_runtime_state, save_runtime_state, write_team_state


class TeamOrchestratorTools:
    def __init__(self, workspace: Path):
        self.workspace = workspace.resolve()
        self.agent_id = _text(os.environ.get("TEAM_AGENT_ID"))

    def assign_task(self, task: dict[str, Any], message: str | None = None) -> dict[str, Any]:
        state = load_runtime_state(self.workspace)
        tasks = state.setdefault("tasks", [])
        existing = next((item for item in tasks if item.get("id") == task.get("id")), None)
        if existing:
            existing.update(task)
        else:
            tasks.append(task)
        save_runtime_state(self.workspace, state)
        content = message or task.get("description") or task.get("title") or json.dumps(task)
        return _compact_tool_result(runtime.send_message(self.workspace, task.get("assignee"), content, task_id=task["id"]))

    def send_message(
        self,
        to: str,
        content: str,
        task_id: str | None = None,
        sender: str | None = None,
        requires_ack: bool | None = None,
    ) -> dict[str, Any]:
        effective_sender = sender or self._infer_agent_id(task_id=task_id, target=to) or "unknown"
        effective_requires_ack = requires_ack if requires_ack is not None else to not in {"leader", "Leader"}
        return _compact_tool_result(
            runtime.send_message(
                self.workspace,
                to,
                content,
                task_id=task_id,
                sender=effective_sender,
                requires_ack=effective_requires_ack,
            )
        )

    def report_result(
        self,
        envelope: dict[str, Any] | None = None,
        summary: str | None = None,
        status: str = "success",
        changes: list[dict[str, Any]] | None = None,
        tests: list[dict[str, Any]] | None = None,
        risks: list[dict[str, Any]] | None = None,
        artifacts: list[dict[str, Any]] | None = None,
        next_actions: list[dict[str, Any]] | None = None,
        task_id: str | None = None,
        agent_id: str | None = None,
    ) -> dict[str, Any]:
        env = dict(envelope or {})
        effective_task = self._infer_task_id(
            agent_id or _text(env.get("agent_id")) or self.agent_id,
            task_id or _text(env.get("task_id")),
        )
        effective_agent = agent_id or _text(env.get("agent_id")) or self._infer_agent_id(task_id=effective_task) or "unknown"
        env.setdefault("schema_version", "result_envelope_v1")
        env.setdefault("agent_id", effective_agent)
        env.setdefault("task_id", effective_task)
        env.setdefault("status", status)
        env.setdefault("summary", summary or env.get("summary") or "completed")
        env.setdefault("changes", changes if changes is not None else [])
        env.setdefault("tests", tests if tests is not None else [])
        env.setdefault("risks", risks if risks is not None else [])
        env.setdefault("artifacts", artifacts if artifacts is not None else [])
        env.setdefault("next_actions", next_actions if next_actions is not None else [])
        env = _normalize_report_envelope(env)
        return _compact_tool_result(runtime.report_result(self.workspace, env))

    def _infer_agent_id(self, provided: str | None = None, task_id: str | None = None, target: str | None = None) -> str | None:
        if _text(provided):
            return _text(provided)
        if self.agent_id:
            return self.agent_id
        state = load_runtime_state(self.workspace)
        leader_id = state.get("leader", {}).get("id") or "leader"
        runtime_agents = {str(agent_id) for agent_id in state.get("agents", {})}
        task = self._task_for_id(state, task_id)
        if task and task.get("assignee") in runtime_agents:
            return str(task["assignee"])
        messages = MessageStore(self.workspace).messages()
        if task_id:
            for row in reversed(messages):
                if row.get("task_id") != task_id:
                    continue
                for key in ("recipient", "sender"):
                    candidate = row.get(key)
                    if candidate in runtime_agents and candidate not in {leader_id, "leader", "Leader"}:
                        return str(candidate)
        active_assignees = {
            str(task_item.get("assignee"))
            for task_item in state.get("tasks", [])
            if task_item.get("assignee") in runtime_agents and task_item.get("status") not in {"done", "failed"}
        }
        if len(active_assignees) == 1:
            return next(iter(active_assignees))
        if len(runtime_agents) == 1:
            return next(iter(runtime_agents))
        for row in reversed(messages):
            for key in ("recipient", "sender"):
                candidate = row.get(key)
                if candidate in runtime_agents and candidate not in {leader_id, "leader", "Leader"}:
                    return str(candidate)
        EventLog(self.workspace).write(
            "mcp.identity_inference_failed",
            target=target,
            task_id=task_id,
            runtime_agents=sorted(runtime_agents),
            fallback="unknown",
        )
        return None

    def _infer_task_id(self, agent_id: str | None, provided: str | None = None) -> str:
        if provided:
            return provided
        state = load_runtime_state(self.workspace)
        for task in reversed(state.get("tasks", [])):
            if agent_id and task.get("assignee") == agent_id and task.get("status") not in {"done", "failed"}:
                return str(task["id"])
        active_tasks = [
            task
            for task in state.get("tasks", [])
            if task.get("assignee") and task.get("status") not in {"done", "failed"}
        ]
        if len(active_tasks) == 1:
            return str(active_tasks[0]["id"])
        messages = MessageStore(self.workspace).messages()
        for row in reversed(messages):
            if agent_id and row.get("recipient") == agent_id and row.get("task_id"):
                return str(row["task_id"])
        for row in reversed(messages):
            if agent_id and row.get("recipient") == agent_id:
                return str(row["message_id"])
        for row in reversed(messages):
            if row.get("task_id"):
                return str(row["task_id"])
        EventLog(self.workspace).write("mcp.task_inference_failed", agent_id=agent_id, fallback="manual")
        return "manual"

    def _task_for_id(self, state: dict[str, Any], task_id: str | None) -> dict[str, Any] | None:
        if not task_id:
            return None
        return next((task for task in state.get("tasks", []) if task.get("id") == task_id), None)

    def update_state(self, note: str) -> dict[str, Any]:
        state = load_runtime_state(self.workspace)
        spec_path = Path(state.get("spec_path", self.workspace / "team.spec.yaml"))
        from team_agent.spec import load_spec

        spec = load_spec(spec_path)
        state.setdefault("notes", []).append(note)
        save_runtime_state(self.workspace, state)
        path = write_team_state(self.workspace, spec, state)
        return {"ok": True, "state_file": str(path)}

    def get_team_status(self) -> dict[str, Any]:
        return runtime.status(self.workspace, as_json=True)

    def request_human(self, question: str, task_id: str | None = None, agent_id: str | None = None) -> dict[str, Any]:
        store = MessageStore(self.workspace)
        sender = agent_id or self._infer_agent_id(task_id=task_id, target="leader") or "unknown"
        message_id = store.create_message(task_id, sender, "leader", question, requires_ack=True)
        return {"ok": True, "message_id": message_id, "status": "needs_human"}


TOOLS = [
    {
        "name": "assign_task",
        "description": "Add or update a task in the team graph and deliver it to its assignee.",
        "inputSchema": {
            "type": "object",
            "required": ["task"],
            "properties": {
                "task": {"type": "object"},
                "message": {"type": "string"},
            },
        },
    },
    {
        "name": "send_message",
        "description": "Send a message to a teammate, the leader, or '*' for all other team members. Provide only target and content; Team Agent fills sender, task id, ack policy, and delivery metadata.",
        "inputSchema": {
            "type": "object",
            "required": ["to", "content"],
            "properties": {
                "to": {"type": "string"},
                "content": {"type": "string"},
            },
            "additionalProperties": False,
        },
    },
    {
        "name": "report_result",
        "description": "Report task completion. Provide a short summary and optional status/details; Team Agent fills schema_version, task_id, and agent_id, and normalizes common change/test field aliases.",
        "inputSchema": {
            "type": "object",
            "required": ["summary"],
            "properties": {
                "summary": {"type": "string"},
                "status": {"type": "string", "enum": ["success", "blocked", "failed", "partial"]},
                "changes": {"type": "array", "items": {"type": "object"}},
                "tests": {"type": "array", "items": {"type": "object"}},
                "risks": {"type": "array", "items": {"type": "object"}},
                "artifacts": {"type": "array", "items": {"type": "object"}},
                "next_actions": {"type": "array", "items": {"type": "object"}},
            },
            "additionalProperties": False,
        },
    },
    {
        "name": "update_state",
        "description": "Append a note to team state and rewrite team_state.md.",
        "inputSchema": {
            "type": "object",
            "required": ["note"],
            "properties": {"note": {"type": "string"}},
        },
    },
    {
        "name": "get_team_status",
        "description": "Return machine-readable team status.",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "request_human",
        "description": "Ask the leader/user for human input.",
        "inputSchema": {
            "type": "object",
            "required": ["question"],
            "properties": {
                "question": {"type": "string"},
                "task_id": {"type": "string"},
                "agent_id": {"type": "string"},
            },
        },
    },
]


def dispatch(tools: TeamOrchestratorTools, request: dict[str, Any]) -> dict[str, Any]:
    tool = request.get("tool") or request.get("method")
    args = request.get("arguments") or request.get("params") or {}
    if tool == "assign_task":
        return tools.assign_task(**args)
    if tool == "send_message":
        return tools.send_message(**args)
    if tool == "report_result":
        return tools.report_result(**args)
    if tool == "update_state":
        return tools.update_state(**args)
    if tool == "get_team_status":
        return tools.get_team_status()
    if tool == "request_human":
        return tools.request_human(**args)
    return {"ok": False, "error": f"unknown tool {tool!r}"}


def _compact_tool_result(result: dict[str, Any]) -> dict[str, Any]:
    if result.get("ok") is False:
        keys = ["ok", "status", "reason", "error", "message_id", "to", "targets", "delivered_count", "failed_count", "fallback_path", "suggestion"]
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


def handle_mcp(tools: TeamOrchestratorTools, request: dict[str, Any]) -> dict[str, Any] | None:
    method = request.get("method")
    msg_id = request.get("id")
    if method and method.startswith("notifications/"):
        return None
    if method == "initialize":
        return {
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {
                "protocolVersion": request.get("params", {}).get("protocolVersion", "2024-11-05"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "team_orchestrator", "version": "0.1.4"},
            },
        }
    if method == "tools/list":
        return {"jsonrpc": "2.0", "id": msg_id, "result": {"tools": TOOLS}}
    if method == "tools/call":
        params = request.get("params", {})
        name = params.get("name")
        arguments = params.get("arguments") or {}
        result = dispatch(tools, {"tool": name, "arguments": arguments})
        is_error = result.get("ok") is False
        return {
            "jsonrpc": "2.0",
            "id": msg_id,
            "result": {
                "content": [
                    {
                        "type": "text",
                        "text": json.dumps(result, ensure_ascii=False),
                    }
                ],
                "isError": is_error,
            },
        }
    return {
        "jsonrpc": "2.0",
        "id": msg_id,
        "error": {"code": -32601, "message": f"unknown method {method!r}"},
    }


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description="TeamSpec team_orchestrator MCP stdio server")
    parser.add_argument("--workspace", default=".", help="Workspace containing .team/runtime")
    args = parser.parse_args(argv)
    tools = TeamOrchestratorTools(Path(args.workspace))
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
            if request.get("jsonrpc") == "2.0":
                response = handle_mcp(tools, request)
                if response is None:
                    continue
                sys.stdout.write(json.dumps(response, ensure_ascii=False) + "\n")
                sys.stdout.flush()
                continue
            result = dispatch(tools, request)
            sys.stdout.write(json.dumps({"ok": result.get("ok", True), "result": result}, ensure_ascii=False) + "\n")
            sys.stdout.flush()
        except Exception as exc:  # MCP transports need errors surfaced on stdout.
            if "request" in locals() and isinstance(request, dict) and request.get("jsonrpc") == "2.0":
                sys.stdout.write(
                    json.dumps(
                        {
                            "jsonrpc": "2.0",
                            "id": request.get("id"),
                            "error": {"code": -32000, "message": str(exc)},
                        },
                        ensure_ascii=False,
                    )
                    + "\n"
                )
            else:
                sys.stdout.write(json.dumps({"ok": False, "error": str(exc)}, ensure_ascii=False) + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
