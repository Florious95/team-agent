from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path
from typing import Any

from team_agent import compiler, profiles, runtime
from team_agent.errors import TeamAgentError
from team_agent.paths import repo_root, team_workspace
from team_agent.spec import validate_result_envelope

from team_agent.cli.helpers import _provider_args


def cmd_quick_start(args: argparse.Namespace) -> dict[str, Any]:
    result = runtime.quick_start(Path(args.agents_dir), name=args.name, yes=args.yes, fresh=args.fresh, team_id=args.team_id)
    if args.json or not result.get("ok"):
        return result
    return result["summary"]


def cmd_codex(args: argparse.Namespace) -> None:
    runtime.start_leader("codex", _provider_args(args.provider_args), Path.cwd().resolve())


def cmd_claude(args: argparse.Namespace) -> None:
    runtime.start_leader("claude_code", _provider_args(args.provider_args), Path.cwd().resolve())


def cmd_init(args: argparse.Namespace) -> dict[str, Any]:
    paths = runtime.init_workspace(Path(args.workspace).resolve(), force=args.force)
    return {"ok": True, "spec": str(paths["spec"]), "state": str(paths["state"])}


def cmd_validate(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.validate_file(Path(args.spec).resolve())


def cmd_compile(args: argparse.Namespace) -> dict[str, Any]:
    result = compiler.compile_team(Path(args.team).resolve(), Path(args.out).resolve())
    return {"ok": True, "team_dir": result["team_dir"], "out": result["out"], "agents": [a["id"] for a in result["spec"]["agents"]]}


def _profile_scope(args: argparse.Namespace) -> tuple[Path, Path | None]:
    team = getattr(args, "team", None)
    if team:
        team_dir = Path(team).resolve()
        return team_workspace(team_dir), team_dir / "profiles"
    return Path(args.workspace).resolve(), None


def cmd_profile_init(args: argparse.Namespace) -> dict[str, Any]:
    workspace, profiles_dir = _profile_scope(args)
    return profiles.init_profile(workspace, args.name, args.auth_mode, profiles_dir=profiles_dir)


def cmd_profile_doctor(args: argparse.Namespace) -> dict[str, Any]:
    workspace, profiles_dir = _profile_scope(args)
    return profiles.doctor_profile(workspace, args.name, profiles_dir=profiles_dir)


def cmd_profile_show(args: argparse.Namespace) -> dict[str, Any]:
    workspace, profiles_dir = _profile_scope(args)
    return profiles.show_profile(workspace, args.name, profiles_dir=profiles_dir)


def cmd_launch(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.launch(Path(args.spec).resolve(), dry_run=args.dry_run, auto_approve=args.yes)


def cmd_preflight(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.preflight(Path(args.team).resolve())


def cmd_start(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.start(Path(args.team).resolve(), yes=args.yes)


def cmd_wait_ready(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.wait_ready(Path(args.workspace).resolve(), timeout=args.timeout)


def cmd_settle(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.settle(Path(args.workspace).resolve())


def cmd_status(args: argparse.Namespace) -> dict[str, Any]:
    if args.json:
        return runtime.status(Path(args.workspace).resolve(), as_json=True, compact=not args.detail)
    return runtime.format_status(Path(args.workspace).resolve(), args.agent)


def cmd_approvals(args: argparse.Namespace) -> dict[str, Any]:
    if args.json:
        return runtime.approvals(Path(args.workspace).resolve(), agent_id=args.agent)
    return runtime.format_approvals(Path(args.workspace).resolve(), agent_id=args.agent)


def cmd_peek(args: argparse.Namespace) -> dict[str, Any]:
    if not args.allow_raw_screen:
        raise TeamAgentError(
            "raw worker terminal inspection requires explicit user authorization and --allow-raw-screen; "
            "normal operation must use status, approvals, inbox, collect, or event logs"
        )
    result = runtime.peek(
        Path(args.workspace).resolve(),
        args.agent,
        head=args.head,
        tail=args.tail,
        search=args.search,
        context=args.context,
    )
    if args.json:
        return result
    return result["text"]


def cmd_inbox(args: argparse.Namespace) -> dict[str, Any]:
    since = getattr(args, "since", None)
    if args.json:
        return runtime.inbox(Path(args.workspace).resolve(), args.agent, limit=args.limit, since=since)
    return runtime.format_inbox(Path(args.workspace).resolve(), args.agent, limit=args.limit, since=since)


def cmd_sessions(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.sessions(Path(args.workspace).resolve())


def cmd_attach_leader(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.attach_leader(Path(args.workspace).resolve(), pane=args.pane, provider=args.provider)


def cmd_takeover(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.takeover(Path(args.workspace).resolve(), team=args.team, confirm=args.confirm)


def cmd_claim_leader(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.claim_leader(Path(args.workspace).resolve(), team=args.team, confirm=args.confirm)


def cmd_identity(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.leader_identity(Path(args.workspace).resolve(), team=args.team)


def cmd_send(args: argparse.Namespace) -> dict[str, Any]:
    target = _send_target(args)
    return runtime.send_message(
        Path(args.workspace).resolve(),
        target,
        " ".join(args.message),
        task_id=args.task,
        sender=args.sender,
        requires_ack=not args.no_ack,
        confirm_human=args.confirm_human,
        wait_visible=not args.no_wait,
        timeout=args.timeout,
        watch_result=args.watch_result,
        team=args.team,
    )


def _send_target(args: argparse.Namespace) -> str | list[str] | None:
    if getattr(args, "targets", None):
        return [item.strip() for item in args.targets.split(",") if item.strip()]
    return args.target


def cmd_collect(args: argparse.Namespace) -> dict[str, Any]:
    result_file = Path(args.result_file).resolve() if args.result_file else None
    return runtime.collect(Path(args.workspace).resolve(), result_file=result_file)


def cmd_diagnose(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.diagnose(Path(args.workspace).resolve())


def cmd_repair_state(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.repair_state(
        Path(args.workspace).resolve(),
        task_id=args.task,
        assignee=args.assignee,
        status_value=args.status,
        summary=args.summary,
    )


def cmd_validate_result(args: argparse.Namespace) -> dict[str, Any]:
    if args.file:
        raw = Path(args.file).read_text(encoding="utf-8")
    elif args.result:
        raw = args.result
    else:
        raw = sys.stdin.read()
    envelope = json.loads(raw)
    validate_result_envelope(envelope)
    return {"ok": True, "task_id": envelope["task_id"], "agent_id": envelope["agent_id"], "status": envelope["status"]}


def cmd_doctor(args: argparse.Namespace) -> dict[str, Any]:
    spec = Path(args.spec).resolve() if args.spec else None
    return runtime.doctor(spec)


def cmd_shutdown(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.shutdown(Path(args.workspace).resolve(), keep_logs=args.keep_logs, team=args.team)


def cmd_restart(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.restart(Path(args.workspace).resolve(), allow_fresh=args.allow_fresh, team=args.team)


def cmd_start_agent(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.start_agent(
        Path(args.workspace).resolve(),
        args.agent,
        force=args.force,
        open_display=not args.no_display,
        allow_fresh=args.allow_fresh,
        team=args.team,
    )


def cmd_stop_agent(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.stop_agent(Path(args.workspace).resolve(), args.agent, team=args.team)


def cmd_reset_agent(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.reset_agent(
        Path(args.workspace).resolve(),
        args.agent,
        discard_session=args.discard_session,
        open_display=not args.no_display,
        team=args.team,
    )


def cmd_add_agent(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.add_agent(
        Path(args.workspace).resolve(),
        args.agent,
        role_file_path=args.role_file,
        open_display=not args.no_display,
        team=args.team,
    )


def cmd_fork_agent(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.fork_agent(
        Path(args.workspace).resolve(),
        args.source_agent,
        as_agent_id=args.as_agent,
        label=args.label,
        open_display=not args.no_display,
        team=args.team,
    )


def cmd_remove_agent(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.remove_agent(
        Path(args.workspace).resolve(),
        args.agent,
        from_spec=args.from_spec,
        confirm=args.confirm,
        force=args.force,
        team=args.team,
    )


def cmd_stuck_list(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.stuck_list(Path(args.workspace).resolve())


def cmd_stuck_cancel(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.stuck_cancel(
        Path(args.workspace).resolve(),
        args.agent,
        alert_type=args.alert_type,
        suppressed_by="leader",
    )


def cmd_acknowledge_idle(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.acknowledge_idle(Path(args.workspace).resolve(), team=args.team)


def cmd_allow_peer_talk(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.allow_peer_talk(Path(args.workspace).resolve(), args.agent_a, args.agent_b)


def cmd_run_overnight(args: argparse.Namespace) -> dict[str, Any]:
    from team_agent import orchestrator
    workspace = Path(args.workspace).resolve()
    if args.status:
        return orchestrator.plan_status(workspace, plan_id=args.plan_id)
    if args.halt:
        if not args.plan_id:
            raise TeamAgentError("--halt requires --plan-id")
        return orchestrator.halt_plan(workspace, args.plan_id, reason=args.reason)
    if not args.plan:
        raise TeamAgentError("--plan PATH is required unless --status or --halt is used")
    return orchestrator.start_plan(workspace, Path(args.plan).resolve(), start=not args.no_start)


def cmd_advanced(args: argparse.Namespace) -> str:
    return "\n".join(
        [
            "Low-level commands:",
            "  init validate compile profile launch preflight start wait-ready settle",
            "  sessions attach-leader collect diagnose repair-state validate-result",
            "  install-skill e2e",
        ]
    )


def cmd_install_skill(args: argparse.Namespace) -> dict[str, Any]:
    source = repo_root() / "skills" / "team-agent"
    if args.dest and args.target == "all":
        raise TeamAgentError("--dest cannot be combined with --target all")
    if args.dest:
        dest_dir = Path(args.dest).expanduser().resolve()
        return _install_skill_to(source, dest_dir, args.dry_run)
    if args.target == "all":
        results = [
            _install_skill_to(source, _skill_dest_dir("codex"), args.dry_run),
            _install_skill_to(source, _skill_dest_dir("claude"), args.dry_run),
        ]
        return {"ok": all(item["ok"] for item in results), "targets": results}
    return _install_skill_to(source, _skill_dest_dir(args.target), args.dry_run)


def _skill_dest_dir(target: str) -> Path:
    if target == "claude":
        dest_dir = Path.home() / ".claude" / "skills" / "team-agent"
    else:
        dest_dir = Path.home() / ".codex" / "skills" / "team-agent"
    return dest_dir


def _install_skill_to(source: Path, dest_dir: Path, dry_run: bool) -> dict[str, Any]:
    dest = dest_dir / "SKILL.md"
    if dry_run:
        return {"ok": True, "source": str(source / "SKILL.md"), "dest": str(dest), "dry_run": True}
    dest_dir.mkdir(parents=True, exist_ok=True)
    shutil.copytree(source, dest_dir, dirs_exist_ok=True)
    return {"ok": True, "source": str(source / "SKILL.md"), "dest": str(dest)}
