from __future__ import annotations

import argparse
import os
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
    runtime.coordinator_pid_path(workspace).write_text(str(os.getpid()), encoding="utf-8")
    runtime.write_coordinator_metadata(workspace, os.getpid(), source="boot")
    event_log = EventLog(workspace)
    event_log.write("coordinator.boot", workspace=str(workspace), once=args.once)
    signal.signal(signal.SIGTERM, _stop)
    signal.signal(signal.SIGINT, _stop)

    interval = args.tick_interval if args.tick_interval is not None else _tick_interval(workspace)
    initial_ppid = os.getppid()
    while not STOP:
        # Stage 14 (Gap 37b) — orphan self-detection. If our original parent (test harness,
        # shell, or supervisor) died, our ppid is reparented to 1 (or to a launchd shim on
        # macOS). When that happens AND the workspace no longer exists on disk, we are an
        # orphan from a torn-down test environment and must self-terminate so we don't
        # accumulate (today's evidence: 35 orphans pointing at /var/folders/...team-agent-
        # watcher-dedupe-* paths long since cleaned up).
        current_ppid = os.getppid()
        if current_ppid != initial_ppid and current_ppid == 1 and not workspace.exists():
            event_log.write(
                "coordinator.orphan_self_terminate",
                initial_ppid=initial_ppid,
                current_ppid=current_ppid,
                workspace=str(workspace),
            )
            break
        result = runtime.coordinator_tick(workspace)
        if result.get("stop") or args.once:
            break
        time.sleep(interval)
    event_log.write("coordinator.exit", stop=STOP)


DEFAULT_TICK_INTERVAL_SEC = 5.0  # Stage 14 (Gap 36c) — bumped from 2.0 (2.5x less CPU)


def _tick_interval(workspace: Path) -> float:
    state = load_runtime_state(workspace)
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    if spec_path.exists():
        try:
            spec = load_spec(spec_path)
            return float(spec.get("runtime", {}).get("tick_interval_sec", DEFAULT_TICK_INTERVAL_SEC))
        except Exception:
            pass
    # Ensure schema exists even before launch; this makes doctor/tick diagnostics deterministic.
    MessageStore(workspace)
    return DEFAULT_TICK_INTERVAL_SEC


if __name__ == "__main__":
    main(sys.argv[1:])
