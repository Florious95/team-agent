from __future__ import annotations

import json
import re
import subprocess
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from team_agent.permissions import resolve_permissions
from team_agent.provider_cli.adapter import (
    ProviderAdapter,
    ResumeUnavailable,
    agent_model,
    parse_time,
)
from team_agent.provider_cli.prompt import compile_system_prompt


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
        start = parse_time(spawn_context.get("spawn_time")) or datetime.now(timezone.utc)
        root = Path(spawn_context.get("sessions_root") or Path.home() / ".codex" / "sessions")
        deadline = time.monotonic() + max(timeout_s, 0.0)
        exclude = {str(item) for item in spawn_context.get("exclude_session_ids", []) if item}
        while True:
            match = find_codex_rollout(root, Path(str(cwd)), start, exclude_session_ids=exclude)
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
        model = agent_model(agent)
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


def find_codex_rollout(
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
        meta = read_codex_session_meta(path)
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
        ts = parse_time(meta.get("timestamp"))
        if ts and (ts < lower_bound or ts > upper_bound):
            continue
        originator = meta.get("originator")
        origin_ok = originator in {"codex-tui", "codex_exec"}
        session_id = meta.get("id") or rollout_id_from_name(path)
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


def read_codex_session_meta(path: Path) -> dict[str, Any] | None:
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


def rollout_id_from_name(path: Path) -> str | None:
    match = re.search(r"([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})\.jsonl$", path.name)
    return match.group(1) if match else None
