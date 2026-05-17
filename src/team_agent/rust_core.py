from __future__ import annotations

import json
import re
import shutil
import subprocess
from pathlib import Path
from typing import Any

from team_agent.paths import repo_root


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
    proc = subprocess.run(
        [
            "tmux",
            "list-panes",
            "-a",
            "-F",
            "#{pane_id}\t#{session_name}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_tty}\t#{pane_current_command}\t#{pane_active}",
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
        if len(parts) != 8:
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
        target["fingerprint"] = f"{target['session_name']}|{target['window_index']}|{target['pane_index']}|{target['pane_tty']}"
        targets.append(target)
    return {"ok": True, "targets": targets, "engine": "python_fallback", "fallback_reason": result.get("error")}


def contains_inline_secret(value: str) -> bool:
    lower = value.lower()
    return (
        "api_key" in lower
        or "apikey" in lower
        or "token" in lower
        or "secret" in lower
        or value.startswith("sk-")
        or _looks_base64_secret(value)
    )


def _looks_base64_secret(value: str) -> bool:
    return len(value) >= 32 and re.fullmatch(r"[A-Za-z0-9+/=_-]+", value) is not None
