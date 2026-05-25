from __future__ import annotations

from pathlib import Path
from typing import Any

from team_agent.state import load_runtime_state
from team_agent.status.constants import (
    PEEK_MAX_LINES,
    PEEK_MAX_MATCHES,
    PEEK_SEARCH_SCAN_LINES,
)


def peek(
    workspace: Path,
    agent_id: str,
    *,
    head: int | None = None,
    tail: int | None = None,
    search: str | None = None,
    context: int = 3,
) -> dict[str, Any]:
    from team_agent.runtime import RuntimeError, _tmux_window_exists, run_cmd
    modes = [head is not None, tail is not None, search is not None]
    if sum(modes) != 1:
        raise RuntimeError("peek requires exactly one of --head, --tail, or --search")
    if head is not None:
        validate_line_count("--head", head)
    if tail is not None:
        validate_line_count("--tail", tail)
    if search is not None and not search.strip():
        raise RuntimeError("--search must not be empty")
    if context < 0 or context > 10:
        raise RuntimeError("--context must be between 0 and 10")
    state = load_runtime_state(workspace)
    agent = state.get("agents", {}).get(agent_id)
    if not agent:
        raise RuntimeError(f"unknown agent id: {agent_id}")
    session_name = state.get("session_name")
    window = agent.get("window", agent_id)
    if not session_name or not _tmux_window_exists(session_name, window):
        raise RuntimeError(f"agent terminal is not available: {agent_id}")
    scan_lines = tail or PEEK_SEARCH_SCAN_LINES
    proc = run_cmd(["tmux", "capture-pane", "-p", "-S", f"-{scan_lines}", "-t", f"{session_name}:{window}"], timeout=5)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"capture failed for {agent_id}")
    captured = proc.stdout.splitlines()
    if head is not None:
        selected = captured[:head]
        return {
            "ok": True,
            "agent_id": agent_id,
            "mode": "head",
            "lines": head,
            "scanned_lines": scan_lines,
            "text": "\n".join(selected),
        }
    if tail is not None:
        return {
            "ok": True,
            "agent_id": agent_id,
            "mode": "tail",
            "lines": tail,
            "scanned_lines": scan_lines,
            "text": "\n".join(captured[-tail:]),
        }
    assert search is not None
    matches = search_lines(captured, search, context)
    return {
        "ok": True,
        "agent_id": agent_id,
        "mode": "search",
        "search": search,
        "context": context,
        "scanned_lines": scan_lines,
        "matches": matches,
        "truncated": len(matches) >= PEEK_MAX_MATCHES,
        "text": format_search_matches(matches),
    }


def validate_line_count(flag: str, value: int) -> None:
    from team_agent.runtime import RuntimeError
    if value < 1 or value > PEEK_MAX_LINES:
        raise RuntimeError(f"{flag} must be between 1 and {PEEK_MAX_LINES}")


def search_lines(lines: list[str], needle: str, context: int) -> list[dict[str, Any]]:
    needle_lower = needle.lower()
    matches: list[dict[str, Any]] = []
    used_ranges: list[tuple[int, int]] = []
    for index, line in enumerate(lines):
        if needle_lower not in line.lower():
            continue
        start = max(0, index - context)
        end = min(len(lines), index + context + 1)
        if used_ranges and start <= used_ranges[-1][1]:
            previous = matches[-1]
            previous["lines"] = lines[previous["start_line"] - 1 : end]
            previous["end_line"] = end
            used_ranges[-1] = (previous["start_line"] - 1, end)
        else:
            matches.append({"line": index + 1, "start_line": start + 1, "end_line": end, "lines": lines[start:end]})
            used_ranges.append((start, end))
        if len(matches) >= PEEK_MAX_MATCHES:
            break
    return matches


def format_search_matches(matches: list[dict[str, Any]]) -> str:
    if not matches:
        return "no matches"
    blocks: list[str] = []
    for match in matches:
        blocks.append(f"match line {match['line']} ({match['start_line']}-{match['end_line']}):")
        blocks.extend(str(line) for line in match["lines"])
    return "\n".join(blocks)
