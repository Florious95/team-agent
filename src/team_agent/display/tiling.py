from __future__ import annotations

import shlex
from typing import Any, Callable


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


def tmux_stdout_last_line(stdout: str) -> str | None:
    lines = [line.strip() for line in stdout.splitlines() if line.strip()]
    return lines[-1] if lines else None


def tmux_attach_pane_command(linked_session: str) -> str:
    return f"TMUX= tmux attach-session -t {shlex.quote(linked_session)}"


def display_pane_title(agent: dict[str, Any]) -> str:
    return f"team-agent:{agent['id']}:{agent.get('role', '')}"


def set_tmux_display_pane_title(pane_id: str, title: str, reason: str) -> dict[str, Any]:
    from team_agent.runtime import run_cmd
    proc = run_cmd(["tmux", "select-pane", "-t", pane_id, "-T", title], timeout=10)
    if proc.returncode != 0:
        return {"ok": False, "reason": reason, "error": proc.stderr.strip()}
    return {"ok": True}


def prepare_tmux_attached_panes(
    host_session: str,
    linked_jobs: list[tuple[str, dict[str, Any], str]],
    *,
    window_name_for_index: Callable[[int], str],
    create_first_as_session: bool,
    panes_per_window: int = DISPLAY_PANES_PER_WINDOW,
    reason_map: dict[str, str] | None = None,
    stderr_reason_allowlist: set[str] | None = None,
    cleanup_session: str | None = None,
    enable_mouse: bool = False,
    select_first_window: bool = False,
) -> dict[str, Any]:
    from team_agent.runtime import run_cmd

    reasons = reason_map or {}

    def reason(key: str) -> str:
        return reasons.get(key, key)

    def fail(key: str, proc: Any | None = None, target: str | None = None) -> dict[str, Any]:
        if cleanup_session:
            run_cmd(["tmux", "kill-session", "-t", cleanup_session], timeout=10)
        result = {"ok": False, "reason": reason(key)}
        if proc is not None:
            detail = (proc.stderr or proc.stdout or "").strip()
            if stderr_reason_allowlist and detail in stderr_reason_allowlist:
                result["reason"] = detail
            result["error"] = detail
        if target:
            result["target"] = target
        return result

    panes: list[dict[str, Any]] = []
    for window_index, _base_window_name, window_jobs in grouped_display_jobs(linked_jobs, panes_per_window):
        window_name = window_name_for_index(window_index)
        first_agent_id, first_agent, first_linked_session = window_jobs[0]
        if create_first_as_session and window_index == 0:
            command = [
                "tmux", "new-session", "-d", "-P", "-F", "#{pane_id}",
                "-s", host_session, "-n", window_name, tmux_attach_pane_command(first_linked_session),
            ]
            fail_key = "create_session"
        else:
            command = [
                "tmux", "new-window", "-t", host_session, "-n", window_name,
                "-P", "-F", "#{pane_id}", tmux_attach_pane_command(first_linked_session),
            ]
            fail_key = "create_window"
        proc = run_cmd(command, timeout=10)
        if proc.returncode != 0:
            return fail(fail_key, proc, f"{host_session}:{window_name}")

        first_pane_id = tmux_stdout_last_line(proc.stdout) or f"{host_session}:{window_name}.0"
        title = display_pane_title(first_agent)
        title_result = set_tmux_display_pane_title(first_pane_id, title, reason("title"))
        if not title_result["ok"]:
            return fail(title_result["reason"], target=first_pane_id)
        panes.append(
            {
                "agent_id": first_agent_id,
                "pane_id": first_pane_id,
                "title": title,
                "linked_session": first_linked_session,
                "window_name": window_name,
            }
        )

        proc = run_cmd(["tmux", "set-window-option", "-t", f"{host_session}:{window_name}", "remain-on-exit", "on"], timeout=10)
        if proc.returncode != 0:
            return fail("remain", proc, f"{host_session}:{window_name}")

        for index, (agent_id, agent, linked_session) in enumerate(window_jobs[1:], start=1):
            proc = run_cmd(
                [
                    "tmux", "split-window", "-t", f"{host_session}:{window_name}",
                    "-h", "-P", "-F", "#{pane_id}", tmux_attach_pane_command(linked_session),
                ],
                timeout=10,
            )
            if proc.returncode != 0:
                return fail("split", proc, f"{host_session}:{window_name}")
            pane_id = tmux_stdout_last_line(proc.stdout) or f"{host_session}:{window_name}.{index}"
            title = display_pane_title(agent)
            title_result = set_tmux_display_pane_title(pane_id, title, reason("title"))
            if not title_result["ok"]:
                return fail(title_result["reason"], target=pane_id)
            panes.append(
                {
                    "agent_id": agent_id,
                    "pane_id": pane_id,
                    "title": title,
                    "linked_session": linked_session,
                    "window_name": window_name,
                }
            )

        proc = run_cmd(["tmux", "select-layout", "-t", f"{host_session}:{window_name}", "even-horizontal"], timeout=10)
        if proc.returncode != 0:
            return fail("layout", proc, f"{host_session}:{window_name}")

    if enable_mouse:
        proc = run_cmd(["tmux", "set-option", "-t", host_session, "mouse", "on"], timeout=10)
        if proc.returncode != 0:
            return fail("mouse", proc)
    if select_first_window:
        run_cmd(["tmux", "select-window", "-t", f"{host_session}:{window_name_for_index(0)}"], timeout=10)
    return {"ok": True, "host_session": host_session, "panes": panes}
