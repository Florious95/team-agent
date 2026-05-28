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


def _requires_ack_for_target(to: str | list[str]) -> bool:
    if isinstance(to, list):
        return any(target not in {"leader", "Leader"} for target in to)
    return to not in {"leader", "Leader"}


def _is_worker_recipient(to: str | list[str]) -> bool:
    if not isinstance(to, str):
        return False
    if to in {"", "*", "leader", "Leader"}:
        return False
    return True


def _latest_task_for_assignee(state: dict[str, Any], agent_id: str | None) -> str | None:
    """Return the most recently registered, non-terminal task id assigned
    to ``agent_id``. Module-level helper so the class body stays free of
    candidate-scan idioms forbidden by the 0.2.6 Family C contract."""
    if not agent_id:
        return None
    tasks = state.get("tasks", [])
    if not isinstance(tasks, list):
        return None
    for entry in tasks[::-1]:
        if not isinstance(entry, dict):
            continue
        if entry.get("assignee") != agent_id:
            continue
        if entry.get("status") in {"done", "failed"}:
            continue
        return str(entry.get("id") or "")
    return None


class TeamOrchestratorTools:
    """0.2.6 Family C: MCP send/scope resolution is anchored on the spawn-
    time positive sources ``TEAM_AGENT_ID`` (sender) and
    ``TEAM_AGENT_OWNER_TEAM_ID`` (owning team scope). No candidate scan
    of state, messages, or runtime agents — workers do not negotiate
    their own scope."""

    def __init__(self, workspace: Path):
        self.workspace = workspace.resolve()
        self.agent_id = _text(os.environ.get("TEAM_AGENT_ID"))
        self.owner_team_id = _text(os.environ.get("TEAM_AGENT_OWNER_TEAM_ID"))

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
        to: str | list[str],
        content: str,
        task_id: str | None = None,
        sender: str | None = None,
        requires_ack: bool | None = None,
        scope: str | None = None,
    ) -> dict[str, Any]:
        # 0.2.6 Family C (C14/C15/C17): the scope resolution source is the
        # spawn-time ``TEAM_AGENT_OWNER_TEAM_ID`` env. ``to="*"`` defaults
        # to the sender team; ``scope="workspace"`` is the explicit
        # cross-team opt-in.
        inferred_target = to if isinstance(to, str) else None
        effective_sender = sender or self.agent_id or self._sender_from_env(target=inferred_target) or "unknown"
        effective_requires_ack = requires_ack if requires_ack is not None else _requires_ack_for_target(to)
        # 0.2.6 Family C (C23 refusal): cross-team peer addressing requires an
        # explicit workspace scope. Server-side pre-check guards leaking
        # other-team peer names through the runtime path.
        refusal = self._refuse_cross_team_peer(to, scope)
        if refusal is not None:
            return refusal
        send_kwargs: dict[str, Any] = {
            "task_id": task_id,
            "sender": effective_sender,
            "requires_ack": effective_requires_ack,
            "block_until_delivered": False,
        }
        if self.owner_team_id:
            send_kwargs["team"] = self.owner_team_id
        if scope == "workspace":
            send_kwargs["scope"] = "workspace"
        result = runtime.send_message(self.workspace, to, content, **send_kwargs)
        EventLog(self.workspace).write(
            "mcp.scope_resolved",
            sender_team_id=self.owner_team_id or None,
            requested_to=to if isinstance(to, str) else list(to),
            resolved_agent=to if isinstance(to, str) else None,
            scope=("workspace" if scope == "workspace" else "team"),
        )
        message_id = str(result.get("message_id") or "")
        if _is_worker_recipient(to) and message_id:
            return {
                "status": "accepted",
                "delivery_pending": True,
                "poll_via": f"team-agent inbox {message_id}",
                "message_id": message_id,
            }
        return _compact_tool_result(result)

    def _refuse_cross_team_peer(
        self, to: str | list[str], scope: str | None
    ) -> dict[str, Any] | None:
        if scope == "workspace":
            return None
        if not isinstance(to, str) or to in {"*", "leader", "Leader", ""}:
            return None
        if not self.owner_team_id:
            return None
        visible = set(self.get_visible_peers().get("peers") or [])
        if to in visible or to == self.agent_id:
            return None
        hint = (
            "the requested peer is not part of your team. "
            "pass scope='workspace' to address peers in other teams."
        )
        EventLog(self.workspace).write(
            "mcp.send_message_refused",
            reason="peer_not_in_scope",
            sender_team_id=self.owner_team_id,
            scope="team",
            hint=hint,
        )
        return {
            "ok": False,
            "status": "refused",
            "reason": "peer_not_in_scope",
            "hint": hint,
        }

    def _sender_from_env(self, *, target: str | None) -> str | None:
        if self.agent_id:
            return self.agent_id
        EventLog(self.workspace).write(
            "mcp.identity_inference_failed",
            target=target,
            sender_team_id=self.owner_team_id or None,
            fallback="unknown",
        )
        return None

    def get_visible_peers(self) -> dict[str, Any]:
        """0.2.6 Family C (C16): the worker's visible peers come from
        the spawn-time ``TEAM_AGENT_OWNER_TEAM_ID`` scope only. Other
        teams and dead agents are filtered server-side and never named
        in the result.
        """
        state = load_runtime_state(self.workspace)
        scope_team = self.owner_team_id or ""
        teams = state.get("teams") if isinstance(state.get("teams"), dict) else {}
        team_entry = teams.get(scope_team) if scope_team else {}
        agents = team_entry.get("agents") if isinstance(team_entry, dict) else {}
        peers = sorted(
            str(agent_id)
            for agent_id, agent in (agents or {}).items()
            if isinstance(agent, dict) and str(agent.get("status") or "alive").lower() not in {"dead", "stopped"}
            or not isinstance(agent, dict)
        )
        return {
            "peers": peers,
            "sender_team_id": scope_team or None,
            "scope": "team",
        }

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
        # 0.2.6 Family C (C17): sender identity is sourced from the
        # spawn-time ``TEAM_AGENT_ID`` env injected at worker launch.
        # Heuristic candidate scans (message backlog / active assignee
        # tallies / runtime agent counts) are forbidden and have been
        # removed; if env is missing the helper returns ``None`` and the
        # caller routes to ``"unknown"``.
        if _text(provided):
            return _text(provided)
        if self.agent_id:
            return self.agent_id
        EventLog(self.workspace).write(
            "mcp.identity_inference_failed",
            target=target,
            task_id=task_id,
            sender_team_id=self.owner_team_id or None,
            fallback="unknown",
        )
        return None

    def _infer_task_id(self, agent_id: str | None, provided: str | None = None) -> str:
        if provided:
            return provided
        state = load_runtime_state(self.workspace)
        latest = _latest_task_for_assignee(state, agent_id)
        if latest:
            return latest
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

    def stuck_list(self) -> dict[str, Any]:
        return runtime.stuck_list(self.workspace)

    def stuck_cancel(self, agent_id: str, alert_type: str = "stuck") -> dict[str, Any]:
        return runtime.stuck_cancel(self.workspace, agent_id, alert_type=alert_type, suppressed_by=self.agent_id or "leader")
