from __future__ import annotations

import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import time
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from team_agent.permissions import resolve_permissions
from team_agent.paths import repo_root
from team_agent.provider_cli.prompt import TEAMMATE_SYSTEM_PROMPT, compile_system_prompt
from team_agent.profiles import ensure_compatible_claude_mcp_config, prepare_agent_profile_launch


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


class ClaudeCodeAdapter(ProviderAdapter):
    provider = "claude_code"
    command_name = "claude"

    def build_command(self, agent: dict[str, Any], workspace: Path, mcp_config: dict[str, Any]) -> list[str]:
        session_id = agent.get("_session_id") or str(uuid.uuid4())
        agent["_session_id"] = session_id
        cmd = self._base_command(agent, mcp_config)
        cmd.extend(["--session-id", session_id])
        return cmd

    def build_resume_command(
        self,
        agent_state: dict[str, Any],
        workspace: Path,
        mcp_config: dict[str, Any] | None = None,
    ) -> list[str]:
        _ = workspace
        session_id = agent_state.get("session_id")
        if not session_id:
            raise ResumeUnavailable("claude resume requires session_id")
        if not self.session_is_resumable(agent_state, workspace):
            diagnostics = self.session_lookup_diagnostics(agent_state, workspace)
            raise ResumeUnavailable(
                "claude resume transcript not found "
                f"for session_id {session_id}; diagnostics={json.dumps(diagnostics, sort_keys=True)}"
            )
        agent = dict(agent_state.get("_agent_spec") or agent_state)
        cmd = self._base_command(agent, mcp_config or {})
        cmd.extend(["--resume", str(session_id)])
        return cmd

    def supports_session_fork(self, agent: dict[str, Any] | None = None) -> bool:
        return not agent or agent.get("auth_mode") != "compatible_api"

    def build_fork_command(
        self,
        agent: dict[str, Any],
        source_session_id: str,
        workspace: Path,
        mcp_config: dict[str, Any],
    ) -> list[str]:
        _ = workspace
        if not source_session_id:
            raise ResumeUnavailable("claude fork requires source session_id")
        session_id = agent.get("_session_id") or str(uuid.uuid4())
        agent["_session_id"] = session_id
        cmd = self._base_command(agent, mcp_config)
        cmd.extend(["--session-id", session_id, "--resume", str(source_session_id), "--fork-session"])
        return cmd

    def capture_session_id(
        self,
        agent_id: str,
        spawn_context: dict[str, Any],
        timeout_s: float = 3.0,
    ) -> dict[str, Any] | None:
        cwd = spawn_context.get("cwd")
        if not cwd:
            return None
        start = _parse_time(spawn_context.get("spawn_time")) or datetime.now(timezone.utc)
        root = Path(spawn_context.get("claude_projects_root") or Path.home() / ".claude" / "projects")
        deadline = time.monotonic() + max(timeout_s, 0.0)
        exclude = {str(item) for item in spawn_context.get("exclude_session_ids", []) if item}
        predetermined = spawn_context.get("predetermined_session_id")
        allow_older = bool(spawn_context.get("allow_older"))
        while True:
            match = _find_claude_transcript(
                root,
                Path(str(cwd)),
                start,
                agent_id=agent_id,
                predetermined_session_id=str(predetermined) if predetermined else None,
                exclude_session_ids=exclude,
                allow_older=allow_older,
            )
            if match:
                return {
                    "session_id": match["session_id"],
                    "rollout_path": match["rollout_path"],
                    "captured_at": datetime.now(timezone.utc).isoformat(),
                    "captured_via": match["captured_via"],
                    "attribution_confidence": match["confidence"],
                    "spawn_cwd": str(cwd),
                }
            if time.monotonic() >= deadline:
                return None
            time.sleep(0.2)

    def session_is_resumable(self, agent_state: dict[str, Any], workspace: Path) -> bool:
        session_id = agent_state.get("session_id")
        if not session_id:
            return False
        cwd = Path(str(agent_state.get("spawn_cwd") or workspace))
        root = Path(agent_state.get("claude_projects_root") or Path.home() / ".claude" / "projects")
        for path in _claude_transcript_paths(root, cwd, str(session_id)):
            meta = _read_claude_transcript_meta(path, cwd)
            if meta and meta.get("same_cwd") and meta.get("has_user_message"):
                return True
        return False

    def session_lookup_diagnostics(self, agent_state: dict[str, Any], workspace: Path) -> dict[str, Any]:
        session_id = str(agent_state.get("session_id") or "")
        cwd = Path(str(agent_state.get("spawn_cwd") or workspace))
        root = Path(agent_state.get("claude_projects_root") or Path.home() / ".claude" / "projects")
        paths = _claude_transcript_paths(root, cwd, session_id) if session_id else []
        return {
            "provider": self.provider,
            "expected_session_id": session_id,
            "spawn_cwd": str(cwd),
            "claude_projects_root": str(root),
            "encoded_dir_claude_actual": _claude_project_dir(root, cwd).name,
            "encoded_dir_team_agent_legacy": _claude_legacy_project_dir(root, cwd).name,
            "transcript_paths_checked": [str(path) for path in paths],
            "path_exists": {str(path): path.exists() for path in paths},
        }

    def recover_session_id(
        self,
        agent_id: str,
        agent_state: dict[str, Any],
        workspace: Path,
        exclude_session_ids: set[str] | None = None,
    ) -> dict[str, Any] | None:
        cwd = Path(str(agent_state.get("spawn_cwd") or workspace))
        root = Path(agent_state.get("claude_projects_root") or Path.home() / ".claude" / "projects")
        pending_session_id = agent_state.get("_pending_session_id")
        match = _find_claude_transcript(
            root,
            cwd,
            _parse_time(agent_state.get("spawned_at")) or datetime.fromtimestamp(0, timezone.utc),
            agent_id=agent_id,
            predetermined_session_id=str(pending_session_id) if pending_session_id else None,
            exclude_session_ids=exclude_session_ids or set(),
            allow_older=True,
            require_agent_match=True,
            require_cwd=True,
        )
        if not match:
            return None
        return {
            "session_id": match["session_id"],
            "rollout_path": match["rollout_path"],
            "captured_at": datetime.now(timezone.utc).isoformat(),
            "captured_via": "fs_repair",
            "attribution_confidence": match["confidence"],
            "spawn_cwd": str(cwd),
        }

    def _base_command(self, agent: dict[str, Any], mcp_config: dict[str, Any]) -> list[str]:
        prompt = compile_system_prompt(agent)
        cmd = ["claude"]
        if agent.get("_runtime", {}).get("dangerous_auto_approve"):
            cmd.append("--dangerously-skip-permissions")
        else:
            cmd.extend(["--permission-mode", "default"])
        model = _agent_model(agent)
        if model:
            cmd.extend(["--model", model])
        if prompt:
            cmd.extend(["--append-system-prompt", prompt])
        if mcp_config:
            managed_compatible_config = (
                agent.get("auth_mode") == "compatible_api"
                and bool(agent.get("_provider_profile", {}).get("claude_projects_root"))
            )
            if not managed_compatible_config:
                cmd.extend(["--mcp-config", json.dumps({"mcpServers": mcp_config})])
                cmd.append("--strict-mcp-config")
        allowed = set(resolve_permissions(agent)["tools"])
        disallowed = _claude_disallowed_tools(allowed)
        for tool in disallowed:
            cmd.extend(["--disallowedTools", tool])
        return cmd

    def auth_hint(self) -> dict[str, Any]:
        if not self.is_installed():
            return {"status": "missing", "detail": "claude command not found"}
        try:
            proc = subprocess.run(
                ["claude", "auth", "status"],
                text=True,
                capture_output=True,
                timeout=8,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            return {"status": "missing_or_unknown", "detail": f"claude auth status failed: {exc}"}
        text = (proc.stdout or proc.stderr).strip()
        try:
            status = json.loads(text) if text else {}
        except json.JSONDecodeError:
            status = {}
        if status.get("loggedIn") is True or proc.returncode == 0:
            method = status.get("authMethod") or "configured"
            return {"status": "present", "detail": f"claude auth status ok: {method}"}
        return {"status": "missing", "detail": text or "run claude auth login or claude setup-token"}

    def status_patterns(self) -> dict[str, str]:
        return {"idle": r"[>❯]\s", "processing": r"[✶✢✽✻✳·].*…", "error": "Error|Traceback"}

    def handle_startup_prompts(
        self,
        session_name: str,
        window_name: str,
        checks: int = 30,
        sleep_s: float = 0.5,
    ) -> list[dict[str, Any]]:
        handled: list[dict[str, Any]] = []
        target = f"{session_name}:{window_name}"
        for _ in range(max(checks, 0)):
            proc = subprocess.run(
                ["tmux", "capture-pane", "-p", "-S", "-", "-t", target],
                text=True,
                capture_output=True,
                timeout=5,
                check=False,
            )
            output = proc.stdout if proc.returncode == 0 else ""
            if "Quick safety check" in output or "Yes, I trust this folder" in output:
                subprocess.run(["tmux", "send-keys", "-t", target, "Enter"], check=False)
                handled.append({"prompt": "claude_workspace_trust", "action": "sent_enter"})
                break
            if "Claude Code" in output and ("❯" in output or ">" in output):
                break
            if sleep_s > 0:
                time.sleep(sleep_s)
        return handled


class CodexAdapter(ProviderAdapter):
    provider = "codex"
    command_name = "codex"
    _model_catalog_cache: dict[str, Any] | None = None

    def build_command(self, agent: dict[str, Any], workspace: Path, mcp_config: dict[str, Any]) -> list[str]:
        cmd = self._base_command(agent, mcp_config, resume=False)
        return cmd

    def build_resume_command(
        self,
        agent_state: dict[str, Any],
        workspace: Path,
        mcp_config: dict[str, Any] | None = None,
    ) -> list[str]:
        _ = workspace
        session_id = agent_state.get("session_id")
        if not session_id:
            raise ResumeUnavailable("codex resume requires session_id")
        agent = dict(agent_state.get("_agent_spec") or agent_state)
        cmd = self._base_command(agent, mcp_config or {}, resume=True)
        cmd.append(str(session_id))
        return cmd

    def supports_session_fork(self, agent: dict[str, Any] | None = None) -> bool:
        return not agent or agent.get("auth_mode") != "compatible_api"

    def build_fork_command(
        self,
        agent: dict[str, Any],
        source_session_id: str,
        workspace: Path,
        mcp_config: dict[str, Any],
    ) -> list[str]:
        _ = workspace
        if not source_session_id:
            raise ResumeUnavailable("codex fork requires source session_id")
        cmd = self._base_command(agent, mcp_config, resume=False, fork=True)
        cmd.append(str(source_session_id))
        return cmd

    def capture_session_id(
        self,
        agent_id: str,
        spawn_context: dict[str, Any],
        timeout_s: float = 3.0,
    ) -> dict[str, Any] | None:
        _ = agent_id
        cwd = spawn_context.get("cwd")
        if not cwd:
            return None
        start = _parse_time(spawn_context.get("spawn_time")) or datetime.now(timezone.utc)
        root = Path(spawn_context.get("sessions_root") or Path.home() / ".codex" / "sessions")
        deadline = time.monotonic() + max(timeout_s, 0.0)
        exclude = {str(item) for item in spawn_context.get("exclude_session_ids", []) if item}
        while True:
            match = _find_codex_rollout(root, Path(str(cwd)), start, exclude_session_ids=exclude)
            if match:
                return {
                    "session_id": match["session_id"],
                    "rollout_path": match["rollout_path"],
                    "captured_at": datetime.now(timezone.utc).isoformat(),
                    "captured_via": "fs_watch",
                    "attribution_confidence": match["confidence"],
                    "spawn_cwd": str(cwd),
                }
            if time.monotonic() >= deadline:
                return None
            time.sleep(0.2)

    def _base_command(
        self,
        agent: dict[str, Any],
        mcp_config: dict[str, Any],
        resume: bool,
        fork: bool = False,
    ) -> list[str]:
        prompt = compile_system_prompt(agent)
        cmd = ["codex"]
        if resume:
            cmd.append("resume")
        elif fork:
            cmd.append("fork")
        cmd.extend(["--no-alt-screen", "--disable", "shell_snapshot", "--disable", "apps"])
        profile_overrides = agent.get("_provider_profile", {}).get("command_overrides", {})
        if profile_overrides.get("codex_profile"):
            cmd.extend(["--profile", str(profile_overrides["codex_profile"])])
        if agent.get("_runtime", {}).get("dangerous_auto_approve"):
            cmd.append("--dangerously-bypass-approvals-and-sandbox")
        else:
            tools = set(resolve_permissions(agent)["tools"])
            sandbox = "workspace-write" if {"fs_write", "execute_bash"} & tools else "read-only"
            cmd.extend(["--sandbox", sandbox, "--ask-for-approval", "on-request"])
        model = _agent_model(agent)
        if model:
            cmd.extend(["--model", model])
        for config in profile_overrides.get("codex_config", []):
            cmd.extend(["-c", str(config)])
        if prompt:
            escaped = prompt.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")
            cmd.extend(["-c", f'developer_instructions="{escaped}"'])
        for server_name, cfg in mcp_config.items():
            prefix = f"mcp_servers.{server_name}"
            cmd.extend(["-c", f'{prefix}.command="{cfg["command"]}"'])
            args = "[" + ", ".join(json.dumps(str(arg)) for arg in cfg.get("args", [])) + "]"
            cmd.extend(["-c", f"{prefix}.args={args}"])
            for env_key, env_val in cfg.get("env", {}).items():
                cmd.extend(["-c", f'{prefix}.env.{env_key}="{env_val}"'])
            cmd.extend(["-c", f"{prefix}.tool_timeout_sec=600.0"])
        return cmd

    def auth_hint(self) -> dict[str, Any]:
        if "OPENAI_API_KEY" in __import__("os").environ:
            return {"status": "present", "detail": "OPENAI_API_KEY is set"}
        if Path.home().joinpath(".codex").exists():
            return {"status": "present", "detail": "~/.codex exists; run codex login if startup fails"}
        return {"status": "missing_or_unknown", "detail": "run codex login or set OPENAI_API_KEY"}

    def status_patterns(self) -> dict[str, str]:
        return {"idle": r"(›|❯|codex>)", "processing": r"•.*esc to interrupt", "error": "Error|Traceback|panic"}

    def handle_startup_prompts(
        self,
        session_name: str,
        window_name: str,
        checks: int = 30,
        sleep_s: float = 0.5,
    ) -> list[dict[str, Any]]:
        handled: list[dict[str, Any]] = []
        target = f"{session_name}:{window_name}"
        for _ in range(max(checks, 0)):
            proc = subprocess.run(
                ["tmux", "capture-pane", "-p", "-S", "-", "-t", target],
                text=True,
                capture_output=True,
                timeout=5,
                check=False,
            )
            output = proc.stdout if proc.returncode == 0 else ""
            trust_pos = max(
                output.rfind("Do you trust the contents of this directory?"),
                output.rfind("Do you trust the files in this folder?"),
                output.rfind("Do you trust this folder?"),
            )
            update_pos = max(output.rfind("Update available!"), output.rfind("Update now"))
            ready_pos = max(output.rfind("OpenAI Codex"), output.rfind("›"), output.rfind("codex>"))
            if update_pos >= 0 and update_pos > ready_pos:
                subprocess.run(["tmux", "send-keys", "-t", target, "Down", "Enter"], check=False)
                handled.append({"prompt": "codex_update_available", "action": "sent_skip"})
                if sleep_s > 0:
                    time.sleep(sleep_s)
                continue
            if trust_pos >= 0 and trust_pos > ready_pos:
                subprocess.run(["tmux", "send-keys", "-t", target, "Enter"], check=False)
                handled.append({"prompt": "codex_workspace_trust", "action": "sent_enter"})
                if sleep_s > 0:
                    time.sleep(sleep_s)
                continue
            if ready_pos >= 0:
                break
            if sleep_s > 0:
                time.sleep(sleep_s)
        return handled

    def handle_runtime_prompts(self, session_name: str, window_name: str) -> list[dict[str, Any]]:
        _ = session_name, window_name
        return []

    def validate_model(self, model: str | None) -> dict[str, Any]:
        if not model:
            return {"ok": True, "status": "model_not_set", "provider": self.provider, "model": model}
        catalog = self._model_catalog()
        if not catalog.get("ok"):
            details = {key: value for key, value in catalog.items() if key != "ok"}
            return {"ok": False, "status": "model_catalog_unavailable", "provider": self.provider, "model": model, **details}
        models = catalog.get("models", [])
        slugs = {str(item.get("slug") or "") for item in models if item.get("slug")}
        if model in slugs:
            return {"ok": True, "status": "model_supported", "provider": self.provider, "model": model}
        slug_by_lower = {slug.lower(): slug for slug in slugs}
        display_to_slug = {
            str(item.get("display_name") or "").lower(): str(item.get("slug"))
            for item in models
            if item.get("display_name") and item.get("slug")
        }
        normalized = model.lower()
        suggested = slug_by_lower.get(normalized) or display_to_slug.get(normalized)
        result = {
            "ok": False,
            "status": "unsupported_model",
            "reason": "model_id_not_found",
            "provider": self.provider,
            "model": model,
            "available_models": sorted(slugs),
        }
        if suggested:
            result["reason"] = "model_id_not_exact"
            result["suggested_model"] = suggested
        return result

    def _model_catalog(self) -> dict[str, Any]:
        if self._model_catalog_cache is not None:
            return self._model_catalog_cache
        if not self.is_installed():
            return {"ok": False, "reason": "codex_command_missing", "command": self.command_name}
        try:
            proc = subprocess.run(
                [self.command_name, "debug", "models"],
                text=True,
                capture_output=True,
                timeout=12,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            return {"ok": False, "reason": "model_catalog_command_failed", "command": "codex debug models", "error": str(exc)}
        if proc.returncode != 0:
            return {
                "ok": False,
                "reason": "model_catalog_command_failed",
                "command": "codex debug models",
                "stderr": proc.stderr.strip(),
            }
        try:
            data = json.loads(proc.stdout or "{}")
        except json.JSONDecodeError as exc:
            return {"ok": False, "reason": "model_catalog_parse_failed", "command": "codex debug models", "error": str(exc)}
        models = data.get("models")
        if not isinstance(models, list):
            return {"ok": False, "reason": "model_catalog_shape_invalid", "command": "codex debug models"}
        self._model_catalog_cache = {"ok": True, "command": "codex debug models", "models": models}
        return self._model_catalog_cache


class GeminiCliAdapter(ProviderAdapter):
    provider = "gemini_cli"
    command_name = "gemini"

    def build_command(self, agent: dict[str, Any], workspace: Path, mcp_config: dict[str, Any]) -> list[str]:
        prompt = compile_system_prompt(agent)
        cmd = ["gemini"]
        if agent.get("_runtime", {}).get("dangerous_auto_approve"):
            cmd.extend(["--yolo", "--sandbox", "false"])
        model = _agent_model(agent)
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
        settings = _read_json_object(settings_path)
        mcp_servers = settings.setdefault("mcpServers", {})
        if not isinstance(mcp_servers, dict):
            raise ValueError(f"{settings_path}: mcpServers must be an object")

        backup = {
            "settings_path": str(settings_path),
            "servers": {name: mcp_servers.get(name) for name in config},
        }
        _gemini_backup_path(mcp_path).write_text(json.dumps(backup, indent=2), encoding="utf-8")

        for name, server in config.items():
            mcp_servers[name] = {
                "command": server["command"],
                "args": server.get("args", []),
                "env": server.get("env", {}),
            }
        settings_path.write_text(json.dumps(settings, indent=2), encoding="utf-8")

    def _restore_mcp_servers(self, mcp_path: Path) -> None:
        backup_path = _gemini_backup_path(mcp_path)
        if not backup_path.exists():
            return
        backup = json.loads(backup_path.read_text(encoding="utf-8"))
        settings_path = Path(backup["settings_path"])
        settings = _read_json_object(settings_path)
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


ADAPTERS: dict[str, ProviderAdapter] = {
    "claude": ClaudeCodeAdapter(),
    "claude_code": ClaudeCodeAdapter(),
    "codex": CodexAdapter(),
    "gemini_cli": GeminiCliAdapter(),
    "fake": FakeAdapter(),
}

def get_adapter(provider: str) -> ProviderAdapter:
    try:
        return ADAPTERS[provider]
    except KeyError as exc:
        raise KeyError(f"Unsupported provider: {provider}") from exc


def shell_command_for_agent(agent: dict[str, Any], workspace: Path, mcp_config: dict[str, Any]) -> str:
    adapter = get_adapter(agent["provider"])
    command_agent = dict(agent)
    profile_launch = command_agent.get("_provider_profile") or prepare_agent_profile_launch(workspace, command_agent)
    if profile_launch:
        command_agent["_provider_profile"] = profile_launch
        agent["_provider_profile"] = profile_launch
    if (
        agent.get("provider") in {"claude", "claude_code"}
        and profile_launch
        and profile_launch.get("auth_mode") == "compatible_api"
        and profile_launch.get("claude_projects_root")
    ):
        ensure_compatible_claude_mcp_config(workspace, agent["id"], mcp_config)
    cmd = adapter.build_command(command_agent, workspace, mcp_config)
    if command_agent.get("_session_id"):
        agent["_session_id"] = command_agent["_session_id"]
    return shell_command(cmd, agent["id"], workspace, profile_launch)


def shell_resume_command_for_agent(
    agent: dict[str, Any],
    agent_state: dict[str, Any],
    workspace: Path,
    mcp_config: dict[str, Any],
) -> str:
    adapter = get_adapter(agent["provider"])
    command_agent = dict(agent)
    profile_launch = command_agent.get("_provider_profile") or prepare_agent_profile_launch(workspace, command_agent)
    if profile_launch:
        command_agent["_provider_profile"] = profile_launch
        agent["_provider_profile"] = profile_launch
    if (
        agent.get("provider") in {"claude", "claude_code"}
        and profile_launch
        and profile_launch.get("auth_mode") == "compatible_api"
        and profile_launch.get("claude_projects_root")
    ):
        ensure_compatible_claude_mcp_config(workspace, agent["id"], mcp_config)
    resume_state = dict(agent_state)
    resume_state["_agent_spec"] = command_agent
    cmd = adapter.build_resume_command(resume_state, workspace, mcp_config)
    return shell_command(cmd, agent["id"], workspace, profile_launch)


def shell_fork_command_for_agent(
    agent: dict[str, Any],
    source_session_id: str,
    workspace: Path,
    mcp_config: dict[str, Any],
) -> str:
    adapter = get_adapter(agent["provider"])
    command_agent = dict(agent)
    profile_launch = command_agent.get("_provider_profile") or prepare_agent_profile_launch(workspace, command_agent)
    if profile_launch:
        command_agent["_provider_profile"] = profile_launch
        agent["_provider_profile"] = profile_launch
    if (
        agent.get("provider") in {"claude", "claude_code"}
        and profile_launch
        and profile_launch.get("auth_mode") == "compatible_api"
        and profile_launch.get("claude_projects_root")
    ):
        ensure_compatible_claude_mcp_config(workspace, agent["id"], mcp_config)
    cmd = adapter.build_fork_command(command_agent, source_session_id, workspace, mcp_config)
    if command_agent.get("_session_id"):
        agent["_session_id"] = command_agent["_session_id"]
    return shell_command(cmd, agent["id"], workspace, profile_launch)


def shell_command(
    cmd: list[str],
    agent_id: str,
    workspace: Path,
    profile_launch: dict[str, Any] | None = None,
) -> str:
    env = {
        "TEAM_AGENT_ID": agent_id,
        "TEAM_AGENT_WORKSPACE": str(workspace),
        "PYTHONPATH": str(repo_root() / "src"),
    }
    if os.environ.get("PATH"):
        # tmux commands inherit the tmux server's old environment, not the
        # current Codex shell. Preserve PATH so local wrappers such as
        # ~/.local/bin/codex remain effective without logging proxy secrets.
        env["PATH"] = os.environ["PATH"]
    exports = " ".join(f"{key}={shlex.quote(value)}" for key, value in env.items())
    source_profile = ""
    env_file = profile_launch.get("env_file") if profile_launch else None
    if env_file:
        source_profile = f". {shlex.quote(str(env_file))} && "
    return f"cd {shlex.quote(str(workspace))} && export {exports} && {source_profile}exec {shlex.join(cmd)}"


def _agent_model(agent: dict[str, Any]) -> str | None:
    if agent.get("model"):
        return str(agent["model"])
    profile_overrides = agent.get("_provider_profile", {}).get("command_overrides", {})
    if profile_overrides.get("model"):
        return str(profile_overrides["model"])
    return None


def _claude_disallowed_tools(allowed: set[str]) -> list[str]:
    mapping = {
        "execute_bash": ["Bash"],
        "fs_read": ["Read"],
        "fs_write": ["Edit", "Write", "MultiEdit", "NotebookEdit"],
        "fs_list": ["Glob", "Grep"],
    }
    disallowed: list[str] = []
    for canonical, native in mapping.items():
        if canonical not in allowed:
            disallowed.extend(native)
    return disallowed


def _read_json_object(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {}
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError(f"{path}: expected a JSON object")
    return data


def _gemini_backup_path(mcp_path: Path) -> Path:
    return mcp_path.with_suffix(".gemini-backup.json")


def _find_claude_transcript(
    root: Path,
    cwd: Path,
    spawn_time: datetime,
    *,
    agent_id: str,
    predetermined_session_id: str | None,
    exclude_session_ids: set[str] | None = None,
    allow_older: bool = False,
    require_agent_match: bool = False,
    require_cwd: bool = True,
) -> dict[str, Any] | None:
    if not root.exists():
        return None
    exclude_session_ids = exclude_session_ids or set()
    if predetermined_session_id and predetermined_session_id not in exclude_session_ids:
        for path in _claude_transcript_paths(root, cwd, predetermined_session_id):
            meta = _read_claude_transcript_meta(path, cwd)
            if meta and (not require_cwd or meta.get("same_cwd")) and meta.get("has_user_message"):
                return {
                    "session_id": str(predetermined_session_id),
                    "rollout_path": str(path),
                    "timestamp": meta.get("timestamp") or datetime.fromtimestamp(path.stat().st_mtime, timezone.utc),
                    "captured_via": "fs_watch",
                    "confidence": "high",
                }
    lower_bound = spawn_time - timedelta(seconds=2)
    upper_bound = datetime.now(timezone.utc) + timedelta(seconds=5)
    candidates: list[dict[str, Any]] = []
    for directory in _claude_project_dirs(root, cwd):
        for path in sorted(directory.glob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True)[:300]:
            meta = _read_claude_transcript_meta(path, cwd)
            if not meta or not meta.get("has_user_message"):
                continue
            if require_cwd and not meta.get("same_cwd"):
                continue
            session_id = str(meta.get("session_id") or path.stem)
            if session_id in exclude_session_ids:
                continue
            ts = meta.get("timestamp") or datetime.fromtimestamp(path.stat().st_mtime, timezone.utc)
            if not allow_older and (ts < lower_bound or ts > upper_bound):
                continue
            text = str(meta.get("text") or "")
            score = _claude_agent_match_score(agent_id, text)
            if require_agent_match and score < 2:
                continue
            if score <= 0 and not allow_older:
                continue
            candidates.append(
                {
                    "session_id": session_id,
                    "rollout_path": str(path),
                    "timestamp": ts,
                    "captured_via": "fs_watch",
                    "confidence": "high" if score >= 2 else "medium",
                    "score": score,
                }
            )
    if not candidates:
        return None
    candidates.sort(key=lambda item: (item["score"], item["timestamp"]), reverse=True)
    return candidates[0]


def _claude_project_dirs(root: Path, cwd: Path) -> list[Path]:
    return [directory for directory in _unique_paths([_claude_project_dir(root, cwd), _claude_legacy_project_dir(root, cwd)]) if directory.exists()]


def _claude_project_dir(root: Path, cwd: Path) -> Path:
    try:
        cwd_text = str(cwd.resolve())
    except OSError:
        cwd_text = str(cwd)
    return root / re.sub(r"[^A-Za-z0-9.-]", "-", cwd_text)


def _claude_legacy_project_dir(root: Path, cwd: Path) -> Path:
    try:
        cwd_text = str(cwd.resolve())
    except OSError:
        cwd_text = str(cwd)
    return root / re.sub(r"[^A-Za-z0-9._-]", "-", cwd_text)


def _claude_transcript_path(root: Path, cwd: Path, session_id: str) -> Path:
    return _claude_project_dir(root, cwd) / f"{session_id}.jsonl"


def _claude_transcript_paths(root: Path, cwd: Path, session_id: str) -> list[Path]:
    if not session_id:
        return []
    return _unique_paths(
        [
            _claude_project_dir(root, cwd) / f"{session_id}.jsonl",
            _claude_legacy_project_dir(root, cwd) / f"{session_id}.jsonl",
        ]
    )


def _unique_paths(paths: list[Path]) -> list[Path]:
    seen: set[str] = set()
    result: list[Path] = []
    for path in paths:
        key = str(path)
        if key in seen:
            continue
        seen.add(key)
        result.append(path)
    return result


def _read_claude_transcript_meta(path: Path, cwd: Path | None = None) -> dict[str, Any] | None:
    if not path.exists():
        return None
    session_id: str | None = None
    transcript_cwd: str | None = None
    timestamp: datetime | None = None
    has_user_message = False
    text_parts: list[str] = []
    try:
        with path.open(encoding="utf-8") as handle:
            for index, line in enumerate(handle):
                if index >= 200:
                    break
                try:
                    data = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if not session_id and data.get("sessionId"):
                    session_id = str(data.get("sessionId"))
                if not transcript_cwd and data.get("cwd"):
                    transcript_cwd = str(data.get("cwd"))
                timestamp = timestamp or _parse_time(data.get("timestamp"))
                if data.get("type") == "user":
                    text = _claude_message_text(data.get("message", {}).get("content"))
                    if text.strip():
                        has_user_message = True
                        if sum(len(part) for part in text_parts) < 8000:
                            text_parts.append(text[:4000])
    except OSError:
        return None
    same_cwd = True
    if cwd is not None:
        same_cwd = _same_path(transcript_cwd, cwd)
    return {
        "session_id": session_id or path.stem,
        "cwd": transcript_cwd,
        "same_cwd": same_cwd,
        "timestamp": timestamp,
        "has_user_message": has_user_message,
        "text": "\n".join(text_parts),
    }


def _claude_message_text(content: Any) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts: list[str] = []
        for item in content:
            if isinstance(item, dict) and isinstance(item.get("text"), str):
                parts.append(item["text"])
            elif isinstance(item, dict) and isinstance(item.get("content"), str):
                parts.append(item["content"])
        return "\n".join(parts)
    return ""


def _claude_agent_match_score(agent_id: str, text: str) -> int:
    if not agent_id:
        return 0
    lowered = text.lower()
    agent = agent_id.lower()
    score = 0
    if f"agents/{agent}.md" in lowered or f"agents\\/{agent}.md" in lowered:
        score += 1
    if f"team_agent_id={agent}" in lowered or f"team_agent_id={agent_id}" in text:
        score += 2
    if f"your agent id: {agent}" in lowered:
        score += 2
    if f"team agent worker {agent}" in lowered or f"worker `{agent}`" in lowered:
        score += 2
    return score


def _same_path(value: str | None, path: Path) -> bool:
    if not value:
        return True
    try:
        return Path(value).resolve() == path.resolve()
    except OSError:
        return str(value) == str(path)


def _find_codex_rollout(
    root: Path,
    cwd: Path,
    spawn_time: datetime,
    exclude_session_ids: set[str] | None = None,
) -> dict[str, Any] | None:
    if not root.exists():
        return None
    exclude_session_ids = exclude_session_ids or set()
    lower_bound = spawn_time - timedelta(seconds=2)
    upper_bound = datetime.now(timezone.utc) + timedelta(seconds=5)
    candidates: list[dict[str, Any]] = []
    for path in sorted(root.glob("**/rollout-*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True)[:1500]:
        meta = _read_codex_session_meta(path)
        if not meta:
            continue
        meta_cwd = meta.get("cwd")
        if not meta_cwd:
            continue
        try:
            same_cwd = Path(str(meta_cwd)).resolve() == cwd.resolve()
        except OSError:
            same_cwd = str(meta_cwd) == str(cwd)
        if not same_cwd:
            continue
        ts = _parse_time(meta.get("timestamp"))
        if ts and (ts < lower_bound or ts > upper_bound):
            continue
        originator = meta.get("originator")
        origin_ok = originator in {"codex-tui", "codex_exec"}
        session_id = meta.get("id") or _rollout_id_from_name(path)
        if not session_id:
            continue
        if str(session_id) in exclude_session_ids:
            continue
        candidates.append(
            {
                "session_id": str(session_id),
                "rollout_path": str(path),
                "timestamp": ts or datetime.fromtimestamp(path.stat().st_mtime, timezone.utc),
                "confidence": "high" if origin_ok and ts else "medium",
            }
        )
    if not candidates:
        return None
    candidates.sort(key=lambda item: item["timestamp"])
    return candidates[0]


def _read_codex_session_meta(path: Path) -> dict[str, Any] | None:
    try:
        with path.open(encoding="utf-8") as handle:
            first = handle.readline()
        data = json.loads(first)
    except (OSError, json.JSONDecodeError):
        return None
    if "session_meta" in data:
        payload = data.get("session_meta", {}).get("payload")
    else:
        payload = data.get("payload")
    return payload if isinstance(payload, dict) else None


def _rollout_id_from_name(path: Path) -> str | None:
    match = re.search(r"([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})\.jsonl$", path.name)
    return match.group(1) if match else None


def _parse_time(value: Any) -> datetime | None:
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
