from __future__ import annotations

import os
import re
import subprocess
from typing import Any


DANGEROUS_LEADER_FLAGS = (
    ("claude", "--dangerously-skip-permissions"),
    ("claude", "--dangerously-skip-permission"),
    ("codex", "--dangerously-bypass-approvals-and-sandbox"),
)


def effective_runtime_config(runtime_cfg: dict[str, Any]) -> dict[str, Any]:
    # Route via the runtime alias surface so tests patching
    # team_agent.runtime._detect_inherited_dangerous_permissions still take effect.
    from team_agent.runtime import _detect_inherited_dangerous_permissions
    effective = dict(runtime_cfg)
    if effective.get("dangerous_auto_approve"):
        effective["dangerous_auto_approve_source"] = "runtime_config"
        effective["dangerous_auto_approve_inherited"] = False
        return effective
    inherited = _detect_inherited_dangerous_permissions()
    if not inherited.get("enabled"):
        effective["dangerous_auto_approve"] = False
        effective["dangerous_auto_approve_source"] = "disabled"
        effective["dangerous_auto_approve_inherited"] = False
        return effective
    effective["dangerous_auto_approve"] = True
    effective["dangerous_auto_approve_source"] = "leader_process"
    effective["dangerous_auto_approve_inherited"] = True
    effective["dangerous_auto_approve_provider"] = inherited.get("provider")
    effective["dangerous_auto_approve_flag"] = inherited.get("flag")
    return effective


def requires_direct_leader_receiver(spec: dict[str, Any], runtime_cfg: dict[str, Any]) -> bool:
    if runtime_cfg.get("require_leader_receiver") is not None:
        return bool(runtime_cfg.get("require_leader_receiver"))
    return any(agent.get("provider") != "fake" for agent in spec.get("agents", []))


def detect_inherited_dangerous_permissions() -> dict[str, Any]:
    # Route via runtime alias so existing patches of
    # team_agent.runtime._process_ancestry take effect at call time.
    from team_agent.runtime import _process_ancestry
    for proc in _process_ancestry(os.getpid()):
        command = str(proc.get("command") or "")
        for provider, flag in DANGEROUS_LEADER_FLAGS:
            if command_has_flag(command, flag):
                return {
                    "enabled": True,
                    "provider": provider,
                    "flag": flag,
                    "pid": proc.get("pid"),
                }
    return {"enabled": False}


def command_has_flag(command: str, flag: str) -> bool:
    return re.search(rf"(?<!\S){re.escape(flag)}(?!\S)", command) is not None


def process_ancestry(pid: int, max_depth: int = 12) -> list[dict[str, Any]]:
    ancestry: list[dict[str, Any]] = []
    current = pid
    seen: set[int] = set()
    for _ in range(max_depth):
        if current in seen or current <= 0:
            break
        seen.add(current)
        info = process_info(current)
        if not info:
            break
        ancestry.append(info)
        parent = info.get("ppid")
        if not isinstance(parent, int) or parent <= 1 or parent == current:
            break
        current = parent
    return ancestry


def process_info(pid: int) -> dict[str, Any] | None:
    try:
        proc = subprocess.run(
            ["ps", "-p", str(pid), "-o", "ppid=", "-o", "command="],
            text=True,
            capture_output=True,
            timeout=2,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    line = proc.stdout.strip()
    if not line:
        return None
    parts = line.split(None, 1)
    try:
        ppid = int(parts[0])
    except (IndexError, ValueError):
        return None
    return {"pid": pid, "ppid": ppid, "command": parts[1] if len(parts) > 1 else ""}
