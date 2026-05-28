from __future__ import annotations

import argparse
import json
import sys
import time
import traceback
from pathlib import Path
from typing import Any


def emit(result: Any, as_json: bool) -> None:
    if as_json:
        print(json.dumps(result, indent=2, ensure_ascii=False, sort_keys=True))
        return
    if isinstance(result, dict):
        for key, value in result.items():
            if isinstance(value, (dict, list)):
                print(f"{key}: {json.dumps(value, ensure_ascii=False)}")
            else:
                print(f"{key}: {value}")
    else:
        print(result)


def consume_leader_inbox_summary(workspace: Path, *, budget: int = 500) -> str | None:
    runtime_dir = workspace / ".team" / "runtime"
    inbox_path = runtime_dir / "leader-inbox.log"
    cursor_path = runtime_dir / "leader-inbox.cursor"
    if not inbox_path.exists():
        return None
    try:
        size = inbox_path.stat().st_size
        offset = int(cursor_path.read_text(encoding="utf-8").strip() or "0") if cursor_path.exists() else 0
    except (OSError, ValueError):
        offset = 0
        size = 0
    if offset < 0 or offset > size:
        offset = 0
    if offset == size:
        return None
    try:
        with inbox_path.open("rb") as handle:
            handle.seek(offset)
            raw = handle.read()
            end = handle.tell()
    except OSError:
        return None
    text = raw.decode("utf-8", errors="replace")
    try:
        cursor_path.parent.mkdir(parents=True, exist_ok=True)
        cursor_path.write_text(str(end), encoding="utf-8")
    except OSError:
        pass
    return _leader_inbox_summary(text, budget=budget)


def _leader_inbox_summary(text: str, *, budget: int) -> str | None:
    entries = _leader_inbox_entries(text)
    if not entries:
        return None
    hint = "team-agent inbox leader"
    lines = [f"Leader inbox: {len(entries)} new fallback entr{'y' if len(entries) == 1 else 'ies'}"]
    truncated = False
    for entry in entries:
        item = _leader_inbox_entry_title(entry)[:80]
        candidate = "\n".join([*lines, f"- {item}", f"Hint: {hint}"])
        if len(candidate) > budget:
            truncated = True
            break
        lines.append(f"- {item}")
    footer = f"Hint: {hint}"
    if truncated or len(lines) - 1 < len(entries):
        footer = f"Truncated: more fallback entries available; run {hint}"
    summary = "\n".join([*lines, footer])
    if len(summary) > budget:
        keep = max(0, budget - len(footer) - 6)
        body = "\n".join(lines)[:keep].rstrip()
        summary = "\n".join([f"{body} ...", footer])
    return summary


def _leader_inbox_entry_title(entry: str) -> str:
    lines = [line.strip() for line in entry.splitlines() if line.strip()]
    content_lines: list[str] = []
    for line in lines:
        if line.startswith("[") and ("fallback" in line or "team-agent-token" in line):
            continue
        if line.startswith("Team Agent"):
            continue
        if line.startswith(("Message id:", "Task id:", "From:", "To:", "Requires ack:", "Artifacts:")):
            continue
        content_lines.append(line)
    if content_lines:
        return " ".join(" ".join(line.split()) for line in content_lines)
    return " ".join(entry.split())


def _leader_inbox_entries(text: str) -> list[str]:
    blocks: list[str] = []
    current: list[str] = []
    for line in text.splitlines():
        if line.startswith("[") and "fallback" in line and current:
            blocks.append("\n".join(current).strip())
            current = [line]
        elif line.strip():
            current.append(line)
    if current:
        blocks.append("\n".join(current).strip())
    if blocks:
        return blocks
    return [line.strip() for line in text.splitlines() if line.strip()]


def _workspace_from_args(args: argparse.Namespace) -> Path:
    return Path(getattr(args, "workspace", ".")).resolve()


def _emit_cli_error(exc: Exception, args: argparse.Namespace) -> None:
    workspace = _workspace_from_args(args)
    log_dir = workspace / ".team" / "logs"
    try:
        log_dir.mkdir(parents=True, exist_ok=True)
    except OSError:
        log_dir = Path.cwd()
    log_path = log_dir / f"cli-error-{int(time.time())}.log"
    log_path.write_text("".join(traceback.format_exception(type(exc), exc, exc.__traceback__)), encoding="utf-8")
    payload = _cli_error_payload(exc, args, log_path)
    if getattr(args, "json", False):
        print(json.dumps(payload, ensure_ascii=False))
        return
    print(f"error: {payload['error']}", file=sys.stderr)
    print(f"action: {payload['action']}", file=sys.stderr)
    print(f"log: {payload['log']}", file=sys.stderr)


def _cli_error_payload(exc: Exception, args: argparse.Namespace, log_path: Path) -> dict[str, Any]:
    error = str(exc)
    payload = {
        "ok": False,
        "error": error,
        "action": "run `team-agent doctor` or inspect the log path shown here",
        "log": str(log_path),
    }
    session_name = _tmux_session_conflict_name(error)
    if session_name:
        payload.update(
            {
                "reason": "tmux_session_name_conflict",
                "session_name": session_name,
                "action": _tmux_session_conflict_action(session_name, getattr(args, "command", "")),
                "next_actions": [_tmux_session_conflict_next_action(getattr(args, "command", ""))],
            }
        )
    return payload


def _tmux_session_conflict_name(error: str) -> str | None:
    marker = "tmux session already exists:"
    if marker not in error:
        return None
    name = error.split(marker, 1)[1].strip()
    name = name.split(";", 1)[0].splitlines()[0].strip()
    if ". Startup" in name:
        name = name.split(". Startup", 1)[0].strip()
    name = name.rstrip(".").strip()
    return name or None


def _tmux_session_conflict_next_action(command: str) -> str:
    if command == "quick-start":
        return "Change `name:` in TEAM.md and run `team-agent quick-start` again."
    return "Use a different team name or runtime.session_name before starting again."


def _tmux_session_conflict_action(session_name: str, command: str) -> str:
    if command == "quick-start":
        return (
            f"tmux session `{session_name}` already exists. It may be an active team. "
            "Do not terminate existing tmux sessions from quick-start; "
            "change `name:` in TEAM.md and run quick-start again."
        )
    return (
        f"tmux session `{session_name}` already exists. It may be an active team. "
        "Do not terminate existing tmux sessions from startup; "
        "use a different team name or runtime.session_name and start again."
    )


def _provider_args(values: list[str]) -> list[str]:
    if values and values[0] == "--":
        return values[1:]
    return values


def _leader_launcher_args(values: list[str]) -> dict[str, Any]:
    provider_args: list[str] = []
    attach_existing = False
    confirm_attach = False
    attach_session: str | None = None
    index = 0
    while index < len(values):
        value = values[index]
        if value == "--":
            provider_args.extend(values[index:])
            break
        if value in {"--attach", "--attach-existing"}:
            attach_existing = True
        elif value == "--confirm":
            confirm_attach = True
        elif value == "--attach-session":
            index += 1
            if index >= len(values):
                raise RuntimeError("--attach-session requires a tmux session name")
            attach_session = values[index]
        elif value.startswith("--attach-session="):
            attach_session = value.split("=", 1)[1]
        else:
            provider_args.append(value)
        index += 1
    return {
        "provider_args": provider_args,
        "attach_existing": attach_existing,
        "confirm_attach": confirm_attach,
        "attach_session": attach_session,
    }
