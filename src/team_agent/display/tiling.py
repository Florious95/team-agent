from __future__ import annotations

from typing import Any


DISPLAY_PANES_PER_WINDOW = 3


def display_window_name(index: int) -> str:
    return "overview" if index == 0 else f"overview-{index + 1}"


def grouped_display_jobs(
    jobs: list[tuple[str, dict[str, Any], str]],
    panes_per_window: int = DISPLAY_PANES_PER_WINDOW,
) -> list[tuple[int, str, list[tuple[str, dict[str, Any], str]]]]:
    groups: list[tuple[int, str, list[tuple[str, dict[str, Any], str]]]] = []
    for window_index, start in enumerate(range(0, len(jobs), panes_per_window)):
        groups.append((window_index, display_window_name(window_index), jobs[start : start + panes_per_window]))
    return groups


def team_scoped_display_window_name(session_name: str, index: int) -> str:
    return f"team-agent:{session_name}:{display_window_name(index)}"
