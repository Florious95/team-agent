from __future__ import annotations

import argparse
import sys
from pathlib import Path

from team_agent import profiles
from team_agent import runtime
from team_agent.errors import TeamAgentError

from team_agent.cli.commands import (
    cmd_quick_start,
    cmd_codex,
    cmd_claude,
    cmd_init,
    cmd_validate,
    cmd_compile,
    cmd_profile_init,
    cmd_profile_doctor,
    cmd_profile_show,
    cmd_launch,
    cmd_preflight,
    cmd_start,
    cmd_wait_ready,
    cmd_settle,
    cmd_status,
    cmd_approvals,
    cmd_peek,
    cmd_inbox,
    cmd_sessions,
    cmd_attach_leader,
    cmd_send,
    cmd_collect,
    cmd_diagnose,
    cmd_repair_state,
    cmd_validate_result,
    cmd_doctor,
    cmd_shutdown,
    cmd_restart,
    cmd_start_agent,
    cmd_stop_agent,
    cmd_reset_agent,
    cmd_add_agent,
    cmd_fork_agent,
    cmd_remove_agent,
    cmd_stuck_list,
    cmd_stuck_cancel,
    cmd_allow_peer_talk,
    cmd_advanced,
    cmd_install_skill,

)
from team_agent.cli.e2e import cmd_e2e
from team_agent.cli.helpers import (
    _cli_error_payload,
    _emit_cli_error,
    _leader_launcher_args,
    _provider_args,
    emit,
)


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
    p.add_argument("--team", help="Explicit team/session target when a workspace has multiple teams")
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

    p = sub.add_parser("remove-agent", help="Remove one worker from runtime state and team spec")
    p.add_argument("agent")
    p.add_argument("--workspace", default=".")
    p.add_argument("--from-spec", action="store_true", help="Allow removing a spec-native worker")
    p.add_argument("--confirm", action="store_true", help="Required with --from-spec")
    p.add_argument("--force", action="store_true", help="Stop a running worker before removing it")
    add_json(p)
    p.set_defaults(func=cmd_remove_agent)

    p = sub.add_parser("stuck-list", help="List manually suppressed idle-triggered alerts")
    p.add_argument("--workspace", default=".")
    add_json(p)
    p.set_defaults(func=cmd_stuck_list)

    p = sub.add_parser("stuck-cancel", help="Suppress repeated stuck/idle alerts for one agent")
    p.add_argument("agent")
    p.add_argument("--workspace", default=".")
    p.add_argument("--alert-type", choices=["stuck", "idle_fallback", "all"], default="stuck")
    add_json(p)
    p.set_defaults(func=cmd_stuck_cancel)

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
    sub.metavar = "{codex,claude,quick-start,send,status,approvals,inbox,shutdown,restart,start-agent,stop-agent,reset-agent,add-agent,fork-agent,remove-agent,stuck-list,stuck-cancel,doctor}"

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
