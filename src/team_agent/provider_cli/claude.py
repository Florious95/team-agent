from __future__ import annotations

import json
import re
import subprocess
import time
import uuid
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
        start = parse_time(spawn_context.get("spawn_time")) or datetime.now(timezone.utc)
        root = Path(spawn_context.get("claude_projects_root") or Path.home() / ".claude" / "projects")
        deadline = time.monotonic() + max(timeout_s, 0.0)
        exclude = {str(item) for item in spawn_context.get("exclude_session_ids", []) if item}
        predetermined = spawn_context.get("predetermined_session_id")
        allow_older = bool(spawn_context.get("allow_older"))
        while True:
            match = find_claude_transcript(
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
            if spawn_context.get("auth_mode") == "compatible_api":
                fallback = find_compatible_api_claude_transcript_fallback(root, Path(str(cwd)), start, agent_id)
                if fallback:
                    return fallback
            if time.monotonic() >= deadline:
                return None
            time.sleep(0.2)

    def session_is_resumable(self, agent_state: dict[str, Any], workspace: Path) -> bool:
        session_id = agent_state.get("session_id")
        if not session_id:
            return False
        cwd = Path(str(agent_state.get("spawn_cwd") or workspace))
        root = Path(agent_state.get("claude_projects_root") or Path.home() / ".claude" / "projects")
        for path in claude_transcript_paths(root, cwd, str(session_id)):
            meta = read_claude_transcript_meta(path, cwd)
            if meta and meta.get("same_cwd") and meta.get("has_user_message"):
                return True
        return False

    def session_lookup_diagnostics(self, agent_state: dict[str, Any], workspace: Path) -> dict[str, Any]:
        session_id = str(agent_state.get("session_id") or "")
        cwd = Path(str(agent_state.get("spawn_cwd") or workspace))
        root = Path(agent_state.get("claude_projects_root") or Path.home() / ".claude" / "projects")
        paths = claude_transcript_paths(root, cwd, session_id) if session_id else []
        return {
            "provider": self.provider,
            "expected_session_id": session_id,
            "spawn_cwd": str(cwd),
            "claude_projects_root": str(root),
            "encoded_dir_claude_actual": claude_project_dir(root, cwd).name,
            "encoded_dir_team_agent_legacy": claude_legacy_project_dir(root, cwd).name,
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
        match = find_claude_transcript(
            root,
            cwd,
            parse_time(agent_state.get("spawned_at")) or datetime.fromtimestamp(0, timezone.utc),
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
        model = agent_model(agent)
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
        disallowed = claude_disallowed_tools(allowed)
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


def claude_disallowed_tools(allowed: set[str]) -> list[str]:
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


def find_claude_transcript(
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
        for path in claude_transcript_paths(root, cwd, predetermined_session_id):
            meta = read_claude_transcript_meta(path, cwd)
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
    for directory in claude_project_dirs(root, cwd):
        for path in sorted(directory.glob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True)[:300]:
            meta = read_claude_transcript_meta(path, cwd)
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
            score = claude_agent_match_score(agent_id, text)
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


def find_compatible_api_claude_transcript_fallback(
    root: Path,
    cwd: Path,
    spawn_time: datetime,
    agent_id: str,
) -> dict[str, Any] | None:
    _ = agent_id
    if not root.exists():
        return None
    lower_bound = spawn_time - timedelta(seconds=5)
    upper_bound = datetime.now(timezone.utc)
    candidates: list[Path] = []
    for directory in claude_project_dirs(root, cwd):
        try:
            candidates.extend(path for path in directory.glob("*.jsonl") if path.is_file())
        except OSError:
            continue
    try:
        ordered = sorted(candidates, key=lambda p: p.stat().st_mtime, reverse=True)[:5]
    except OSError:
        return None
    for path in ordered:
        try:
            stat = path.stat()
        except OSError:
            continue
        if stat.st_size <= 0:
            continue
        timestamp = datetime.fromtimestamp(stat.st_mtime, timezone.utc)
        if timestamp < lower_bound or timestamp > upper_bound:
            continue
        return {
            "session_id": None,
            "rollout_path": str(path),
            "captured_at": datetime.now(timezone.utc).isoformat(),
            "captured_via": "fs_mtime_fallback",
            "attribution_confidence": "low",
            "spawn_cwd": str(cwd),
        }
    return None


def claude_project_dirs(root: Path, cwd: Path) -> list[Path]:
    return [directory for directory in _unique_paths([claude_project_dir(root, cwd), claude_legacy_project_dir(root, cwd)]) if directory.exists()]


def claude_project_dir(root: Path, cwd: Path) -> Path:
    try:
        cwd_text = str(cwd.resolve())
    except OSError:
        cwd_text = str(cwd)
    return root / re.sub(r"[^A-Za-z0-9.-]", "-", cwd_text)


def claude_legacy_project_dir(root: Path, cwd: Path) -> Path:
    try:
        cwd_text = str(cwd.resolve())
    except OSError:
        cwd_text = str(cwd)
    return root / re.sub(r"[^A-Za-z0-9._-]", "-", cwd_text)


def claude_transcript_path(root: Path, cwd: Path, session_id: str) -> Path:
    return claude_project_dir(root, cwd) / f"{session_id}.jsonl"


def claude_transcript_paths(root: Path, cwd: Path, session_id: str) -> list[Path]:
    if not session_id:
        return []
    return _unique_paths(
        [
            claude_project_dir(root, cwd) / f"{session_id}.jsonl",
            claude_legacy_project_dir(root, cwd) / f"{session_id}.jsonl",
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


def read_claude_transcript_meta(path: Path, cwd: Path | None = None) -> dict[str, Any] | None:
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
                timestamp = timestamp or parse_time(data.get("timestamp"))
                if data.get("type") == "user":
                    text = claude_message_text(data.get("message", {}).get("content"))
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


def claude_message_text(content: Any) -> str:
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


def claude_agent_match_score(agent_id: str, text: str) -> int:
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
