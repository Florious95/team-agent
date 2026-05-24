from __future__ import annotations

import hashlib
import json
import re
from typing import Any

from team_agent.approvals.constants import INTERNAL_MCP_APPROVAL_CHOICE


APPROVAL_CHOICE_RE = re.compile(r"(?:[›❯>]\s*)?(\d+)\.\s+(.+?)(?:\s{2,}.+)?$")


def capture_has_approval_prompt(text: str) -> bool:
    return extract_approval_prompt("_", text) is not None


def extract_approval_prompt(agent_id: str, text: str) -> dict[str, Any] | None:
    lines = text.splitlines()
    control_index = active_approval_control_index(lines)
    if control_index is None:
        return None
    for index in range(control_index, -1, -1):
        line = lines[index]
        if "Allow the team_orchestrator MCP server to run tool" not in line:
            continue
        tool_match = re.search(r'run tool "([^"]+)"', line)
        return {
            "agent_id": agent_id,
            "state": "waiting_approval",
            "kind": "mcp_tool",
            "tool": tool_match.group(1) if tool_match else None,
            "prompt": line.strip(),
            "choices": extract_approval_choices(lines[index : control_index + 1]),
        }
    for index in range(control_index, -1, -1):
        line = lines[index]
        if line_is_approval_choice(line):
            continue
        tool_match = re.search(r"\bteam_orchestrator\s*[-.]\s*([A-Za-z_][A-Za-z0-9_]*)\b", line)
        if not tool_match:
            continue
        return {
            "agent_id": agent_id,
            "state": "waiting_approval",
            "kind": "mcp_tool",
            "tool": tool_match.group(1),
            "prompt": f"team_orchestrator - {tool_match.group(1)}",
            "choices": extract_approval_choices(lines[index : control_index + 1]),
        }
    for index in range(control_index, -1, -1):
        line = lines[index]
        if "Would you like to run the following command" not in line:
            continue
        return {
            "agent_id": agent_id,
            "state": "waiting_approval",
            "kind": "command",
            "command": extract_command_approval_subject(lines[: control_index + 1], index),
            "prompt": line.strip(),
            "choices": extract_approval_choices(lines[index : control_index + 1]),
        }
    return {
        "agent_id": agent_id,
        "state": "waiting_approval",
        "kind": "unknown",
        "prompt": "approval prompt detected",
        "choices": extract_approval_choices(lines[: control_index + 1]),
    }


def active_approval_control_index(lines: list[str]) -> int | None:
    control_indices = [
        index
        for index, line in enumerate(lines)
        if is_approval_control_line(line)
    ]
    if not control_indices:
        return None
    control_index = control_indices[-1]
    if any(line.strip() for line in lines[control_index + 1 :]):
        return None
    return control_index


def is_approval_control_line(line: str) -> bool:
    normalized = line.lower()
    return "enter to submit | esc to cancel" in normalized or ("esc to cancel" in normalized and "tab to amend" in normalized)


def extract_approval_choices(lines: list[str]) -> list[str]:
    choices: list[str] = []
    for line in lines:
        stripped = line.strip()
        match = APPROVAL_CHOICE_RE.match(stripped)
        if not match:
            continue
        label = match.group(2).strip()
        if label and label not in choices:
            choices.append(label)
    return choices


def line_is_approval_choice(line: str) -> bool:
    return APPROVAL_CHOICE_RE.match(line.strip()) is not None


def extract_command_approval_subject(lines: list[str], prompt_index: int) -> str | None:
    for line in reversed(lines[:prompt_index]):
        stripped = line.strip()
        if stripped.startswith("Bash(") or stripped.startswith("Shell("):
            return stripped[:200]
    for line in lines[prompt_index + 1 : prompt_index + 8]:
        stripped = line.strip()
        if stripped.startswith("Bash(") or stripped.startswith("Shell("):
            return stripped[:200]
    return None


def active_approval_choice_index(text: str) -> int | None:
    for line in text.splitlines():
        stripped = line.strip()
        if not (stripped.startswith("›") or stripped.startswith("❯") or stripped.startswith(">")):
            continue
        match = re.match(r"[›❯>]\s*(\d+)\.", stripped)
        if match:
            return int(match.group(1)) - 1
    return None


def capture_has_team_orchestrator_mcp_prompt(text: str) -> bool:
    return (
        "Allow the team_orchestrator MCP server to run tool" in text
        or re.search(r"\bteam_orchestrator\s*[-.]\s*[A-Za-z_][A-Za-z0-9_]*\b", text) is not None
    )


def approval_prompt_fingerprint(prompt: dict[str, Any]) -> str:
    data = {
        "kind": prompt.get("kind"),
        "tool": prompt.get("tool"),
        "prompt": prompt.get("prompt"),
        "choices": prompt.get("choices") or [],
    }
    return hashlib.sha256(json.dumps(data, sort_keys=True, ensure_ascii=False).encode("utf-8")).hexdigest()[:16]


def choose_internal_mcp_approval_choice(prompt: dict[str, Any]) -> str:
    choices = prompt.get("choices") or []
    if INTERNAL_MCP_APPROVAL_CHOICE in choices:
        return INTERNAL_MCP_APPROVAL_CHOICE
    for choice in choices:
        if str(choice).startswith("Yes, and don't ask again"):
            return str(choice)
    if "Allow" in choices:
        return "Allow"
    if "Yes" in choices:
        return "Yes"
    return INTERNAL_MCP_APPROVAL_CHOICE


def approval_choice_keys(prompt: dict[str, Any], capture_text: str, choice: str) -> list[str]:
    choices = prompt.get("choices") or []
    try:
        target_index = choices.index(choice)
    except ValueError:
        return ["Down", "Enter"]
    active_index = active_approval_choice_index(capture_text)
    if active_index is None:
        return [str(target_index + 1), "Enter"]
    delta = target_index - active_index
    if delta > 0:
        return ["Down"] * delta + ["Enter"]
    if delta < 0:
        return ["Up"] * abs(delta) + ["Enter"]
    return ["Enter"]
