from __future__ import annotations

from typing import Any

CANONICAL_TOOLS = {
    "fs_read",
    "fs_write",
    "fs_list",
    "execute_bash",
    "git_diff",
    "network",
    "mcp_team",
    "provider_builtin",
}

ROLE_DEFAULTS = {
    "leader": ["fs_read", "fs_list", "mcp_team", "provider_builtin"],
    "supervisor": ["fs_read", "fs_list", "mcp_team", "provider_builtin"],
    "implementation_engineer": [
        "fs_read",
        "fs_write",
        "fs_list",
        "execute_bash",
        "git_diff",
        "mcp_team",
        "provider_builtin",
    ],
    "developer": [
        "fs_read",
        "fs_write",
        "fs_list",
        "execute_bash",
        "git_diff",
        "mcp_team",
        "provider_builtin",
    ],
    "researcher": ["fs_read", "fs_list", "network", "mcp_team", "provider_builtin"],
    "reviewer": ["fs_read", "fs_list", "git_diff", "mcp_team", "provider_builtin"],
    "code_reviewer": ["fs_read", "fs_list", "git_diff", "mcp_team", "provider_builtin"],
}

PROVIDER_ENFORCEMENT = {
    "claude_code": {
        "fs_read": "hard",
        "fs_write": "hard",
        "fs_list": "hard",
        "execute_bash": "hard",
        "git_diff": "hard",
        "network": "prompt_only",
        "mcp_team": "hard",
        "provider_builtin": "hard",
    },
    "codex": {tool: "prompt_only" for tool in CANONICAL_TOOLS},
    "gemini_cli": {
        "fs_read": "hard",
        "fs_write": "hard",
        "fs_list": "hard",
        "execute_bash": "hard",
        "git_diff": "hard",
        "network": "prompt_only",
        "mcp_team": "prompt_only",
        "provider_builtin": "hard",
    },
    "fake": {tool: "hard" for tool in CANONICAL_TOOLS},
}


def expand_tools(tools: list[str]) -> list[str]:
    expanded: list[str] = []
    for tool in tools:
        if tool == "fs_*":
            expanded.extend(["fs_read", "fs_write", "fs_list"])
        elif tool == "@builtin":
            expanded.append("provider_builtin")
        elif tool in {"@team-orchestrator", "@cao-mcp-server"}:
            expanded.append("mcp_team")
        elif tool == "*":
            expanded.extend(sorted(CANONICAL_TOOLS))
        else:
            expanded.append(tool)
    return sorted(set(expanded))


def default_tools_for_role(role: str) -> list[str]:
    return list(ROLE_DEFAULTS.get(role, ROLE_DEFAULTS["developer"]))


def resolve_permissions(agent: dict[str, Any]) -> dict[str, Any]:
    provider = agent["provider"]
    tools = agent.get("tools") or default_tools_for_role(agent.get("role", "developer"))
    resolved = expand_tools(tools)
    enforcement_map = PROVIDER_ENFORCEMENT.get(provider, {})
    entries = [
        {
            "tool": tool,
            "enforcement": enforcement_map.get(tool, "prompt_only"),
        }
        for tool in resolved
    ]
    return {
        "agent_id": agent.get("id"),
        "provider": provider,
        "tools": resolved,
        "resolved_tools": entries,
        "has_prompt_only": any(e["enforcement"] == "prompt_only" for e in entries),
    }


def task_required_tools(task: dict[str, Any]) -> list[str]:
    required = list(task.get("requires_tools", []))
    task_type = task.get("type")
    if task_type in {"implementation", "bug_fix", "test"}:
        required.extend(["fs_write", "execute_bash"])
    if task_type in {"review", "risk_check"}:
        required.extend(["fs_read", "git_diff"])
    if task_type in {"research", "architecture"}:
        required.extend(["fs_read"])
    return expand_tools(required)


def missing_tools(agent: dict[str, Any], task: dict[str, Any]) -> list[str]:
    allowed = set(resolve_permissions(agent)["tools"])
    return [tool for tool in task_required_tools(task) if tool not in allowed]
