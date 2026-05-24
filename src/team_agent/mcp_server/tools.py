from __future__ import annotations

import json
import os
from pathlib import Path
from typing import Any

from team_agent import runtime
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.state import load_runtime_state, save_runtime_state, write_team_state

from team_agent.mcp_server.normalize import _compact_tool_result, _normalize_report_envelope, _text


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
        return runtime.status(self.workspace, as_json=True, compact=True)

    def stop_agent(self, agent_id: str) -> dict[str, Any]:
        return _compact_tool_result(runtime.stop_agent(self.workspace, agent_id))

    def reset_agent(self, agent_id: str, discard_session: bool = False) -> dict[str, Any]:
        return _compact_tool_result(runtime.reset_agent(self.workspace, agent_id, discard_session=discard_session))

    def add_agent(self, new_agent_id: str, role_file_path: str) -> dict[str, Any]:
        return _compact_tool_result(runtime.add_agent(self.workspace, new_agent_id, role_file_path=role_file_path))

    def fork_agent(self, source_agent_id: str, as_agent_id: str, label: str | None = None) -> dict[str, Any]:
        return _compact_tool_result(runtime.fork_agent(self.workspace, source_agent_id, as_agent_id=as_agent_id, label=label))

    def request_human(self, question: str, task_id: str | None = None, agent_id: str | None = None) -> dict[str, Any]:
        store = MessageStore(self.workspace)
        sender = agent_id or self._infer_agent_id(task_id=task_id, target="leader") or "unknown"
        message_id = store.create_message(task_id, sender, "leader", question, requires_ack=True)
        return {"ok": True, "message_id": message_id, "status": "needs_human"}
