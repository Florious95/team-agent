from __future__ import annotations


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
        "name": "stop_agent",
        "description": "Hard-stop one running worker while preserving its session for start_agent resume.",
        "inputSchema": {
            "type": "object",
            "required": ["agent_id"],
            "properties": {"agent_id": {"type": "string"}},
            "additionalProperties": False,
        },
    },
    {
        "name": "reset_agent",
        "description": "Reset one worker to a fresh session. discard_session must be true.",
        "inputSchema": {
            "type": "object",
            "required": ["agent_id", "discard_session"],
            "properties": {
                "agent_id": {"type": "string"},
                "discard_session": {"type": "boolean"},
            },
            "additionalProperties": False,
        },
    },
    {
        "name": "add_agent",
        "description": "Add a first-class worker from a workspace-relative role file.",
        "inputSchema": {
            "type": "object",
            "required": ["new_agent_id", "role_file_path"],
            "properties": {
                "new_agent_id": {"type": "string"},
                "role_file_path": {"type": "string"},
            },
            "additionalProperties": False,
        },
    },
    {
        "name": "fork_agent",
        "description": "Fork a running worker using the provider's native branch/fork support.",
        "inputSchema": {
            "type": "object",
            "required": ["source_agent_id", "as_agent_id"],
            "properties": {
                "source_agent_id": {"type": "string"},
                "as_agent_id": {"type": "string"},
                "label": {"type": "string"},
            },
            "additionalProperties": False,
        },
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
