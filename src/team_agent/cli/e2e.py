from __future__ import annotations

import argparse
import tempfile
from pathlib import Path
from typing import Any

from team_agent import runtime
from team_agent.simple_yaml import dumps


def cmd_e2e(args: argparse.Namespace) -> dict[str, Any]:
    providers = [item.strip() for item in args.providers.split(",") if item.strip()]
    workspace = Path(args.workspace).resolve() if args.workspace else Path(tempfile.mkdtemp(prefix="team-agent-e2e-"))
    workspace.mkdir(parents=True, exist_ok=True)
    results: dict[str, Any] = {"workspace": str(workspace), "providers": {}, "ok": True}
    if "fake" in providers:
        spec_path = workspace / "team.spec.yaml"
        spec_path.write_text(dumps(_fake_spec(workspace)), encoding="utf-8")
        results["providers"]["fake"] = _run_fake_e2e(spec_path, workspace)
        results["ok"] = results["ok"] and results["providers"]["fake"]["ok"]
    for provider in [p for p in providers if p != "fake"]:
        from team_agent.providers import get_adapter

        adapter = get_adapter(provider)
        installed = adapter.is_installed()
        if not installed:
            provider_result = {
                "ok": False,
                "skipped": True,
                "reason": f"{adapter.command_name} not installed",
                "version": None,
            }
        elif not args.real:
            provider_result = {
                "ok": False,
                "skipped": True,
                "reason": "real provider launch disabled; rerun with --real on an authenticated machine",
                "version": adapter.version(),
            }
        else:
            provider_result = _run_real_launch_smoke(provider, workspace)
        results["providers"][provider] = provider_result
        results["ok"] = results["ok"] and provider_result["ok"]
    return results


def _run_fake_e2e(spec_path: Path, workspace: Path) -> dict[str, Any]:
    launched = runtime.launch(spec_path, auto_approve=True)
    sent = runtime.send_message(workspace, None, "implement fake task", task_id="task_impl", requires_ack=True)
    import time

    time.sleep(1.0)
    collected = runtime.collect(workspace)
    stopped = runtime.shutdown(workspace)
    return {"ok": bool(launched["ok"] and sent["ok"] and collected["collected"] and stopped["ok"]), "launch": launched, "send": sent, "collect": collected, "shutdown": stopped}


def _run_real_launch_smoke(provider: str, workspace: Path) -> dict[str, Any]:
    spec_path = workspace / f"team.{provider}.spec.yaml"
    spec = _fake_spec(workspace)
    spec["team"]["name"] = f"real-{provider}-smoke"
    spec["leader"]["provider"] = provider
    spec["agents"][0]["provider"] = provider
    spec["agents"][0]["id"] = f"{provider}_smoke"
    spec["agents"][0]["tools"] = ["fs_read", "fs_list", "git_diff", "mcp_team", "provider_builtin"]
    spec["agents"][0]["role"] = "reviewer"
    spec["agents"][0]["system_prompt"]["inline"] = (
        "Real provider smoke. Do not edit files or run shell. "
        "Do not call team-agent launch and do not create a nested Team Agent team. "
        "When asked, call team_orchestrator.report_result exactly once with result_envelope_v1."
    )
    spec["routing"]["rules"][0]["assign_to"] = spec["agents"][0]["id"]
    spec["runtime"]["session_name"] = f"team-agent-real-{provider}"
    spec["runtime"]["startup_order"] = [spec["agents"][0]["id"]]
    spec["tasks"][0]["id"] = f"task_real_{provider}_callback"
    spec["tasks"][0]["title"] = f"Real {provider} callback smoke"
    spec["tasks"][0]["assignee"] = spec["agents"][0]["id"]
    spec["tasks"][0]["requires_tools"] = ["fs_read", "git_diff"]
    spec["tasks"][0]["type"] = "review"
    spec_path.write_text(dumps(spec), encoding="utf-8")
    launched = runtime.launch(spec_path, auto_approve=True)
    import time

    time.sleep(10.0 if provider == "codex" else 3.0)
    collected = None
    sent = None
    if provider == "codex":
        task_id = spec["tasks"][0]["id"]
        agent_id = spec["agents"][0]["id"]
        message = (
            "Do not call team-agent launch and do not create a nested Team Agent team. "
            "Do not edit files or run shell. "
            "Call team_orchestrator.report_result with envelope "
            f'{{"schema_version":"result_envelope_v1","task_id":"{task_id}",'
            f'"agent_id":"{agent_id}","status":"success","summary":"ok",'
            '"changes":[],"tests":[{"command":"real-codex-callback-smoke","status":"passed"}],'
            '"risks":[],"artifacts":[],"next_actions":[]}. Do not edit files or run shell.'
        )
        sent = runtime.send_message(workspace, agent_id, message, task_id=task_id, requires_ack=True)
        for _ in range(24):
            time.sleep(5.0)
            result = runtime.collect(workspace)
            if result["collected"]:
                collected = result
                break
    status = runtime.status(workspace, as_json=True)
    stopped = runtime.shutdown(workspace)
    agent_id = spec["agents"][0]["id"]
    agent_status = status["agents"].get(agent_id, {})
    callback_ok = provider != "codex" or bool(collected and collected["collected"])
    return {
        "ok": bool(launched["ok"] and stopped["ok"] and agent_status.get("tmux_window_present") and callback_ok),
        "launch": launched,
        "send": sent,
        "collect": collected,
        "status": status,
        "shutdown": stopped,
    }


def _fake_spec(workspace: Path) -> dict[str, Any]:
    return {
        "version": 1,
        "team": {
            "name": "fake-e2e",
            "mode": "supervisor_worker",
            "objective": "Exercise fake provider orchestration.",
            "workspace": str(workspace),
        },
        "leader": {
            "id": "leader",
            "role": "leader",
            "provider": "fake",
            "model": None,
            "tools": ["fs_read", "fs_list", "mcp_team"],
            "context_policy": {
                "keep_user_thread": True,
                "receive_worker_outputs": "structured_only",
                "max_worker_result_tokens": 2000,
            },
        },
        "agents": [
            {
                "id": "fake_impl",
                "role": "implementation_engineer",
                "provider": "fake",
                "model": None,
                "working_directory": str(workspace),
                "system_prompt": {"inline": "Handle fake implementation tasks.", "file": None},
                "tools": ["fs_read", "fs_write", "fs_list", "execute_bash", "git_diff", "mcp_team", "provider_builtin"],
                "permission_mode": "restricted",
                "preferred_for": ["implementation"],
                "avoid_for": [],
                "output_contract": {"format": "result_envelope_v1", "required_fields": ["task_id", "status", "summary", "artifacts"]},
            }
        ],
        "routing": {
            "default_assignee": "leader",
            "rules": [{"id": "implementation-to-fake", "match": {"type": ["implementation"]}, "assign_to": "fake_impl", "priority": 10}],
        },
        "communication": {
            "protocol": "mcp_inbox",
            "topology": "leader_centered",
            "worker_to_worker": True,
            "ack_timeout_sec": 2,
            "result_format": "result_envelope_v1",
            "message_store": {"sqlite": ".team/runtime/team.db", "mirror_files": ".team/messages"},
        },
        "runtime": {
            "backend": "tmux",
            "display_backend": "none",
            "session_name": "team-agent-fake-e2e",
            "auto_launch": True,
            "require_user_approval_before_launch": False,
            "max_active_agents": 1,
            "startup_order": ["fake_impl"],
        },
        "context": {
            "state_file": "team_state.md",
            "artifact_dir": ".team/artifacts",
            "log_dir": ".team/logs",
            "summarization": {
                "worker_full_logs": "retain_outside_leader_context",
                "state_update": "after_each_result",
            },
        },
        "tasks": [
            {
                "id": "task_impl",
                "title": "Fake implementation",
                "type": "implementation",
                "assignee": None,
                "deps": [],
                "acceptance": ["fake result collected"],
                "status": "pending",
                "requires_tools": ["fs_write", "execute_bash"],
                "files": ["src/example.py"],
                "risk": "low",
            }
        ],
    }
