from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

from team_agent.provider_cli.adapter import ProviderAdapter


class FakeAdapter(ProviderAdapter):
    provider = "fake"
    command_name = sys.executable

    def build_command(self, agent: dict[str, Any], workspace: Path, mcp_config: dict[str, Any]) -> list[str]:
        return [
            sys.executable,
            "-m",
            "team_agent.fake_worker",
            "--workspace",
            str(workspace),
            "--agent-id",
            agent["id"],
        ]

    def build_resume_command(
        self,
        agent_state: dict[str, Any],
        workspace: Path,
        mcp_config: dict[str, Any] | None = None,
    ) -> list[str]:
        agent = dict(agent_state.get("_agent_spec") or agent_state)
        agent.setdefault("id", agent_state.get("agent_id") or agent_state.get("id"))
        return self.build_command(agent, workspace, mcp_config or {})

    def auth_hint(self) -> dict[str, Any]:
        return {"status": "present", "detail": "fake provider is local test worker"}

    def status_patterns(self) -> dict[str, str]:
        return {"idle": "TEAM_AGENT_FAKE_READY", "processing": "TEAM_AGENT_FAKE_WORKING", "error": "Traceback"}
