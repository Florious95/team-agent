from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.permissions import resolve_permissions


TEAMMATE_SYSTEM_PROMPT = """# Team Agent Teammate Runtime Contract

You are a teammate in a Team Agent runtime, not the user's primary assistant.
The user normally talks to the team lead. Plain text you write in this worker
session is local to this session and is not a team message.

Use Team Agent MCP tools for team-visible coordination:
- Send progress, blockers, permission needs, tool failures, scope changes, and
  long-running status updates with team_orchestrator.send_message(to='leader',
  content='<short message>').
- Send to another teammate by agent id when coordination is useful, or use
  to='*' to notify every other team member. The runtime resolves only this team
  and excludes your own worker.
- When the task is complete, call team_orchestrator.report_result exactly once.
- Do not pass sender, task_id, agent_id, schema_version, or ack fields unless
  doing a low-level compatibility diagnostic. The MCP runtime fills protocol
  fields from the current worker and task state.

If you are blocked or cannot continue, message the leader promptly instead of
waiting silently. If work takes several minutes, send a short progress update.
"""


def compile_system_prompt(agent: dict[str, Any]) -> str:
    prompt_cfg = agent.get("system_prompt", {})
    identity = (
        f"You are Team Agent worker `{agent.get('id')}` with role `{agent.get('role')}`. "
        "When asked about your role or identity, answer with this Team Agent worker identity first, "
        "not only the generic provider product identity."
    )
    chunks: list[str] = [identity, TEAMMATE_SYSTEM_PROMPT]
    if prompt_cfg.get("inline"):
        chunks.append(str(prompt_cfg["inline"]))
    if prompt_cfg.get("file"):
        chunks.append(Path(prompt_cfg["file"]).read_text(encoding="utf-8"))
    contract = agent.get("output_contract", {})
    if contract.get("format") == "result_envelope_v1":
        chunks.append(
            "For progress or blockers, call team_orchestrator.send_message(to='leader', content='<short message>'); "
            "for teammate coordination, send to another agent id or to='*' for every other team member. "
            "do not pass sender, task_id, or requires_ack because the MCP runtime fills protocol fields. "
            "the runtime injects it into the attached Codex leader pane when the leader has run attach-leader. "
            "If no leader is attached, the tool returns a fallback/failed result instead of completion. "
            "Final completion must call team_orchestrator.report_result exactly once with a short summary "
            "and optional status/changes/tests; MCP fills schema_version, task_id, and agent_id."
        )
    perms = resolve_permissions(agent)
    if perms["has_prompt_only"]:
        prompt_only = [e["tool"] for e in perms["resolved_tools"] if e["enforcement"] == "prompt_only"]
        chunks.append(
            "Permission note: these tools are prompt-only for this provider and not hard-enforced: "
            + ", ".join(prompt_only)
        )
    return "\n\n".join(chunk for chunk in chunks if chunk)
