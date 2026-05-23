from __future__ import annotations

import argparse
import signal
import sys
import time
from pathlib import Path

from team_agent import runtime
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.spec import load_spec
from team_agent.state import load_runtime_state


STOP = False


def _stop(_signum, _frame) -> None:
    global STOP
    STOP = True


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description="Team Agent per-workspace coordinator daemon")
    parser.add_argument("--workspace", required=True)
    parser.add_argument("--once", action="store_true")
    parser.add_argument("--tick-interval", type=float)
    args = parser.parse_args(argv)
    workspace = Path(args.workspace).resolve()
    runtime.ensure_workspace_dirs(workspace)
    runtime.coordinator_pid_path(workspace).write_text(str(__import__("os").getpid()), encoding="utf-8")
    runtime.write_coordinator_metadata(workspace, __import__("os").getpid(), source="boot")
    event_log = EventLog(workspace)
    event_log.write("coordinator.boot", workspace=str(workspace), once=args.once)
    signal.signal(signal.SIGTERM, _stop)
    signal.signal(signal.SIGINT, _stop)

    interval = args.tick_interval if args.tick_interval is not None else _tick_interval(workspace)
    while not STOP:
        result = runtime.coordinator_tick(workspace)
        if result.get("stop") or args.once:
            break
        time.sleep(interval)
    event_log.write("coordinator.exit", stop=STOP)


def _tick_interval(workspace: Path) -> float:
    state = load_runtime_state(workspace)
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    if spec_path.exists():
        try:
            spec = load_spec(spec_path)
            return float(spec.get("runtime", {}).get("tick_interval_sec", 2))
        except Exception:
            pass
    # Ensure schema exists even before launch; this makes doctor/tick diagnostics deterministic.
    MessageStore(workspace)
    return 2.0


if __name__ == "__main__":
    main(sys.argv[1:])
