from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from team_agent.provider_cli.adapter import (
    ProviderAdapter,
    agent_model,
    read_json_object,
)
from team_agent.provider_cli.prompt import compile_system_prompt


class GeminiCliAdapter(ProviderAdapter):
    provider = "gemini_cli"
    command_name = "gemini"

    def build_command(self, agent: dict[str, Any], workspace: Path, mcp_config: dict[str, Any]) -> list[str]:
        prompt = compile_system_prompt(agent)
        cmd = ["gemini"]
        if agent.get("_runtime", {}).get("dangerous_auto_approve"):
            cmd.extend(["--yolo", "--sandbox", "false"])
        model = agent_model(agent)
        if model:
            cmd.extend(["--model", model])
        if prompt:
            cmd.extend(["-i", prompt])
        return cmd

    def install_mcp(self, workspace: Path, agent_id: str, config: dict[str, Any]) -> Path:
        path = super().install_mcp(workspace, agent_id, config)
        self._register_mcp_servers(path, config)
        return path

    def cleanup_mcp(self, workspace: Path, agent_id: str, mcp_path: Path | None = None) -> None:
        path = mcp_path or workspace / ".team" / "runtime" / "mcp" / f"{agent_id}.json"
        self._restore_mcp_servers(path)

    def _register_mcp_servers(self, mcp_path: Path, config: dict[str, Any]) -> None:
        settings_path = Path.home() / ".gemini" / "settings.json"
        settings_path.parent.mkdir(parents=True, exist_ok=True)
        settings = read_json_object(settings_path)
        mcp_servers = settings.setdefault("mcpServers", {})
        if not isinstance(mcp_servers, dict):
            raise ValueError(f"{settings_path}: mcpServers must be an object")

        backup = {
            "settings_path": str(settings_path),
            "servers": {name: mcp_servers.get(name) for name in config},
        }
        gemini_backup_path(mcp_path).write_text(json.dumps(backup, indent=2), encoding="utf-8")

        for name, server in config.items():
            mcp_servers[name] = {
                "command": server["command"],
                "args": server.get("args", []),
                "env": server.get("env", {}),
            }
        settings_path.write_text(json.dumps(settings, indent=2), encoding="utf-8")

    def _restore_mcp_servers(self, mcp_path: Path) -> None:
        backup_path = gemini_backup_path(mcp_path)
        if not backup_path.exists():
            return
        backup = json.loads(backup_path.read_text(encoding="utf-8"))
        settings_path = Path(backup["settings_path"])
        settings = read_json_object(settings_path)
        mcp_servers = settings.setdefault("mcpServers", {})
        if not isinstance(mcp_servers, dict):
            raise ValueError(f"{settings_path}: mcpServers must be an object")
        for name, previous in backup.get("servers", {}).items():
            if previous is None:
                mcp_servers.pop(name, None)
            else:
                mcp_servers[name] = previous
        settings_path.write_text(json.dumps(settings, indent=2), encoding="utf-8")
        backup_path.unlink(missing_ok=True)

    def auth_hint(self) -> dict[str, Any]:
        if "GEMINI_API_KEY" in __import__("os").environ:
            return {"status": "present", "detail": "GEMINI_API_KEY is set"}
        if Path.home().joinpath(".gemini").exists():
            return {"status": "present", "detail": "~/.gemini exists; run gemini to verify OAuth"}
        return {"status": "missing_or_unknown", "detail": "run gemini OAuth setup or set GEMINI_API_KEY"}

    def status_patterns(self) -> dict[str, str]:
        return {"idle": r"\*\s+Type your message", "processing": r"\(esc to cancel", "error": "Error|APIError|Traceback"}

    def exit_text(self) -> str:
        return "\x04"


def gemini_backup_path(mcp_path: Path) -> Path:
    return mcp_path.with_suffix(".gemini-backup.json")
