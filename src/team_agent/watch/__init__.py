from __future__ import annotations

import json
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

from team_agent.message_store import MessageStore
from team_agent.paths import logs_dir, runtime_dir
from team_agent.status.queries import result_summary_from_row


@dataclass
class WatchCursor:
    event_offset: int = 0
    seen_result_ids: set[str] = field(default_factory=set)


def run_watch(
    workspace: Path,
    *,
    team: str | None = None,
    interval: float = 0.5,
    output: Callable[[str], None] = print,
    sleep: Callable[[float], None] = time.sleep,
) -> None:
    cursor = WatchCursor()
    while True:
        for line in collect_watch_lines(workspace, cursor, team=team):
            output(line)
        sleep(interval)


def collect_watch_lines(workspace: Path, cursor: WatchCursor, *, team: str | None = None) -> list[str]:
    lines = _collect_event_lines(workspace, cursor)
    lines.extend(_collect_result_lines(workspace, cursor, team=team))
    return lines


def render_event_line(event: dict[str, Any]) -> str | None:
    kind = event.get("event")
    if kind == "result_received":
        return _result_line(event.get("agent_id"), event.get("summary"))
    if kind in {"leader_receiver.injected", "leader_receiver.submitted"}:
        return f"leader_receiver.injected: {_message_snippet(event)} -> {_recipient(event)}"
    if kind == "send.failed":
        return f"send.failed: {_recipient(event)} reason={_clean(event.get('reason') or event.get('error') or '-')}"
    if kind == "leader_receiver.rebind_required":
        pane = event.get("old_pane_id") or event.get("pane_id") or event.get("target") or "-"
        reason = event.get("reason") or event.get("rediscovery_status") or "-"
        return f"leader_receiver.rebind_required: pane={pane} reason={_clean(reason)}"
    if kind == "leader.api_error":
        error_class = event.get("error_class") or "Unknown"
        provider = event.get("provider") or "-"
        snippet = _clean(event.get("matched_pattern_snippet") or event.get("snippet") or "-")
        return f"leader.api_error: {error_class} provider={provider} snippet={snippet}"
    return None


def _collect_event_lines(workspace: Path, cursor: WatchCursor) -> list[str]:
    path = logs_dir(workspace) / "events.jsonl"
    if not path.exists():
        return []
    size = path.stat().st_size
    if cursor.event_offset > size:
        cursor.event_offset = 0
    lines: list[str] = []
    with path.open("r", encoding="utf-8") as handle:
        handle.seek(cursor.event_offset)
        for raw in handle:
            try:
                event = json.loads(raw)
            except json.JSONDecodeError:
                continue
            rendered = render_event_line(event)
            if rendered:
                lines.append(rendered)
        cursor.event_offset = handle.tell()
    return lines


def _collect_result_lines(workspace: Path, cursor: WatchCursor, *, team: str | None = None) -> list[str]:
    if not (runtime_dir(workspace) / "team.db").exists():
        return []
    store = MessageStore(workspace)
    lines: list[str] = []
    for row in store.latest_results(limit=20, owner_team_id=team):
        result_id = str(row.get("result_id") or "")
        if not result_id or result_id in cursor.seen_result_ids:
            continue
        cursor.seen_result_ids.add(result_id)
        summary = result_summary_from_row(row) or {}
        lines.append(_result_line(summary.get("agent_id"), summary.get("summary")))
    return lines


def _result_line(agent_id: Any, summary: Any) -> str:
    return f"result_received: {agent_id or '-'} -> {_clean(summary or '-')[:80]}"


def _message_snippet(event: dict[str, Any]) -> str:
    message_id = str(event.get("message_id") or event.get("msg_id") or "-")
    return message_id[:12] if message_id != "-" else "-"


def _recipient(event: dict[str, Any]) -> str:
    return str(event.get("recipient") or event.get("to") or event.get("target") or "-")


def _clean(value: Any) -> str:
    return " ".join(str(value).split())


__all__ = ["WatchCursor", "collect_watch_lines", "render_event_line", "run_watch"]
