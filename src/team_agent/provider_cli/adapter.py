from __future__ import annotations

import json
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.paths import repo_root


class ResumeUnavailable(RuntimeError):
    pass


class ProviderAdapter:
    provider = ""
    command_name = ""

    def is_installed(self) -> bool:
        return shutil.which(self.command_name) is not None

    def version(self) -> str | None:
        if not self.is_installed():
            return None
        for args in ([self.command_name, "--version"], [self.command_name, "version"]):
            try:
                proc = subprocess.run(args, text=True, capture_output=True, timeout=8, check=False)
            except (OSError, subprocess.TimeoutExpired):
                continue
            text = (proc.stdout or proc.stderr).strip()
            if text:
                return text.splitlines()[0]
        return "installed"

    def auth_hint(self) -> dict[str, Any]:
        return {"status": "unknown", "detail": "adapter cannot verify auth without starting CLI"}

    def build_command(self, agent: dict[str, Any], workspace: Path, mcp_config: dict[str, Any]) -> list[str]:
        raise NotImplementedError

    def capture_session_id(
        self,
        agent_id: str,
        spawn_context: dict[str, Any],
        timeout_s: float = 3.0,
    ) -> dict[str, Any] | None:
        _ = agent_id, spawn_context, timeout_s
        return None

    def build_resume_command(
        self,
        agent_state: dict[str, Any],
        workspace: Path,
        mcp_config: dict[str, Any] | None = None,
    ) -> list[str]:
        _ = workspace, mcp_config
        session_id = agent_state.get("session_id")
        if not session_id:
            raise ResumeUnavailable("session_id is required to resume")
        raise ResumeUnavailable(f"{self.provider} does not support resume")

    def supports_session_fork(self, agent: dict[str, Any] | None = None) -> bool:
        _ = agent
        return False

    def build_fork_command(
        self,
        agent: dict[str, Any],
        source_session_id: str,
        workspace: Path,
        mcp_config: dict[str, Any],
    ) -> list[str]:
        _ = agent, source_session_id, workspace, mcp_config
        raise ResumeUnavailable(f"{self.provider} does not support native session fork")

    def session_is_resumable(self, agent_state: dict[str, Any], workspace: Path) -> bool:
        _ = workspace
        return bool(agent_state.get("session_id"))

    def recover_session_id(
        self,
        agent_id: str,
        agent_state: dict[str, Any],
        workspace: Path,
        exclude_session_ids: set[str] | None = None,
    ) -> dict[str, Any] | None:
        _ = agent_id, agent_state, workspace, exclude_session_ids
        return None

    def mcp_config(self, workspace: Path, agent_id: str) -> dict[str, Any]:
        return {
            "team_orchestrator": {
                "type": "stdio",
                "command": sys.executable,
                "args": ["-m", "team_agent.mcp_server", "--workspace", str(workspace)],
                "env": {
                    "TEAM_AGENT_ID": agent_id,
                    "PYTHONPATH": str(repo_root() / "src"),
                },
            }
        }

    def install_mcp(self, workspace: Path, agent_id: str, config: dict[str, Any]) -> Path:
        path = workspace / ".team" / "runtime" / "mcp" / f"{agent_id}.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"mcpServers": config}, indent=2), encoding="utf-8")
        return path

    def cleanup_mcp(self, workspace: Path, agent_id: str, mcp_path: Path | None = None) -> None:
        return None

    def status_patterns(self) -> dict[str, str]:
        return {"idle": "", "processing": "", "error": "Error|Traceback|panic"}

    def exit_text(self) -> str:
        return "/exit"

    def handle_startup_prompts(
        self,
        session_name: str,
        window_name: str,
        checks: int = 30,
        sleep_s: float = 0.5,
    ) -> list[dict[str, Any]]:
        return []

    def handle_runtime_prompts(self, session_name: str, window_name: str) -> list[dict[str, Any]]:
        return []

    def validate_model(self, model: str | None) -> dict[str, Any]:
        return {"ok": True, "status": "not_checked", "provider": self.provider, "model": model}


def agent_model(agent: dict[str, Any]) -> str | None:
    if agent.get("model"):
        return str(agent["model"])
    profile_overrides = agent.get("_provider_profile", {}).get("command_overrides", {})
    if profile_overrides.get("model"):
        return str(profile_overrides["model"])
    return None


def read_json_object(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError(f"{path}: expected a JSON object")
    return data


def parse_time(value: Any) -> datetime | None:
    if isinstance(value, datetime):
        return value if value.tzinfo else value.replace(tzinfo=timezone.utc)
    if not value:
        return None
    text = str(value)
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    try:
        dt = datetime.fromisoformat(text)
    except ValueError:
        return None
    return dt if dt.tzinfo else dt.replace(tzinfo=timezone.utc)
