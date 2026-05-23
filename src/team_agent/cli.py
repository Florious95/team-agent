from __future__ import annotations

import argparse
import json
import time
import shutil
import sys
import tempfile
import traceback
from pathlib import Path
from typing import Any

from team_agent import runtime
from team_agent import compiler
from team_agent import profiles
from team_agent.errors import TeamAgentError
from team_agent.paths import repo_root, team_workspace
from team_agent.simple_yaml import dumps
from team_agent.spec import validate_result_envelope


SEND_ORDER_HINT = (
    "options must appear before target/message. Use: "
    "team-agent send --task <task_id> --json \"<message>\" or "
    "team-agent send --no-ack --json <agent_id> \"<message>\""
)


class TeamAgentArgumentParser(argparse.ArgumentParser):
    def error(self, message: str) -> None:
        send_command = "send" in sys.argv[1:]
        if (getattr(self, "send_order_hint", False) or send_command) and "unrecognized arguments" in message:
            message = f"{message}\nHint: {SEND_ORDER_HINT}"
        super().error(message)


def main(argv: list[str] | None = None) -> None:
    raw_argv = list(sys.argv[1:] if argv is None else argv)
    if raw_argv and raw_argv[0] in {"codex", "claude"}:
        _run_leader_passthrough(raw_argv[0], raw_argv[1:])
        return

    parser = TeamAgentArgumentParser(
        prog="team-agent",
        description="TeamSpec Agent Mode CLI",
        epilog="See `team-agent advanced --help` for low-level commands (debugging only).",
    )
    sub = parser.add_subparsers(dest="command", required=True, parser_class=TeamAgentArgumentParser)

    p = sub.add_parser("codex", help="Start a tmux-managed Codex leader in the current directory")
    p.add_argument("provider_args", nargs=argparse.REMAINDER, help="Arguments passed through to codex")
    p.set_defaults(func=cmd_codex)

    p = sub.add_parser("claude", help="Start a tmux-managed Claude leader in the current directory")
    p.add_argument("provider_args", nargs=argparse.REMAINDER, help="Arguments passed through to claude")
    p.set_defaults(func=cmd_claude)

    p = sub.add_parser("quick-start", help="Start a team from a role-doc directory")
    p.add_argument("agents_dir")
    p.add_argument("--name")
    p.add_argument("--team-id", help="Store loose role docs under .team/<team-id> instead of .team/current")
    p.add_argument("--yes", action="store_true")
    p.add_argument("--fresh", action="store_true", help="Start fresh worker sessions even when prior runtime state exists")
    add_json(p)
    p.set_defaults(func=cmd_quick_start)

    p = sub.add_parser("init", help=argparse.SUPPRESS)
    p.add_argument("--workspace", default=".")
    p.add_argument("--force", action="store_true")
    add_json(p)
    p.set_defaults(func=cmd_init)

    p = sub.add_parser("validate", help=argparse.SUPPRESS)
    p.add_argument("spec", nargs="?", default="team.spec.yaml")
    add_json(p)
    p.set_defaults(func=cmd_validate)

    p = sub.add_parser("compile", help=argparse.SUPPRESS)
    p.add_argument("--team", required=True, help="Team doc directory, for example .team/current")
    p.add_argument("--out", default="team.spec.yaml")
    add_json(p)
    p.set_defaults(func=cmd_compile)

    p = sub.add_parser("profile", help=argparse.SUPPRESS)
    profile_sub = p.add_subparsers(dest="profile_command", required=True)
    p_init = profile_sub.add_parser("init", help="Create an example profile template without real secrets")
    p_init.add_argument("name")
    p_init.add_argument("--workspace", default=".")
    p_init.add_argument("--team", help="Team directory whose profiles/ directory should be used")
    p_init.add_argument("--auth-mode", required=True, choices=sorted(profiles.AUTH_MODES))
    add_json(p_init)
    p_init.set_defaults(func=cmd_profile_init)
    p_doctor = profile_sub.add_parser("doctor", help="Check whether a profile exists without printing secrets")
    p_doctor.add_argument("name")
    p_doctor.add_argument("--workspace", default=".")
    p_doctor.add_argument("--team", help="Team directory whose profiles/ directory should be used")
    add_json(p_doctor)
    p_doctor.set_defaults(func=cmd_profile_doctor)
    p_show = profile_sub.add_parser("show", help="Show redacted profile status without printing secrets")
    p_show.add_argument("name")
    p_show.add_argument("--workspace", default=".")
    p_show.add_argument("--team", help="Team directory whose profiles/ directory should be used")
    add_json(p_show)
    p_show.set_defaults(func=cmd_profile_show)

    p = sub.add_parser("launch", help=argparse.SUPPRESS)
    p.add_argument("spec", nargs="?", default="team.spec.yaml")
    p.add_argument("--yes", action="store_true", help="Confirm launch after permission summary review")
    p.add_argument("--dry-run", action="store_true")
    add_json(p)
    p.set_defaults(func=cmd_launch)

    p = sub.add_parser("preflight", help=argparse.SUPPRESS)
    p.add_argument("--team", required=True)
    add_json(p)
    p.set_defaults(func=cmd_preflight)

    p = sub.add_parser("start", help=argparse.SUPPRESS)
    p.add_argument("--team", required=True)
    p.add_argument("--yes", action="store_true")
    add_json(p)
    p.set_defaults(func=cmd_start)

    p = sub.add_parser("wait-ready", help=argparse.SUPPRESS)
    p.add_argument("--workspace", default=".")
    p.add_argument("--timeout", type=int, default=120)
    add_json(p)
    p.set_defaults(func=cmd_wait_ready)

    p = sub.add_parser("settle", help=argparse.SUPPRESS)
    p.add_argument("--workspace", default=".")
    add_json(p)
    p.set_defaults(func=cmd_settle)

    p = sub.add_parser("status", help="Show team runtime status")
    p.add_argument("agent", nargs="?")
    p.add_argument("--workspace", default=".")
    p.add_argument("--detail", action="store_true", help="Include full raw runtime state in --json output")
    add_json(p)
    p.set_defaults(func=cmd_status)

    p = sub.add_parser("approvals", help="Show structured pending worker approval prompts")
    p.add_argument("agent", nargs="?")
    p.add_argument("--workspace", default=".")
    add_json(p)
    p.set_defaults(func=cmd_approvals)

    p = sub.add_parser("peek", help=argparse.SUPPRESS, description="Explicit raw-screen diagnostic only")
    p.add_argument("agent")
    p.add_argument("--workspace", default=".")
    p.add_argument(
        "--allow-raw-screen",
        action="store_true",
        help="Required after explicit user authorization to capture worker terminal output",
    )
    mode = p.add_mutually_exclusive_group(required=True)
    mode.add_argument("--head", type=int, help="Show the first N lines from the bounded recent capture")
    mode.add_argument("--tail", type=int, help="Show the last N lines")
    mode.add_argument("--search", help="Search the bounded recent capture and show matching context only")
    p.add_argument("--context", type=int, default=3, help="Context lines around --search matches, max 10")
    add_json(p)
    p.set_defaults(func=cmd_peek)

    p = sub.add_parser("inbox", help="Show message history for one agent")
    p.add_argument("agent")
    p.add_argument("--workspace", default=".")
    p.add_argument("--limit", type=int, default=20)
    add_json(p)
    p.set_defaults(func=cmd_inbox)

    p = sub.add_parser("sessions", help=argparse.SUPPRESS)
    p.add_argument("--workspace", default=".")
    add_json(p)
    p.set_defaults(func=cmd_sessions)

    p = sub.add_parser("attach-leader", help=argparse.SUPPRESS)
    p.add_argument("--workspace", default=".")
    p.add_argument("--pane", help="Explicit tmux pane id or target, for example %%173")
    p.add_argument("--provider", default="codex")
    add_json(p)
    p.set_defaults(func=cmd_attach_leader)

    p = sub.add_parser(
        "send",
        help="Send a message to an agent, task assignee, or attached leader",
        epilog=(
            "Canonical examples:\n"
            "  team-agent send --task <task_id> --json \"<message>\"\n"
            "  team-agent send --no-ack --json <agent_id> \"<message>\""
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.send_order_hint = True
    p.add_argument("target", nargs="?")
    p.add_argument("message", nargs="+")
    p.add_argument("--workspace", default=".")
    p.add_argument("--task")
    p.add_argument("--from", dest="sender", default="leader")
    p.add_argument("--no-ack", action="store_true")
    p.add_argument("--no-wait", action="store_true", help="Return after injection without visible verification")
    p.add_argument(
        "--watch-result",
        action="store_true",
        help="Return after delivery and let the coordinator collect/report the task result asynchronously",
    )
    p.add_argument("--timeout", type=float, default=30.0)
    p.add_argument("--confirm-human", action="store_true", help="Confirm dispatch for a task marked human_confirmation: true")
    add_json(p)
    p.set_defaults(func=cmd_send)

    p = sub.add_parser("collect", help=argparse.SUPPRESS)
    p.add_argument("--workspace", default=".")
    p.add_argument("--result-file")
    add_json(p)
    p.set_defaults(func=cmd_collect)

    p = sub.add_parser("diagnose", help=argparse.SUPPRESS)
    p.add_argument("--workspace", default=".")
    add_json(p)
    p.set_defaults(func=cmd_diagnose)

    p = sub.add_parser("repair-state", help=argparse.SUPPRESS)
    p.add_argument("--workspace", default=".")
    p.add_argument("--task", required=True)
    p.add_argument("--assignee")
    p.add_argument("--status")
    p.add_argument("--summary")
    add_json(p)
    p.set_defaults(func=cmd_repair_state)

    p = sub.add_parser("validate-result", help=argparse.SUPPRESS)
    p.add_argument("result", nargs="?", help="JSON string. If omitted, read stdin.")
    p.add_argument("--file", help="Read JSON envelope from a file")
    add_json(p)
    p.set_defaults(func=cmd_validate_result)

    p = sub.add_parser("doctor", help="Check local dependencies, providers, auth hints, tmux, and MCP")
    p.add_argument("spec", nargs="?")
    add_json(p)
    p.set_defaults(func=cmd_doctor)

    p = sub.add_parser("shutdown", help="Shutdown team tmux session and keep logs")
    p.add_argument("--workspace", default=".")
    p.add_argument("--keep-logs", action="store_true", default=True)
    add_json(p)
    p.set_defaults(func=cmd_shutdown)

    p = sub.add_parser("restart", help="Restart a stopped team from stored worker sessions")
    p.add_argument("workspace", nargs="?", default=".")
    p.add_argument("--team", help="Restart a specific stored team/session when the workspace has multiple teams")
    p.add_argument("--allow-fresh", action="store_true", help="Allow fresh worker sessions if stored sessions cannot resume")
    add_json(p)
    p.set_defaults(func=cmd_restart)

    p = sub.add_parser("start-agent", help="Start or repair one worker in the current team")
    p.add_argument("agent")
    p.add_argument("--workspace", default=".")
    p.add_argument("--force", action="store_true", help="Replace an existing tmux window for this worker")
    p.add_argument("--allow-fresh", action="store_true", help="Allow a fresh session if the stored session cannot resume")
    p.add_argument("--no-display", action="store_true", help="Do not open a Ghostty display window")
    add_json(p)
    p.set_defaults(func=cmd_start_agent)

    p = sub.add_parser("stop-agent", help="Hard-stop one running worker while preserving its session for start-agent")
    p.add_argument("agent")
    p.add_argument("--workspace", default=".")
    add_json(p)
    p.set_defaults(func=cmd_stop_agent)

    p = sub.add_parser("reset-agent", help="Reset one worker to a fresh session after explicit confirmation")
    p.add_argument("agent")
    p.add_argument("--workspace", default=".")
    p.add_argument("--discard-session", action="store_true", help="Required: discard this worker's prior provider session")
    p.add_argument("--no-display", action="store_true", help="Do not update a Ghostty display window")
    add_json(p)
    p.set_defaults(func=cmd_reset_agent)

    p = sub.add_parser("add-agent", help="Add a first-class worker from an explicit workspace-relative role file")
    p.add_argument("agent")
    p.add_argument("--workspace", default=".")
    p.add_argument("--role-file", required=True, help="Workspace-relative YAML/Markdown agent entry")
    p.add_argument("--no-display", action="store_true", help="Do not open a Ghostty display window")
    add_json(p)
    p.set_defaults(func=cmd_add_agent)

    p = sub.add_parser("fork-agent", help="Fork a running worker using the provider's native branch/fork support")
    p.add_argument("source_agent")
    p.add_argument("--workspace", default=".")
    p.add_argument("--as", dest="as_agent", required=True, help="New worker agent id")
    p.add_argument("--label", help="Optional audit label")
    p.add_argument("--no-display", action="store_true", help="Do not open a Ghostty display window")
    add_json(p)
    p.set_defaults(func=cmd_fork_agent)

    p = sub.add_parser("install-skill", help=argparse.SUPPRESS)
    p.add_argument("--target", choices=["codex", "claude", "all"], default="codex")
    p.add_argument("--dest", help="Explicit destination directory; overrides --target")
    p.add_argument("--dry-run", action="store_true")
    add_json(p)
    p.set_defaults(func=cmd_install_skill)

    p = sub.add_parser("e2e", help=argparse.SUPPRESS)
    p.add_argument("--providers", default="fake")
    p.add_argument("--workspace")
    p.add_argument("--real", action="store_true", help="Launch real provider CLIs; may use authenticated accounts")
    add_json(p)
    p.set_defaults(func=cmd_e2e)

    p = sub.add_parser("allow-peer-talk", help=argparse.SUPPRESS)
    p.add_argument("agent_a")
    p.add_argument("agent_b")
    p.add_argument("--workspace", default=".")
    add_json(p)
    p.set_defaults(func=cmd_allow_peer_talk)

    p = sub.add_parser(
        "advanced",
        help=argparse.SUPPRESS,
        description="Low-level Team Agent commands",
        epilog=(
            "Commands: init validate compile profile launch preflight start wait-ready settle "
            "sessions attach-leader collect diagnose repair-state validate-result install-skill e2e"
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.set_defaults(func=cmd_advanced)

    sub._choices_actions = [  # type: ignore[attr-defined]
        action for action in sub._choices_actions if action.help != argparse.SUPPRESS  # type: ignore[attr-defined]
    ]
    sub.metavar = "{codex,claude,quick-start,send,status,approvals,inbox,shutdown,restart,start-agent,stop-agent,reset-agent,add-agent,fork-agent,doctor}"

    args = parser.parse_args(raw_argv)
    try:
        result = args.func(args)
    except TeamAgentError as exc:
        _emit_cli_error(exc, args)
        raise SystemExit(1)
    except Exception as exc:
        _emit_cli_error(exc, args)
        raise SystemExit(1)
    emit(result, getattr(args, "json", False))
    if isinstance(result, dict) and result.get("ok") is False:
        raise SystemExit(1)


def add_json(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--json", action="store_true", help="Emit stable machine-readable JSON")


def _run_leader_passthrough(command: str, provider_args: list[str]) -> None:
    if provider_args in (["-h"], ["--help"]):
        print(f"usage: team-agent {command} [--attach --confirm | --attach-session SESSION --confirm] [args passed to {command}]")
        print()
        print(f"Start a tmux-managed {command} leader in the current directory.")
        print("Default starts a new independent leader session; explicit attach requires --confirm.")
        print(f"Use `team-agent {command} -- --help` to pass --help to the provider CLI.")
        return
    args = argparse.Namespace(command=command, workspace=".")
    try:
        provider = "codex" if command == "codex" else "claude_code"
        launcher_args = _leader_launcher_args(provider_args)
        runtime.start_leader(
            provider,
            _provider_args(launcher_args["provider_args"]),
            Path.cwd().resolve(),
            attach_existing=launcher_args["attach_existing"],
            confirm_attach=launcher_args["confirm_attach"],
            attach_session=launcher_args["attach_session"],
        )
    except TeamAgentError as exc:
        _emit_cli_error(exc, args)
        raise SystemExit(1)
    except Exception as exc:
        _emit_cli_error(exc, args)
        raise SystemExit(1)


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


def cmd_quick_start(args: argparse.Namespace) -> dict[str, Any]:
    result = runtime.quick_start(Path(args.agents_dir), name=args.name, yes=args.yes, fresh=args.fresh, team_id=args.team_id)
    if args.json or not result.get("ok"):
        return result
    return result["summary"]


def cmd_codex(args: argparse.Namespace) -> None:
    runtime.start_leader("codex", _provider_args(args.provider_args), Path.cwd().resolve())


def cmd_claude(args: argparse.Namespace) -> None:
    runtime.start_leader("claude_code", _provider_args(args.provider_args), Path.cwd().resolve())


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
    if args.json:
        return runtime.inbox(Path(args.workspace).resolve(), args.agent, limit=args.limit)
    return runtime.format_inbox(Path(args.workspace).resolve(), args.agent, limit=args.limit)


def cmd_sessions(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.sessions(Path(args.workspace).resolve())


def cmd_attach_leader(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.attach_leader(Path(args.workspace).resolve(), pane=args.pane, provider=args.provider)


def cmd_send(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.send_message(
        Path(args.workspace).resolve(),
        args.target,
        " ".join(args.message),
        task_id=args.task,
        sender=args.sender,
        requires_ack=not args.no_ack,
        confirm_human=args.confirm_human,
        wait_visible=not args.no_wait,
        timeout=args.timeout,
        watch_result=args.watch_result,
    )


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
    return runtime.shutdown(Path(args.workspace).resolve(), keep_logs=args.keep_logs)


def cmd_restart(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.restart(Path(args.workspace).resolve(), allow_fresh=args.allow_fresh, team=args.team)


def cmd_start_agent(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.start_agent(
        Path(args.workspace).resolve(),
        args.agent,
        force=args.force,
        open_display=not args.no_display,
        allow_fresh=args.allow_fresh,
    )


def cmd_stop_agent(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.stop_agent(Path(args.workspace).resolve(), args.agent)


def cmd_reset_agent(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.reset_agent(
        Path(args.workspace).resolve(),
        args.agent,
        discard_session=args.discard_session,
        open_display=not args.no_display,
    )


def cmd_add_agent(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.add_agent(
        Path(args.workspace).resolve(),
        args.agent,
        role_file_path=args.role_file,
        open_display=not args.no_display,
    )


def cmd_fork_agent(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.fork_agent(
        Path(args.workspace).resolve(),
        args.source_agent,
        as_agent_id=args.as_agent,
        label=args.label,
        open_display=not args.no_display,
    )


def cmd_allow_peer_talk(args: argparse.Namespace) -> dict[str, Any]:
    return runtime.allow_peer_talk(Path(args.workspace).resolve(), args.agent_a, args.agent_b)


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
