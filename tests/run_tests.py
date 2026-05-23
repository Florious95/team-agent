from __future__ import annotations

import json
import copy
import fcntl
import os
import shlex
import shutil
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from unittest.mock import Mock, patch

from team_agent import cli, runtime
from team_agent.compiler import compile_team
from team_agent.errors import RuntimeError as TeamAgentRuntimeError
from team_agent.message_store import MessageStore
from team_agent.mcp_server import TeamOrchestratorTools
from team_agent.permissions import missing_tools, resolve_permissions
from team_agent.providers import ResumeUnavailable, compile_system_prompt, get_adapter
from team_agent.profiles import doctor_profile, init_profile, prepare_agent_profile_launch, show_profile
from team_agent.routing import route_task
from team_agent.rust_core import render_message as core_render_message
from team_agent.simple_yaml import dumps
from team_agent.spec import ValidationError, load_spec, validate_result_envelope, validate_spec
from team_agent.state import load_runtime_state, save_runtime_state


ROOT = Path(__file__).resolve().parents[1]










def _fake_spec(workspace: Path) -> dict:
    from team_agent.cli import _fake_spec as cli_fake_spec

    spec = cli_fake_spec(workspace)
    spec["runtime"]["session_name"] = "team-agent-test"
    return spec


def _fake_spec_with_agents(workspace: Path, count: int) -> dict:
    spec = _fake_spec(workspace)
    base = copy.deepcopy(spec["agents"][0])
    ids = ["fake_impl", "fake_peer", "fake_review", "fake_test", "fake_ops", "fake_docs", "fake_qa", "fake_design"]
    agents = []
    for index, agent_id in enumerate(ids[:count]):
        agent = copy.deepcopy(base)
        agent["id"] = agent_id
        agent["role"] = f"Worker {index + 1}"
        agents.append(agent)
    spec["agents"] = agents
    spec["runtime"]["max_active_agents"] = count
    spec["runtime"]["startup_order"] = [agent["id"] for agent in agents]
    spec["routing"]["default_assignee"] = agents[0]["id"]
    return spec


def _make_fake_tmux_window_run_cmd(started_windows: set[str]):
    def fake_run_cmd(args: list[str], timeout: int = 20):
        proc = Mock(returncode=1 if args[:2] == ["tmux", "has-session"] else 0, stdout="", stderr="")
        if args[:3] == ["tmux", "new-session", "-d"]:
            started_windows.add(args[6])
        elif args[:2] == ["tmux", "new-window"]:
            started_windows.add(args[5])
        elif args[:3] == ["tmux", "list-windows", "-t"]:
            proc.stdout = "\n".join(sorted(started_windows))
        return proc

    return fake_run_cmd


def _make_fake_ghostty_workspace_run_cmd(
    started_windows: set[str],
    calls: list[list[str]],
    existing_sessions: set[str] | None = None,
    fail_select_windows: set[str] | None = None,
):
    sessions = set(existing_sessions or set())
    fail_select_windows = fail_select_windows or set()
    pane_ids: list[str] = []

    def fake_run_cmd(args: list[str], timeout: int = 20):
        calls.append(args)
        proc = Mock(returncode=0, stdout="", stderr="")
        if args[:2] == ["tmux", "has-session"]:
            proc.returncode = 0 if args[-1] in sessions else 1
        elif args[:3] == ["tmux", "new-session", "-d"]:
            if "-P" in args:
                sessions.add(args[args.index("-s") + 1])
                pane_id = f"%{len(pane_ids) + 1}"
                pane_ids.append(pane_id)
                proc.stdout = pane_id + "\n"
            elif "-t" in args:
                sessions.add(args[args.index("-s") + 1])
            else:
                sessions.add(args[4])
                started_windows.add(args[6])
        elif args[:2] == ["tmux", "new-window"]:
            if "-P" in args:
                pane_id = f"%{len(pane_ids) + 1}"
                pane_ids.append(pane_id)
                proc.stdout = pane_id + "\n"
            else:
                started_windows.add(args[5])
        elif args[:3] == ["tmux", "list-windows", "-t"]:
            proc.returncode = 0 if args[3] in sessions else 1
            proc.stdout = "\n".join(sorted(started_windows)) if proc.returncode == 0 else ""
        elif args[:2] == ["tmux", "split-window"]:
            pane_id = f"%{len(pane_ids) + 1}"
            pane_ids.append(pane_id)
            proc.stdout = pane_id + "\n"
        elif args[:3] == ["tmux", "select-window", "-t"]:
            window = args[3].rsplit(":", 1)[-1]
            if window in fail_select_windows:
                proc.returncode = 1
                proc.stderr = f"can't find window: {window}"
        elif args[:3] == ["tmux", "kill-session", "-t"]:
            sessions.discard(args[3])
        elif args[:2] == ["pgrep", "-f"]:
            proc.stdout = "4321\n"
        return proc

    return fake_run_cmd


def _write_doc_team(workspace: Path) -> Path:
    team = workspace / ".team" / "current"
    (team / "agents").mkdir(parents=True, exist_ok=True)
    (team / "profiles").mkdir(parents=True, exist_ok=True)
    (team / "TEAM.md").write_text(
        """---
name: doc-team
objective: Compile role docs.
provider: codex
model: gpt-5.5
---

Document-driven team.
""",
        encoding="utf-8",
    )
    (team / "profiles" / "codex-default.example.env").write_text(
        "AUTH_MODE=subscription\nPROFILE_NAME=codex-default\n",
        encoding="utf-8",
    )
    (team / "agents" / "implementer.md").write_text(
        """---
name: implementer
role: Implementation Engineer
provider: codex
model: gpt-5.5
auth_mode: subscription
profile: codex-default
tools:
  - fs_read
  - fs_write
  - execute_bash
  - mcp_team
---

Implement bounded tasks and report result_envelope_v1.
""",
        encoding="utf-8",
    )
    return team


def _provider_agent(provider: str, agent_id: str) -> dict:
    return {
        "id": agent_id,
        "role": "reviewer",
        "provider": provider,
        "model": None,
        "working_directory": ".",
        "system_prompt": {"inline": "Review the task and report a result envelope.", "file": None},
        "tools": ["fs_read", "git_diff", "mcp_team", "provider_builtin"],
        "permission_mode": "restricted",
        "preferred_for": ["review"],
        "avoid_for": [],
        "output_contract": {"format": "result_envelope_v1", "required_fields": []},
    }


def _write_codex_rollout(root: Path, session_id: str, cwd: Path, timestamp: datetime) -> Path:
    day = root / f"{timestamp:%Y}" / f"{timestamp:%m}" / f"{timestamp:%d}"
    day.mkdir(parents=True, exist_ok=True)
    path = day / f"rollout-{timestamp:%Y-%m-%dT%H-%M-%S}-{session_id}.jsonl"
    payload = {
        "session_meta": {
            "payload": {
                "id": session_id,
                "timestamp": timestamp.isoformat().replace("+00:00", "Z"),
                "cwd": str(cwd),
                "originator": "codex-tui",
            }
        }
    }
    path.write_text(json.dumps(payload) + "\n", encoding="utf-8")
    return path


def _write_claude_transcript(root: Path, cwd: Path, session_id: str, content: str) -> Path:
    path = _claude_transcript_path(root, cwd, session_id)
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        {"type": "permission-mode", "permissionMode": "default", "sessionId": session_id},
        {
            "parentUuid": None,
            "isSidechain": False,
            "type": "user",
            "message": {"role": "user", "content": content},
            "uuid": "message-1",
            "timestamp": "2026-05-16T00:00:00+00:00",
            "cwd": str(cwd),
            "sessionId": session_id,
        },
    ]
    path.write_text("".join(json.dumps(line) + "\n" for line in lines), encoding="utf-8")
    return path


def _claude_transcript_path(root: Path, cwd: Path, session_id: str) -> Path:
    try:
        cwd_text = str(cwd.resolve())
    except OSError:
        cwd_text = str(cwd)
    encoded = "".join(ch if (ch.isascii() and (ch.isalnum() or ch in "._-")) else "-" for ch in cwd_text)
    return root / encoded / f"{session_id}.jsonl"


def _events(workspace: Path) -> list[dict]:
    path = workspace / ".team" / "logs" / "events.jsonl"
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def _start_fake_leader_pane(workspace: Path) -> tuple[str, str, Path]:
    session = "team-agent-leader-" + workspace.name[-8:]
    capture_path = workspace / "fake-leader-input.log"
    command = f"cat > {shlex.quote(str(capture_path))}"
    proc = runtime.run_cmd(["tmux", "new-session", "-d", "-s", session, "-n", "leader", command], timeout=5)
    if proc.returncode != 0:
        raise AssertionError(proc.stderr)
    pane_proc = runtime.run_cmd(
        ["tmux", "display-message", "-p", "-t", f"{session}:leader", "-F", "#{pane_id}"],
        timeout=5,
    )
    if pane_proc.returncode != 0:
        runtime.run_cmd(["tmux", "kill-session", "-t", session], timeout=5)
        raise AssertionError(pane_proc.stderr)
    return session, pane_proc.stdout.strip(), capture_path


def _wait_for_file_line(path: Path, needle: str, timeout_sec: float = 5.0) -> str:
    import time

    deadline = time.monotonic() + timeout_sec
    while time.monotonic() < deadline:
        if path.exists():
            for line in path.read_text(encoding="utf-8").splitlines():
                if needle in line:
                    return line
        time.sleep(0.05)
    raise AssertionError(f"{needle!r} not found in {path}")


def _valid_envelope(status: str) -> dict:
    return {
        "schema_version": "result_envelope_v1",
        "task_id": "t1",
        "agent_id": "a1",
        "status": status,
        "summary": status,
        "changes": [],
        "tests": [],
        "risks": [],
        "artifacts": [],
        "next_actions": [],
    }


def _result_envelope(status: str) -> dict:
    return {
        "schema_version": "result_envelope_v1",
        "task_id": "task_impl",
        "agent_id": "fake_impl",
        "status": status,
        "summary": status,
        "changes": [],
        "tests": [],
        "risks": [],
        "artifacts": [],
        "next_actions": [],
    }


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.discover(str(Path(__file__).parent), pattern="test*.py")
    raise SystemExit(0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1)
