from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.state import load_runtime_state
from team_agent.status.constants import APPROVAL_SCAN_LINES


def approvals(workspace: Path, agent_id: str | None = None) -> dict[str, Any]:
    from team_agent.runtime import RuntimeError, _extract_approval_prompt, _tmux_window_exists, run_cmd
    state = load_runtime_state(workspace)
    session_name = state.get("session_name")
    approvals_found: list[dict[str, Any]] = []
    agents = state.get("agents", {})
    target_ids = [agent_id] if agent_id else sorted(agents)
    for target_id in target_ids:
        agent = agents.get(target_id)
        if not agent:
            raise RuntimeError(f"unknown agent id: {target_id}")
        window = agent.get("window", target_id)
        if not session_name or not _tmux_window_exists(session_name, window):
            continue
        proc = run_cmd(["tmux", "capture-pane", "-p", "-S", f"-{APPROVAL_SCAN_LINES}", "-t", f"{session_name}:{window}"], timeout=5)
        if proc.returncode != 0:
            continue
        prompt = _extract_approval_prompt(target_id, proc.stdout)
        if prompt:
            approvals_found.append(prompt)
    return {
        "ok": True,
        "waiting": bool(approvals_found),
        "waiting_count": len(approvals_found),
        "approvals": approvals_found,
        "scan": {"mode": "tail", "lines": APPROVAL_SCAN_LINES, "raw_output": False},
    }


def format_approvals(workspace: Path, agent_id: str | None = None) -> str:
    result = approvals(workspace, agent_id=agent_id)
    if not result["approvals"]:
        return "No pending approvals."
    lines: list[str] = []
    for item in result["approvals"]:
        detail = item.get("tool") or item.get("command") or item.get("kind")
        lines.append(f"{item['agent_id']}: {item['state']} {item['kind']} {detail}".rstrip())
        if item.get("prompt"):
            lines.append(f"  prompt: {item['prompt']}")
        if item.get("choices"):
            lines.append("  choices: " + "; ".join(item["choices"]))
        lines.append("  raw terminal output omitted; use debug-only peek with --search/--tail/--head if the user explicitly asks.")
    return "\n".join(lines)
