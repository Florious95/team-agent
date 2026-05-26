from __future__ import annotations

import json
import platform
import re
import shutil
import subprocess
from pathlib import Path
from typing import Any

from team_agent.paths import repo_root


_LEADER_ENV_KEYS = (
    "TEAM_AGENT_LEADER_SESSION_UUID",
    "TEAM_AGENT_LEADER_PANE_ID",
    "TEAM_AGENT_LEADER_PROVIDER",
    "TEAM_AGENT_MACHINE_FINGERPRINT",
    "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
)
_LEADER_SHAPED_COMMANDS = {"codex", "claude", "claude.exe", "node", "nodejs"}
_PANE_ENV_SCAN_TIMEOUT_SECONDS = 2.0
_run_subprocess = subprocess.run  # test-injectable indirection


def core_binary() -> Path | None:
    configured = shutil.which("team-agent-core")
    if configured:
        return Path(configured)
    local = repo_root() / "crates" / "team-agent-core" / "target" / "debug" / "team-agent-core"
    if local.exists():
        return local
    return None


def call_core(command: str, payload: dict[str, Any] | str | None = None) -> dict[str, Any]:
    binary = core_binary()
    if not binary:
        return {"ok": False, "error": "team-agent-core binary not found", "fallback": True}
    raw = json.dumps(payload, ensure_ascii=False) if isinstance(payload, dict) else (payload or "")
    proc = subprocess.run(
        [str(binary), command, "--json"],
        input=raw,
        text=True,
        capture_output=True,
        timeout=10,
        check=False,
    )
    try:
        result = json.loads(proc.stdout or "{}")
    except json.JSONDecodeError:
        result = {"ok": False, "error": proc.stdout.strip() or proc.stderr.strip()}
    if proc.returncode != 0:
        result.setdefault("ok", False)
        result.setdefault("error", proc.stderr.strip() or "team-agent-core failed")
    result["engine"] = "rust" if result.get("ok") else "rust_failed"
    return result


def render_message(payload: dict[str, Any]) -> dict[str, Any]:
    result = call_core("render-message", payload)
    if result.get("ok"):
        return result
    sender = payload.get("from") or payload.get("sender") or "unknown"
    task_id = payload.get("task_id")
    content = payload.get("content") or ""
    token = payload.get("message_id") or "missing"
    header = f"Team Agent message from {sender}"
    if task_id:
        header += f" for {task_id}"
    return {
        "ok": True,
        "text": f"{header}:\n\n{content}\n\n[team-agent-token:{token}]",
        "token": token,
        "engine": "python_fallback",
        "fallback_reason": result.get("error"),
    }


def redact_text(text: str) -> dict[str, Any]:
    result = call_core("redact", {"text": text})
    if result.get("ok"):
        return result
    redacted = []
    for chunk in text.split():
        lower = chunk.lower()
        if (
            "api_key" in lower
            or "apikey" in lower
            or "token=" in lower
            or "secret" in lower
            or lower == "bearer"
            or chunk.startswith("sk-")
            or _looks_base64_secret(chunk)
        ):
            redacted.append("[REDACTED]")
        else:
            redacted.append(chunk)
    return {"ok": True, "text": " ".join(redacted), "engine": "python_fallback", "fallback_reason": result.get("error")}


def validate_profile_metadata(profile: dict[str, Any]) -> dict[str, Any]:
    result = call_core("validate-profile", profile)
    if result.get("ok") or result.get("errors"):
        return result
    errors: list[str] = []
    if profile.get("auth_mode") not in {"subscription", "official_api", "compatible_api"}:
        errors.append("auth_mode must be subscription, official_api, or compatible_api")
    for field in ["provider", "model", "profile"]:
        if not profile.get(field):
            errors.append(f"{field} must not be empty")
        if contains_inline_secret(str(profile.get(field) or "")):
            errors.append("profile metadata contains a probable inline secret")
    return {"ok": not errors, "errors": errors, "engine": "python_fallback", "fallback_reason": result.get("error")}


def list_targets() -> dict[str, Any]:
    result = call_core("list-targets")
    if result.get("ok"):
        return result
    proc = _run_subprocess(
        [
            "tmux",
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{session_name}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_tty}\t#{pane_current_command}\t#{pane_active}\t#{pane_pid}",
        ],
        text=True,
        capture_output=True,
        timeout=5,
        check=False,
    )
    if proc.returncode != 0:
        return {"ok": False, "error": proc.stderr.strip() or "tmux list-panes failed", "engine": "python_fallback"}
    targets = []
    for line in proc.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) not in {8, 9}:
            continue
        target = {
            "pane_id": parts[0],
            "session_name": parts[1],
            "window_index": parts[2],
            "window_name": parts[3],
            "pane_index": parts[4],
            "pane_tty": parts[5],
            "pane_current_command": parts[6],
            "pane_active": parts[7] == "1",
        }
        pane_pid = parts[8].strip() if len(parts) == 9 else ""
        if pane_pid:
            target["pane_pid"] = pane_pid
        target["fingerprint"] = f"{target['session_name']}|{target['window_index']}|{target['pane_index']}|{target['pane_tty']}"
        _attach_leader_env(target)
        targets.append(target)
    return {"ok": True, "targets": targets, "engine": "python_fallback", "fallback_reason": result.get("error")}


def _attach_leader_env(target: dict[str, Any]) -> None:
    pane_pid = str(target.get("pane_pid") or "").strip()
    if not pane_pid:
        target["leader_env"] = None
        return
    env = _read_process_env(pane_pid)
    if env is None:
        target["leader_env"] = None
        return
    leader_env = {key: env[key] for key in _LEADER_ENV_KEYS if key in env}
    if "TEAM_AGENT_LEADER_SESSION_UUID" not in leader_env:
        for child_pid in _walk_leader_shaped_children(pane_pid):
            child_env = _read_process_env(child_pid)
            if child_env is None:
                continue
            for key in _LEADER_ENV_KEYS:
                if key not in leader_env and key in child_env:
                    leader_env[key] = child_env[key]
            if "TEAM_AGENT_LEADER_SESSION_UUID" in leader_env:
                break
    target["leader_env"] = leader_env
    uuid_value = leader_env.get("TEAM_AGENT_LEADER_SESSION_UUID")
    if uuid_value:
        target["leader_session_uuid"] = uuid_value


def _read_process_env(pid: str) -> dict[str, str] | None:
    if platform.system() == "Linux":
        return _read_proc_environ(pid)
    return _read_ps_eww_env(pid)


def _read_proc_environ(pid: str) -> dict[str, str] | None:
    path = Path(f"/proc/{pid}/environ")
    try:
        raw = path.read_bytes()
    except (FileNotFoundError, PermissionError, OSError):
        return None
    env: dict[str, str] = {}
    for token in raw.split(b"\x00"):
        if not token or b"=" not in token:
            continue
        try:
            text = token.decode("utf-8", errors="replace")
        except Exception:
            continue
        key, _, value = text.partition("=")
        env[key] = value
    return env


def _read_ps_eww_env(pid: str) -> dict[str, str] | None:
    try:
        proc = _run_subprocess(
            ["ps", "-E", "-ww", "-p", str(pid)],
            text=True,
            capture_output=True,
            timeout=_PANE_ENV_SCAN_TIMEOUT_SECONDS,
            check=False,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError):
        return None
    if proc.returncode != 0 or not proc.stdout:
        return None
    return _parse_ps_eww_output(proc.stdout, pid)


def _parse_ps_eww_output(text: str, pid: str) -> dict[str, str]:
    env: dict[str, str] = {}
    lines = text.splitlines()
    if len(lines) < 2:
        return env
    target_row = None
    for line in lines[1:]:
        stripped = line.lstrip()
        if stripped.split(" ", 1)[0] == str(pid):
            target_row = stripped
            break
    if target_row is None:
        # Spark MEDIUM #2 (da436a3): never fall back to lines[1] — that row may belong to
        # an unrelated process and would leak its env (incl. another team's
        # TEAM_AGENT_LEADER_SESSION_UUID) into this pane's leader_env, corrupting rediscovery.
        return env
    for token in target_row.split():
        if "=" not in token:
            continue
        key, _, value = token.partition("=")
        if not key or " " in key:
            continue
        if not (key[0].isalpha() or key[0] == "_"):
            continue
        if not all(ch.isalnum() or ch == "_" for ch in key):
            continue
        env[key] = value
    return env


def _walk_leader_shaped_children(parent_pid: str) -> list[str]:
    try:
        proc = _run_subprocess(
            ["ps", "-o", "pid=,ppid=,comm="],
            text=True,
            capture_output=True,
            timeout=_PANE_ENV_SCAN_TIMEOUT_SECONDS,
            check=False,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError):
        return []
    if proc.returncode != 0 or not proc.stdout:
        return []
    return _select_leader_shaped_descendants(proc.stdout, parent_pid)


def _select_leader_shaped_descendants(ps_output: str, parent_pid: str) -> list[str]:
    rows: list[tuple[str, str, str]] = []
    for line in ps_output.splitlines():
        parts = line.split()
        if len(parts) < 3:
            continue
        pid, ppid, command = parts[0], parts[1], " ".join(parts[2:])
        rows.append((pid, ppid, Path(command).name))
    descendants: set[str] = set()
    frontier = {str(parent_pid)}
    while frontier:
        next_frontier: set[str] = set()
        for pid, ppid, _ in rows:
            if ppid in frontier and pid not in descendants:
                descendants.add(pid)
                next_frontier.add(pid)
        frontier = next_frontier
    return [
        pid
        for pid, _, command in rows
        if pid in descendants and command in _LEADER_SHAPED_COMMANDS
    ]


def contains_inline_secret(value: str) -> bool:
    return (
        _contains_secret_assignment(value)
        or _contains_bearer_secret(value)
        or any(chunk.startswith("sk-") or _looks_base64_secret(chunk) for chunk in value.split())
        or value.startswith("sk-")
        or _looks_base64_secret(value)
    )


def _contains_secret_assignment(value: str) -> bool:
    for line in value.splitlines():
        for separator in ("=", ":"):
            if separator not in line:
                continue
            key, raw = line.split(separator, 1)
            normalized = re.sub(r"[^a-z0-9]", "", key.lower())
            if normalized not in {"apikey", "token", "secret", "password", "credential"}:
                continue
            candidate = raw.strip().strip("'\"")
            if candidate.startswith("sk-") or len(candidate) >= 8 or _looks_base64_secret(candidate):
                return True
    return False


def _contains_bearer_secret(value: str) -> bool:
    return re.search(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{16,}", value) is not None


def _looks_base64_secret(value: str) -> bool:
    return len(value) >= 32 and re.fullmatch(r"[A-Za-z0-9+/=_-]+", value) is not None
